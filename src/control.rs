//! Prise de contrôle locale du démon : la ligne de commande (`astroportd tunnel …`, `astroportd route …`) parle au démon
//! qui tourne, plutôt que de lancer un second nœud P2P (autre PeerID, aucune place chez les relais, aucun accès partagé).
//!
//! Un socket Unix `<dossier de la base>/control.sock`, **sans HTTP ni jeton** : l'accès est celui du système de fichiers.
//! Une ligne JSON en entrée, `{ "section": "tunnel", "body": {…} | null }`, une ligne en sortie, `{ "ok": … }` ou
//! `{ "error": "…" }`. Seules les sections déclarées au démarrage répondent (jamais la console, les clés, les fonds). Une
//! connexion est refusée si son propriétaire n'est pas celui du dossier de la station, même si les droits du fichier
//! avaient été élargis. Ce n'est pas un second port : la règle du port unique (relai, WebDAV, API) reste vraie.

use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::ui_backend::AdminFn;

/// Taille maximale d'une demande.
const MAX_REQUEST: u64 = 64 * 1024;
/// Délai pour recevoir la demande, puis pour la traiter (une demande de services interroge des stations distantes).
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WORK_TIMEOUT: Duration = Duration::from_secs(60);

/// Emplacement du socket pour la base `db`.
pub fn socket_path(db: &Path) -> PathBuf {
    db.parent().map_or_else(|| PathBuf::from("control.sock"), |d| d.join("control.sock"))
}

/// Écoute sur `path` jusqu'à ce que la tâche rendue soit abandonnée. Remplace un socket resté d'une exécution précédente
/// (jamais un autre fichier) ; le retire à l'arrêt propre.
pub fn serve(path: PathBuf, sections: BTreeMap<&'static str, AdminFn>) -> Result<Control, String> {
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type()) {
            return Err(format!("{} existe et n'est pas un socket : non remplacé", path.display()));
        }
        std::fs::remove_file(&path).map_err(|e| format!("{} : {e}", path.display()))?;
    }
    let listener = UnixListener::bind(&path).map_err(|e| format!("{} : {e}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| format!("{} : {e}", path.display()))?;
    let owner = std::fs::metadata(path.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?.uid();
    let sections = std::sync::Arc::new(sections);
    Ok(Control(tokio::spawn(async move {
        let _guard = Remove(path);
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let sections = sections.clone();
            tokio::spawn(async move {
                let allowed = stream.peer_cred().is_ok_and(|c| c.uid() == owner);
                let (read, mut write) = stream.into_split();
                let reply = if allowed { handle(read, &sections).await } else { Err("connexion refusée : propriétaire différent".to_owned()) };
                let line = match reply {
                    Ok(v) => json!({ "ok": v }),
                    Err(e) => json!({ "error": e }),
                };
                let _ = write.write_all(format!("{line}\n").as_bytes()).await;
                let _ = write.shutdown().await;
            });
        }
    })))
}

/// Écoute en cours : l'abandonner l'arrête et retire le socket.
pub struct Control(tokio::task::JoinHandle<()>);

impl Drop for Control {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Remove(PathBuf);

impl Drop for Remove {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn handle(stream: tokio::net::unix::OwnedReadHalf, sections: &BTreeMap<&'static str, AdminFn>) -> Result<Value, String> {
    let mut line = String::new();
    let read = tokio::time::timeout(READ_TIMEOUT, BufReader::new(stream.take(MAX_REQUEST)).read_line(&mut line)).await;
    match read {
        Ok(Ok(n)) if n > 0 && line.ends_with('\n') => {}
        Ok(Ok(_)) => return Err("demande vide ou trop longue".into()),
        Ok(Err(e)) => return Err(format!("lecture : {e}")),
        Err(_) => return Err("demande trop lente".into()),
    }
    let req: Value = serde_json::from_str(&line).map_err(|e| format!("JSON illisible : {e}"))?;
    let section = req["section"].as_str().unwrap_or_default();
    let f = sections.get(section).ok_or_else(|| format!("section « {section} » inconnue (attendu : {})", sections.keys().copied().collect::<Vec<_>>().join(", ")))?;
    let body = req.get("body").filter(|b| !b.is_null()).cloned();
    tokio::time::timeout(WORK_TIMEOUT, f(section.to_owned(), body)).await.map_err(|_| "délai dépassé".to_owned())?
}

/// Côté client : envoie `section`/`body` au démon et rend sa réponse.
pub async fn call(socket: &Path, section: &str, body: Option<Value>) -> Result<Value, String> {
    let mut stream = UnixStream::connect(socket).await.map_err(|e| format!("{} : {e} — astroportd serve tourne-t-il (même utilisateur) ?", socket.display()))?;
    let request = json!({ "section": section, "body": body });
    stream.write_all(format!("{request}\n").as_bytes()).await.map_err(|e| e.to_string())?;
    let mut out = String::new();
    stream.take(8 * 1024 * 1024).read_to_string(&mut out).await.map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(out.trim()).map_err(|_| "réponse illisible du démon".to_owned())?;
    match v.get("ok") {
        Some(ok) => Ok(ok.clone()),
        None => Err(v["error"].as_str().unwrap_or("erreur inconnue").to_owned()),
    }
}
