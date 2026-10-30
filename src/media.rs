//! Dépôt de films et de séries : copie privée attestée, sondage, encodage H.264/AAC, uCloud chiffré, événement NIP-71.
//!
//! Chaîne d'un dépôt (héritage d'`ajouter_media.sh`), chaque étape refuse en le disant :
//!
//! 1. **attestation de copie privée** du titulaire (kind 31903, projetée dans `private_copy_consent`) ;
//! 2. **plafond frugal** de 1 024 Mo ;
//! 3. **sondage** (`ffprobe`) puis plan d'encodage : un fichier déjà MP4/H.264/AAC n'est jamais réencodé ;
//! 4. **encodage** (`ffmpeg`, crochet externe) en mode `Auto` ; en mode `Notify` le fichier est déposé tel quel et
//!    un réencodage est recommandé sur le bus d'activité ; la piste audio française est conservée ;
//! 5. dépôt **chiffré** (UENC v2) dans `/Vidéos/Films` ou `/Vidéos/Séries/<série>/Saison NN` ;
//! 6. publication d'un événement **kind 21 / 22**, signé du titulaire, réservé à son cercle N¹ (politique du relai).
//!
//! Les outils externes tournent sous `kill_on_drop` avec un délai ; jamais de shell, jamais de `fork` du démon.
//! Le fichier encodé transite en clair dans le dossier de travail (0700) le temps du dépôt, puis est supprimé.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use astro_bro::publisher::Publisher;
use astro_core::ActivityBus;
use astro_crypto::KeySource;
use astro_dav::blob::BlobStore;
use astro_dav::ingest::{IngestError, ingest};
use astro_dav::media::{self, MediaError, MediaMeta, Probe, VideoRef};
use astro_dav::vfs;
use astro_dav::DavState;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use astro_sync::{Ingested, ingest_event};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Délai d'un sondage.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// Délai d'un encodage (long métrage sur un petit processeur).
const ENCODE_TIMEOUT: Duration = Duration::from_secs(4 * 3600);

/// Que faire d'un fichier qui n'est pas en H.264/AAC ?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transcode {
    /// Réencoder avant le dépôt.
    Auto,
    /// Déposer tel quel et recommander l'encodage.
    Notify,
}

/// Outils externes.
#[derive(Debug, Clone)]
pub struct Tools {
    /// `ffprobe`.
    pub ffprobe: String,
    /// `ffmpeg`.
    pub ffmpeg: String,
    /// Délai du sondage.
    pub probe_timeout: Duration,
    /// Délai de l'encodage.
    pub encode_timeout: Duration,
}

impl Default for Tools {
    fn default() -> Self {
        Self { ffprobe: "ffprobe".into(), ffmpeg: "ffmpeg".into(), probe_timeout: PROBE_TIMEOUT, encode_timeout: ENCODE_TIMEOUT }
    }
}

/// Échecs d'un dépôt, tous dits en clair.
#[derive(Debug, thiserror::Error)]
pub enum MediaFailure {
    /// Pas d'attestation de copie privée.
    #[error("attestation de copie privée absente : le titulaire doit d'abord attester (article L. 122-5 du CPI)")]
    NoConsent,
    /// Clé du titulaire absente de cette station.
    #[error("clé du compte introuvable sur cette station")]
    NoKey,
    /// Fichier source illisible.
    #[error("fichier source : {0}")]
    Source(String),
    /// Règle média.
    #[error("{0}")]
    Media(#[from] MediaError),
    /// Outil externe en échec.
    #[error("{0}")]
    Tool(String),
    /// Déjà déposé.
    #[error("déjà déposé : {0}")]
    Duplicate(String),
    /// Dépôt uCloud.
    #[error("{0}")]
    Ingest(#[from] IngestError),
    /// Base ou événement.
    #[error("{0}")]
    Internal(String),
}

/// Résultat d'un dépôt.
#[derive(Debug, Clone)]
pub struct MediaReport {
    /// Chemin dans l'uCloud.
    pub path: String,
    /// Kind publié (21 ou 22).
    pub kind: u32,
    /// Identifiant de l'événement vidéo.
    pub event_id: String,
    /// Octets déposés.
    pub size: u64,
    /// Le fichier a été réencodé.
    pub transcoded: bool,
    /// Remarques (encodage recommandé, piste française absente, publication distante…).
    pub notes: Vec<String>,
}

/// Le dépôt de médias d'une station.
pub struct MediaPipeline<B: BlobStore, P: Publisher> {
    /// État WebDAV (base, blobs, limites).
    pub dav: Arc<DavState<B>>,
    /// Clés des comptes hébergés.
    pub keys: Arc<dyn KeySource>,
    /// Publication distante (relai local + relais) ; facultative.
    pub publisher: Option<Arc<P>>,
    /// Journal d'activité.
    pub activity: Option<ActivityBus>,
    /// Outils externes.
    pub tools: Tools,
    /// Politique d'encodage.
    pub mode: Transcode,
    /// Dossier de travail (0700).
    pub work_dir: PathBuf,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Efface le fichier de travail quoi qu'il arrive.
struct Scratch(Option<PathBuf>);

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

async fn run_tool(bin: &str, args: &[String], timeout: Duration) -> Result<Vec<u8>, MediaFailure> {
    let child = Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| MediaFailure::Tool(format!("{bin} introuvable ou non exécutable : {e}")))?;
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| MediaFailure::Tool(format!("{bin} : délai de {} s dépassé, processus arrêté", timeout.as_secs())))?
        .map_err(|e| MediaFailure::Tool(format!("{bin} : {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(MediaFailure::Tool(format!("{bin} a échoué ({}) : {}", out.status, tail.trim())));
    }
    Ok(out.stdout)
}

fn mime_of(ext: &str) -> &'static str {
    match ext {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

impl<B: BlobStore, P: Publisher> MediaPipeline<B, P> {
    fn say(&self, kind: &str, text: String) {
        if let Some(bus) = &self.activity {
            bus.publish(kind, "media", text, None);
        }
    }

    /// Le titulaire a-t-il attesté la copie privée ?
    pub async fn has_consent(&self, owner: &str) -> Result<bool, MediaFailure> {
        let owner = owner.to_owned();
        let min = i64::from(media::CONSENT_VERSION);
        self.dav
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM private_copy_consent WHERE owner_hex = ?1 AND version >= ?2)", rusqlite::params![owner, min], |r| r.get::<_, bool>(0))?))
            .await
            .map_err(|e| MediaFailure::Internal(e.to_string()))
    }

    /// Enregistre l'attestation du titulaire : événement 31903 signé de sa clé, stocké, projeté et publié.
    pub async fn record_consent(&self, owner: &str) -> Result<Event, MediaFailure> {
        let secret = self.keys.secret(owner).ok_or(MediaFailure::NoKey)?;
        let at = now();
        let event = UnsignedEvent { created_at: at, kind: kind::APP_PARAMS, tags: media::consent_tags(), content: media::consent_content(at) }.sign(&secret).map_err(|e| MediaFailure::Internal(e.to_string()))?;
        self.store_and_publish(&event, owner, &mut Vec::new()).await?;
        self.say("media", "attestation de copie privée enregistrée (CPI L. 122-5)".into());
        Ok(event)
    }

    async fn store_and_publish(&self, event: &Event, owner: &str, notes: &mut Vec<String>) -> Result<(), MediaFailure> {
        match ingest_event(self.dav.db.writer(), event.clone(), owner).await.map_err(|e| MediaFailure::Internal(e.to_string()))? {
            Ingested::Inserted | Ingested::Duplicate | Ingested::Superseded => {}
            Ingested::Rejected(why) => return Err(MediaFailure::Internal(format!("événement refusé : {why}"))),
        }
        if let Some(p) = &self.publisher {
            if let Err(e) = p.publish(event.clone()).await {
                notes.push(format!("publication distante impossible : {e}"));
            }
        }
        Ok(())
    }

    async fn probe(&self, path: &Path) -> Result<Probe, MediaFailure> {
        let out = run_tool(&self.tools.ffprobe, &media::ffprobe_args(&path.to_string_lossy()), self.tools.probe_timeout).await?;
        Ok(media::parse_ffprobe(&String::from_utf8_lossy(&out))?)
    }

    /// Dépose et publie `source` sous les métadonnées `meta`, pour le compte `owner`.
    pub async fn submit(&self, owner: &str, source: &Path, meta: &MediaMeta) -> Result<MediaReport, MediaFailure> {
        if !self.has_consent(owner).await? {
            return Err(MediaFailure::NoConsent);
        }
        let secret = self.keys.secret(owner).ok_or(MediaFailure::NoKey)?;
        let size = tokio::fs::metadata(source).await.map_err(|e| MediaFailure::Source(format!("{} : {e}", source.display())))?.len();
        media::check_size(size)?;
        let mut notes = Vec::new();
        let mut probe = self.probe(source).await?;
        let plan = media::plan_encoding(&probe);
        if plan.no_french_audio {
            notes.push("aucune piste audio française : la première piste est conservée".into());
        }

        // Encodage (crochet externe) ou recommandation.
        let mut upload: PathBuf = source.to_owned();
        let mut scratch = Scratch(None);
        let mut transcoded = false;
        let mut ext = source.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).filter(|e| !e.is_empty() && e.len() <= 5 && e.bytes().all(|b| b.is_ascii_alphanumeric())).unwrap_or_else(|| "mp4".into());
        if plan.needs_transcode() {
            let why = plan.reasons.join(" ; ");
            match self.mode {
                Transcode::Auto => {
                    self.say("media", format!("encodage H.264/AAC de « {} » ({why})", meta.display_title()));
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::DirBuilderExt;
                        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.work_dir).map_err(|e| MediaFailure::Internal(format!("dossier de travail : {e}")))?;
                    }
                    #[cfg(not(unix))]
                    std::fs::create_dir_all(&self.work_dir).map_err(|e| MediaFailure::Internal(format!("dossier de travail : {e}")))?;
                    let out = self.work_dir.join(format!("media-{}-{}.mp4", std::process::id(), now()));
                    scratch = Scratch(Some(out.clone()));
                    run_tool(&self.tools.ffmpeg, &media::ffmpeg_args(&source.to_string_lossy(), &out.to_string_lossy(), &plan), self.tools.encode_timeout).await?;
                    let new_size = tokio::fs::metadata(&out).await.map_err(|e| MediaFailure::Tool(format!("sortie de ffmpeg absente : {e}")))?.len();
                    media::check_size(new_size)?;
                    probe = self.probe(&out).await?;
                    upload = out;
                    ext = "mp4".into();
                    transcoded = true;
                    notes.push(format!("réencodé en H.264/AAC ({why})"));
                }
                Transcode::Notify => {
                    let msg = format!("encodage optimisé H.264/AAC recommandé pour « {} » : {why}", meta.display_title());
                    self.say("media", msg.clone());
                    notes.push(msg);
                }
            }
        }

        // Déjà déposé ? (même contenu, même titulaire)
        let (sha, final_size) = sha256_file(&upload).await?;
        {
            let (o, s) = (owner.to_owned(), sha.clone());
            let existing: Option<String> = self.dav.db.readers().read(move |c| Ok(c.query_row("SELECT path FROM ucloud_entries WHERE owner_hex = ?1 AND sha_plain = ?2 AND type = 'file' LIMIT 1", rusqlite::params![o, s], |r| r.get(0)).ok())).await.map_err(|e| MediaFailure::Internal(e.to_string()))?;
            if let Some(path) = existing {
                return Err(MediaFailure::Duplicate(path));
            }
        }

        // Dépôt chiffré ; les dossiers parents sont créés un à un.
        let folder = vfs::normalize_path(&meta.folder()).map_err(|e| MediaFailure::Internal(e.to_string()))?;
        let mut cur = String::new();
        for part in folder.split('/').filter(|p| !p.is_empty()) {
            cur = format!("{cur}/{part}");
            let (o, d) = (owner.to_owned(), cur.clone());
            vfs::vfs_tx(&self.dav.db, move |tx| match vfs::mkdir_tx(tx, &o, &d, i64::try_from(now()).unwrap_or(0)) {
                Ok(()) | Err(vfs::VfsError::Exists) => Ok(()),
                Err(e) => Err(e),
            })
            .await
            .map_err(|e| MediaFailure::Internal(format!("dossier {cur} : {e}")))?;
        }
        let file = tokio::fs::File::open(&upload).await.map_err(|e| MediaFailure::Source(e.to_string()))?;
        let mime = mime_of(&ext);
        let done = ingest(&self.dav, owner, &folder, &meta.file_name(&ext), Some(mime), file).await?;
        drop(scratch);

        // Événement vidéo.
        let (o, p) = (owner.to_owned(), done.path.clone());
        let cid: String = self.dav.db.readers().read(move |c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", rusqlite::params![o, p], |r| r.get(0))?)).await.map_err(|e| MediaFailure::Internal(e.to_string()))?;
        let vref = VideoRef { cid, sha256: sha, size: final_size, mime: done.mime.clone(), probe, published_at: now() };
        let (video_kind, tags, content) = media::video_event(meta, &vref);
        let event = UnsignedEvent { created_at: now(), kind: video_kind, tags, content }.sign(&secret).map_err(|e| MediaFailure::Internal(e.to_string()))?;
        self.store_and_publish(&event, owner, &mut notes).await?;
        self.say("media", format!("« {} » déposé dans {} et publié (kind {video_kind}, cercle N¹)", meta.display_title(), done.path));
        Ok(MediaReport { path: done.path, kind: video_kind, event_id: event.id, size: final_size, transcoded, notes })
    }
}

async fn sha256_file(path: &Path) -> Result<(String, u64), MediaFailure> {
    let mut f = tokio::fs::File::open(path).await.map_err(|e| MediaFailure::Source(format!("{} : {e}", path.display())))?;
    let (mut hasher, mut total, mut buf) = (Sha256::new(), 0u64, vec![0u8; 1 << 20]);
    loop {
        let n = f.read(&mut buf).await.map_err(|e| MediaFailure::Source(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), total))
}

/// Publication sur des relais distants (commande `astroportd media`, sans démon) ; réussie dès qu'un relai accepte.
pub struct RelaysPublisher {
    /// Client WebSocket (NIP-42 avec la clé de la station).
    pub client: astro_sync::WsClient,
    /// Relais cibles.
    pub relays: Vec<String>,
}

impl Publisher for RelaysPublisher {
    async fn publish(&self, event: Event) -> Result<(), String> {
        if self.relays.is_empty() {
            return Ok(()); // l'événement est déjà sur le relai local de la base
        }
        let mut notes = Vec::new();
        for url in &self.relays {
            match self.client.publish(url, &event).await {
                Ok((true, _)) => return Ok(()),
                Ok((false, why)) => notes.push(format!("{url} : {why}")),
                Err(e) => notes.push(format!("{url} : {e}")),
            }
        }
        Err(notes.join(" ; "))
    }
}
