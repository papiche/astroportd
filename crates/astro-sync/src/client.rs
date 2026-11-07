//! Client de relais Nostr pour le crawl : `REQ` jusqu'à `EOSE`, avec réponse au défi NIP-42.
//!
//! Une [`RelaySession`] garde la connexion (WebSocket, TLS, authentification NIP-42) ouverte d'une page à l'autre : le
//! crawl d'une constellation ouvre une connexion par relai et par passage, plus une par clé et par page.

use std::future::Future;
use std::pin::Pin;
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

/// Futur en boîte, envoyable entre fils (les sessions sont des objets dynamiques).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Connexion à un relai, réutilisée pour plusieurs `REQ` successifs (une souscription à la fois).
pub trait RelaySession: Send {
    /// Comme [`RelayClient::fetch`], sur la connexion de la session.
    fn fetch<'s>(&'s mut self, filter: &'s Filter) -> BoxFuture<'s, Result<Vec<Event>, ClientError>>;
}

/// Source d'événements : un relai interrogeable par filtre.
pub trait RelayClient: Send + Sync {
    /// Tous les événements du relai `relay` correspondant à `filter`, du plus récent au plus ancien.
    fn fetch<'a>(&'a self, relay: &'a str, filter: &'a Filter) -> impl Future<Output = Result<Vec<Event>, ClientError>> + Send + 'a;

    /// Session vers `relay` pour plusieurs requêtes. Par défaut, chaque requête repasse par [`RelayClient::fetch`]
    /// (sources sans connexion durable : relais simulés, maillage) ; [`WsClient`] garde, lui, une seule connexion.
    fn session<'a>(&'a self, relay: &'a str) -> impl Future<Output = Result<Box<dyn RelaySession + 'a>, ClientError>> + Send + 'a {
        async move { Ok(Box::new(PerCall { client: self, relay }) as Box<dyn RelaySession + 'a>) }
    }
}

/// Session sans connexion durable : une requête = un [`RelayClient::fetch`].
struct PerCall<'a, C: ?Sized> {
    client: &'a C,
    relay: &'a str,
}

impl<C: RelayClient + ?Sized> RelaySession for PerCall<'_, C> {
    fn fetch<'s>(&'s mut self, filter: &'s Filter) -> BoxFuture<'s, Result<Vec<Event>, ClientError>> {
        Box::pin(self.client.fetch(self.relay, filter))
    }
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
        let work = async {
            let mut session = self.connect_session(connect, auth_as).await?;
            let events = session.req(filter).await?;
            session.close().await;
            Ok(events)
        };
        tokio::time::timeout(self.timeout, work).await.map_err(|_| ClientError::Timeout)?
    }

    /// Ouvre une session durable vers `connect` (NIP-42 au nom de `auth_as`, voir [`WsClient::fetch_as`]) : la
    /// connexion est établie tout de suite, pour qu'un appelant puisse choisir une autre voie si elle échoue.
    pub async fn open_session_as(&self, connect: &str, auth_as: &str) -> Result<WsSession<'_>, ClientError> {
        tokio::time::timeout(self.timeout, self.connect_session(connect, auth_as)).await.map_err(|_| ClientError::Timeout)?
    }

    async fn connect_session(&self, connect: &str, auth_as: &str) -> Result<WsSession<'_>, ClientError> {
        let (ws, _) = connect_async_tls_with_config(connect, None, false, Some(self.connector()?)).await.map_err(|e| ClientError::Connection(e.to_string()))?;
        Ok(WsSession { client: self, auth_as: auth_as.to_owned(), ws, challenge: None, authenticated: false, next_sub: 0, broken: None })
    }
}

/// Connexion WebSocket durable vers un relai : les `REQ` s'y succèdent (souscriptions `crawl-1`, `crawl-2`…, chacune
/// fermée à son `EOSE`) et l'authentification NIP-42 n'est faite qu'une fois.
pub struct WsSession<'a> {
    client: &'a WsClient,
    auth_as: String,
    ws: Socket,
    challenge: Option<String>,
    authenticated: bool,
    next_sub: u64,
    /// Raison pour laquelle la connexion n'est plus utilisable (délai dépassé au milieu d'une souscription, envoi
    /// impossible) : les requêtes suivantes échouent avec elle au lieu de lire un flux désynchronisé.
    broken: Option<String>,
}

impl std::fmt::Debug for WsSession<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsSession").field("auth_as", &self.auth_as).field("authenticated", &self.authenticated).field("next_sub", &self.next_sub).finish_non_exhaustive()
    }
}

impl WsSession<'_> {
    /// Un `REQ` jusqu'à son `EOSE`, borné par le délai du client.
    pub async fn fetch(&mut self, filter: &Filter) -> Result<Vec<Event>, ClientError> {
        if let Some(reason) = &self.broken {
            return Err(ClientError::Connection(format!("connexion inutilisable : {reason}")));
        }
        let outcome = match tokio::time::timeout(self.client.timeout, self.req(filter)).await {
            Ok(r) => r,
            Err(_) => Err(ClientError::Timeout),
        };
        // Une souscription interrompue laisse des messages en vol : la connexion n'est plus fiable.
        if let Err(e) = &outcome
            && !matches!(e, ClientError::Closed(_) | ClientError::AuthRequired | ClientError::AuthRejected(_))
        {
            self.broken = Some(e.to_string());
        }
        outcome
    }

    async fn req(&mut self, filter: &Filter) -> Result<Vec<Event>, ClientError> {
        self.next_sub += 1;
        let sub = format!("crawl-{}", self.next_sub);
        send(&mut self.ws, ClientMessage::Req { sub_id: sub.clone(), filters: vec![filter.clone()] }.to_json()).await?;
        let mut events = Vec::new();
        loop {
            let frame = self.ws.next().await.ok_or_else(|| ClientError::Connection("connexion fermée".into()))?;
            let Message::Text(text) = frame.map_err(|e| ClientError::Connection(e.to_string()))? else { continue };
            match RelayMessage::parse(text.as_str()).map_err(|e| ClientError::Protocol(e.to_string()))? {
                RelayMessage::Auth(c) => self.challenge = Some(c),
                RelayMessage::Event { sub_id, event } if sub_id == sub => events.push(*event),
                RelayMessage::Eose(sub_id) if sub_id == sub => {
                    // La souscription est close de notre côté ; un échec d'envoi rend la connexion inutilisable, ce
                    // que la requête suivante dira (les événements de celle-ci sont complets).
                    if let Err(e) = send(&mut self.ws, ClientMessage::Close(sub).to_json()).await {
                        self.broken = Some(e.to_string());
                    }
                    return Ok(events);
                }
                RelayMessage::Closed { sub_id, message } if sub_id == sub && message.starts_with("auth-required") && !self.authenticated => {
                    let (Some(signer), Some(challenge)) = (&self.client.signer, &self.challenge) else { return Err(ClientError::AuthRequired) };
                    authenticate(&mut self.ws, signer, &self.auth_as, challenge).await?;
                    self.authenticated = true;
                    events.clear();
                    send(&mut self.ws, ClientMessage::Req { sub_id: sub.clone(), filters: vec![filter.clone()] }.to_json()).await?;
                }
                RelayMessage::Closed { sub_id, message } if sub_id == sub => return Err(ClientError::Closed(message)),
                // Restes d'une souscription précédente (événements en direct arrivés avant notre `CLOSE`).
                RelayMessage::Event { .. } | RelayMessage::Eose(_) | RelayMessage::Closed { .. } => {}
                RelayMessage::Notice(_) | RelayMessage::Ok { .. } | RelayMessage::Count { .. } => {}
            }
        }
    }

    /// Ferme poliment la connexion. Les données sont déjà reçues : un échec ici (relai parti entre-temps) ne change
    /// rien au résultat, le socket est libéré de toute façon.
    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }
}

impl RelaySession for WsSession<'_> {
    fn fetch<'s>(&'s mut self, filter: &'s Filter) -> BoxFuture<'s, Result<Vec<Event>, ClientError>> {
        Box::pin(WsSession::fetch(self, filter))
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
        self.fetch_as(relay, relay, filter).await
    }

    async fn session<'a>(&'a self, relay: &'a str) -> Result<Box<dyn RelaySession + 'a>, ClientError> {
        Ok(Box::new(self.open_session_as(relay, relay).await?))
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
