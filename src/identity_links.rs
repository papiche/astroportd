//! Annuaire des contacts et rapprochement des identités : photo ↔ fiche de contact ↔ MULTIPASS.
//!
//! ```text
//! visage nommé « Léa »  ────►  fiche vCard « Léa Martin »  ────►  clé Nostr du MULTIPASS de Léa
//!  (FaceCloud, faces)           (/Contacts, CardDAV)               (X-NOSTR-PUBKEY, ou email hébergé ici)
//! ```
//!
//! Les fiches sont des fichiers chiffrés du uCloud (`/Contacts/*.vcf`) : ce module n'en garde qu'un **index**
//! (`identity_links`, reconstructible) et fait le lien avec FaceCloud :
//!
//! - une fiche qui porte une clé Nostr (`X-NOSTR-PUBKEY` / `X-NOSTR-NPUB`) ou l'email d'un MULTIPASS **hébergé ici**
//!   donne sa clé aux visages du même nom (comparaison sans accents ni casse) ; un nom ambigu n'est jamais rapproché ;
//! - une personne reliée voit ses photos déjà analysées partagées tout de suite si l'amitié est réciproque (N¹) ;
//! - une fiche se crée depuis une personne reconnue, et se relie à la main (email d'un MULTIPASS hébergé, `npub`).
//!
//! Un email déclaré dans un DID distant n'est **pas** une preuve (n'importe qui peut l'écrire) : seul l'email d'un
//! compte hébergé par cette station relie une fiche à un MULTIPASS sans clé explicite.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use astro_core::{ActivityBus, Audience};
use astro_dav::{BlobStore, DavState, carddav, ingest, vcard};
use astro_db::Db;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Partage des photos d'une personne qui vient d'être reliée : `(titulaire, clé de l'ami)`.
pub type ShareFn = Arc<dyn Fn(String, String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Échec d'une opération sur l'annuaire.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkError {
    /// Saisie refusée (message pour l'utilisateur).
    Invalid(String),
    /// Fiche ou compte introuvable.
    NotFound(String),
    /// La fiche existe déjà.
    Conflict(String),
    /// Panne de la station (détail pour le journal).
    Internal(String),
}

fn internal(e: impl std::fmt::Display) -> LinkError {
    LinkError::Internal(e.to_string())
}

/// Bilan d'un balayage.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Fiches (re)lues.
    pub indexed: usize,
    /// Fiches disparues de l'index.
    pub removed: usize,
    /// Personnes reliées à un MULTIPASS : `(nom, clé)`.
    pub linked: Vec<(String, String)>,
}

/// Annuaire d'une station.
pub struct IdentityLinks<B: BlobStore> {
    db: Arc<Db>,
    dav: Arc<DavState<B>>,
    share: Option<ShareFn>,
    activity: Option<ActivityBus>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Visage non nommé : `Inconnu_<hash8>`.
const UNKNOWN: &str = "Inconnu\\_%";

/// Identifiant de fiche unique : jamais deux fois le même pour un compte (empreinte du compte, du nom et de l'instant).
fn new_uid(owner: &str, name: &str) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut h = Sha256::new();
    h.update(owner.as_bytes());
    h.update(name.as_bytes());
    h.update(nanos.to_le_bytes());
    h.update(N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_le_bytes());
    let d = hex::encode(&h.finalize()[..16]);
    format!("{}-{}-{}-{}-{}", &d[..8], &d[8..12], &d[12..16], &d[16..20], &d[20..32])
}

impl<B: BlobStore> IdentityLinks<B> {
    /// `share` : partage des photos d'une personne qui vient d'être reliée (FaceCloud).
    pub fn new(db: Arc<Db>, dav: Arc<DavState<B>>, share: Option<ShareFn>, activity: Option<ActivityBus>) -> Self {
        Self { db, dav, share, activity }
    }

    /// Clé Nostr d'un MULTIPASS hébergé ici qui porte cette adresse.
    async fn hosted_by_email(&self, email: &str) -> Option<String> {
        let e = email.to_ascii_lowercase();
        self.db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT hex FROM identities WHERE hosted = 1 AND lower(email) = ?1 AND role IN ('multipass','zencard','captain') LIMIT 1", [e], |r| r.get::<_, String>(0)).optional()?))
            .await
            .ok()
            .flatten()
    }

    /// Clé rattachée à une fiche : celle que la fiche déclare, sinon l'email d'un compte hébergé ici.
    async fn resolve(&self, owner: &str, card: &vcard::Card) -> (Option<String>, Option<&'static str>) {
        if let Some(k) = card.nostr.as_ref().filter(|k| k.as_str() != owner) {
            return (Some(k.clone()), Some("vcard"));
        }
        for email in &card.emails {
            if let Some(hex) = self.hosted_by_email(email).await.filter(|h| h != owner) {
                return (Some(hex), Some("email"));
            }
        }
        (None, None)
    }

    async fn index_one(&self, owner: &str, entry: &astro_dav::vfs::Entry) -> Result<(), LinkError> {
        let cid = entry.cid.clone().unwrap_or_default();
        let bytes = carddav::read_file(&self.dav, owner, &entry.path).await;
        let stem = astro_dav::vfs::name_of(&entry.path).trim_end_matches(".vcf").to_owned();
        let parsed = match bytes.as_deref().map(vcard::parse) {
            Some(Ok(card)) => Ok(card),
            Some(Err(e)) => Err(e.to_string()),
            None => Err("fiche illisible (déchiffrement impossible)".to_owned()),
        };
        let (name, key, emails, uid, pubkey, via, g1, warning) = match &parsed {
            Ok(card) => {
                let (pubkey, via) = self.resolve(owner, card).await;
                (card.name.clone(), vcard::name_key(&card.name), card.emails.clone(), card.uid.clone(), pubkey, via, card.g1.clone(), card.nostr_invalid.clone())
            }
            // Fiche invalide : indexée avec son avertissement, jamais rapprochée d'un visage (clé de nom vide).
            Err(why) => (stem, String::new(), Vec::new(), None, None, None, None, Some(why.clone())),
        };
        let (o, p) = (owner.to_owned(), entry.path.clone());
        self.db
            .writer()
            .execute(
                "INSERT INTO identity_links(owner_hex, path, uid, name, name_key, emails, pubkey, via, g1pub, warning, cid, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
                 ON CONFLICT(owner_hex, path) DO UPDATE SET uid=excluded.uid, name=excluded.name, name_key=excluded.name_key, emails=excluded.emails, pubkey=excluded.pubkey, via=excluded.via, g1pub=excluded.g1pub, warning=excluded.warning, cid=excluded.cid, updated_at=excluded.updated_at",
                vec![o.into(), p.into(), uid.into(), name.into(), key.into(), json!(emails).to_string().into(), pubkey.into(), via.map(str::to_owned).into(), g1.into(), warning.into(), cid.into(), now().into()],
            )
            .await
            .map_err(internal)?;
        Ok(())
    }

    /// Relit les fiches modifiées de `/Contacts`, retire celles qui ont disparu, puis rapproche les visages.
    pub async fn sync_owner(&self, owner: &str) -> Result<SyncReport, LinkError> {
        let entries = carddav::list_cards(&self.dav, owner).await;
        let o = owner.to_owned();
        let known: HashMap<String, String> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT path, cid FROM identity_links WHERE owner_hex = ?1")?;
                let rows = stmt.query_map([o], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
            .map_err(internal)?;
        let mut report = SyncReport::default();
        for e in &entries {
            if known.get(&e.path).map(String::as_str) != e.cid.as_deref() {
                self.index_one(owner, e).await?;
                report.indexed += 1;
            }
        }
        let alive: BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        for gone in known.keys().filter(|p| !alive.contains(p.as_str())) {
            let (o, p) = (owner.to_owned(), gone.clone());
            self.db.writer().execute("DELETE FROM identity_links WHERE owner_hex = ?1 AND path = ?2", vec![o.into(), p.into()]).await.map_err(internal)?;
            report.removed += 1;
        }
        report.linked = self.reconcile(owner).await?;
        Ok(report)
    }

    /// Donne leur clé aux visages nommés comme une fiche reliée à un MULTIPASS, puis partage leurs photos (amis réciproques).
    pub async fn reconcile(&self, owner: &str) -> Result<Vec<(String, String)>, LinkError> {
        let o = owner.to_owned();
        let (contacts, names): (Vec<(String, String)>, Vec<String>) = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT name_key, pubkey FROM identity_links WHERE owner_hex = ?1 AND pubkey IS NOT NULL AND name_key <> ''")?;
                let contacts = stmt.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut stmt = c.prepare(&format!("SELECT DISTINCT name FROM faces WHERE owner_hex = ?1 AND pubkey IS NULL AND ref IS NULL AND name IS NOT NULL AND name NOT LIKE '{UNKNOWN}' ESCAPE '\\'"))?;
                let names = stmt.query_map([&o], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
                Ok((contacts, names))
            })
            .await
            .map_err(internal)?;
        let mut by_name: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (key, pk) in contacts {
            by_name.entry(key).or_default().insert(pk);
        }
        let mut linked = Vec::new();
        for name in names {
            // Deux fiches de même nom pour deux clés différentes : ambigu, on ne devine pas.
            let Some(pk) = by_name.get(&vcard::name_key(&name)).filter(|s| s.len() == 1).and_then(|s| s.iter().next().cloned()) else { continue };
            let (o, n, k) = (owner.to_owned(), name.clone(), pk.clone());
            let changed = self
                .db
                .writer()
                .execute(
                    "UPDATE faces SET pubkey = ?3, suggest_pubkey = NULL, suggest_name = NULL, suggest_score = NULL WHERE owner_hex = ?1 AND name = ?2 AND pubkey IS NULL AND ref IS NULL",
                    vec![o.into(), n.into(), k.into()],
                )
                .await
                .map_err(internal)?
                .changes;
            if changed > 0 {
                linked.push((name, pk));
            }
        }
        for (name, pk) in &linked {
            if let Some(bus) = &self.activity {
                bus.publish_to(Audience::Owner(owner.to_owned()), "facecloud", "contacts", format!("« {name} » reliée à son MULTIPASS par la fiche de contact"), None);
            }
            if let Some(share) = &self.share {
                share(owner.to_owned(), pk.clone()).await;
            }
        }
        Ok(linked)
    }

    /// Amitié réciproque (N¹ dans les deux sens), pour l'affichage.
    async fn reciprocal(&self, owner: &str, friend: &str) -> bool {
        let (o, f) = (owner.to_owned(), friend.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let follows: Option<i64> = c.query_row("SELECT 1 FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", [&o, &f], |r| r.get(0)).optional()?;
                let back: Option<i64> = c.query_row("SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.id WHERE e.kind = 3 AND e.pubkey = ?1 AND t.name = 'p' AND t.value = ?2 LIMIT 1", [&f, &o], |r| r.get(0)).optional()?;
                Ok(follows.is_some() && back.is_some())
            })
            .await
            .unwrap_or(false)
    }

    /// Vue pour l'interface : les fiches et leur état de liaison, les personnes reconnues sans fiche.
    pub async fn list(&self, owner: &str) -> Result<Value, LinkError> {
        let o = owner.to_owned();
        let (rows, faces) = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT path, name, name_key, emails, pubkey, via, g1pub, warning FROM identity_links WHERE owner_hex = ?1 ORDER BY name_key, path")?;
                let rows = stmt.query_map([&o], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, Option<String>>(5)?, r.get::<_, Option<String>>(6)?, r.get::<_, Option<String>>(7)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut stmt = c.prepare(&format!("SELECT name, count(*), count(pubkey) FROM faces WHERE owner_hex = ?1 AND ref IS NULL AND name IS NOT NULL AND name NOT LIKE '{UNKNOWN}' ESCAPE '\\' GROUP BY name"))?;
                let faces = stmt.query_map([&o], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok((rows, faces))
            })
            .await
            .map_err(internal)?;
        let mut faces_by_key: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for (name, n, keyed) in &faces {
            let e = faces_by_key.entry(vcard::name_key(name)).or_default();
            e.0 += n;
            e.1 += keyed;
        }
        let mut contacts = Vec::new();
        let mut with_card: BTreeSet<String> = BTreeSet::new();
        for (path, name, key, emails, pubkey, via, g1, warning) in rows {
            with_card.insert(key.clone());
            let (n, keyed) = faces_by_key.get(&key).copied().unwrap_or((0, 0));
            let (mut station, mut friends) = (Value::Null, false);
            if let Some(pk) = &pubkey {
                let k = pk.clone();
                station = json!(self.db.readers().read(move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1 AND hosted = 1", [k], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten());
                friends = self.reciprocal(owner, pk).await;
            }
            contacts.push(json!({
                "path": path, "name": name, "emails": serde_json::from_str::<Value>(&emails).unwrap_or_default(), "pubkey": pubkey, "via": via, "g1pub": g1, "warning": warning,
                "hosted_here": station, "friends": friends, "faces": n, "faces_linked": keyed,
            }));
        }
        let without: Vec<Value> = faces.iter().filter(|(name, ..)| !with_card.contains(&vcard::name_key(name))).map(|(name, n, _)| json!({ "name": name, "faces": n })).collect();
        Ok(json!({ "contacts": contacts, "people_without_contact": without }))
    }

    /// Crée la fiche d'une personne reconnue (avec sa clé Nostr si elle en a une) et rapproche aussitôt.
    pub async fn create_from_person(self: &Arc<Self>, owner: &str, name: &str, email: Option<&str>) -> Result<Value, LinkError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
            return Err(LinkError::Invalid("nom attendu (80 caractères, sans caractère de contrôle)".into()));
        }
        let email = email.map(str::trim).filter(|e| !e.is_empty());
        if email.is_some_and(|e| !e.contains('@') || e.len() > 120 || e.chars().any(|c| c.is_control() || c.is_whitespace())) {
            return Err(LinkError::Invalid("adresse email invalide".into()));
        }
        let key = vcard::name_key(name);
        let (o, k) = (owner.to_owned(), key.clone());
        let exists: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT path FROM identity_links WHERE owner_hex = ?1 AND name_key = ?2", [o, k], |r| r.get(0)).optional()?)).await.map_err(internal)?;
        if let Some(path) = exists {
            return Err(LinkError::Conflict(format!("une fiche porte déjà ce nom ({path})")));
        }
        let (o, n) = (owner.to_owned(), name.to_owned());
        let pubkey: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT pubkey FROM faces WHERE owner_hex = ?1 AND name = ?2 AND pubkey IS NOT NULL AND ref IS NULL LIMIT 1", [o, n], |r| r.get(0)).optional()?)).await.map_err(internal)?;
        let card = vcard::new_card(&new_uid(owner, name), name, email, pubkey.as_deref(), now());
        let file = ingest::sanitize_name(name).ok_or_else(|| LinkError::Invalid("nom de fiche invalide".into()))?;
        let done = ingest::ingest(&self.dav, owner, carddav::CONTACTS_DIR, &format!("{file}.vcf"), Some("text/vcard"), std::io::Cursor::new(card.into_bytes())).await.map_err(|e| match e {
            ingest::IngestError::TooLarge(m) => LinkError::Invalid(m),
            other => internal(other),
        })?;
        let report = self.sync_owner(owner).await?;
        Ok(json!({ "path": done.path, "linked": report.linked.len() }))
    }

    /// Relie (ou délie, `target = None`) une fiche à un MULTIPASS : l'email d'un compte hébergé ici, une clé `npub…`/hex.
    pub async fn link(self: &Arc<Self>, owner: &str, path: &str, target: Option<&str>) -> Result<Value, LinkError> {
        if !path.starts_with("/Contacts/") {
            return Err(LinkError::Invalid("fiche attendue dans /Contacts".into()));
        }
        let bytes = carddav::read_file(&self.dav, owner, path).await.ok_or_else(|| LinkError::NotFound("fiche introuvable".into()))?;
        let text = String::from_utf8(bytes).map_err(|_| LinkError::Invalid("fiche illisible".into()))?;
        vcard::parse(text.as_bytes()).map_err(|e| LinkError::Invalid(e.to_string()))?;
        let updated = match target.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => {
                let hex = if t.contains('@') {
                    self.hosted_by_email(t).await.ok_or_else(|| LinkError::NotFound("aucun MULTIPASS hébergé ici ne porte cette adresse : demandez sa clé publique (npub…)".into()))?
                } else {
                    vcard::nostr_key(t).ok_or_else(|| LinkError::Invalid("clé attendue : npub1… ou 64 caractères hexadécimaux (ou l'email d'un MULTIPASS de cette station)".into()))?
                };
                if hex == owner {
                    return Err(LinkError::Invalid("c'est votre propre clé".into()));
                }
                let step = vcard::set_property(&text, "X-NOSTR-NPUB", None, now());
                vcard::set_property(&step, "X-NOSTR-PUBKEY", Some(&hex), now())
            }
            None => {
                let step = vcard::set_property(&text, "X-NOSTR-NPUB", None, now());
                vcard::set_property(&step, "X-NOSTR-PUBKEY", None, now())
            }
        };
        let name = astro_dav::vfs::name_of(path).to_owned();
        ingest::replace(&self.dav, owner, carddav::CONTACTS_DIR, &name, Some("text/vcard"), std::io::Cursor::new(updated.into_bytes())).await.map_err(internal)?;
        let report = self.sync_owner(owner).await?;
        Ok(json!({ "path": path, "linked": report.linked.len() }))
    }

    /// Balaye les carnets de tous les comptes qui en ont un (filet de sécurité : suppressions, modifications hors hook).
    pub async fn sync_all(&self) -> usize {
        let owners: Vec<String> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT DISTINCT owner_hex FROM ucloud_entries WHERE parent = '/Contacts' UNION SELECT DISTINCT owner_hex FROM identity_links")?;
                Ok(stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut done = 0;
        for o in &owners {
            match self.sync_owner(o).await {
                Ok(_) => done += 1,
                Err(e) => eprintln!("annuaire : balayage de {}… impossible : {e:?}", &o[..8.min(o.len())]),
            }
        }
        done
    }
}

/// Ce que le crochet de dépôt sait demander à l'annuaire, sans connaître le type du magasin de blobs.
pub trait ContactSync: Send + Sync {
    /// Relit le carnet d'un compte.
    fn sync(&self, owner: String) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

impl<B: BlobStore> ContactSync for IdentityLinks<B> {
    fn sync(&self, owner: String) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Err(e) = self.sync_owner(&owner).await {
                eprintln!("annuaire : relecture du carnet de {}… impossible : {e:?}", &owner[..8.min(owner.len())]);
            }
        })
    }
}

/// Annuaire branché après coup (il a besoin de l'état WebDAV, qui a besoin du crochet de dépôt).
pub type HookCell = Arc<std::sync::OnceLock<Arc<dyn ContactSync>>>;

/// Crochet de dépôt : une fiche déposée sous `/Contacts/` (WebDAV, CardDAV, interface) relance la relecture du carnet ;
/// le reste est confié à `next` (FaceCloud met les photos en file d'analyse).
pub fn upload_hook(cell: HookCell, next: Option<astro_dav::server::UploadHook>) -> astro_dav::server::UploadHook {
    Arc::new(move |owner: &str, path: &str, mime: &str| {
        if let Some(next) = &next {
            next(owner, path, mime);
        }
        if path.starts_with("/Contacts/") && path.to_ascii_lowercase().ends_with(".vcf") {
            if let Some(links) = cell.get().cloned() {
                let owner = owner.to_owned();
                tokio::spawn(async move { links.sync(owner).await });
            }
        }
    })
}
