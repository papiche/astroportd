//! Reprise d'un dossier d'un **autre serveur WebDAV** (Nextcloud, ownCloud, une autre station) dans le uCloud : mode « pull ».
//!
//! - **Sources** : adresse, identifiant, mot de passe d'application, dossier distant et dossier de destination. Elles sont
//!   rangées dans la base **chiffrées pour leur titulaire seul** (NIP-44 vers soi) : un mot de passe d'application n'est jamais
//!   écrit en clair, ni publié sur Nostr (état local, comme les files de tâches).
//! - **Parcours** : un `PROPFIND` de profondeur 1 par dossier, borné (fichiers, dossiers, profondeur, taille d'une réponse).
//!   Les noms viennent du **dernier segment** de chaque `href`, jamais d'un chemin que le serveur imposerait : un serveur
//!   hostile ne peut ni sortir du dossier demandé ni écrire ailleurs dans le uCloud.
//! - **Reprise** : chaque fichier est téléchargé en flux, chiffré en UENC v2 et rangé par [`astro_dav::ingest`] **tel qu'il est
//!   sur le serveur** (même arborescence sous le dossier de destination, jamais de tri par dates : réorganiser le uCloud comme
//!   un seul disque, alimenté par de nombreuses sources, est le travail d'une fonction de rangement, à part).
//! - **Mémoire** : la table `pull_imported` retient ce qui vient de quelle source (`origin = pull:<id>` sur l'entrée), de sorte
//!   qu'un fichier déplacé par le rangement n'est pas repris en double. Le passage est rejouable : il n'écrase ni ne duplique rien.
//! - **Réseau** : `crate::fetch` (adresses publiques seulement, redirections revérifiées). Les adresses privées ne s'admettent que
//!   pour une source marquée `allow_private`, ce que seule la ligne de commande de l'opérateur sait faire. Les identifiants
//!   ne suivent jamais une redirection vers un autre serveur.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::{KeySource, nip44};
use astro_dav::DavState;
use astro_dav::blob::BlobStore;
use astro_dav::ingest::{ingest, sanitize_name};
use astro_dav::vfs::normalize_path;
use astro_db::Db;
use base64::Engine;
use bytes::Bytes;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::fetch::{FetchError, FetchLimits, request};

/// Portée `kv` des sources (valeur chiffrée, une ligne par titulaire).
pub const KV_SCOPE: &str = "webdav_sources";
/// Fichiers distants au plus par source et par passage.
pub const MAX_FILES: usize = 50_000;
/// Dossiers parcourus au plus.
pub const MAX_DIRS: usize = 40_000;
/// Profondeur d'arborescence au plus.
pub const MAX_DEPTH: usize = 40;
/// Taille d'une réponse `PROPFIND` au plus.
const MAX_LISTING: u64 = 16 << 20;
/// Plafond par fichier par défaut (celui d'Astroport.ONE).
pub const DEFAULT_MAX_FILE: u64 = 200 << 20;
/// Durée totale d'un fichier (téléchargement, chiffrement, rangement).
const FILE_TIMEOUT: Duration = Duration::from_secs(900);
/// Sources par titulaire au plus.
const MAX_SOURCES: usize = 8;

const PATH_SEGMENT: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>').add(b'?').add(b'`').add(b'{').add(b'}').add(b'/').add(b'\\');

/// Échec d'une opération de reprise.
#[derive(Debug, thiserror::Error)]
pub enum PullError {
    /// Saisie refusée (message pour l'utilisateur).
    #[error("{0}")]
    Invalid(String),
    /// Le serveur distant a refusé ou n'a pas répondu comme attendu.
    #[error("serveur distant : {0}")]
    Remote(String),
    /// Réseau ou garde-fou (adresse refusée…).
    #[error("{0}")]
    Fetch(#[from] FetchError),
    /// Base ou clé.
    #[error("{0}")]
    Internal(String),
}

/// Une source WebDAV externe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    /// Identifiant court (16 hex).
    pub id: String,
    /// Libellé donné par le titulaire.
    pub label: String,
    /// Racine WebDAV (`https://nextcloud.example/remote.php/dav/files/fred`).
    pub url: String,
    /// Identifiant de connexion.
    pub username: String,
    /// Mot de passe (d'application de préférence).
    pub password: String,
    /// Dossier distant à reprendre, relatif à la racine (`Photos`, vide pour tout).
    #[serde(default)]
    pub remote_path: String,
    /// Dossier du uCloud où ranger la reprise (`/Photos/Import_Fred`).
    pub dest_prefix: String,
    /// Adresses privées admises (serveur sur le réseau local). Réservé à la ligne de commande de l'opérateur.
    #[serde(default)]
    pub allow_private: bool,
    /// Création (secondes Unix).
    pub created_at: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn basic(user: &str, pass: &str) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}")))
}

/// Les sources d'un titulaire, rangées chiffrées pour lui seul.
pub struct Sources {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
}

impl Sources {
    /// Registre sur `db`, chiffrant avec la clé du titulaire que détient `keys`.
    #[must_use]
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>) -> Self {
        Self { db, keys }
    }

    /// Sources de `owner`.
    ///
    /// # Errors
    /// Base ou clé indisponibles, contenu illisible.
    pub async fn list(&self, owner: &str) -> Result<Vec<Source>, PullError> {
        let o = owner.to_owned();
        let stored: Option<String> = self
            .db
            .readers()
            .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![KV_SCOPE, o], |r| r.get(0)))?))
            .await
            .map_err(|e| PullError::Internal(e.to_string()))?;
        let Some(payload) = stored else { return Ok(Vec::new()) };
        let secret = self.keys.secret(owner).ok_or_else(|| PullError::Internal("clé du compte absente de cette station".into()))?;
        let plain = nip44::decrypt_from_self(&secret, owner, &payload).map_err(|e| PullError::Internal(format!("sources illisibles : {e}")))?;
        serde_json::from_str(&plain).map_err(|e| PullError::Internal(format!("sources illisibles : {e}")))
    }

    async fn save(&self, owner: &str, sources: &[Source]) -> Result<(), PullError> {
        let secret = self.keys.secret(owner).ok_or_else(|| PullError::Internal("clé du compte absente de cette station".into()))?;
        let plain = serde_json::to_string(sources).map_err(|e| PullError::Internal(e.to_string()))?;
        let sealed = nip44::encrypt_to_self(&secret, owner, &plain).map_err(|e| PullError::Internal(e.to_string()))?;
        self.db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![KV_SCOPE.to_owned().into(), owner.to_owned().into(), sealed.into(), i64::try_from(now()).unwrap_or(0).into()],
            )
            .await
            .map(|_| ())
            .map_err(|e| PullError::Internal(e.to_string()))
    }

    /// Ajoute une source après l'avoir vérifiée (adresse, dossiers).
    ///
    /// # Errors
    /// Saisie invalide ou trop de sources.
    pub async fn add(&self, owner: &str, mut src: Source) -> Result<Source, PullError> {
        validate(&mut src)?;
        let mut all = self.list(owner).await?;
        if all.len() >= MAX_SOURCES {
            return Err(PullError::Invalid(format!("{MAX_SOURCES} sources au plus : en retirer une d'abord")));
        }
        let mut raw = [0u8; 8];
        getrandom::fill(&mut raw).map_err(|e| PullError::Internal(format!("aléa du système indisponible : {e}")))?;
        src.id = hex::encode(raw);
        src.created_at = now();
        all.push(src.clone());
        self.save(owner, &all).await?;
        Ok(src)
    }

    /// Retire une source (ce qui a déjà été repris reste dans le uCloud).
    ///
    /// # Errors
    /// Source inconnue.
    pub async fn remove(&self, owner: &str, id: &str) -> Result<(), PullError> {
        let mut all = self.list(owner).await?;
        let before = all.len();
        all.retain(|s| s.id != id);
        if all.len() == before {
            return Err(PullError::Invalid("source inconnue".into()));
        }
        self.save(owner, &all).await
    }
}

fn validate(src: &mut Source) -> Result<(), PullError> {
    let url = url::Url::parse(src.url.trim()).map_err(|e| PullError::Invalid(format!("adresse invalide : {e}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(PullError::Invalid("adresse WebDAV attendue sans identifiants ni paramètres (https://serveur/remote.php/dav/files/identifiant)".into()));
    }
    src.url = url.as_str().trim_end_matches('/').to_owned();
    if src.username.trim().is_empty() || src.username.chars().any(char::is_control) || src.username.contains(':') {
        return Err(PullError::Invalid("identifiant attendu (sans « : »)".into()));
    }
    if src.password.is_empty() || src.password.len() > 512 || src.password.chars().any(char::is_control) {
        return Err(PullError::Invalid("mot de passe attendu".into()));
    }
    src.remote_path = src.remote_path.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/");
    if src.remote_path.split('/').any(|s| s == "." || s == "..") {
        return Err(PullError::Invalid("dossier distant : « . » et « .. » refusés".into()));
    }
    src.dest_prefix = normalize_path(&src.dest_prefix).map_err(|_| PullError::Invalid("dossier de destination invalide".into()))?;
    if src.dest_prefix == "/" || astro_dav::vfs::is_vault(&src.dest_prefix) {
        return Err(PullError::Invalid("dossier de destination : choisir un sous-dossier (pas la racine, ni le coffre)".into()));
    }
    if src.label.trim().is_empty() {
        src.label = src.url.clone();
    }
    src.label = src.label.chars().filter(|c| !c.is_control()).take(80).collect();
    Ok(())
}

/// Un fichier distant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    /// Segments de dossier sous la racine reprise (déjà nettoyés).
    pub dirs: Vec<String>,
    /// Nom du fichier.
    pub name: String,
    /// Taille annoncée.
    pub size: u64,
    /// Type MIME annoncé.
    pub mime: Option<String>,
}

/// Résultat d'un parcours.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Walk {
    /// Fichiers trouvés.
    pub files: Vec<RemoteFile>,
    /// Dossiers parcourus.
    pub dirs: usize,
    /// Le parcours a été borné (trop de fichiers, de dossiers ou profondeur).
    pub capped: bool,
}

fn dir_url(src: &Source, segs: &[String]) -> String {
    let mut out = src.url.clone();
    for part in src.remote_path.split('/').filter(|s| !s.is_empty()).chain(segs.iter().map(String::as_str)) {
        out.push('/');
        out.push_str(&utf8_percent_encode(part, PATH_SEGMENT).to_string());
    }
    out.push('/');
    out
}

#[derive(Default)]
struct Entry {
    href: String,
    is_dir: bool,
    size: Option<u64>,
    mime: Option<String>,
}

fn parse_listing(xml: &[u8]) -> Result<Vec<Entry>, PullError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let (mut out, mut cur, mut field) = (Vec::new(), None::<Entry>, String::new());
    loop {
        match reader.read_event().map_err(|e| PullError::Remote(format!("réponse XML illisible : {e}")))? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_ascii_lowercase();
                match name.as_str() {
                    "response" => cur = Some(Entry::default()),
                    "collection" => {
                        if let Some(c) = cur.as_mut() {
                            c.is_dir = true;
                        }
                    }
                    _ => field = name,
                }
            }
            Event::Empty(e) => {
                if e.local_name().as_ref().eq_ignore_ascii_case(b"collection") {
                    if let Some(c) = cur.as_mut() {
                        c.is_dir = true;
                    }
                }
            }
            Event::Text(t) => {
                let text = t.unescape().map_err(|e| PullError::Remote(format!("réponse XML illisible : {e}")))?.into_owned();
                if let Some(c) = cur.as_mut() {
                    match field.as_str() {
                        "href" => c.href = text,
                        "getcontentlength" => c.size = text.trim().parse().ok(),
                        "getcontenttype" => c.mime = Some(text.split(';').next().unwrap_or("").trim().to_ascii_lowercase()).filter(|m| !m.is_empty()),
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                field.clear();
                if e.local_name().as_ref().eq_ignore_ascii_case(b"response") {
                    out.extend(cur.take());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Segments décodés du chemin d'un `href` (absolu ou relatif), sans le dernier `/`.
fn href_segments(href: &str) -> Option<Vec<String>> {
    let path = href.split(['?', '#']).next().unwrap_or("");
    let path = match url::Url::parse(path) {
        Ok(u) => u.path().to_owned(),
        Err(_) => path.to_owned(),
    };
    path.split('/').filter(|s| !s.is_empty()).map(|s| percent_decode_str(s).decode_utf8().ok().map(|c| c.into_owned())).collect()
}

fn limits(src: &Source, max_bytes: u64) -> FetchLimits {
    let mut l = FetchLimits::new(max_bytes);
    l.allow_private = src.allow_private;
    l
}

async fn list_dir(src: &Source, segs: &[String]) -> Result<Vec<Entry>, PullError> {
    let url = dir_url(src, segs);
    let body = Bytes::from_static(br#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getcontenttype/></d:prop></d:propfind>"#);
    let headers = [("Authorization", basic(&src.username, &src.password)), ("Depth", "1".to_owned()), ("Content-Type", "application/xml".to_owned())];
    let mut got = request("PROPFIND", &url, &headers, Some(body), &limits(src, MAX_LISTING)).await.map_err(|e| match e {
        FetchError::Status(401 | 403) => PullError::Remote("identifiants refusés par le serveur".into()),
        FetchError::Status(404) => PullError::Remote("dossier introuvable sur le serveur".into()),
        other => PullError::Fetch(other),
    })?;
    let mut xml = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut got.body, &mut xml).await.map_err(|e| PullError::Remote(e.to_string()))?;
    if xml.len() as u64 > MAX_LISTING {
        return Err(PullError::Remote("réponse trop volumineuse".into()));
    }
    parse_listing(&xml)
}

/// Vérifie une source en listant son premier niveau (identifiants, adresse, dossier distant) : rend le nombre d'entrées vues.
///
/// # Errors
/// Serveur injoignable, identifiants refusés ou dossier introuvable.
pub async fn check(src: &Source) -> Result<usize, PullError> {
    list_dir(src, &[]).await.map(|e| e.len().saturating_sub(1))
}

/// Parcourt la source (au plus [`MAX_FILES`] fichiers, [`MAX_DIRS`] dossiers, [`MAX_DEPTH`] niveaux).
///
/// # Errors
/// Serveur injoignable, identifiants refusés, dossier introuvable ou réponse illisible (au premier niveau : plus loin, un dossier
/// illisible est ignoré et le parcours est marqué borné).
pub async fn walk(src: &Source) -> Result<Walk, PullError> {
    let mut out = Walk::default();
    let mut queue: VecDeque<Vec<String>> = VecDeque::from([Vec::new()]);
    while let Some(segs) = queue.pop_front() {
        if out.dirs >= MAX_DIRS {
            out.capped = true;
            break;
        }
        out.dirs += 1;
        let entries = match list_dir(src, &segs).await {
            Ok(e) => e,
            Err(e) if segs.is_empty() => return Err(e),
            Err(_) => {
                out.capped = true;
                continue;
            }
        };
        // Les enfants sont les `href` qui prolongent EXACTEMENT le chemin demandé d'un seul segment : le dossier lui-même, un
        // chemin voisin, ou un `href` qui sortirait du dossier sont ignorés.
        let Some(want) = href_segments(&dir_url(src, &segs)) else { continue };
        for e in entries {
            let Some(path) = href_segments(&e.href) else { continue };
            if path.len() != want.len() + 1 || path[..want.len()] != want[..] {
                continue;
            }
            let name = path[want.len()].clone();
            if name == "." || name == ".." || name.contains('\\') || name.chars().any(char::is_control) {
                continue;
            }
            if e.is_dir {
                if segs.len() + 1 > MAX_DEPTH {
                    out.capped = true;
                } else {
                    let mut next = segs.clone();
                    next.push(name);
                    queue.push_back(next);
                }
            } else if out.files.len() >= MAX_FILES {
                out.capped = true;
            } else {
                out.files.push(RemoteFile { dirs: segs.clone(), name, size: e.size.unwrap_or(0), mime: e.mime });
            }
        }
    }
    Ok(out)
}

/// Options d'un passage.
#[derive(Debug, Clone)]
pub struct PullOptions {
    /// Compte et n'écrit rien : annonce ce qui serait repris.
    pub dry_run: bool,
    /// Fichiers repris au plus pendant ce passage (0 : aucune borne).
    pub limit: usize,
    /// Plafond par fichier.
    pub max_file: u64,
    /// Demande d'arrêt : le fichier en cours s'achève, les suivants ne sont pas pris.
    pub stop: Option<Arc<AtomicBool>>,
}

impl Default for PullOptions {
    fn default() -> Self {
        Self { dry_run: false, limit: 0, max_file: DEFAULT_MAX_FILE, stop: None }
    }
}

/// Avancement d'un passage, rendu à chaque fichier.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Progress {
    /// Fichiers examinés.
    pub done: usize,
    /// Fichiers vus sur le serveur.
    pub total: usize,
    /// Fichiers repris.
    pub imported: usize,
    /// Fichiers déjà repris.
    pub skipped: usize,
    /// Échecs.
    pub errors: usize,
}

/// Bilan d'un passage.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PullReport {
    /// Fichiers vus sur le serveur.
    pub remote: usize,
    /// Dossiers parcourus.
    pub dirs: usize,
    /// Fichiers repris (ou qui seraient repris, en `dry_run`).
    pub imported: usize,
    /// Fichiers déjà repris (ou déjà présents au même endroit), laissés tels quels.
    pub skipped: usize,
    /// Fichiers déjà repris dont la taille a changé sur le serveur : laissés tels quels, jamais dupliqués.
    pub changed: usize,
    /// Fichiers plus gros que le plafond.
    pub too_large: usize,
    /// Octets de clair repris.
    pub bytes: u64,
    /// Le parcours a été borné.
    pub capped: bool,
    /// Le passage a été arrêté à la demande avant la fin.
    pub stopped: bool,
    /// Échecs : `(chemin distant, raison)`. Jamais avalés.
    pub errors: Vec<(String, String)>,
}

/// Reprend la source dans le uCloud de `owner`. `progress` est appelé à chaque fichier.
///
/// # Errors
/// Seulement si le parcours échoue ; l'échec d'un fichier est dit dans [`PullReport::errors`] et le passage continue.
pub async fn pull<B: BlobStore>(st: &Arc<DavState<B>>, owner: &str, src: &Source, opts: &PullOptions, mut progress: impl FnMut(Progress)) -> Result<PullReport, PullError> {
    let walked = walk(src).await?;
    let mut report = PullReport { remote: walked.files.len(), dirs: walked.dirs, capped: walked.capped, ..PullReport::default() };
    let total = walked.files.len();
    let snap = |n: usize, r: &PullReport| Progress { done: n, total, imported: r.imported, skipped: r.skipped, errors: r.errors.len() };
    progress(snap(0, &report));
    for (n, file) in walked.files.iter().enumerate() {
        if opts.stop.as_ref().is_some_and(|s| s.load(Ordering::Relaxed)) {
            report.stopped = true;
            break;
        }
        progress(snap(n, &report));
        let remote_path = file.dirs.iter().map(String::as_str).chain(std::iter::once(file.name.as_str())).collect::<Vec<_>>().join("/");
        let Some(name) = sanitize_name(&file.name) else {
            report.errors.push((remote_path, "nom de fichier inutilisable".into()));
            continue;
        };
        let dirs: Option<Vec<String>> = file.dirs.iter().map(|d| sanitize_name(d)).collect();
        let Some(dirs) = dirs else {
            report.errors.push((remote_path, "nom de dossier inutilisable".into()));
            continue;
        };
        let dest_dir = normalize_path(&std::iter::once(src.dest_prefix.as_str()).chain(dirs.iter().map(String::as_str)).collect::<Vec<_>>().join("/")).unwrap_or_else(|_| src.dest_prefix.clone());
        let dest = if dest_dir == "/" { format!("/{name}") } else { format!("{dest_dir}/{name}") };
        // Déjà repris de cette source (où que le fichier soit allé depuis), ou déjà là au chemin de destination (reprise d'une
        // ancienne importation) : dans ce dernier cas on l'adopte, pour que la mémoire survive à un déplacement.
        let (o, sid, rp, d) = (owner.to_owned(), src.id.clone(), remote_path.clone(), dest.clone());
        let known: (Option<i64>, Option<i64>) = st
            .db
            .readers()
            .read(move |c| {
                let remembered = rusqlite::OptionalExtension::optional(c.query_row("SELECT size FROM pull_imported WHERE owner_hex = ?1 AND source_id = ?2 AND remote_path = ?3", rusqlite::params![o, sid, rp], |r| r.get::<_, i64>(0)))?;
                let present = rusqlite::OptionalExtension::optional(c.query_row("SELECT coalesce(size, 0) FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", rusqlite::params![o, d], |r| r.get::<_, i64>(0)))?;
                Ok((remembered, present))
            })
            .await
            .map_err(|e| PullError::Internal(e.to_string()))?;
        match known {
            (Some(size), _) => {
                report.skipped += 1;
                if u64::try_from(size).ok() != Some(file.size) && file.size > 0 {
                    report.changed += 1;
                }
                continue;
            }
            (None, Some(size)) => {
                report.skipped += 1;
                if !opts.dry_run {
                    remember(st, owner, &src.id, &remote_path, u64::try_from(size).unwrap_or(0), &dest).await?;
                }
                continue;
            }
            (None, None) => {}
        }
        if file.size > opts.max_file {
            report.too_large += 1;
            report.errors.push((remote_path, format!("{} Mo : au-delà du plafond de {} Mo", file.size >> 20, opts.max_file >> 20)));
            continue;
        }
        if opts.limit > 0 && report.imported >= opts.limit {
            break;
        }
        if opts.dry_run {
            report.imported += 1;
            report.bytes += file.size;
            continue;
        }
        let url = format!("{}{}", dir_url(src, &file.dirs), utf8_percent_encode(&file.name, PATH_SEGMENT));
        let headers = [("Authorization", basic(&src.username, &src.password))];
        let mime = file.mime.as_deref().filter(|m| !matches!(*m, "application/octet-stream" | "binary/octet-stream"));
        let fetched = match tokio::time::timeout(FILE_TIMEOUT, async {
            let got = request("GET", &url, &headers, None, &limits(src, opts.max_file)).await?;
            ingest(st, owner, &dest_dir, &name, mime, got.body).await.map_err(|e| PullError::Remote(e.to_string()))
        })
        .await
        {
            Ok(r) => r,
            Err(_) => Err(PullError::Remote("délai dépassé".into())),
        };
        match fetched {
            Ok(done) => {
                report.imported += 1;
                report.bytes += done.size;
                // Provenance sur l'entrée (le rangement s'en servira) et mémoire de la reprise.
                let (o, p, origin) = (owner.to_owned(), done.path.clone(), format!("pull:{}", src.id));
                if let Err(e) = st.db.writer().execute("UPDATE ucloud_entries SET origin = ?3 WHERE owner_hex = ?1 AND path = ?2", vec![o.into(), p.into(), origin.into()]).await {
                    report.errors.push((remote_path.clone(), format!("repris, mais provenance non notée : {e}")));
                }
                if let Err(e) = remember(st, owner, &src.id, &remote_path, done.size, &done.path).await {
                    report.errors.push((remote_path.clone(), format!("repris, mais mémoire non écrite (il serait repris à nouveau) : {e}")));
                }
            }
            Err(e) => report.errors.push((remote_path, e.to_string())),
        }
    }
    progress(snap(total, &report));
    Ok(report)
}

/// Note qu'un fichier de `source_id` a été repris (ou adopté) : le passage suivant le laisse en paix, où qu'il soit rangé ensuite.
async fn remember<B: BlobStore>(st: &Arc<DavState<B>>, owner: &str, source_id: &str, remote_path: &str, size: u64, local_path: &str) -> Result<(), PullError> {
    st.db
        .writer()
        .execute(
            "INSERT INTO pull_imported(owner_hex, source_id, remote_path, size, local_path, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(owner_hex, source_id, remote_path) DO UPDATE SET size = excluded.size, local_path = excluded.local_path, at = excluded.at",
            vec![owner.to_owned().into(), source_id.to_owned().into(), remote_path.to_owned().into(), i64::try_from(size).unwrap_or(0).into(), local_path.to_owned().into(), i64::try_from(now()).unwrap_or(0).into()],
        )
        .await
        .map(|_| ())
        .map_err(|e| PullError::Internal(e.to_string()))
}

// ── Service : les reprises lancées depuis l'interface ───────────────────────────────────────────────────────────

/// Passages simultanés au plus sur la station (le travail est surtout du chiffrement et du réseau).
const MAX_JOBS: usize = 2;
/// Portée `kv` du dernier bilan de chaque source (aucun secret).
const KV_LAST: &str = "webdav_pull_last";

#[derive(Clone)]
struct Job {
    source: String,
    dry_run: bool,
    started: u64,
    finished: Option<u64>,
    progress: Progress,
    outcome: Option<Result<Value, String>>,
    stop: Arc<AtomicBool>,
}

/// Saisie d'une source depuis l'interface.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Draft {
    /// Libellé (défaut : le nom du serveur).
    #[serde(default)]
    pub label: String,
    /// Racine WebDAV.
    pub url: String,
    /// Identifiant de connexion.
    pub username: String,
    /// Mot de passe (d'application de préférence).
    pub password: String,
    /// Dossier distant (défaut : tout).
    #[serde(default)]
    pub remote_path: String,
    /// Dossier de dépôt dans le uCloud (défaut : `/Imports/<libellé>`).
    #[serde(default)]
    pub dest: Option<String>,
}

/// Les reprises d'un titulaire : sources, passages en cours, derniers bilans.
pub struct PullService<B: BlobStore> {
    dav: Arc<DavState<B>>,
    sources: Sources,
    jobs: Mutex<HashMap<String, Job>>,
    slots: Arc<Semaphore>,
    activity: Option<ActivityBus>,
}

fn summary(r: &PullReport, dry_run: bool) -> Value {
    json!({ "remote": r.remote, "dirs": r.dirs, "imported": r.imported, "skipped": r.skipped, "changed": r.changed, "too_large": r.too_large, "errors": r.errors.len(), "bytes": r.bytes, "capped": r.capped, "stopped": r.stopped, "dry_run": dry_run,
            "first_errors": r.errors.iter().take(5).map(|(p, why)| json!({ "path": p, "why": why })).collect::<Vec<_>>() })
}

impl<B: BlobStore> PullService<B> {
    /// Service sur l'état WebDAV de la station ; `keys` chiffre les sources pour leur titulaire.
    #[must_use]
    pub fn new(dav: Arc<DavState<B>>, keys: Arc<dyn KeySource>, activity: Option<ActivityBus>) -> Arc<Self> {
        Arc::new(Self { sources: Sources::new(dav.db.clone(), keys), dav, jobs: Mutex::new(HashMap::new()), slots: Arc::new(Semaphore::new(MAX_JOBS)), activity })
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
        self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn last(&self, owner: &str, id: &str) -> Option<Value> {
        let key = format!("{owner}:{id}");
        let raw: Option<String> = self.dav.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![KV_LAST, key], |r| r.get(0)))?)).await.ok()?;
        serde_json::from_str(&raw?).ok()
    }

    /// Les sources du titulaire, sans mot de passe, avec le passage en cours ou le dernier bilan.
    ///
    /// # Errors
    /// Base ou clé indisponibles.
    pub async fn list(&self, owner: &str) -> Result<Value, PullError> {
        let mut out = Vec::new();
        for s in self.sources.list(owner).await? {
            let job = self.jobs().get(&format!("{owner}:{}", s.id)).cloned();
            let job = job.map(|j| {
                let state = match (&j.outcome, j.finished) {
                    (None, _) => "running",
                    (Some(Ok(_)), _) => "done",
                    (Some(Err(_)), _) => "failed",
                };
                json!({ "state": state, "dry_run": j.dry_run, "started": j.started, "finished": j.finished, "progress": j.progress,
                        "report": j.outcome.as_ref().and_then(|o| o.as_ref().ok()), "error": j.outcome.as_ref().and_then(|o| o.as_ref().err()) })
            });
            out.push(json!({ "id": s.id, "label": s.label, "url": s.url, "username": s.username, "remote_path": s.remote_path, "dest_prefix": s.dest_prefix, "private_ok": s.allow_private, "job": job, "last": self.last(owner, &s.id).await }));
        }
        Ok(json!({ "sources": out }))
    }

    /// Enregistre une source après l'avoir vérifiée. **Jamais** d'adresse privée depuis l'interface.
    ///
    /// # Errors
    /// Saisie invalide, serveur injoignable ou identifiants refusés.
    pub async fn add(&self, owner: &str, d: Draft) -> Result<Value, PullError> {
        let name = if d.label.trim().is_empty() { url::Url::parse(&d.url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| "source".into()) } else { d.label.clone() };
        let dest_prefix = match d.dest.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
            Some(x) => x.to_owned(),
            None => format!("/Imports/{}", sanitize_name(&name).unwrap_or_else(|| "source".into())),
        };
        let draft = Source { id: String::new(), label: d.label, url: d.url, username: d.username, password: d.password, remote_path: d.remote_path, dest_prefix, allow_private: false, created_at: 0 };
        let mut probe = draft.clone();
        validate(&mut probe)?;
        check(&probe).await?;
        let saved = self.sources.add(owner, draft).await?;
        Ok(json!({ "id": saved.id, "label": saved.label, "dest_prefix": saved.dest_prefix }))
    }

    /// Retire une source (pas de passage en cours).
    ///
    /// # Errors
    /// Source inconnue ou passage en cours.
    pub async fn remove(&self, owner: &str, id: &str) -> Result<(), PullError> {
        if self.jobs().get(&format!("{owner}:{id}")).is_some_and(|j| j.outcome.is_none()) {
            return Err(PullError::Invalid("une reprise de cette source est en cours : l'arrêter d'abord".into()));
        }
        self.sources.remove(owner, id).await?;
        self.jobs().remove(&format!("{owner}:{id}"));
        Ok(())
    }

    /// Lance un passage en arrière-plan (un seul par titulaire, [`MAX_JOBS`] sur la station).
    ///
    /// # Errors
    /// Source inconnue, passage déjà en cours ou station chargée.
    pub async fn run(self: &Arc<Self>, owner: &str, id: &str, dry_run: bool) -> Result<(), PullError> {
        let src = self.sources.list(owner).await?.into_iter().find(|s| s.id == id).ok_or_else(|| PullError::Invalid("source inconnue".into()))?;
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| PullError::Invalid("trop de reprises en cours sur la station : réessayez dans quelques minutes".into()))?;
        let key = format!("{owner}:{id}");
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut jobs = self.jobs();
            if jobs.iter().any(|(k, j)| k.starts_with(&format!("{owner}:")) && j.outcome.is_none()) {
                return Err(PullError::Invalid("une reprise est déjà en cours pour ce compte".into()));
            }
            jobs.insert(key.clone(), Job { source: id.to_owned(), dry_run, started: now(), finished: None, progress: Progress::default(), outcome: None, stop: stop.clone() });
        }
        let me = self.clone();
        let owner = owner.to_owned();
        tokio::spawn(async move {
            let opts = PullOptions { dry_run, stop: Some(stop), ..PullOptions::default() };
            let tracker = me.clone();
            let k = key.clone();
            let result = pull(&me.dav, &owner, &src, &opts, move |p| {
                if let Some(j) = tracker.jobs().get_mut(&k) {
                    j.progress = p;
                }
            })
            .await;
            let outcome = result.map(|r| summary(&r, dry_run)).map_err(|e| e.to_string());
            if let Some(bus) = &me.activity {
                let text = match &outcome {
                    Ok(v) if dry_run => format!("Reprise à blanc de « {} » : {} fichier(s) à reprendre, {} déjà là", src.label, v["imported"], v["skipped"]),
                    Ok(v) => format!("Reprise de « {} » : {} fichier(s) repris, {} déjà là, {} échec(s)", src.label, v["imported"], v["skipped"], v["errors"]),
                    Err(e) => format!("Reprise de « {} » impossible : {e}", src.label),
                };
                bus.publish_to(Audience::Owner(owner.clone()), if outcome.is_ok() { "pull" } else { "alert" }, "pull", text, None);
            }
            if let Ok(v) = &outcome {
                let _ = me.dav.db.writer().execute(
                    "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                    vec![KV_LAST.to_owned().into(), key.clone().into(), json!({ "at": now(), "report": v }).to_string().into(), i64::try_from(now()).unwrap_or(0).into()],
                ).await;
            }
            if let Some(j) = me.jobs().get_mut(&key) {
                j.finished = Some(now());
                j.outcome = Some(outcome);
            }
            drop(permit);
        });
        Ok(())
    }

    /// Demande l'arrêt du passage en cours du titulaire (le fichier en cours s'achève).
    ///
    /// # Errors
    /// Aucun passage en cours.
    pub fn stop(&self, owner: &str) -> Result<(), PullError> {
        let jobs = self.jobs();
        let running = jobs.iter().find(|(k, j)| k.starts_with(&format!("{owner}:")) && j.outcome.is_none()).ok_or_else(|| PullError::Invalid("aucune reprise en cours".into()))?;
        running.1.stop.store(true, Ordering::Relaxed);
        let _ = &running.1.source;
        Ok(())
    }
}
