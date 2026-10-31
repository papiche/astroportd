//! Rangement du uCloud par type, comme sur un ordinateur de bureau : Documents, Images, Musique, Vidéos, Archives.
//!
//! Le uCloud est alimenté par de nombreuses sources (dépôts, reprises WebDAV, téléphones…) : cette fonction le réorganise
//! **comme un seul disque**, à la demande du titulaire.
//!
//! - **Ne déplace que des entrées** du VFS : aucun blob n'est relu, recopié ni re-chiffré. Les visages suivent leur photo.
//! - **Aperçu d'abord** ([`plan`]) : ce qui serait déplacé, par type, avec la taille et des exemples.
//! - **Prudent** : le coffre, les contacts, l'agenda, les partages reçus, les sauvegardes et tout dossier caché ne sont jamais
//!   touchés ; ce qui est déjà dans un dossier de type reste en place ; un fichier d'un type inconnu reste où il est ;
//!   un nom déjà pris reçoit un numéro (jamais d'écrasement).
//! - **Annulable** : chaque passage est journalisé (`organize_runs`, `organize_moves`) et [`undo`] remet chaque fichier à sa place.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use astro_core::{ActivityBus, Audience};
use astro_dav::DavState;
use astro_dav::blob::BlobStore;
use astro_dav::vfs::{self, VfsError, mkdir_p_tx, move_tx, normalize_path, parent_of, vfs_tx};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

/// Déplacements par transaction.
const BATCH: usize = 200;
/// Fichiers au plus par passage (au-delà, un second passage prend la suite).
pub const MAX_MOVES: usize = 200_000;
/// Passages simultanés au plus sur la station.
const MAX_JOBS: usize = 2;
/// Dossiers que le rangement ne touche jamais (premier segment du chemin).
const PROTECTED: [&str; 5] = ["Coffre", "Contacts", "Calendrier", "Partages", "Sauvegardes"];

/// Types de fichiers rangés.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Textes, PDF, bureautique, livres.
    Documents,
    /// Photos et images.
    Images,
    /// Audio.
    Music,
    /// Vidéos.
    Videos,
    /// Archives compressées.
    Archives,
}

impl Kind {
    /// Tous les types, dans l'ordre d'affichage.
    pub const ALL: [Kind; 5] = [Self::Documents, Self::Images, Self::Music, Self::Videos, Self::Archives];

    /// Nom du dossier par défaut.
    #[must_use]
    pub fn folder(self) -> &'static str {
        match self {
            Self::Documents => "Documents",
            Self::Images => "Images",
            Self::Music => "Musique",
            Self::Videos => "Vidéos",
            Self::Archives => "Archives",
        }
    }

    /// Identifiant stable (API).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Documents => "documents",
            Self::Images => "images",
            Self::Music => "music",
            Self::Videos => "videos",
            Self::Archives => "archives",
        }
    }
}

/// Type d'un fichier d'après son type MIME, à défaut d'après son extension. `None` : inconnu, le fichier reste en place.
#[must_use]
pub fn classify(mime: &str, name: &str) -> Option<Kind> {
    let mime = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if mime.starts_with("image/") {
        return Some(Kind::Images);
    }
    if mime.starts_with("audio/") {
        return Some(Kind::Music);
    }
    if mime.starts_with("video/") {
        return Some(Kind::Videos);
    }
    if matches!(mime.as_str(), "application/zip" | "application/gzip" | "application/x-7z-compressed" | "application/x-tar" | "application/vnd.rar" | "application/x-rar-compressed" | "application/x-bzip2" | "application/x-xz") {
        return Some(Kind::Archives);
    }
    if mime == "application/pdf" || mime == "application/rtf" || mime == "application/epub+zip" || mime.starts_with("application/vnd.openxmlformats") || mime.starts_with("application/vnd.oasis.opendocument") || mime.starts_with("application/vnd.ms-") || mime == "application/msword" || (mime.starts_with("text/") && mime != "text/vcard" && mime != "text/calendar") {
        return Some(Kind::Documents);
    }
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tif" | "tiff" | "svg" | "avif" | "cr2" | "nef" | "arw" | "dng" | "raw" => Some(Kind::Images),
        "mp3" | "flac" | "ogg" | "opus" | "m4a" | "wav" | "aac" | "wma" => Some(Kind::Music),
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "mpg" | "mpeg" | "wmv" => Some(Kind::Videos),
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "tgz" | "iso" => Some(Kind::Archives),
        "pdf" | "doc" | "docx" | "odt" | "ods" | "odp" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf" | "csv" | "epub" | "tex" => Some(Kind::Documents),
        _ => None,
    }
}

/// Sous-dossiers par date dans chaque dossier de type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ByDate {
    /// Tout à plat dans le dossier du type.
    #[default]
    None,
    /// `Images/2003/…`
    Year,
    /// `Images/2003/06/…`
    Month,
}

/// Réglages d'un rangement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Options {
    /// Dossier à ranger (`/` : tout le uCloud, sauf ce qui est protégé).
    #[serde(default = "root")]
    pub scope: String,
    /// Sous-dossiers par date.
    #[serde(default)]
    pub by_date: ByDate,
    /// Types à ranger (défaut : tous).
    #[serde(default = "all_kinds")]
    pub kinds: Vec<String>,
}

fn root() -> String {
    "/".into()
}

fn all_kinds() -> Vec<String> {
    Kind::ALL.iter().map(|k| k.id().to_owned()).collect()
}

impl Default for Options {
    fn default() -> Self {
        Self { scope: root(), by_date: ByDate::None, kinds: all_kinds() }
    }
}

/// Échec d'un rangement.
#[derive(Debug, thiserror::Error)]
pub enum OrganizeError {
    /// Saisie refusée.
    #[error("{0}")]
    Invalid(String),
    /// Base.
    #[error("{0}")]
    Internal(String),
}

/// Ce qu'un rangement ferait.
#[derive(Debug, Default, Clone)]
pub struct Plan {
    /// Déplacements `(de, vers)`.
    pub moves: Vec<(String, String)>,
    /// Par type : fichiers et octets.
    pub by_kind: BTreeMap<Kind, (usize, u64)>,
    /// Fichiers déjà dans un dossier de type.
    pub placed: usize,
    /// Fichiers dans un dossier protégé ou caché.
    pub protected: usize,
    /// Fichiers d'un type inconnu, laissés en place.
    pub unknown: usize,
    /// Le plan a été borné à [`MAX_MOVES`].
    pub capped: bool,
}

/// Un fichier candidat : chemin, type MIME, taille, date de prise de vue (si connue), date de modification.
type Candidate = (String, String, i64, Option<i64>, i64);

fn bounds(scope: &str) -> (String, String) {
    if scope == "/" { ("/".into(), "0".into()) } else { (format!("{scope}/"), format!("{scope}0")) }
}

fn numbered(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 8 => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

fn is_protected(path: &str) -> bool {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    first.starts_with('.') || PROTECTED.iter().any(|p| p.eq_ignore_ascii_case(first)) || path.split('/').any(|s| s.starts_with('.'))
}

fn validate(opts: &Options) -> Result<(String, Vec<Kind>), OrganizeError> {
    let scope = normalize_path(&opts.scope).map_err(|_| OrganizeError::Invalid("dossier à ranger invalide".into()))?;
    if scope != "/" && is_protected(&scope) {
        return Err(OrganizeError::Invalid("ce dossier est protégé : le rangement n'y touche jamais".into()));
    }
    let kinds: Vec<Kind> = Kind::ALL.into_iter().filter(|k| opts.kinds.iter().any(|x| x == k.id())).collect();
    if kinds.is_empty() {
        return Err(OrganizeError::Invalid("aucun type choisi".into()));
    }
    Ok((scope, kinds))
}

/// Calcule ce que ferait un rangement, sans rien écrire.
///
/// # Errors
/// Saisie invalide ou base indisponible.
pub async fn plan(db: &astro_db::Db, owner: &str, opts: &Options) -> Result<Plan, OrganizeError> {
    let (scope, kinds) = validate(opts)?;
    let (low, high) = bounds(&scope);
    let o = owner.to_owned();
    let (files, taken): (Vec<Candidate>, HashSet<String>) = db
        .readers()
        .read(move |c| {
            let files = c
                .prepare("SELECT path, coalesce(mime, ''), coalesce(size, 0), taken_at, coalesce(mtime, 0) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND path >= ?2 AND path < ?3 ORDER BY path")?
                .query_map(params![o, low, high], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            // Chemins déjà occupés dans les dossiers de type : un nom pris reçoit un numéro.
            let mut taken = HashSet::new();
            for k in Kind::ALL {
                let (l, h) = bounds(&format!("/{}", k.folder()));
                for p in c.prepare("SELECT path FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND path >= ?2 AND path < ?3")?.query_map(params![o, l, h], |r| r.get::<_, String>(0))? {
                    taken.insert(p?);
                }
            }
            Ok((files, taken))
        })
        .await
        .map_err(|e| OrganizeError::Internal(e.to_string()))?;
    let mut taken = taken;
    let mut out = Plan::default();
    let type_roots: Vec<String> = Kind::ALL.iter().map(|k| format!("/{}/", k.folder())).collect();
    for (path, mime, size, shot, mtime) in files {
        if is_protected(&path) {
            out.protected += 1;
            continue;
        }
        if type_roots.iter().any(|r| path.starts_with(r.as_str())) {
            out.placed += 1;
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or("");
        let Some(kind) = classify(&mime, name).filter(|k| kinds.contains(k)) else {
            out.unknown += 1;
            continue;
        };
        if out.moves.len() >= MAX_MOVES {
            out.capped = true;
            break;
        }
        let (year, month) = best_date(&path, shot, mtime);
        let date = match opts.by_date {
            ByDate::None => String::new(),
            ByDate::Year => format!("/{year}"),
            ByDate::Month if year == "Sans date" => format!("/{year}"),
            ByDate::Month => format!("/{year}/{}", month.as_deref().unwrap_or("Sans mois")),
        };
        let dir = format!("/{}{date}", kind.folder());
        let mut target = format!("{dir}/{name}");
        let mut n = 1;
        while taken.contains(&target) {
            n += 1;
            target = format!("{dir}/{}", numbered(name, n));
        }
        taken.insert(target.clone());
        let e = out.by_kind.entry(kind).or_default();
        e.0 += 1;
        e.1 += u64::try_from(size).unwrap_or(0);
        out.moves.push((path, target));
    }
    Ok(out)
}

/// Date de rangement d'un fichier : sa prise de vue si elle est connue ; sinon celle que dit son dossier d'origine
/// (`…/1980/01/photo.jpg`, l'arborescence que la reprise d'un autre serveur a gardée) ; sinon sa date de modification.
fn best_date(path: &str, taken: Option<i64>, mtime: i64) -> (String, Option<String>) {
    if let Some(t) = taken.filter(|t| *t > 0) {
        let (y, m) = year_month(t);
        return (y, Some(m));
    }
    if let Some(found) = path_date(path) {
        return found;
    }
    let (y, m) = year_month(mtime);
    if mtime > 0 { (y, Some(m)) } else { (y, None) }
}

/// `…/2003/06/…` ou `…/2003/…` dans les dossiers d'un chemin (année entre 1900 et 2100, mois de 01 à 12).
fn path_date(path: &str) -> Option<(String, Option<String>)> {
    let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let dirs = segs.get(..segs.len().saturating_sub(1))?;
    let year = |s: &str| s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit()) && (1900..=2100).contains(&s.parse::<u32>().unwrap_or(0));
    let month = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()) && (1..=12).contains(&s.parse::<u32>().unwrap_or(0));
    for (i, s) in dirs.iter().enumerate() {
        if year(s) {
            return Some(((*s).to_owned(), dirs.get(i + 1).filter(|m| month(m)).map(|m| (*m).to_owned())));
        }
    }
    None
}

/// Année et mois (`2003`, `06`) d'un horodatage Unix (UTC ; `taken_at` est déjà l'heure murale de la prise de vue).
fn year_month(ts: i64) -> (String, String) {
    let days = ts.div_euclid(86_400);
    // Algorithme civil de Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    if ts <= 0 { ("Sans date".into(), "00".into()) } else { (format!("{year:04}"), format!("{m:02}")) }
}

/// Bilan d'un rangement.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Identifiant du passage (pour l'annuler).
    pub run_id: String,
    /// Fichiers déplacés.
    pub moved: usize,
    /// Dossiers laissés vides puis supprimés.
    pub pruned: usize,
    /// Échecs `(chemin, raison)`.
    pub errors: Vec<(String, String)>,
    /// Arrêté à la demande.
    pub stopped: bool,
}

/// Avancement.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Progress {
    /// Déplacements faits (réussis ou non).
    pub done: usize,
    /// Déplacements prévus.
    pub total: usize,
    /// Réussis.
    pub moved: usize,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Exécute `plan` pour `owner`, par lots, en journalisant chaque déplacement.
///
/// # Errors
/// Base indisponible ; l'échec d'un déplacement est dit dans [`Report::errors`] et le passage continue.
pub async fn apply<B: BlobStore>(st: &Arc<DavState<B>>, owner: &str, opts: &Options, plan: &Plan, stop: Option<&AtomicBool>, mut progress: impl FnMut(Progress)) -> Result<Report, OrganizeError> {
    let mut raw = [0u8; 6];
    getrandom::fill(&mut raw).map_err(|e| OrganizeError::Internal(format!("aléa du système indisponible : {e}")))?;
    let run_id = hex::encode(raw);
    let mut report = Report { run_id: run_id.clone(), ..Report::default() };
    let (o, r, opt) = (owner.to_owned(), run_id.clone(), serde_json::to_string(opts).unwrap_or_default());
    st.db.writer().execute("INSERT INTO organize_runs(run_id, owner_hex, at, options) VALUES (?1, ?2, ?3, ?4)", vec![r.into(), o.into(), now().into(), opt.into()]).await.map_err(|e| OrganizeError::Internal(e.to_string()))?;
    let total = plan.moves.len();
    let mut seq: i64 = 0;
    let mut sources: HashSet<String> = HashSet::new();
    for chunk in plan.moves.chunks(BATCH) {
        if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
            report.stopped = true;
            break;
        }
        let (o, r, batch, first) = (owner.to_owned(), run_id.clone(), chunk.to_vec(), seq);
        let done: Vec<(usize, Result<(), VfsError>)> = vfs_tx(&st.db, move |tx| {
            let mut out = Vec::with_capacity(batch.len());
            for (i, (from, to)) in batch.iter().enumerate() {
                let step = mkdir_p_tx(tx, &o, &parent_of(to), now()).and_then(|()| move_tx(tx, &o, from, to, false, now()).map(|_| ()));
                if step.is_ok() {
                    let n = first + i64::try_from(i).unwrap_or(0);
                    tx.execute("INSERT INTO organize_moves(run_id, owner_hex, seq, from_path, to_path) VALUES (?1, ?2, ?3, ?4, ?5)", params![r, o, n, from, to])?;
                }
                out.push((i, step));
            }
            Ok(out)
        })
        .await
        .map_err(|e| OrganizeError::Internal(e.to_string()))?;
        seq += i64::try_from(chunk.len()).unwrap_or(0);
        for (i, step) in done {
            match step {
                Ok(()) => {
                    report.moved += 1;
                    sources.insert(parent_of(&chunk[i].0));
                }
                Err(e) => report.errors.push((chunk[i].0.clone(), e.to_string())),
            }
        }
        progress(Progress { done: usize::try_from(seq).unwrap_or(total).min(total), total, moved: report.moved });
    }
    // Les dossiers laissés vides par le rangement disparaissent (jamais la racine, un dossier protégé ou un dossier de type).
    report.pruned = prune_empty(st, owner, &sources, &run_id).await?;
    let (o, r, moved) = (owner.to_owned(), run_id, i64::try_from(report.moved).unwrap_or(0));
    st.db.writer().execute("UPDATE organize_runs SET moved = ?3 WHERE owner_hex = ?1 AND run_id = ?2", vec![o.into(), r.into(), moved.into()]).await.map_err(|e| OrganizeError::Internal(e.to_string()))?;
    if report.moved > 0 {
        if let Some(p) = &st.publisher {
            p.notify(owner);
        }
    }
    progress(Progress { done: total, total, moved: report.moved });
    Ok(report)
}

async fn prune_empty<B: BlobStore>(st: &Arc<DavState<B>>, owner: &str, sources: &HashSet<String>, _run: &str) -> Result<usize, OrganizeError> {
    let mut candidates: Vec<String> = Vec::new();
    for dir in sources {
        let mut p = dir.clone();
        while p != "/" && !is_protected(&p) && !Kind::ALL.iter().any(|k| p == format!("/{}", k.folder()) || p.starts_with(&format!("/{}/", k.folder()))) {
            candidates.push(p.clone());
            p = parent_of(&p);
        }
    }
    candidates.sort_by_key(|p| std::cmp::Reverse(p.len()));
    candidates.dedup();
    let o = owner.to_owned();
    vfs_tx(&st.db, move |tx| {
        let mut n = 0;
        for dir in candidates {
            let (low, high) = bounds(&dir);
            let is_dir: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'dir')", params![o, dir], |r| r.get(0))?;
            let empty: bool = tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM ucloud_entries WHERE owner_hex = ?1 AND path >= ?2 AND path < ?3)", params![o, low, high], |r| r.get(0))?;
            if is_dir && empty {
                tx.execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", params![o, dir])?;
                n += 1;
            }
        }
        Ok(n)
    })
    .await
    .map_err(|e| OrganizeError::Internal(e.to_string()))
}

/// Remet chaque fichier du passage `run_id` à sa place d'avant (un nom pris depuis reçoit un numéro).
///
/// # Errors
/// Passage inconnu ou déjà annulé.
pub async fn undo<B: BlobStore>(st: &Arc<DavState<B>>, owner: &str, run_id: &str) -> Result<usize, OrganizeError> {
    let (o, r) = (owner.to_owned(), run_id.to_owned());
    let known: Option<Option<i64>> = st.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT undone_at FROM organize_runs WHERE owner_hex = ?1 AND run_id = ?2", params![o, r], |row| row.get::<_, Option<i64>>(0)))?)).await.map_err(|e| OrganizeError::Internal(e.to_string()))?;
    match known {
        None => return Err(OrganizeError::Invalid("rangement inconnu".into())),
        Some(Some(_)) => return Err(OrganizeError::Invalid("ce rangement est déjà annulé".into())),
        Some(None) => {}
    }
    let (o, r) = (owner.to_owned(), run_id.to_owned());
    let moves: Vec<(i64, String, String)> = st.db.readers().read(move |c| Ok(c.prepare("SELECT seq, from_path, to_path FROM organize_moves WHERE owner_hex = ?1 AND run_id = ?2 ORDER BY seq DESC")?.query_map(params![o, r], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<Result<Vec<_>, _>>()?)).await.map_err(|e| OrganizeError::Internal(e.to_string()))?;
    let mut back = 0;
    for chunk in moves.chunks(BATCH) {
        let (o, r, batch) = (owner.to_owned(), run_id.to_owned(), chunk.to_vec());
        let n = vfs_tx(&st.db, move |tx| {
            let mut n = 0;
            for (seq, from, to) in &batch {
                // Le fichier est là où le rangement l'a mis ; sa place d'avant est libre, sinon on la numérote.
                if vfs::get_entry(tx, &o, to)?.is_none() {
                    continue;
                }
                let mut target = from.clone();
                let mut k = 1;
                while vfs::get_entry(tx, &o, &target)?.is_some() {
                    k += 1;
                    let (dir, name) = (parent_of(from), from.rsplit('/').next().unwrap_or(""));
                    target = if dir == "/" { format!("/{}", numbered(name, k)) } else { format!("{dir}/{}", numbered(name, k)) };
                }
                mkdir_p_tx(tx, &o, &parent_of(&target), now())?;
                move_tx(tx, &o, to, &target, false, now())?;
                tx.execute("DELETE FROM organize_moves WHERE owner_hex = ?1 AND run_id = ?2 AND seq = ?3", params![o, r, seq])?;
                n += 1;
            }
            Ok(n)
        })
        .await
        .map_err(|e| OrganizeError::Internal(e.to_string()))?;
        back += n;
    }
    let (o, r) = (owner.to_owned(), run_id.to_owned());
    st.db.writer().execute("UPDATE organize_runs SET undone_at = ?3 WHERE owner_hex = ?1 AND run_id = ?2", vec![o.into(), r.into(), now().into()]).await.map_err(|e| OrganizeError::Internal(e.to_string()))?;
    if back > 0 {
        if let Some(p) = &st.publisher {
            p.notify(owner);
        }
    }
    Ok(back)
}

// ── Service : les rangements lancés depuis l'interface ─────────────────────────────────────────────────────────

#[derive(Clone)]
struct Job {
    started: u64,
    finished: Option<u64>,
    progress: Progress,
    outcome: Option<Result<Value, String>>,
    stop: Arc<AtomicBool>,
}

/// Rangements d'un titulaire : aperçu, passage en arrière-plan, annulation.
pub struct OrganizeService<B: BlobStore> {
    dav: Arc<DavState<B>>,
    jobs: Mutex<std::collections::HashMap<String, Job>>,
    slots: Arc<Semaphore>,
    activity: Option<ActivityBus>,
}

fn secs() -> u64 {
    u64::try_from(now()).unwrap_or(0)
}

impl<B: BlobStore> OrganizeService<B> {
    /// Service sur l'état WebDAV de la station.
    #[must_use]
    pub fn new(dav: Arc<DavState<B>>, activity: Option<ActivityBus>) -> Arc<Self> {
        Arc::new(Self { dav, jobs: Mutex::new(std::collections::HashMap::new()), slots: Arc::new(Semaphore::new(MAX_JOBS)), activity })
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, Job>> {
        self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Aperçu : ce qu'un rangement ferait.
    ///
    /// # Errors
    /// Saisie invalide.
    pub async fn preview(&self, owner: &str, opts: &Options) -> Result<Value, OrganizeError> {
        let p = plan(&self.dav.db, owner, opts).await?;
        let by_kind: Vec<Value> = Kind::ALL
            .iter()
            .map(|k| {
                let (n, bytes) = p.by_kind.get(k).copied().unwrap_or_default();
                let sample: Vec<Value> = p.moves.iter().filter(|(_, to)| to.starts_with(&format!("/{}/", k.folder()))).take(3).map(|(a, b)| json!({ "from": a, "to": b })).collect();
                json!({ "kind": k.id(), "folder": k.folder(), "files": n, "bytes": bytes, "sample": sample })
            })
            .collect();
        Ok(json!({ "total": p.moves.len(), "by_kind": by_kind, "placed": p.placed, "protected": p.protected, "unknown": p.unknown, "capped": p.capped }))
    }

    /// État : passage en cours ou dernier bilan, et passages annulables.
    ///
    /// # Errors
    /// Base indisponible.
    pub async fn status(&self, owner: &str) -> Result<Value, OrganizeError> {
        let o = owner.to_owned();
        let runs: Vec<Value> = self
            .dav
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT run_id, at, moved, undone_at, options FROM organize_runs WHERE owner_hex = ?1 ORDER BY at DESC, run_id LIMIT 5")?;
                let rows = stmt.query_map([o], |r| {
                    Ok(json!({ "run_id": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "moved": r.get::<_, i64>(2)?, "undone": r.get::<_, Option<i64>>(3)?.is_some(), "options": serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(Value::Null) }))
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(|e| OrganizeError::Internal(e.to_string()))?;
        let job = self.jobs().get(owner).cloned().map(|j| {
            let state = if j.outcome.is_none() { "running" } else if matches!(j.outcome, Some(Ok(_))) { "done" } else { "failed" };
            json!({ "state": state, "started": j.started, "finished": j.finished, "progress": j.progress, "report": j.outcome.as_ref().and_then(|o| o.as_ref().ok()), "error": j.outcome.as_ref().and_then(|o| o.as_ref().err()) })
        });
        Ok(json!({ "folders": Kind::ALL.iter().map(|k| json!({ "kind": k.id(), "folder": k.folder() })).collect::<Vec<_>>(), "job": job, "runs": runs }))
    }

    /// Lance un rangement en arrière-plan.
    ///
    /// # Errors
    /// Saisie invalide, rangement déjà en cours ou station chargée.
    pub async fn run(self: &Arc<Self>, owner: &str, opts: Options) -> Result<(), OrganizeError> {
        validate(&opts)?;
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| OrganizeError::Invalid("trop de rangements en cours sur la station : réessayez dans quelques minutes".into()))?;
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut jobs = self.jobs();
            if jobs.get(owner).is_some_and(|j| j.outcome.is_none()) {
                return Err(OrganizeError::Invalid("un rangement est déjà en cours pour ce compte".into()));
            }
            jobs.insert(owner.to_owned(), Job { started: secs(), finished: None, progress: Progress::default(), outcome: None, stop: stop.clone() });
        }
        let (me, owner) = (self.clone(), owner.to_owned());
        tokio::spawn(async move {
            let tracker = me.clone();
            let who = owner.clone();
            let result = async {
                let p = plan(&me.dav.db, &owner, &opts).await?;
                apply(&me.dav, &owner, &opts, &p, Some(&stop), move |pr| {
                    if let Some(j) = tracker.jobs().get_mut(&who) {
                        j.progress = pr;
                    }
                })
                .await
            }
            .await;
            let outcome = result.map(|r| json!({ "run_id": r.run_id, "moved": r.moved, "pruned": r.pruned, "errors": r.errors.len(), "stopped": r.stopped, "first_errors": r.errors.iter().take(5).map(|(p, w)| json!({ "path": p, "why": w })).collect::<Vec<_>>() })).map_err(|e| e.to_string());
            if let Some(bus) = &me.activity {
                let text = match &outcome {
                    Ok(v) => format!("Rangement du uCloud : {} fichier(s) rangé(s), {} échec(s)", v["moved"], v["errors"]),
                    Err(e) => format!("Rangement du uCloud impossible : {e}"),
                };
                bus.publish_to(Audience::Owner(owner.clone()), if outcome.is_ok() { "organize" } else { "alert" }, "organize", text, None);
            }
            if let Some(j) = me.jobs().get_mut(&owner) {
                j.finished = Some(secs());
                j.outcome = Some(outcome);
            }
            drop(permit);
        });
        Ok(())
    }

    /// Demande l'arrêt du rangement en cours (le lot en cours s'achève).
    ///
    /// # Errors
    /// Aucun rangement en cours.
    pub fn stop(&self, owner: &str) -> Result<(), OrganizeError> {
        let jobs = self.jobs();
        let job = jobs.get(owner).filter(|j| j.outcome.is_none()).ok_or_else(|| OrganizeError::Invalid("aucun rangement en cours".into()))?;
        job.stop.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Annule un passage.
    ///
    /// # Errors
    /// Passage inconnu, déjà annulé ou rangement en cours.
    pub async fn undo(&self, owner: &str, run_id: &str) -> Result<usize, OrganizeError> {
        if self.jobs().get(owner).is_some_and(|j| j.outcome.is_none()) {
            return Err(OrganizeError::Invalid("un rangement est en cours : l'arrêter d'abord".into()));
        }
        undo(&self.dav, owner, run_id).await
    }
}
