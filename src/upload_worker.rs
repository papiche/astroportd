//! Traitement des tâches `bro.upload` : rapatrier un document demandé par `#upload`, le chiffrer (UENC v2) et
//! l'ajouter à l'uCloud du demandeur, dans `/Uploads`.
//!
//! La file est la table `tasks` (`kind = 'bro.upload'`, `payload = {owner, source, name}`). Une tâche réclamée passe
//! à `running` (`not_before` y porte l'instant de la réclamation) ; un échec **passager** (réseau) la remet en file
//! avec un délai croissant, trois essais au plus ; un échec **définitif** (adresse interdite, trop gros, quota)
//! la clôt tout de suite. Dans tous les cas le demandeur reçoit une réponse : jamais d'abandon silencieux.
//! Une tâche restée `running` plus de 10 minutes (démon arrêté en route) est reprise.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_dav::blob::{BlobError, BlobStore, is_cid};
use astro_dav::ingest::{IngestError, ingest};
use astro_dav::DavState;
use rusqlite::params;
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::watch;

use crate::fetch::{FetchLimits, fetch};

/// Essais au plus par tâche.
pub const MAX_ATTEMPTS: i64 = 3;
/// Une tâche `running` depuis plus longtemps est considérée comme interrompue.
const STUCK_SECS: i64 = 600;
/// Durée maximale du rapatriement et du dépôt d'un document.
const JOB_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Dossier d'accueil des documents rapatriés.
pub const UPLOAD_DIR: &str = "/Uploads";

/// Prévient un titulaire (message privé de la station) : `(propriétaire, texte)`.
pub type Notify = Arc<dyn Fn(String, String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Issue d'un traitement.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Document ajouté (chemin).
    Done(String),
    /// Remis en file pour un nouvel essai.
    Retry(String),
    /// Abandonné.
    Failed(String),
}

enum Failure {
    Permanent(String),
    Transient(String),
}

/// Le travailleur.
pub struct UploadWorker<B: BlobStore> {
    dav: Arc<DavState<B>>,
    activity: ActivityBus,
    notify: Option<Notify>,
    allow_private: bool,
    extra_roots: Vec<CertificateDer<'static>>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Début de la lecture : refuse un blob déjà chiffré par la station (UENC), qui n'est pas un document lisible.
async fn plain_or_refuse(mut reader: impl AsyncRead + Unpin + Send + 'static) -> Result<Pin<Box<dyn AsyncRead + Send>>, Failure> {
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < head.len() {
        match reader.read(&mut head[got..]).await {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) => return Err(Failure::Transient(format!("lecture du blob : {e}"))),
        }
    }
    if got == 4 && &head == astro_crypto::uenc::MAGIC {
        return Err(Failure::Permanent("ce CID est un blob chiffré de la station (UENC) : donnez le CID d'un document en clair".into()));
    }
    Ok(Box::pin(std::io::Cursor::new(head[..got].to_vec()).chain(reader)))
}

fn url_basename(url: &url::Url) -> Option<String> {
    let last = url.path_segments()?.rfind(|s| !s.is_empty())?;
    Some(percent_encoding::percent_decode_str(last).decode_utf8_lossy().into_owned())
}

impl<B: BlobStore> UploadWorker<B> {
    /// Travailleur sur l'uCloud `dav`.
    pub fn new(dav: Arc<DavState<B>>, activity: ActivityBus, notify: Option<Notify>) -> Self {
        Self { dav, activity, notify, allow_private: false, extra_roots: Vec::new() }
    }

    /// Autorise les adresses locales et ajoute des racines TLS. **Tests seulement.**
    pub fn for_tests(mut self, extra_roots: Vec<CertificateDer<'static>>) -> Self {
        self.allow_private = true;
        self.extra_roots = extra_roots;
        self
    }

    async fn tell(&self, owner: &str, text: String) {
        if let Some(n) = &self.notify {
            n(owner.to_owned(), text).await;
        }
    }

    /// Remet en file les tâches interrompues. Retourne leur nombre.
    pub async fn recover(&self, at: i64) -> usize {
        self.dav.db.writer().execute("UPDATE tasks SET status = 'pending' WHERE kind = 'bro.upload' AND status = 'running' AND not_before < ?1", vec![(at - STUCK_SECS).into()]).await.map_or(0, |o| o.changes)
    }

    /// Traite, une par une, les tâches prêtes à l'instant `at`. Retourne leurs issues.
    pub async fn run_once(&self, at: i64) -> Vec<(i64, Outcome)> {
        let mut out = Vec::new();
        loop {
            let claimed = self
                .dav
                .db
                .writer()
                .transaction(move |tx| {
                    let row: Option<(i64, String, i64)> = rusqlite::OptionalExtension::optional(tx.query_row(
                        "SELECT id, payload, attempts FROM tasks WHERE kind = 'bro.upload' AND status = 'pending' AND (not_before IS NULL OR not_before <= ?1) ORDER BY id LIMIT 1",
                        [at],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    ))?;
                    let Some((id, payload, attempts)) = row else { return Ok(None) };
                    tx.execute("UPDATE tasks SET status = 'running', attempts = attempts + 1, not_before = ?2 WHERE id = ?1", params![id, at])?;
                    Ok(Some((id, payload, attempts + 1)))
                })
                .await;
            let Ok(Some((id, payload, attempts))) = claimed else { break };
            let outcome = self.process(&payload).await;
            out.push((id, self.settle(id, &payload, attempts, at, outcome).await));
        }
        out
    }

    async fn settle(&self, id: i64, payload: &str, attempts: i64, at: i64, result: Result<(String, u64), Failure>) -> Outcome {
        let owner = serde_json::from_str::<serde_json::Value>(payload).ok().and_then(|v| v["owner"].as_str().map(str::to_owned)).unwrap_or_default();
        let finish = |status: &'static str, result: serde_json::Value, not_before: Option<i64>| {
            let db = self.dav.db.clone();
            async move {
                let _ = db
                    .writer()
                    .execute("UPDATE tasks SET status = ?2, not_before = ?3, payload = json_set(payload, '$.result', json(?4)) WHERE id = ?1", vec![id.into(), status.to_owned().into(), not_before.map_or(rusqlite::types::Value::Null, Into::into), result.to_string().into()])
                    .await;
            }
        };
        match result {
            Ok((path, size)) => {
                finish("done", serde_json::json!({ "path": path, "size": size }), None).await;
                self.activity.publish_to(astro_core::Audience::Owner(owner.clone()), "bro", "bro:upload", format!("document ajouté à l'uCloud : {path}"), None);
                self.tell(&owner, format!("✅ Document ajouté à votre uCloud : {path} ({} Ko).", size.div_ceil(1024))).await;
                Outcome::Done(path)
            }
            Err(Failure::Transient(why)) if attempts < MAX_ATTEMPTS => {
                let delay = 60 * attempts * attempts;
                finish("pending", serde_json::json!({ "retry": why, "attempt": attempts }), Some(at + delay)).await;
                self.activity.publish_to(astro_core::Audience::Owner(owner.clone()), "bro", "bro:upload", format!("document : échec passager, nouvel essai dans {delay} s ({why})"), None);
                Outcome::Retry(why)
            }
            Err(Failure::Transient(why) | Failure::Permanent(why)) => {
                finish("failed", serde_json::json!({ "error": why }), None).await;
                self.activity.publish_to(astro_core::Audience::Owner(owner.clone()), "bro", "bro:upload", format!("document refusé : {why}"), None);
                self.tell(&owner, format!("❌ Document non ajouté : {why}")).await;
                Outcome::Failed(why)
            }
        }
    }

    async fn process(&self, payload: &str) -> Result<(String, u64), Failure> {
        let v: serde_json::Value = serde_json::from_str(payload).map_err(|_| Failure::Permanent("tâche illisible".into()))?;
        let (Some(owner), Some(source)) = (v["owner"].as_str(), v["source"].as_str()) else { return Err(Failure::Permanent("tâche incomplète".into())) };
        let wanted_name = v["name"].as_str().filter(|n| !n.trim().is_empty()).map(str::to_owned);
        let account = astro_dav::auth::lookup_by_hex(&self.dav.db, owner).await.ok_or_else(|| Failure::Permanent("compte non hébergé ici".into()))?;
        let max = self.dav.limits_for(&account).await.max_file;

        let job = async {
            if is_cid(source) {
                let (reader, _) = self.dav.blobs.open(source).await.map_err(|e| match e {
                    BlobError::NotFound => Failure::Permanent("ce CID est introuvable (ni sur cette station, ni chez ses pairs)".into()),
                    other => Failure::Transient(other.to_string()),
                })?;
                let body = plain_or_refuse(reader).await?;
                let name = wanted_name.unwrap_or_else(|| source.to_owned());
                ingest(&self.dav, owner, UPLOAD_DIR, &name, None, body).await.map_err(classify_ingest)
            } else {
                let mut limits = FetchLimits::new(max);
                limits.allow_private = self.allow_private;
                limits.extra_roots = self.extra_roots.clone();
                let got = fetch(source, &limits).await.map_err(|e| if e.transient() { Failure::Transient(e.to_string()) } else { Failure::Permanent(e.to_string()) })?;
                let name = wanted_name.or_else(|| url_basename(&got.final_url)).unwrap_or_else(|| "document".into());
                ingest(&self.dav, owner, UPLOAD_DIR, &name, got.content_type.as_deref(), got.body).await.map_err(classify_ingest)
            }
        };
        let done = tokio::time::timeout(JOB_TIMEOUT, job).await.map_err(|_| Failure::Transient("délai dépassé".into()))??;
        Ok((done.path, done.size))
    }

    /// Boucle de service : récupération au démarrage, puis un passage toutes les `every`.
    pub async fn run(self: Arc<Self>, every: Duration, mut stop: watch::Receiver<bool>) {
        let n = self.recover(now()).await;
        if n > 0 {
            self.activity.publish_to(astro_core::Audience::Captain, "bro", "bro:upload", format!("{n} document(s) interrompu(s) repris"), None);
        }
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.changed() => return,
            }
            self.run_once(now()).await;
        }
    }
}

fn classify_ingest(e: IngestError) -> Failure {
    match e {
        IngestError::Source(m) => Failure::Transient(format!("source interrompue : {m}")),
        IngestError::Blob(b) => Failure::Transient(b.to_string()),
        other => Failure::Permanent(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_basenames_are_decoded() {
        let u = url::Url::parse("https://exemple.org/docs/mon%20contrat%C3%A9.pdf?x=1").unwrap();
        assert_eq!(url_basename(&u).as_deref(), Some("mon contraté.pdf"));
        assert_eq!(url_basename(&url::Url::parse("https://exemple.org/").unwrap()), None);
    }
}
