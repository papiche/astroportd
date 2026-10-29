//! Invitations : envoyer des photos à quelqu'un qui n'a pas encore de MULTIPASS, par un lien `/i/<jeton>`.
//!
//! 1. L'expéditeur choisit des photos (ou « toutes celles où Léa apparaît ») : [`Invites::create`] retient la liste et rend
//!    un jeton aléatoire (128 bits), seule preuve de possession du lien. Rien ne part, aucune clé ne sort de la station.
//! 2. Le destinataire ouvre la page publique : [`Invites::preview`] dit qui l'invite et combien de photos ; les vignettes
//!    ([`Invites::thumb`]) sont réduites à quelques pixels : de quoi reconnaître une scène, pas un visage.
//! 3. Il donne **son** adresse : [`Invites::claim`] crée son MULTIPASS, le rend proche réciproque de l'expéditeur (double
//!    suivi), range les photos dans `/Partages/De_<expéditeur>/` (même blob, même clé) et relie la fiche de contact.
//!
//! Le lien est à **usage unique** : la réclamation est prise de façon atomique avant toute création, et rendue si la
//! création échoue (le lien reste utilisable). Une adresse qui a déjà un MULTIPASS ici est refusée : lier deux comptes
//! sur la seule foi d'une adresse saisie serait un moyen de s'imposer comme proche de n'importe qui.

use std::io::Cursor;
use std::net::IpAddr;
use std::sync::Arc;

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_crypto::UencKey;
use astro_crypto::uenc;
use astro_dav::adopt::{SharedFile, adopt_shared_file, sender_label};
use astro_dav::blob::{BlobStore, is_cid};
use astro_dav::ManifestPublisher;
use astro_db::Db;
use astro_nostr_proto::{UnsignedEvent, kind};
use astro_sync::Projector;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::identity_links::IdentityLinks;
use crate::register::{PublishFn, RegisterError, Registered, Registrar};
use crate::sharing::{MAX_FILES, resolve_files};

/// Durée de validité par défaut, et maximale.
pub const DEFAULT_DAYS: i64 = 14;
/// Plafond de validité.
pub const MAX_DAYS: i64 = 90;
/// Invitations en attente au plus par compte : un lien oublié ne s'accumule pas indéfiniment.
const MAX_PENDING: i64 = 50;
/// Vignettes montrées au plus sur la page publique.
pub const PREVIEW_THUMBS: usize = 8;
/// Plus grand côté d'une vignette d'invitation (pixels).
const BLUR_EDGE: u32 = 16;
/// Une réclamation sans suite depuis ce délai (arrêt en pleine création) est reprise.
const STALE_CLAIM_SECS: i64 = 600;

/// Échec d'une opération sur les invitations.
#[derive(Debug, PartialEq, Eq)]
pub enum InviteError {
    /// Demande refusée (champ invalide).
    Invalid(String),
    /// Lien ou ressource inconnue.
    NotFound(String),
    /// Lien déjà utilisé (`invite_used`), retiré (`invite_revoked`) ou expiré (`invite_expired`) : le code stable est porté.
    Gone(&'static str),
    /// L'adresse a déjà un MULTIPASS ici (`email_taken`).
    EmailTaken,
    /// Conflit (adresse déjà inscrite).
    Conflict(String),
    /// Trop de demandes depuis cette adresse (minutes à attendre dans le message).
    RateLimited(String),
    /// Inscription fermée ou station pleine.
    Unavailable(String),
    /// Défaillance interne : le détail est pour les journaux.
    Internal(String),
}

impl InviteError {
    /// Code stable, pour que l'interface traduise le message sans l'analyser.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::NotFound(_) => "not_found",
            Self::Gone(code) => code,
            Self::EmailTaken => "email_taken",
            Self::Conflict(_) => "conflict",
            Self::RateLimited(_) => "rate_limited",
            Self::Unavailable(_) => "registration_closed",
            Self::Internal(_) => "internal",
        }
    }
}

/// Message (français) d'un lien qui ne sert plus.
pub fn gone_message(code: &str) -> &'static str {
    match code {
        "invite_revoked" => "ce lien a été retiré par la personne qui vous l'a envoyé",
        "invite_expired" => "ce lien a expiré : demandez-en un nouveau",
        _ => "ce lien a déjà été utilisé",
    }
}

fn internal(e: impl std::fmt::Display) -> InviteError {
    InviteError::Internal(e.to_string())
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Une demande de création.
#[derive(Debug, Default, Deserialize)]
pub struct CreateRequest {
    /// Fichiers ou dossiers du uCloud à envoyer.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Ou : toutes les photos où cette personne (visage nommé) apparaît.
    #[serde(default)]
    pub person: Option<String>,
    /// Nom du destinataire, tel que l'expéditeur le connaît.
    #[serde(default)]
    pub label: Option<String>,
    /// Adresse du destinataire, si l'expéditeur la connaît.
    #[serde(default)]
    pub email: Option<String>,
    /// Fiche `/Contacts/…vcf` à relier au MULTIPASS créé.
    #[serde(default)]
    pub contact_path: Option<String>,
    /// Validité en jours (14 par défaut, 90 au plus).
    #[serde(default)]
    pub days: Option<i64>,
}

/// Un fichier retenu à la création. La clé n'y figure jamais : elle reste dans `ucloud_keyring`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Item {
    path: String,
    cid: String,
    name: String,
    mime: String,
    size: u64,
    sha_plain: String,
}

struct Row {
    owner: String,
    label: String,
    email: Option<String>,
    contact_path: Option<String>,
    /// Fichiers retenus à la création (invitations anciennes, ou chemins disparus) ; voir `person` et `paths`.
    items: Vec<Item>,
    /// Sélecteur : si la personne ou les chemins sont connus, la liste est **recalculée** à chaque lecture et à la réclamation.
    person: Option<String>,
    paths: Vec<String>,
    created_at: i64,
    expires_at: i64,
    opened_at: Option<i64>,
    claimed_at: Option<i64>,
    claimed_by: Option<String>,
    revoked_at: Option<i64>,
}

impl Row {
    /// `pending` (jamais ouvert), `opened`, `claiming` (création en cours), `claimed`, `expired`, `revoked`.
    fn state(&self, at: i64) -> &'static str {
        match (self.revoked_at, self.claimed_by.is_some(), self.claimed_at) {
            (Some(_), ..) => "revoked",
            (_, true, _) => "claimed",
            (_, _, Some(t)) if at - t < STALE_CLAIM_SECS => "claiming",
            _ if self.expires_at <= at => "expired",
            _ if self.opened_at.is_some() => "opened",
            _ => "pending",
        }
    }
}

/// Ce que la réclamation rend à l'API : le compte créé et ce qui lui a été partagé.
pub struct Claimed {
    /// Le MULTIPASS tout juste créé.
    pub registered: Registered,
    /// Nom de l'expéditeur.
    pub from: String,
    /// Fichiers rangés chez le nouveau titulaire.
    pub shared: usize,
    /// Étapes qui n'ont pas abouti (le compte, lui, existe) : dites à l'humain, jamais cachées.
    pub warnings: Vec<String>,
}

/// Ce que rend l'acceptation d'une invitation avec un compte existant.
pub struct Accepted {
    /// Nom de l'expéditeur.
    pub from: String,
    /// Fichiers rangés chez le titulaire.
    pub shared: usize,
    /// Étapes qui n'ont pas abouti (le partage, lui, a eu lieu).
    pub warnings: Vec<String>,
}

/// Le service d'invitations.
pub struct Invites<B: BlobStore> {
    db: Arc<Db>,
    blobs: Arc<B>,
    keys: Arc<dyn KeySource>,
    registrar: Arc<Registrar>,
    links: Option<Arc<IdentityLinks<B>>>,
    publish: PublishFn,
    publisher: Option<ManifestPublisher>,
    activity: Option<ActivityBus>,
}

impl<B: BlobStore> Invites<B> {
    /// `keys` : clés des comptes hébergés (l'expéditeur et le nouveau titulaire s'abonnent l'un à l'autre) ;
    /// `publish` : relais où partent ces abonnements ; `publisher` : prévient la publication du uCloud du destinataire.
    pub fn new(db: Arc<Db>, blobs: Arc<B>, keys: Arc<dyn KeySource>, registrar: Arc<Registrar>, publish: PublishFn) -> Self {
        Self { db, blobs, keys, registrar, links: None, publish, publisher: None, activity: None }
    }

    /// Annuaire : relie la fiche de contact au MULTIPASS créé.
    pub fn with_links(mut self, links: Arc<IdentityLinks<B>>) -> Self {
        self.links = Some(links);
        self
    }

    /// Publication du uCloud du destinataire après l'adoption des photos.
    pub fn with_publisher(mut self, p: ManifestPublisher) -> Self {
        self.publisher = Some(p);
        self
    }

    /// Flux d'activité : l'expéditeur apprend que son invité a rejoint.
    pub fn with_activity(mut self, bus: ActivityBus) -> Self {
        self.activity = Some(bus);
        self
    }

    /// Photos (chemins) où apparaît la personne nommée `name`, les plus récentes d'abord.
    async fn person_paths(&self, owner: &str, name: &str) -> Result<Vec<String>, InviteError> {
        let (o, n) = (owner.to_owned(), name.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT ucloud_path FROM faces WHERE owner_hex = ?1 AND name = ?2 AND ref IS NULL AND ucloud_path IS NOT NULL GROUP BY ucloud_path ORDER BY MAX(ts) DESC LIMIT ?3")?;
                let rows = stmt.query_map(rusqlite::params![o, n, i64::try_from(MAX_FILES).unwrap_or(200)], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(internal)
    }

    /// Fichiers de `owner` désignés par `paths` et par les photos où apparaît `person` (au plus [`MAX_FILES`]).
    async fn resolve_items(&self, owner: &str, paths: &[String], person: Option<&str>) -> Result<Vec<Item>, InviteError> {
        let mut all = paths.to_vec();
        if let Some(person) = person {
            all.extend(self.person_paths(owner, person).await?);
        }
        all.sort();
        all.dedup();
        if all.is_empty() {
            return Ok(Vec::new());
        }
        let mut files = resolve_files(&self.db, owner, &all).await.map_err(internal)?;
        files.dedup_by(|a, b| a.0 == b.0);
        if files.len() > MAX_FILES {
            return Err(InviteError::Invalid(format!("trop de fichiers (maximum {MAX_FILES})")));
        }
        Ok(files.into_iter().map(|(path, f)| Item { path, cid: f.cid, name: f.name, mime: f.mime, size: f.size, sha_plain: f.sha_plain }).collect())
    }

    /// Fichiers à partager **maintenant** : si l'invitation porte un sélecteur (personne, chemins), il est recalculé, de sorte que les
    /// photos traitées entre la création du lien et son acceptation en font partie, et que celles supprimées n'en font plus partie.
    async fn current_items(&self, row: &Row) -> Result<Vec<Item>, InviteError> {
        if row.person.is_some() || !row.paths.is_empty() {
            return self.resolve_items(&row.owner, &row.paths, row.person.as_deref()).await;
        }
        Ok(row.items.clone())
    }

    /// Crée une invitation. Rend `{ token, count, label, expires_at }` ; l'adresse du lien se construit côté appelant.
    pub async fn create(&self, owner: &str, req: CreateRequest) -> Result<Value, InviteError> {
        let label = req.label.as_deref().map(str::trim).unwrap_or_default().to_owned();
        if label.chars().count() > 80 || label.chars().any(char::is_control) {
            return Err(InviteError::Invalid("nom : 80 caractères au plus, sans caractère de contrôle".into()));
        }
        let email = req.email.as_deref().map(str::trim).filter(|e| !e.is_empty()).map(str::to_ascii_lowercase);
        if email.as_deref().is_some_and(|e| !e.contains('@') || e.len() > 120 || e.chars().any(|c| c.is_control() || c.is_whitespace())) {
            return Err(InviteError::Invalid("adresse email invalide".into()));
        }
        if req.contact_path.as_deref().is_some_and(|p| !p.starts_with("/Contacts/")) {
            return Err(InviteError::Invalid("fiche attendue dans /Contacts".into()));
        }
        let days = req.days.unwrap_or(DEFAULT_DAYS);
        if !(1..=MAX_DAYS).contains(&days) {
            return Err(InviteError::Invalid(format!("validité : 1 à {MAX_DAYS} jours")));
        }
        let person = req.person.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned);
        let mut paths = req.paths;
        paths.sort();
        paths.dedup();
        if paths.is_empty() && person.is_none() {
            return Err(InviteError::Invalid("rien à envoyer : choisissez des photos, ou une personne qui en a".into()));
        }
        if paths.len() > MAX_FILES * 4 {
            return Err(InviteError::Invalid("trop de chemins".into()));
        }
        let items = self.resolve_items(owner, &paths, person.as_deref()).await?;
        if items.is_empty() {
            return Err(InviteError::Invalid("aucun fichier à envoyer (chemins introuvables, dossiers vides, ou personne sans photo reconnue)".into()));
        }
        let count = items.len();
        let (at, o) = (now(), owner.to_owned());
        let pending: i64 = self
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM invites WHERE owner_hex = ?1 AND claimed_at IS NULL AND revoked_at IS NULL AND expires_at > ?2", rusqlite::params![o, at], |r| r.get(0))?))
            .await
            .map_err(internal)?;
        if pending >= MAX_PENDING {
            return Err(InviteError::Conflict(format!("{MAX_PENDING} invitations déjà en attente : retirez-en avant d'en créer")));
        }
        let token = new_token()?;
        let expires_at = at + days * 86_400;
        self.db
            .writer()
            .execute(
                "INSERT INTO invites(token, owner_hex, label, email, contact_path, items, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                vec![token.clone().into(), owner.to_owned().into(), label.clone().into(), email.into(), req.contact_path.into(), serde_json::to_string(&json!({ "items": items, "person": person, "paths": paths })).map_err(internal)?.into(), at.into(), expires_at.into()],
            )
            .await
            .map_err(internal)?;
        Ok(json!({ "token": token, "count": count, "label": label, "expires_at": expires_at }))
    }

    /// Invitations de `owner` (les plus récentes d'abord) : jeton, état, dates.
    pub async fn list(&self, owner: &str) -> Result<Value, InviteError> {
        let o = owner.to_owned();
        let rows = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT token FROM invites WHERE owner_hex = ?1 ORDER BY created_at DESC LIMIT 200")?;
                let tokens = stmt.query_map([&o], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok(tokens)
            })
            .await
            .map_err(internal)?;
        let at = now();
        let mut out = Vec::new();
        for token in rows {
            if let Some(r) = self.load(&token).await? {
                out.push(json!({
                    "token": token, "label": r.label, "email": r.email, "contact_path": r.contact_path, "count": r.items.len(), "state": r.state(at),
                    "created_at": r.created_at, "expires_at": r.expires_at, "opened_at": r.opened_at, "claimed_at": r.claimed_by.as_ref().and(r.claimed_at),
                }));
            }
        }
        Ok(json!({ "invites": out }))
    }

    /// Retire une invitation (le lien cesse de fonctionner). Les photos déjà reçues restent à leur nouveau titulaire.
    pub async fn revoke(&self, owner: &str, token: &str) -> Result<Value, InviteError> {
        let (t, o, at) = (token.to_owned(), owner.to_owned(), now());
        let out = self.db.writer().execute("UPDATE invites SET revoked_at = ?3 WHERE token = ?1 AND owner_hex = ?2 AND revoked_at IS NULL AND claimed_by IS NULL", vec![t.into(), o.into(), at.into()]).await.map_err(internal)?;
        if out.changes == 0 {
            return Err(InviteError::NotFound("invitation introuvable, déjà utilisée ou déjà retirée".into()));
        }
        Ok(json!({ "revoked": true }))
    }

    async fn load(&self, token: &str) -> Result<Option<Row>, InviteError> {
        if token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        let t = token.to_owned();
        let raw = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT owner_hex, label, email, contact_path, items, created_at, expires_at, opened_at, claimed_at, claimed_by, revoked_at FROM invites WHERE token = ?1",
                    [t],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, String>(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?)),
                )
                .optional()?)
            })
            .await
            .map_err(internal)?;
        let Some((owner, label, email, contact_path, items, created_at, expires_at, opened_at, claimed_at, claimed_by, revoked_at)) = raw else { return Ok(None) };
        let parsed: Value = serde_json::from_str(&items).map_err(|e| internal(format!("invitation illisible : {e}")))?;
        // Ancien format : un tableau de fichiers. Format courant : `{ items, person, paths }`.
        let (list, person, paths) = match &parsed {
            Value::Array(_) => (parsed.clone(), None, Vec::new()),
            obj => (obj["items"].clone(), obj["person"].as_str().map(str::to_owned), obj["paths"].as_array().map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_owned)).collect()).unwrap_or_default()),
        };
        let items = serde_json::from_value(list).map_err(|e| internal(format!("invitation illisible : {e}")))?;
        Ok(Some(Row { owner, label, email, contact_path, items, person, paths, created_at, expires_at, opened_at, claimed_at, claimed_by, revoked_at }))
    }

    /// Nom sous lequel l'expéditeur se présente : son profil Nostr, sinon la partie locale de son email.
    async fn sender_name(&self, owner: &str) -> String {
        let o = owner.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let name: Option<String> = c.query_row("SELECT json_extract(content, '$.name') FROM nostr_events WHERE pubkey = ?1 AND kind = 0 ORDER BY created_at DESC LIMIT 1", [&o], |r| r.get(0)).optional()?.flatten();
                let email: Option<String> = c.query_row("SELECT email FROM identities WHERE hex = ?1", [&o], |r| r.get(0)).optional()?.flatten();
                Ok(name.filter(|n| !n.trim().is_empty()).or_else(|| email.map(|e| e.split('@').next().unwrap_or_default().to_owned())).filter(|n| !n.is_empty()).unwrap_or_else(|| o.chars().take(8).collect()))
            })
            .await
            .unwrap_or_default()
    }

    /// Ce que voit le porteur du lien : qui invite, combien de photos, jusqu'à quand. Note la première ouverture.
    pub async fn preview(&self, token: &str) -> Result<Value, InviteError> {
        let row = self.load(token).await?.ok_or_else(|| InviteError::NotFound("lien inconnu".into()))?;
        let at = now();
        let state = row.state(at);
        match state {
            "revoked" => return Err(InviteError::Gone("invite_revoked")),
            "claimed" | "claiming" => return Err(InviteError::Gone("invite_used")),
            "expired" => return Err(InviteError::Gone("invite_expired")),
            _ => {}
        }
        if row.opened_at.is_none() {
            let _ = self.db.writer().execute("UPDATE invites SET opened_at = ?2 WHERE token = ?1 AND opened_at IS NULL", vec![token.to_owned().into(), at.into()]).await;
        }
        let items = self.current_items(&row).await?;
        let photos = items.iter().filter(|i| astro_dav::thumb::is_thumbable(&i.mime)).count();
        Ok(json!({
            "from": self.sender_name(&row.owner).await, "label": row.label, "count": items.len(), "photos": photos,
            "thumbs": items.iter().take(PREVIEW_THUMBS).enumerate().filter(|(_, i)| astro_dav::thumb::is_thumbable(&i.mime)).map(|(n, _)| n).collect::<Vec<_>>(),
            "expires_at": row.expires_at, "email_hint": row.email.as_deref().map(mask_email),
        }))
    }

    /// Vignette n° `index` réduite à quelques pixels (PNG) : la page la montre agrandie et floutée.
    pub async fn thumb(&self, token: &str, index: usize) -> Result<Vec<u8>, InviteError> {
        let row = self.load(token).await?.ok_or_else(|| InviteError::NotFound("lien inconnu".into()))?;
        if !matches!(row.state(now()), "pending" | "opened") || index >= PREVIEW_THUMBS {
            return Err(InviteError::NotFound("pas de vignette".into()));
        }
        let items = self.current_items(&row).await?;
        let item = items.get(index).ok_or_else(|| InviteError::NotFound("pas de vignette".into()))?;
        let (o, cid) = (row.owner.clone(), item.cid.clone());
        let found: Option<(String, String)> = self
            .db
            .readers()
            .read(move |c| {
                let t: Option<String> = c.query_row("SELECT json_extract(data, '$.thumb.cid') FROM ucloud_entries WHERE owner_hex = ?1 AND cid = ?2 AND json_extract(data, '$.thumb.cid') IS NOT NULL LIMIT 1", [&o, &cid], |r| r.get(0)).optional()?.flatten();
                let Some(t) = t else { return Ok(None) };
                let key: Option<String> = c.query_row("SELECT key_hex FROM ucloud_keyring WHERE owner_hex = ?1 AND cid = ?2", [&o, &t], |r| r.get(0)).optional()?;
                Ok(key.map(|k| (t, k)))
            })
            .await
            .map_err(internal)?;
        let (thumb_cid, key_hex) = found.ok_or_else(|| InviteError::NotFound("pas de vignette".into()))?;
        if !is_cid(&thumb_cid) {
            return Err(InviteError::NotFound("pas de vignette".into()));
        }
        let key = UencKey::from_hex(&key_hex).map_err(internal)?;
        let (mut reader, _) = self.blobs.open(&thumb_cid).await.map_err(|e| internal(format!("vignette : {e}")))?;
        let mut webp = Vec::new();
        uenc::decrypt_stream(&key, &mut reader, &mut webp).await.map_err(|e| internal(format!("vignette illisible : {e}")))?;
        tokio::task::spawn_blocking(move || blur_png(&webp)).await.map_err(internal)?
    }

    /// Réclame l'invitation : crée le MULTIPASS de `email`, l'abonne à l'expéditeur (et inversement), range les photos.
    pub async fn claim(&self, token: &str, ip: Option<IpAddr>, email: &str, lang: &str) -> Result<Claimed, InviteError> {
        self.claim_as(token, ip, Some(email), None, lang).await
    }

    /// Comme [`Invites::claim`], pour quelqu'un qui préfère ne pas donner son email : la station génère l'adresse
    /// (`<alias>-<6 caractères>@<domaine>` : aucune boîte aux lettres n'est créée par la station).
    pub async fn claim_guest(&self, token: &str, ip: Option<IpAddr>, alias: Option<&str>, lang: &str) -> Result<Claimed, InviteError> {
        self.claim_as(token, ip, None, alias, lang).await
    }

    async fn claim_as(&self, token: &str, ip: Option<IpAddr>, email: Option<&str>, alias: Option<&str>, lang: &str) -> Result<Claimed, InviteError> {
        let row = self.take(token).await?;
        let registered = match match email {
            Some(email) => self.registrar.register(ip, email, lang).await,
            None => self.registrar.register_guest(ip, alias, lang).await,
        } {
            Ok(r) => r,
            Err(e) => {
                // La création a échoué : le lien reste valable, la personne peut corriger son adresse.
                let _ = self.db.writer().execute("UPDATE invites SET claimed_at = NULL WHERE token = ?1 AND claimed_by IS NULL", vec![token.to_owned().into()]).await;
                return Err(match e {
                    RegisterError::Closed | RegisterError::Full => InviteError::Unavailable(e.to_string()),
                    RegisterError::RateLimited(_) => InviteError::RateLimited(e.to_string()),
                    RegisterError::Invalid(m) => InviteError::Invalid(m),
                    RegisterError::Conflict(_) => InviteError::EmailTaken,
                    RegisterError::Internal(m) => internal(m),
                });
            }
        };
        let recipient = registered.account.hex.clone();
        let label = if row.label.is_empty() { registered.account.email.clone() } else { row.label.clone() };
        let (shared, from, warnings) = self.finish(token, &row, &recipient, &label).await;
        Ok(Claimed { registered, from, shared, warnings })
    }

    /// Accepte l'invitation avec un compte **déjà hébergé ici** (`recipient`, authentifié par l'appelant) : pas de formulaire, pas de
    /// nouveau compte. L'identité est prouvée par la session, jamais par une adresse saisie. Même suite que la réclamation : double
    /// suivi, photos rangées, fiche reliée, expéditeur prévenu.
    pub async fn accept(&self, token: &str, recipient: &str) -> Result<Accepted, InviteError> {
        let peek = self.load(token).await?.ok_or_else(|| InviteError::NotFound("lien inconnu".into()))?;
        if peek.owner == recipient {
            return Err(InviteError::Invalid("c'est votre propre invitation".into()));
        }
        let row = self.take(token).await?;
        let r = recipient.to_owned();
        let label: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1", [r], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten();
        let label = if row.label.is_empty() { label.unwrap_or_else(|| recipient.chars().take(8).collect()) } else { row.label.clone() };
        let (shared, from, warnings) = self.finish(token, &row, recipient, &label).await;
        Ok(Accepted { from, shared, warnings })
    }

    /// Prend la réclamation de façon **atomique** (une seule requête gagne, même si deux personnes valident en même temps) et rend
    /// l'invitation ; `Gone` si elle ne sert plus.
    async fn take(&self, token: &str) -> Result<Row, InviteError> {
        let row = self.load(token).await?.ok_or_else(|| InviteError::NotFound("lien inconnu".into()))?;
        let at = now();
        match row.state(at) {
            "revoked" => return Err(InviteError::Gone("invite_revoked")),
            "claimed" | "claiming" => return Err(InviteError::Gone("invite_used")),
            "expired" => return Err(InviteError::Gone("invite_expired")),
            _ => {}
        }
        let taken = self
            .db
            .writer()
            .execute(
                "UPDATE invites SET claimed_at = ?2, claimed_by = NULL WHERE token = ?1 AND revoked_at IS NULL AND expires_at > ?2 AND (claimed_at IS NULL OR (claimed_by IS NULL AND claimed_at < ?3))",
                vec![token.to_owned().into(), at.into(), (at - STALE_CLAIM_SECS).into()],
            )
            .await
            .map_err(internal)?;
        if taken.changes == 0 {
            return Err(InviteError::Gone("invite_used"));
        }
        Ok(row)
    }

    /// Suite commune d'une réclamation et d'une acceptation, une fois le compte du destinataire connu : l'invitation est marquée
    /// utilisée, les deux comptes se suivent, les photos (liste recalculée) sont rangées, la fiche reliée, l'expéditeur prévenu.
    /// Rend `(fichiers rangés, nom de l'expéditeur, étapes en échec)` : le compte existe, rien n'est défait.
    async fn finish(&self, token: &str, row: &Row, recipient: &str, label: &str) -> (usize, String, Vec<String>) {
        let _ = self.db.writer().execute("UPDATE invites SET claimed_by = ?2 WHERE token = ?1", vec![token.to_owned().into(), recipient.to_owned().into()]).await;
        let mut warnings = Vec::new();
        for (who, target) in [(&row.owner, recipient), (&recipient.to_owned(), row.owner.as_str())] {
            if let Err(e) = self.follow(who, target).await {
                warnings.push(format!("abonnement mutuel : {e}"));
            }
        }
        let items = match self.current_items(row).await {
            Ok(i) => i,
            Err(e) => {
                warnings.push(format!("liste des fichiers : {}", e.code()));
                row.items.clone()
            }
        };
        let (shared, skipped) = self.adopt_all(&row.owner, &items, recipient).await;
        if skipped > 0 {
            warnings.push(format!("{skipped} fichier(s) n'ont pas pu être partagés (supprimés depuis l'invitation ?)"));
        }
        if let Some(path) = row.contact_path.as_deref() {
            if let Some(links) = &self.links {
                if let Err(e) = links.link(&row.owner, path, Some(recipient)).await {
                    warnings.push(format!("fiche de contact non reliée : {e:?}"));
                }
            }
        }
        if let Some(bus) = &self.activity {
            bus.publish_to(Audience::Owner(row.owner.clone()), "facecloud", "invite", format!("{label} a rejoint la constellation grâce à votre invitation : {shared} fichier(s) partagé(s)"), None);
        }
        (shared, self.sender_name(&row.owner).await, warnings)
    }

    /// Range chaque fichier de l'invitation chez le destinataire. Rend `(rangés, écartés)`.
    async fn adopt_all(&self, owner: &str, items: &[Item], recipient: &str) -> (usize, usize) {
        let label = {
            let o = owner.to_owned();
            self.db.readers().read(move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1", [&o], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten()
        };
        let label = sender_label(label.as_deref(), owner);
        let (mut done, mut skipped) = (0usize, 0usize);
        for item in items {
            let (o, cid) = (owner.to_owned(), item.cid.clone());
            let key: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT key_hex FROM ucloud_keyring WHERE owner_hex = ?1 AND cid = ?2", [&o, &cid], |r| r.get(0)).optional()?)).await.ok().flatten();
            let Some(key_hex) = key else {
                skipped += 1;
                continue;
            };
            let file = SharedFile { cid: item.cid.clone(), key_hex, mime: item.mime.clone(), size: item.size, sha_plain: item.sha_plain.clone(), name: item.name.clone() };
            match adopt_shared_file(&self.db, recipient, owner, &label, &file, "invite").await {
                Ok(_) => done += 1,
                Err(_) => skipped += 1,
            }
        }
        if done > 0 {
            if let Some(p) = &self.publisher {
                p.notify(recipient);
            }
        }
        (done, skipped)
    }

    /// `who` suit `target` : nouvelle liste de contacts (kind 3) signée avec la clé de `who`, rangée et projetée tout de
    /// suite (le double suivi doit être visible avant l'envoi des photos), puis publiée sur les relais.
    async fn follow(&self, who: &str, target: &str) -> Result<(), String> {
        let secret = self.keys.secret(who).ok_or("clé du compte introuvable sur cette station")?;
        let w = who.to_owned();
        let latest: Option<(String, String, i64)> = self
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT content, tags, created_at FROM nostr_events WHERE pubkey = ?1 AND kind = 3 ORDER BY created_at DESC LIMIT 1", [w], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?))
            .await
            .map_err(|e| e.to_string())?;
        let (content, mut tags, previous) = match latest {
            Some((c, t, at)) => (c, serde_json::from_str::<Vec<Vec<String>>>(&t).map_err(|e| format!("liste de contacts illisible : {e}"))?, at),
            None => (String::new(), Vec::new(), 0),
        };
        if tags.iter().any(|t| t.first().map(String::as_str) == Some("p") && t.get(1).map(String::as_str) == Some(target)) {
            return Ok(());
        }
        tags.push(vec!["p".into(), target.to_owned()]);
        let created_at = u64::try_from(now().max(previous + 1)).unwrap_or(0);
        let event = UnsignedEvent { created_at, kind: kind::CONTACTS, tags, content }.sign(&secret).map_err(|e| e.to_string())?;
        let e2 = event.clone();
        self.db
            .writer()
            .transaction(move |tx| {
                astro_db::events::insert_event(tx, &e2)?;
                astro_db::apply_statements(tx, &Projector::plan(&e2).map_err(|err| astro_db::DbError::Corrupt(err.to_string()))?)?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        let publish = self.publish.clone();
        let bus = self.activity.clone();
        tokio::spawn(async move {
            if let Err(why) = publish(event).await {
                if let Some(bus) = bus {
                    bus.publish_to(Audience::Captain, "alert", "invite", format!("liste de contacts non publiée ({why})"), None);
                }
            }
        });
        Ok(())
    }
}

/// 128 bits aléatoires en hexadécimal.
fn new_token() -> Result<String, InviteError> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| internal(format!("aléa du système indisponible : {e}")))?;
    Ok(hex::encode(b))
}

/// `lea@exemple.org` → `l**@exemple.org`.
fn mask_email(e: &str) -> String {
    match e.split_once('@') {
        Some((local, domain)) => format!("{}{}@{domain}", local.chars().next().map(String::from).unwrap_or_default(), "*".repeat(local.chars().count().saturating_sub(1).min(6))),
        None => "***".into(),
    }
}

/// Décode une vignette (WebP) et la réduit à [`BLUR_EDGE`] pixels : trop peu pour reconnaître un visage.
fn blur_png(webp: &[u8]) -> Result<Vec<u8>, InviteError> {
    let img = image::load_from_memory(webp).map_err(|e| internal(format!("vignette : {e}")))?;
    let small = img.thumbnail(BLUR_EDGE, BLUR_EDGE);
    let mut out = Cursor::new(Vec::new());
    small.write_to(&mut out, image::ImageFormat::Png).map_err(|e| internal(format!("vignette : {e}")))?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_are_masked_and_tokens_are_unguessable_hex() {
        assert_eq!(mask_email("lea@exemple.org"), "l**@exemple.org");
        assert_eq!(mask_email("x@y.fr"), "x@y.fr");
        assert_eq!(mask_email("pas-un-email"), "***");
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn states_follow_the_life_of_a_link() {
        let mut r = Row { owner: String::new(), label: String::new(), email: None, contact_path: None, items: vec![], person: None, paths: vec![], created_at: 0, expires_at: 1000, opened_at: None, claimed_at: None, claimed_by: None, revoked_at: None };
        assert_eq!(r.state(10), "pending");
        r.opened_at = Some(5);
        assert_eq!(r.state(10), "opened");
        assert_eq!(r.state(1000), "expired");
        r.claimed_at = Some(20);
        assert_eq!(r.state(30), "claiming");
        assert_eq!(r.state(20 + STALE_CLAIM_SECS), "opened");
        r.claimed_by = Some("ab".into());
        assert_eq!(r.state(2000), "claimed");
        r.revoked_at = Some(1);
        assert_eq!(r.state(0), "revoked");
    }
}
