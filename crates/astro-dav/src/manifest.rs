//! Manifeste uCloud continu : le VFS et son trousseau sont ancrés sur Nostr + IPFS, donc rejouables.
//!
//! **Événement** : kind 30078 remplaçable, `d = "ucloud:manifest"`, contenu chiffré en NIP-44 v2 vers soi-même.
//! Son contenu déchiffré est un [`Pointer`] : CID du manifeste, clé UENC du manifeste, compteurs.
//!
//! **Manifeste** : JSON ([`Manifest`]) chiffré en UENC v2 et stocké comme blob (IPFS) ; il contient toutes les
//! entrées du VFS et le trousseau `cid ↔ clé`. Le trousseau n'est volontairement pas inline dans l'événement :
//! à 100 octets par fichier il dépasserait la limite d'événement des relais (64 Kio) au-delà de quelques centaines
//! de fichiers. Sans la clé du manifeste (dans l'événement chiffré) le blob public ne révèle rien.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use astro_crypto::uenc::{self, Cipher};
use astro_crypto::{KeySource, UencKey, nip44};
use astro_db::events::{InsertOutcome, insert_event};
use astro_db::{Db, DbError, Statement};
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use rusqlite::Connection;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

use crate::blob::{BlobError, BlobStore, BlobWriter, is_cid};
use crate::vfs::normalize_path;

/// Valeur du tag `d` de l'événement.
pub const D_TAG: &str = "ucloud:manifest";
/// Version du format.
pub const VERSION: u8 = 1;

/// Erreurs de manifeste.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// La station ne détient pas la clé du compte.
    #[error("clé du compte indisponible")]
    NoKey,
    /// L'événement n'est pas un manifeste.
    #[error("ce n'est pas un événement de manifeste uCloud")]
    NotManifest,
    /// Déchiffrement NIP-44 impossible.
    #[error("déchiffrement NIP-44 : {0}")]
    Nip44(#[from] nip44::Nip44Error),
    /// Contenu incohérent ou malveillant.
    #[error("manifeste invalide : {0}")]
    Invalid(&'static str),
    /// JSON illisible.
    #[error("JSON : {0}")]
    Json(#[from] serde_json::Error),
    /// Blob indisponible.
    #[error("blob : {0}")]
    Blob(#[from] BlobError),
    /// Chiffrement UENC.
    #[error("UENC : {0}")]
    Uenc(#[from] uenc::UencError),
    /// Base de données.
    #[error("base : {0}")]
    Db(#[from] DbError),
    /// Événement non signable.
    #[error("signature : {0}")]
    Sign(#[from] astro_nostr_proto::EventError),
}

/// Contenu (chiffré en NIP-44 vers soi) de l'événement de manifeste.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pointer {
    /// Version du format.
    pub v: u8,
    /// CID du blob de manifeste.
    pub manifest_cid: String,
    /// Clé UENC (64 hex) du blob de manifeste.
    pub manifest_key: String,
    /// Numéro de révision, croissant.
    pub rev: u64,
    /// Nombre d'entrées (dossiers compris).
    pub entries: u64,
    /// Nombre de fichiers.
    pub files: u64,
}

/// Ligne de `ucloud_entries`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Chemin normalisé.
    pub path: String,
    /// Parent.
    pub parent: String,
    /// `file` ou `dir`.
    #[serde(rename = "type")]
    pub kind: String,
    /// CID du blob.
    pub cid: Option<String>,
    /// Type MIME.
    pub mime: Option<String>,
    /// Taille du clair.
    pub size: Option<i64>,
    /// Date de modification.
    pub mtime: Option<i64>,
    /// Date de prise de vue.
    pub taken_at: Option<i64>,
    /// sha256 du fichier d'origine.
    pub sha_orig: Option<String>,
    /// sha256 du clair.
    pub sha_plain: Option<String>,
    /// État FaceID.
    pub faceid_status: Option<String>,
    /// Origine du dépôt.
    pub origin: Option<String>,
    /// dHash.
    pub phash: Option<i64>,
    /// Latitude.
    pub lat: Option<f64>,
    /// Longitude.
    pub lon: Option<f64>,
    /// Description.
    pub desc_text: Option<String>,
    /// JSON complet de l'entrée.
    pub data: String,
}

/// Ligne de `ucloud_keyring`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyringEntry {
    /// CID du blob.
    pub cid: String,
    /// Clé de fichier (64 hex).
    pub key_hex: String,
    /// Algorithme.
    pub enc: String,
}

/// VFS complet d'un compte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Version du format.
    pub v: u8,
    /// Propriétaire (64 hex).
    pub owner: String,
    /// Révision.
    pub rev: u64,
    /// Date de création.
    pub created_at: u64,
    /// Entrées.
    pub entries: Vec<ManifestEntry>,
    /// Trousseau.
    pub keyring: Vec<KeyringEntry>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Lit le VFS et le trousseau de `owner` dans un instantané cohérent.
pub fn load_manifest(conn: &Connection, owner: &str, rev: u64, created_at: u64) -> Result<Manifest, DbError> {
    let tx = conn.unchecked_transaction()?;
    let entries = {
        let mut s = tx.prepare(
            "SELECT path, parent, type, cid, mime, size, mtime, taken_at, sha_orig, sha_plain, faceid_status, origin, phash, lat, lon, desc_text, data \
             FROM ucloud_entries WHERE owner_hex = ?1 ORDER BY path",
        )?;
        let rows = s.query_map([owner], |r| {
            Ok(ManifestEntry {
                path: r.get(0)?,
                parent: r.get(1)?,
                kind: r.get(2)?,
                cid: r.get(3)?,
                mime: r.get(4)?,
                size: r.get(5)?,
                mtime: r.get(6)?,
                taken_at: r.get(7)?,
                sha_orig: r.get(8)?,
                sha_plain: r.get(9)?,
                faceid_status: r.get(10)?,
                origin: r.get(11)?,
                phash: r.get(12)?,
                lat: r.get(13)?,
                lon: r.get(14)?,
                desc_text: r.get(15)?,
                data: r.get(16)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let keyring = {
        let mut s = tx.prepare("SELECT cid, key_hex, enc FROM ucloud_keyring WHERE owner_hex = ?1 ORDER BY cid")?;
        let rows = s.query_map([owner], |r| Ok(KeyringEntry { cid: r.get(0)?, key_hex: r.get(1)?, enc: r.get(2)? }))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    Ok(Manifest { v: VERSION, owner: owner.to_owned(), rev, created_at, entries, keyring })
}

/// Publication terminée.
#[derive(Debug, Clone)]
pub struct Published {
    /// Événement signé et stocké localement.
    pub event: Event,
    /// Pointeur qu'il contient.
    pub pointer: Pointer,
}

async fn kv_get(db: &Db, scope: &'static str, key: &str) -> Result<Option<String>, DbError> {
    let key = key.to_owned();
    db.readers()
        .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![scope, key], |r| r.get(0)))?))
        .await
}

async fn kv_set(db: &Db, scope: &'static str, key: &str, value: String) -> Result<(), DbError> {
    db.writer()
        .execute(
            "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            vec![scope.to_owned().into(), key.to_owned().into(), value.into(), i64::try_from(now_secs()).unwrap_or(0).into()],
        )
        .await
        .map(|_| ())
}

/// Construit, chiffre, stocke et ancre le manifeste de `owner`.
///
/// Écrit le blob, signe l'événement 30078 avec la clé du compte et l'insère dans `nostr_events`
/// (le relai local le sert, le crawl N² le réplique). L'horodatage est strictement croissant par compte :
/// à `created_at` égal, un remplaçable garderait l'identifiant le plus petit, pas le plus récent.
pub async fn publish<B: BlobStore>(db: &Db, blobs: &B, keys: &dyn KeySource, owner: &str) -> Result<Published, ManifestError> {
    let secret = keys.secret(owner).ok_or(ManifestError::NoKey)?;
    let rev = kv_get(db, "ucloud_manifest_rev", owner).await?.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) + 1;
    let previous_cid = kv_get(db, "ucloud_manifest_cid", owner).await?;
    let last_ts: Option<i64> = {
        let owner = owner.to_owned();
        db.readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT max(created_at) FROM nostr_events WHERE pubkey = ?1 AND kind = 30078 AND d_tag = ?2",
                    rusqlite::params![owner, D_TAG],
                    |r| r.get(0),
                )?)
            })
            .await?
    };
    let created_at = now_secs().max(last_ts.map_or(0, |t| u64::try_from(t).unwrap_or(0) + 1));

    let manifest = {
        let owner = owner.to_owned();
        db.readers().read(move |c| load_manifest(c, &owner, rev, created_at)).await?
    };
    let (entries, files) = (manifest.entries.len() as u64, manifest.entries.iter().filter(|e| e.kind == "file").count() as u64);
    let json = serde_json::to_vec(&manifest)?;

    let key = UencKey::generate()?;
    let blob = uenc::encrypt_to_vec(&key, Cipher::detect(), &json).await?;
    let mut writer = blobs.begin().await?;
    writer.write_all(&blob).await.map_err(BlobError::from)?;
    let meta = writer.commit().await?;

    let pointer = Pointer { v: VERSION, manifest_cid: meta.cid.clone(), manifest_key: key.to_hex(), rev, entries, files };
    let content = nip44::encrypt_to_self(&secret, owner, &serde_json::to_string(&pointer)?)?;
    let event = UnsignedEvent {
        created_at,
        kind: kind::APP_DATA,
        tags: vec![vec!["d".into(), D_TAG.into()], vec!["rev".into(), rev.to_string()]],
        content,
    }
    .sign(&secret)?;

    let stored = event.clone();
    let outcome = db.writer().transaction(move |tx| insert_event(tx, &stored)).await?;
    if outcome != InsertOutcome::Inserted {
        let _ = blobs.remove(&meta.cid).await;
        return Err(ManifestError::Invalid("événement de manifeste non inséré"));
    }
    kv_set(db, "ucloud_manifest_rev", owner, rev.to_string()).await?;
    kv_set(db, "ucloud_manifest_cid", owner, meta.cid.clone()).await?;
    if let Some(old) = previous_cid.filter(|c| *c != meta.cid) {
        let _ = blobs.remove(&old).await; // l'ancien manifeste n'est plus référencé
    }
    Ok(Published { event, pointer })
}

/// Un événement 30078 est-il un manifeste uCloud ?
pub fn is_manifest_event(e: &Event) -> bool {
    e.kind == kind::APP_DATA && e.tag_value("d") == Some(D_TAG)
}

/// Déchiffre le pointeur d'un événement de manifeste avec la clé de son auteur.
pub fn open_pointer(event: &Event, keys: &dyn KeySource) -> Result<Pointer, ManifestError> {
    if !is_manifest_event(event) {
        return Err(ManifestError::NotManifest);
    }
    let secret = keys.secret(&event.pubkey).ok_or(ManifestError::NoKey)?;
    let plain = nip44::decrypt_from_self(&secret, &event.pubkey, &event.content)?;
    let pointer: Pointer = serde_json::from_str(&plain)?;
    if pointer.v != VERSION {
        return Err(ManifestError::Invalid("version inconnue"));
    }
    if !is_cid(&pointer.manifest_cid) {
        return Err(ManifestError::Invalid("CID de manifeste invalide"));
    }
    Ok(pointer)
}

/// Récupère et déchiffre le manifeste désigné par `pointer`, et vérifie qu'il appartient à `owner`.
pub async fn fetch_manifest<B: BlobStore>(blobs: &B, pointer: &Pointer, owner: &str) -> Result<Manifest, ManifestError> {
    let key = UencKey::from_hex(&pointer.manifest_key)?;
    let (mut reader, _) = blobs.open(&pointer.manifest_cid).await?;
    let mut plain = Vec::new();
    uenc::decrypt_stream(&key, &mut reader, &mut plain).await?;
    let manifest: Manifest = serde_json::from_slice(&plain)?;
    if manifest.v != VERSION {
        return Err(ManifestError::Invalid("version du manifeste inconnue"));
    }
    if manifest.owner != owner {
        return Err(ManifestError::Invalid("manifeste d'un autre compte"));
    }
    Ok(manifest)
}

/// Requêtes qui remplacent le VFS et le trousseau de `manifest.owner` par le contenu du manifeste.
///
/// Les entrées sont revalidées : chemins normalisés, clés de 64 hexadécimaux, pas de dossier avec CID.
pub fn restore_statements(manifest: &Manifest, src_cid: &str) -> Result<Vec<Statement>, ManifestError> {
    let owner = Value::Text(manifest.owner.clone());
    let mut out = vec![
        Statement::new("DELETE FROM ucloud_entries WHERE owner_hex = ?1", vec![owner.clone()]),
        Statement::new("DELETE FROM ucloud_keyring WHERE owner_hex = ?1", vec![owner.clone()]),
    ];
    let mut seen = HashSet::new();
    for e in &manifest.entries {
        if normalize_path(&e.path).map_err(|_| ManifestError::Invalid("chemin invalide"))? != e.path || e.path == "/" {
            return Err(ManifestError::Invalid("chemin non normalisé"));
        }
        if !seen.insert(e.path.as_str()) {
            return Err(ManifestError::Invalid("chemin en double"));
        }
        if !matches!(e.kind.as_str(), "file" | "dir") || (e.kind == "dir" && e.cid.is_some()) {
            return Err(ManifestError::Invalid("type d'entrée invalide"));
        }
        let opt = |v: &Option<String>| v.clone().map_or(Value::Null, Value::Text);
        out.push(Statement::new(
            "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, sha_orig, sha_plain, faceid_status, origin, phash, lat, lon, desc_text, data, src_cid) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            vec![
                owner.clone(),
                Value::Text(e.path.clone()),
                Value::Text(e.parent.clone()),
                Value::Text(e.kind.clone()),
                opt(&e.cid),
                opt(&e.mime),
                e.size.map_or(Value::Null, Value::Integer),
                e.mtime.map_or(Value::Null, Value::Integer),
                e.taken_at.map_or(Value::Null, Value::Integer),
                opt(&e.sha_orig),
                opt(&e.sha_plain),
                opt(&e.faceid_status),
                opt(&e.origin),
                e.phash.map_or(Value::Null, Value::Integer),
                e.lat.map_or(Value::Null, Value::Real),
                e.lon.map_or(Value::Null, Value::Real),
                opt(&e.desc_text),
                Value::Text(e.data.clone()),
                Value::Text(src_cid.to_owned()),
            ],
        ));
    }
    for k in &manifest.keyring {
        if k.key_hex.len() != 64 || !k.key_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ManifestError::Invalid("clé de fichier invalide"));
        }
        out.push(Statement::new(
            "INSERT INTO ucloud_keyring(owner_hex, cid, key_hex, enc, src_cid) VALUES (?1, ?2, ?3, ?4, ?5)",
            vec![owner.clone(), Value::Text(k.cid.clone()), Value::Text(k.key_hex.clone()), Value::Text(k.enc.clone()), Value::Text(src_cid.to_owned())],
        ));
    }
    Ok(out)
}

/// Restaure d'un coup le VFS d'un événement de manifeste : pointeur → blob → requêtes SQL.
pub async fn restore_from_event<B: BlobStore>(db: &Db, blobs: &B, keys: &dyn KeySource, event: &Event) -> Result<Pointer, ManifestError> {
    let pointer = open_pointer(event, keys)?;
    let manifest = fetch_manifest(blobs, &pointer, &event.pubkey).await?;
    db.writer().batch(restore_statements(&manifest, &pointer.manifest_cid)?).await?;
    Ok(pointer)
}

enum Msg {
    Dirty(String),
    Flush(oneshot::Sender<Vec<(String, String)>>),
}

/// Publie le manifeste après chaque modification, en regroupant les rafales (une photothèque de 50 fichiers
/// ne produit qu'une publication).
#[derive(Clone)]
pub struct ManifestPublisher {
    tx: mpsc::UnboundedSender<Msg>,
}

impl ManifestPublisher {
    /// Lance le travailleur. `debounce` : délai de regroupement après la première modification.
    pub fn spawn<B: BlobStore>(db: Arc<Db>, blobs: Arc<B>, keys: Arc<dyn KeySource>, debounce: Duration) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        tokio::spawn(async move {
            let mut pending: HashSet<String> = HashSet::new();
            let mut failures: Vec<(String, String)> = Vec::new();
            loop {
                let msg = if pending.is_empty() {
                    rx.recv().await
                } else {
                    tokio::select! {
                        m = rx.recv() => m,
                        () = tokio::time::sleep(debounce) => {
                            drain(&db, blobs.as_ref(), keys.as_ref(), &mut pending, &mut failures).await;
                            continue;
                        }
                    }
                };
                match msg {
                    Some(Msg::Dirty(owner)) => {
                        pending.insert(owner);
                    }
                    Some(Msg::Flush(reply)) => {
                        drain(&db, blobs.as_ref(), keys.as_ref(), &mut pending, &mut failures).await;
                        let _ = reply.send(std::mem::take(&mut failures));
                    }
                    None => {
                        drain(&db, blobs.as_ref(), keys.as_ref(), &mut pending, &mut failures).await;
                        return;
                    }
                }
            }
        });
        Self { tx }
    }

    /// Signale que le VFS de `owner` a changé.
    pub fn notify(&self, owner: &str) {
        let _ = self.tx.send(Msg::Dirty(owner.to_owned()));
    }

    /// Publie tout de suite les manifestes en attente ; retourne les échecs `(compte, raison)` depuis le dernier appel.
    pub async fn flush(&self) -> Vec<(String, String)> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Msg::Flush(reply)).is_err() {
            return vec![("*".into(), "travailleur arrêté".into())];
        }
        rx.await.unwrap_or_else(|_| vec![("*".into(), "travailleur arrêté".into())])
    }
}

async fn drain<B: BlobStore>(db: &Db, blobs: &B, keys: &dyn KeySource, pending: &mut HashSet<String>, failures: &mut Vec<(String, String)>) {
    for owner in pending.drain() {
        if let Err(e) = publish(db, blobs, keys, &owner).await {
            failures.push((owner, e.to_string()));
        }
    }
}
