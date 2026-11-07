//! Questions courtes entre stations : un pair demande, la station répond **selon qui demande** (son PeerID, prouvé par la
//! connexion chiffrée). Sert par exemple à « quels services puis-je ouvrir chez toi ? » : la réponse dépend de la politique
//! de la station interrogée, rien n'est publié.
//!
//! Format : le demandeur ouvre un flux et ne dit rien ; la station écrit la longueur (4 octets, gros-boutiste) puis la
//! réponse, et ferme. Réponse bornée à [`MAX_ANSWER`] ; au plus [`MAX_CONCURRENT`] réponses calculées à la fois.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
use libp2p::{PeerId, StreamProtocol};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::{P2pError, P2pHandle};

/// Taille maximale d'une réponse.
pub const MAX_ANSWER: usize = 64 * 1024;
/// Réponses calculées en même temps, au plus (les autres demandes sont refermées sans réponse).
pub const MAX_CONCURRENT: usize = 16;
/// Temps laissé à la station pour calculer sa réponse.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// Calcul de la réponse pour un demandeur.
pub type AnswerFn = Arc<dyn Fn(PeerId) -> Pin<Box<dyn Future<Output = Vec<u8>> + Send>> + Send + Sync>;

/// Serveur de réponses : s'arrête à la destruction.
pub struct Answers {
    task: JoinHandle<()>,
}

impl Drop for Answers {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Répond aux pairs qui ouvrent `protocol` (ex. `/astroport/services/1`).
pub fn answer(handle: &P2pHandle, protocol: &'static str, f: AnswerFn) -> Result<Answers, P2pError> {
    let mut incoming = handle.control().accept(StreamProtocol::new(protocol)).map_err(|e| P2pError::Stream(e.to_string()))?;
    let limit = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let task = tokio::spawn(async move {
        while let Some((peer, mut stream)) = incoming.next().await {
            let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
            let f = f.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let Ok(mut body) = tokio::time::timeout(ANSWER_TIMEOUT, f(peer)).await else { return };
                body.truncate(MAX_ANSWER);
                let len = u32::try_from(body.len()).unwrap_or(0).to_be_bytes();
                // Un demandeur parti avant la fin n'est pas une erreur de la station : rien à signaler.
                if stream.write_all(&len).await.is_ok() && stream.write_all(&body).await.is_ok() {
                    let _ = stream.close().await;
                }
            });
        }
    });
    Ok(Answers { task })
}

/// Pose la question `protocol` à `peer` et rend sa réponse.
pub async fn ask(handle: &P2pHandle, peer: PeerId, protocol: &'static str, timeout: Duration) -> Result<Vec<u8>, String> {
    let work = async {
        let mut control = handle.control();
        let mut stream = control.open_stream(peer, StreamProtocol::new(protocol)).await.map_err(|e| format!("station injoignable : {e}"))?;
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).await.map_err(|e| format!("pas de réponse : {e}"))?;
        let len = usize::try_from(u32::from_be_bytes(len)).unwrap_or(usize::MAX);
        if len > MAX_ANSWER {
            return Err(format!("réponse annoncée trop grande ({len} octets)"));
        }
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).await.map_err(|e| format!("réponse tronquée : {e}"))?;
        Ok(body)
    };
    tokio::time::timeout(timeout, work).await.unwrap_or_else(|_| Err("délai dépassé".to_owned()))
}
