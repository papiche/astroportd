//! Client de relais Nostr qui passe par le maillage : `ipfs p2p`, l'astuce de DRAGON.
//!
//! Une cible `p2p://<PeerID>` ouvre (une fois) un tunnel `/x/strfry-<PeerID>` vers une extrémité locale
//! `127.0.0.1:<port>` puis parle WebSocket à travers. L'adresse publique du relai (`p2p://<PeerID>|wss://…`), quand
//! elle existe, ne sert que de repli **après un échec de connexion** : un refus d'authentification est la réponse du
//! relai, la changer de route n'y changerait rien. Chaque cible garde la dernière voie qui a servi, ou l'erreur : la
//! constellation affiche ainsi « par le maillage », « par l'adresse publique » ou « injoignable » sans rien deviner.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;

use astro_nostr_proto::{Event, Filter};
use astro_p2p::tunnel::{Forward, forward};
use astro_p2p::P2pHandle;
use astro_sync::peers::{RELAY_SERVICE, parse_target};
use astro_sync::{ClientError, RelayClient, WsClient};
use astro_p2p::PeerId;

/// Dernière voie empruntée vers une cible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteState {
    /// Cible demandée.
    pub target: String,
    /// `p2p` (tunnel du maillage), `wss` (adresse publique, repli), `direct` (adresse ordinaire) ou `down`.
    pub via: &'static str,
    /// Secondes UNIX du dernier essai.
    pub at: u64,
    /// Raison du dernier échec (de la voie principale quand le repli a servi).
    pub error: Option<String>,
}

impl RouteState {
    /// Représentation pour l'interface.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "target": self.target, "via": self.via, "at": self.at, "error": self.error })
    }
}

/// Les erreurs de transport justifient de tenter une autre route ; celles du relai lui-même, non.
fn transport(e: &ClientError) -> bool {
    matches!(e, ClientError::Connection(_) | ClientError::Timeout)
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Client de relais : adresses ordinaires et cibles `p2p://`.
pub struct P2pRelayClient {
    ws: WsClient,
    p2p: Option<P2pHandle>,
    forwards: tokio::sync::Mutex<HashMap<String, (SocketAddr, Forward)>>,
    routes: Mutex<HashMap<String, RouteState>>,
}

impl P2pRelayClient {
    /// `p2p` : le maillage de cette station (absent : seules les adresses publiques servent).
    pub fn new(ws: WsClient, p2p: Option<P2pHandle>) -> Self {
        Self { ws, p2p, forwards: tokio::sync::Mutex::new(HashMap::new()), routes: Mutex::new(HashMap::new()) }
    }

    /// Dernière voie de chaque cible, triée par cible.
    pub fn routes(&self) -> Vec<RouteState> {
        let mut v: Vec<RouteState> = self.routes.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        v.sort_by(|a, b| a.target.cmp(&b.target));
        v
    }

    fn note(&self, target: &str, via: &'static str, error: Option<String>) {
        self.routes.lock().unwrap_or_else(|e| e.into_inner()).insert(target.to_owned(), RouteState { target: target.to_owned(), via, at: now(), error });
    }

    /// Extrémité locale du tunnel vers `peer`, ouverte à la première demande.
    async fn local_for(&self, handle: &P2pHandle, peer: &str) -> Result<SocketAddr, ClientError> {
        let mut forwards = self.forwards.lock().await;
        if let Some((addr, _)) = forwards.get(peer) {
            return Ok(*addr);
        }
        let id: PeerId = peer.parse().map_err(|e| ClientError::Connection(format!("PeerID illisible « {peer} » : {e}")))?;
        let bind = SocketAddr::from(([127, 0, 0, 1], 0));
        let fwd = forward(handle, bind, id, RELAY_SERVICE).await.map_err(|e| ClientError::Connection(format!("tunnel vers {peer} : {e}")))?;
        let addr = fwd.local_addr;
        forwards.insert(peer.to_owned(), (addr, fwd));
        Ok(addr)
    }
}

impl RelayClient for P2pRelayClient {
    async fn fetch(&self, relay: &str, filter: &Filter) -> Result<Vec<Event>, ClientError> {
        let target = parse_target(relay);
        let Some(peer) = target.peer else {
            let got = self.ws.fetch(relay, filter).await;
            self.note(relay, if got.is_ok() { "direct" } else { "down" }, got.as_ref().err().map(ToString::to_string));
            return got;
        };
        let primary: Result<Vec<Event>, ClientError> = match &self.p2p {
            None => Err(ClientError::Connection("maillage P2P inactif sur cette station".into())),
            Some(handle) => match self.local_for(handle, peer).await {
                // L'authentification NIP-42 déclare l'adresse que le relai distant connaît, pas le port local du tunnel.
                Ok(addr) => self.ws.fetch_as(&format!("ws://{addr}"), target.url.unwrap_or(relay), filter).await,
                Err(e) => Err(e),
            },
        };
        match primary {
            Ok(events) => {
                self.note(relay, "p2p", None);
                Ok(events)
            }
            Err(e) if transport(&e) && target.url.is_some() => {
                let why = e.to_string();
                let url = target.url.unwrap_or_default();
                match self.ws.fetch(url, filter).await {
                    Ok(events) => {
                        self.note(relay, "wss", Some(format!("tunnel : {why}")));
                        Ok(events)
                    }
                    Err(e2) => {
                        self.note(relay, "down", Some(format!("tunnel : {why} ; adresse publique : {e2}")));
                        Err(e2)
                    }
                }
            }
            Err(e) => {
                self.note(relay, "down", Some(e.to_string()));
                Err(e)
            }
        }
    }
}

impl P2pRelayClient {
    /// Publie `event` sur la cible `relay` (adresse ordinaire ou `p2p://<PeerID>[|url]`) : même choix de voie que pour la
    /// lecture (tunnel du maillage, adresse publique en repli après un échec de connexion). `(accepté, message)`.
    pub async fn publish(&self, relay: &str, event: &Event) -> Result<(bool, String), ClientError> {
        let target = parse_target(relay);
        let Some(peer) = target.peer else {
            let got = self.ws.publish(relay, event).await;
            self.note(relay, if got.is_ok() { "direct" } else { "down" }, got.as_ref().err().map(ToString::to_string));
            return got;
        };
        let primary = match &self.p2p {
            None => Err(ClientError::Connection("maillage P2P inactif sur cette station".into())),
            Some(handle) => match self.local_for(handle, peer).await {
                Ok(addr) => self.ws.publish(&format!("ws://{addr}"), event).await,
                Err(e) => Err(e),
            },
        };
        match primary {
            Ok(r) => {
                self.note(relay, "p2p", None);
                Ok(r)
            }
            Err(e) if transport(&e) && target.url.is_some() => {
                let why = e.to_string();
                match self.ws.publish(target.url.unwrap_or_default(), event).await {
                    Ok(r) => {
                        self.note(relay, "wss", Some(format!("tunnel : {why}")));
                        Ok(r)
                    }
                    Err(e2) => {
                        self.note(relay, "down", Some(format!("tunnel : {why} ; adresse publique : {e2}")));
                        Err(e2)
                    }
                }
            }
            Err(e) => {
                self.note(relay, "down", Some(e.to_string()));
                Err(e)
            }
        }
    }
}

/// Client partagé : le crawler et l'état des routes (affiché dans l'interface) regardent le même objet.
#[derive(Clone)]
pub struct SharedRelayClient(pub std::sync::Arc<P2pRelayClient>);

impl RelayClient for SharedRelayClient {
    async fn fetch(&self, relay: &str, filter: &Filter) -> Result<Vec<Event>, ClientError> {
        self.0.fetch(relay, filter).await
    }
}
