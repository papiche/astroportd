//! Client de relais Nostr pour le crawl : `REQ` jusqu'à `EOSE`, avec réponse au défi NIP-42.

use std::future::Future;
use std::time::Duration;

use astro_nostr_proto::{ClientMessage, Event, Filter, RelayMessage, UnsignedEvent, kind};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;

use tokio_tungstenite::{Connector, connect_async_tls_with_config};
use tokio_tungstenite::tungstenite::Message;
use zeroize::Zeroizing;

/// Erreurs d'un échange avec un relai.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Connexion impossible ou perdue.
    #[error("connexion : {0}")]
    Connection(String),
    /// Délai dépassé.
    #[error("délai dépassé")]
    Timeout,
    /// Le relai a fermé la souscription.
    #[error("souscription fermée par le relai : {0}")]
    Closed(String),
    /// Le relai exige une authentification que nous ne pouvons pas fournir.
    #[error("authentification requise")]
    AuthRequired,
    /// Le relai a refusé notre authentification.
    #[error("authentification refusée : {0}")]
    AuthRejected(String),
    /// Message illisible.
    #[error("message illisible : {0}")]
    Protocol(String),
}

/// Clé de signature de la station pour répondre aux défis NIP-42, effacée de la mémoire à la destruction.
#[derive(Clone)]
pub struct Signer(Zeroizing<[u8; 32]>);

impl Signer {
    /// Signeur à partir d'une clé secrète de 32 octets.
    pub fn new(secret: [u8; 32]) -> Self {
        Self(Zeroizing::new(secret))
    }
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Signer(**)")
    }
}

/// Source d'événements : un relai interrogeable par filtre.
pub trait RelayClient: Send + Sync {
    /// Tous les événements du relai `relay` correspondant à `filter`, du plus récent au plus ancien.
    fn fetch<'a>(&'a self, relay: &'a str, filter: &'a Filter) -> impl Future<Output = Result<Vec<Event>, ClientError>> + Send + 'a;
}

/// Client WebSocket : `ws://` et `wss://` (rustls, racines Mozilla `webpki-roots`).
///
/// Les relais d'une constellation privée à certificat maison se déclarent avec [`WsClient::with_extra_root`].
#[derive(Debug, Clone)]
pub struct WsClient {
    signer: Option<Signer>,
    timeout: Duration,
    extra_roots: Vec<Vec<u8>>,
}

impl WsClient {
    /// Client sans authentification.
    pub fn new(timeout: Duration) -> Self {
        Self { signer: None, timeout, extra_roots: Vec::new() }
    }

    /// Répond aux défis NIP-42 avec `signer`.
    pub fn with_signer(mut self, signer: Signer) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Fait confiance à un certificat racine supplémentaire (DER), en plus des racines publiques.
    pub fn with_extra_root(mut self, der: Vec<u8>) -> Self {
        self.extra_roots.push(der);
        self
    }

    fn connector(&self) -> Result<Connector, ClientError> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for der in &self.extra_roots {
            roots.add(rustls::pki_types::CertificateDer::from(der.clone())).map_err(|e| ClientError::Connection(format!("certificat racine : {e}")))?;
        }
        // Fournisseur explicite : aucune dépendance à un fournisseur installé globalement.
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| ClientError::Connection(format!("TLS : {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Connector::Rustls(Arc::new(config)))
    }

    /// Comme [`RelayClient::fetch`], mais la connexion se fait à `connect` (par exemple l'extrémité locale d'un tunnel
    /// `ipfs p2p`) et la réponse au défi NIP-42 déclare le relai `auth_as` : c'est l'adresse que le relai distant connaît.
    pub async fn fetch_as(&self, connect: &str, auth_as: &str, filter: &Filter) -> Result<Vec<Event>, ClientError> {
        tokio::time::timeout(self.timeout, self.fetch_inner(connect, auth_as, filter)).await.map_err(|_| ClientError::Timeout)?
    }

    async fn fetch_inner(&self, relay: &str, auth_as: &str, filter: &Filter) -> Result<Vec<Event>, ClientError> {
        let (mut ws, _) = connect_async_tls_with_config(relay, None, false, Some(self.connector()?)).await.map_err(|e| ClientError::Connection(e.to_string()))?;
        let req = ClientMessage::Req { sub_id: "crawl".into(), filters: vec![filter.clone()] };
        send(&mut ws, req.to_json()).await?;
        let (mut challenge, mut events, mut authenticated) = (None::<String>, Vec::new(), false);
        loop {
            let frame = ws.next().await.ok_or_else(|| ClientError::Connection("connexion fermée".into()))?;
            let Message::Text(text) = frame.map_err(|e| ClientError::Connection(e.to_string()))? else { continue };
            match RelayMessage::parse(text.as_str()).map_err(|e| ClientError::Protocol(e.to_string()))? {
                RelayMessage::Auth(c) => challenge = Some(c),
                RelayMessage::Event { event, .. } => events.push(*event),
                RelayMessage::Eose(_) => {
                    let _ = send(&mut ws, ClientMessage::Close("crawl".into()).to_json()).await;
                    let _ = ws.close(None).await;
                    return Ok(events);
                }
                RelayMessage::Closed { message, .. } if message.starts_with("auth-required") && !authenticated => {
                    let (Some(signer), Some(challenge)) = (&self.signer, &challenge) else { return Err(ClientError::AuthRequired) };
                    authenticate(&mut ws, signer, auth_as, challenge).await?;
                    authenticated = true;
                    events.clear();
                    send(&mut ws, ClientMessage::Req { sub_id: "crawl".into(), filters: vec![filter.clone()] }.to_json()).await?;
                }
                RelayMessage::Closed { message, .. } => return Err(ClientError::Closed(message)),
                RelayMessage::Notice(_) | RelayMessage::Ok { .. } => {}
            }
        }
    }
}

impl WsClient {
    /// Publie `event` sur `relay` et attend l'accusé `OK` : `(accepté, message)`.
    ///
    /// Un gift wrap (kind 1059) vers un MULTIPASS hébergé par le relai est accepté sans authentification.
    pub async fn publish(&self, relay: &str, event: &Event) -> Result<(bool, String), ClientError> {
        let work = async {
            let (mut ws, _) = connect_async_tls_with_config(relay, None, false, Some(self.connector()?)).await.map_err(|e| ClientError::Connection(e.to_string()))?;
            send(&mut ws, ClientMessage::Event(event.clone()).to_json()).await?;
            loop {
                let frame = ws.next().await.ok_or_else(|| ClientError::Connection("connexion fermée".into()))?;
                let Message::Text(text) = frame.map_err(|e| ClientError::Connection(e.to_string()))? else { continue };
                if let Ok(RelayMessage::Ok { event_id, accepted, message }) = RelayMessage::parse(text.as_str()) {
                    if event_id == event.id {
                        let _ = ws.close(None).await;
                        return Ok((accepted, message));
                    }
                }
            }
        };
        tokio::time::timeout(self.timeout, work).await.map_err(|_| ClientError::Timeout)?
    }
}

impl RelayClient for WsClient {
    async fn fetch(&self, relay: &str, filter: &Filter) -> Result<Vec<Event>, ClientError> {
        tokio::time::timeout(self.timeout, self.fetch_inner(relay, relay, filter)).await.map_err(|_| ClientError::Timeout)?
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send(ws: &mut Socket, text: String) -> Result<(), ClientError> {
    ws.send(Message::text(text)).await.map_err(|e| ClientError::Connection(e.to_string()))
}

async fn authenticate(ws: &mut Socket, signer: &Signer, relay: &str, challenge: &str) -> Result<(), ClientError> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let event = UnsignedEvent {
        created_at: now,
        kind: kind::AUTH,
        tags: vec![vec!["relay".into(), relay.into()], vec!["challenge".into(), challenge.into()]],
        content: String::new(),
    }
    .sign(&signer.0)
    .map_err(|e| ClientError::Protocol(e.to_string()))?;
    let id = event.id.clone();
    send(ws, ClientMessage::Auth(event).to_json()).await?;
    loop {
        let frame = ws.next().await.ok_or_else(|| ClientError::Connection("connexion fermée".into()))?;
        let Message::Text(text) = frame.map_err(|e| ClientError::Connection(e.to_string()))? else { continue };
        if let Ok(RelayMessage::Ok { event_id, accepted, message }) = RelayMessage::parse(text.as_str()) {
            if event_id == id {
                return if accepted { Ok(()) } else { Err(ClientError::AuthRejected(message)) };
            }
        }
    }
}
