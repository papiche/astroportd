//! Serveur WebSocket Nostr (axum) : une tâche par connexion, un canal de diffusion pour le temps réel.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use astro_db::events::{InsertOutcome, classify, insert_event, query_events, KindClass};
use astro_db::{Db, Statement, apply_statements};
use astro_nostr_proto::{ClientMessage, Event, Filter, RelayMessage, kind};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::sync::broadcast;

use crate::circle::CircleProvider;
use crate::limits::RateLimiter;
use crate::policy::{AuthState, Policy, ReadAccess, WriteDecision};

/// Projection appliquée dans la même transaction que l'insertion d'un événement (ex. `Projector::plan` d'`astro-sync`).
pub type PlanFn = Arc<dyn Fn(&Event) -> Vec<Statement> + Send + Sync>;

/// Réglages du relai.
#[derive(Clone)]
pub struct RelayConfig {
    /// Nom annoncé dans le document NIP-11.
    pub name: String,
    /// URL publique du relai : si présente, le tag `relay` d'un AUTH doit avoir le même hôte.
    pub relay_url: Option<String>,
    /// Taille maximale d'un message WebSocket.
    pub max_message_bytes: usize,
    /// Taille maximale d'un événement sérialisé.
    pub max_event_bytes: usize,
    /// Souscriptions simultanées par connexion.
    pub max_subscriptions: usize,
    /// Filtres par `REQ`.
    pub max_filters: usize,
    /// Événements d'historique renvoyés au plus par filtre.
    pub query_cap: usize,
    /// Rafale de gift wraps d'amorçage par IP.
    pub gift_wrap_burst: u32,
    /// Gift wraps d'amorçage rendus par minute et par IP.
    pub gift_wrap_per_minute: u32,
    /// Lire l'IP cliente dans `X-Forwarded-For` (derrière un reverse proxy de confiance uniquement).
    pub trust_forwarded_for: bool,
    /// Écart maximal toléré pour `created_at` d'un AUTH, en secondes.
    pub auth_max_age_secs: u64,
    /// Avance maximale tolérée pour `created_at` d'un événement, en secondes.
    pub future_skew_secs: u64,
    /// Politique d'accès.
    pub policy: Policy,
    /// Projection exécutée avec chaque insertion.
    pub projection: Option<PlanFn>,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            name: "astroportd".into(),
            relay_url: None,
            max_message_bytes: 128 * 1024,
            max_event_bytes: 64 * 1024,
            max_subscriptions: 20,
            max_filters: 10,
            query_cap: 500,
            gift_wrap_burst: 10,
            gift_wrap_per_minute: 10,
            trust_forwarded_for: false,
            auth_max_age_secs: 600,
            future_skew_secs: 900,
            policy: Policy::default(),
            projection: None,
        }
    }
}

/// Cœur du relai, partagé par toutes les connexions.
pub struct Relay {
    db: Arc<Db>,
    circle: Arc<dyn CircleProvider>,
    config: RelayConfig,
    policy: std::sync::RwLock<Arc<Policy>>,
    gift_limiter: RateLimiter,
    live: broadcast::Sender<Arc<Event>>,
}

struct Conn {
    challenge: String,
    auth: AuthState,
    subs: HashMap<String, Vec<Filter>>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Plafond d'un décompte (NIP-45) : au-delà, la réponse est marquée approximative.
const COUNT_CAP: usize = 10_000;

impl Relay {
    /// Crée le relai.
    pub fn new(db: Arc<Db>, circle: Arc<dyn CircleProvider>, config: RelayConfig) -> Arc<Self> {
        let (live, _) = broadcast::channel(1024);
        let gift_limiter = RateLimiter::new(config.gift_wrap_burst, config.gift_wrap_per_minute);
        let policy = std::sync::RwLock::new(Arc::new(config.policy.clone()));
        Arc::new(Self { db, circle, config, policy, gift_limiter, live })
    }

    /// Politique en vigueur (changeable à chaud par [`Relay::set_policy`]).
    pub fn policy(&self) -> Arc<Policy> {
        self.policy.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Remplace la politique : les connexions et requêtes suivantes l'appliquent, sans redémarrage.
    pub fn set_policy(&self, policy: Policy) {
        *self.policy.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(policy);
    }

    /// Routeur axum : WebSocket sur `/`, document NIP-11 pour un GET simple.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new().route("/", get(root)).with_state(self)
    }

    fn client_ip(&self, req: &Request) -> IpAddr {
        if self.config.trust_forwarded_for {
            let forwarded = req
                .headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .and_then(|v| v.trim().parse::<IpAddr>().ok());
            if let Some(ip) = forwarded {
                return ip;
            }
        }
        req.extensions().get::<ConnectInfo<SocketAddr>>().map_or(IpAddr::from([0, 0, 0, 0]), |c| c.0.ip())
    }

    fn info_document(&self) -> String {
        serde_json::json!({
            "name": self.config.name,
            "description": "Relais Nostr souverain astroportd (pare-feu social NIP-42)",
            "supported_nips": [1, 9, 11, 17, 40, 42, 45, 50, 59, 70],
            "software": "astroportd",
            "version": env!("CARGO_PKG_VERSION"),
            "limitation": {
                "max_message_length": self.config.max_message_bytes,
                "max_subid_length": astro_nostr_proto::message::MAX_SUB_ID_LEN,
                "max_limit": self.config.query_cap,
                "max_event_tags": 2000,
                "created_at_upper_limit": self.config.future_skew_secs,
                "max_subscriptions": self.config.max_subscriptions,
                "max_filters": self.config.max_filters,
                "auth_required": false,
                "payment_required": false,
                "restricted_writes": true
            }
        })
        .to_string()
    }

    async fn run(self: Arc<Self>, mut socket: WebSocket, ip: IpAddr) {
        let mut challenge_bytes = [0u8; 16];
        if getrandom::fill(&mut challenge_bytes).is_err() {
            return;
        }
        let mut conn = Conn { challenge: hex::encode(challenge_bytes), auth: AuthState::default(), subs: HashMap::new() };
        if !send(&mut socket, &RelayMessage::Auth(conn.challenge.clone())).await {
            return;
        }
        let mut live = self.live.subscribe();
        loop {
            tokio::select! {
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if !self.on_text(&mut socket, &mut conn, ip, text.as_str()).await {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                event = live.recv() => match event {
                    Ok(event) => {
                        if !self.deliver_live(&mut socket, &conn, &event).await {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        if !send(&mut socket, &RelayMessage::Notice(format!("error: slow consumer, {n} live events missed"))).await {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    }

    /// Traite un message texte ; `false` si la connexion est morte.
    async fn on_text(&self, socket: &mut WebSocket, conn: &mut Conn, ip: IpAddr, text: &str) -> bool {
        let msg = match ClientMessage::parse(text) {
            Ok(m) => m,
            Err(e) => return send(socket, &RelayMessage::Notice(format!("invalid: {e}"))).await,
        };
        match msg {
            ClientMessage::Event(event) => {
                // NIP-70 : un événement protégé (`["-"]`) n'est accepté que de son auteur, authentifié sur cette connexion.
                if event.tags.iter().any(|t| t.len() == 1 && t[0] == "-") && !conn.auth.contains(&event.pubkey) {
                    let message = if conn.auth.is_empty() { "auth-required: this event may only be published by its author" } else { "restricted: this event may only be published by its author" };
                    return send(socket, &RelayMessage::Ok { event_id: event.id, accepted: false, message: message.into() }).await;
                }
                let (accepted, message) = self.process_event(&event, ip).await;
                send(socket, &RelayMessage::Ok { event_id: event.id, accepted, message }).await
            }
            ClientMessage::Auth(event) => {
                let reply = match self.check_auth(&event, &conn.challenge) {
                    Ok(()) => {
                        conn.auth.add(event.pubkey.clone());
                        (true, String::new())
                    }
                    Err(reason) => (false, reason.to_owned()),
                };
                send(socket, &RelayMessage::Ok { event_id: event.id, accepted: reply.0, message: reply.1 }).await
            }
            ClientMessage::Close(sub_id) => {
                conn.subs.remove(&sub_id);
                true
            }
            ClientMessage::Req { sub_id, filters } => self.on_req(socket, conn, sub_id, filters).await,
            ClientMessage::Count { sub_id, filters } => self.on_count(socket, conn, sub_id, filters).await,
        }
    }

    fn check_auth(&self, e: &Event, challenge: &str) -> Result<(), &'static str> {
        if e.kind != kind::AUTH {
            return Err("invalid: kind must be 22242");
        }
        if e.verify().is_err() {
            return Err("invalid: bad id or signature");
        }
        if e.created_at.abs_diff(now_secs()) > self.config.auth_max_age_secs {
            return Err("invalid: created_at is too far from the relay clock");
        }
        if e.tag_value("challenge") != Some(challenge) {
            return Err("invalid: challenge mismatch");
        }
        if let Some(expected) = &self.config.relay_url {
            let host = |u: &str| url::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_owned));
            let ok = e.tag_value("relay").and_then(host).is_some_and(|h| Some(h) == host(expected));
            if !ok {
                return Err("invalid: relay tag does not match this relay");
            }
        }
        Ok(())
    }

    /// Flux des événements acceptés (stockés ou éphémères), en direct : pour le journal d'activité de la station.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.live.subscribe()
    }

    /// Publie un événement produit par la station elle-même (réponse de BRO…) : même contrôle de signature et de
    /// politique que pour un client, depuis la boucle locale. Retourne `(accepté, message)`.
    pub async fn publish_local(&self, e: &Event) -> (bool, String) {
        self.process_event(e, IpAddr::from([127, 0, 0, 1])).await
    }

    /// Valide, filtre, stocke et diffuse un événement. Retourne `(accepté, message)`.
    async fn process_event(&self, e: &Event, ip: IpAddr) -> (bool, String) {
        if e.verify().is_err() {
            return (false, "invalid: bad id or signature".into());
        }
        let size = serde_json::to_string(e).map_or(usize::MAX, |s| s.len());
        if size > self.config.max_event_bytes {
            return (false, "invalid: event too large".into());
        }
        if e.created_at > now_secs().saturating_add(self.config.future_skew_secs) {
            return (false, "invalid: created_at is too far in the future".into());
        }
        // NIP-40 : un événement déjà expiré n'est pas conservé.
        if e.tag_value("expiration").and_then(|v| v.parse::<u64>().ok()).is_some_and(|t| t <= now_secs()) {
            return (false, "invalid: event expired".into());
        }
        let policy = self.policy();
        match policy.write_decision(e, self.circle.as_ref()) {
            WriteDecision::Reject(reason) => return (false, reason.to_owned()),
            WriteDecision::AcceptGiftWrap | WriteDecision::AcceptLimited if !self.gift_limiter.check(ip) => {
                return (false, "rate-limited: too many events from this address".into());
            }
            WriteDecision::AcceptGiftWrap | WriteDecision::AcceptLimited | WriteDecision::Accept => {}
        }
        if classify(e.kind) == KindClass::Ephemeral {
            let _ = self.live.send(Arc::new(e.clone()));
            return (true, String::new());
        }
        let (stored, plan) = (e.clone(), self.config.projection.clone().filter(|_| policy.projects(e.kind)));
        let outcome = self
            .db
            .writer()
            .transaction(move |tx| {
                let outcome = insert_event(tx, &stored)?;
                if outcome == InsertOutcome::Inserted {
                    if let Some(plan) = &plan {
                        apply_statements(tx, &plan(&stored))?;
                    }
                }
                Ok(outcome)
            })
            .await;
        match outcome {
            Ok(InsertOutcome::Inserted) | Ok(InsertOutcome::Ephemeral) => {
                let _ = self.live.send(Arc::new(e.clone()));
                (true, String::new())
            }
            Ok(InsertOutcome::Duplicate) => (true, "duplicate: already have this event".into()),
            Ok(InsertOutcome::Superseded) => (false, "invalid: a newer version of this replaceable event is stored".into()),
            Ok(InsertOutcome::Deleted) => (false, "invalid: this event was deleted by its author".into()),
            Err(_) => (false, "error: storage failure".into()),
        }
    }

    /// Événements lisibles par la connexion pour `filters`, du plus récent au plus ancien, ou la raison d'un `CLOSED`.
    async fn readable(&self, conn: &Conn, filters: &[Filter], cap: usize) -> Result<Vec<Event>, &'static str> {
        if filters.len() > self.config.max_filters {
            return Err("error: too many filters");
        }
        let policy = self.policy();
        let policy = &*policy;
        for f in filters {
            match policy.filter_access(f, &conn.auth, self.circle.as_ref()) {
                ReadAccess::Open => {}
                ReadAccess::NeedsAuth => return Err("auth-required: this subscription targets private kinds"),
                ReadAccess::Restricted => return Err("restricted: authenticated key is outside the social circle"),
            }
        }
        let (query, now) = (filters.to_vec(), now_secs());
        let found = self
            .db
            .readers()
            .read(move |c| {
                let mut out = Vec::new();
                for f in &query {
                    out.extend(query_events(c, f, now, cap)?);
                }
                Ok(out)
            })
            .await;
        let Ok(mut events) = found else { return Err("error: storage failure") };
        let mut seen = HashSet::new();
        events.retain(|e| seen.insert(e.id.clone()) && policy.can_read(e, &conn.auth, self.circle.as_ref()));
        events.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
        Ok(events)
    }

    async fn on_req(&self, socket: &mut WebSocket, conn: &mut Conn, sub_id: String, filters: Vec<Filter>) -> bool {
        let closed = |message: &str| RelayMessage::Closed { sub_id: sub_id.clone(), message: message.to_owned() };
        if filters.len() > self.config.max_filters {
            return send(socket, &closed("error: too many filters")).await;
        }
        if !conn.subs.contains_key(&sub_id) && conn.subs.len() >= self.config.max_subscriptions {
            return send(socket, &closed("error: too many subscriptions")).await;
        }
        let events = match self.readable(conn, &filters, self.config.query_cap).await {
            Ok(events) => events,
            Err(reason) => return send(socket, &closed(reason)).await,
        };
        for event in events {
            if !send(socket, &RelayMessage::Event { sub_id: sub_id.clone(), event: Box::new(event) }).await {
                return false;
            }
        }
        conn.subs.insert(sub_id.clone(), filters);
        send(socket, &RelayMessage::Eose(sub_id)).await
    }

    /// NIP-45 : nombre d'événements que la connexion a le droit de lire (même contrôle d'accès qu'un `REQ`).
    async fn on_count(&self, socket: &mut WebSocket, conn: &Conn, sub_id: String, filters: Vec<Filter>) -> bool {
        let reply = match self.readable(conn, &filters, COUNT_CAP).await {
            Ok(events) => RelayMessage::Count { sub_id, count: events.len() as u64, approximate: events.len() >= COUNT_CAP },
            Err(reason) => RelayMessage::Closed { sub_id, message: reason.to_owned() },
        };
        send(socket, &reply).await
    }

    async fn deliver_live(&self, socket: &mut WebSocket, conn: &Conn, event: &Event) -> bool {
        if !self.policy().can_read(event, &conn.auth, self.circle.as_ref()) {
            return true;
        }
        for (sub_id, filters) in &conn.subs {
            if filters.iter().any(|f| f.matches(event)) {
                let msg = RelayMessage::Event { sub_id: sub_id.clone(), event: Box::new(event.clone()) };
                if !send(socket, &msg).await {
                    return false;
                }
            }
        }
        true
    }
}

async fn send(socket: &mut WebSocket, m: &RelayMessage) -> bool {
    matches!(tokio::time::timeout(Duration::from_secs(10), socket.send(Message::Text(m.to_json().into()))).await, Ok(Ok(())))
}

async fn root(State(relay): State<Arc<Relay>>, req: Request) -> Response {
    let ip = relay.client_ip(&req);
    let wants_info = req.headers().get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|v| v.contains("application/nostr+json"));
    let (mut parts, _body) = req.into_parts();
    match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(ws) => ws.max_message_size(relay.config.max_message_bytes).on_upgrade(move |socket| relay.run(socket, ip)),
        // CORS : les clients web (Coracle…) lisent le document NIP-11 depuis un autre domaine.
        Err(_) if wants_info => (StatusCode::OK, [(header::CONTENT_TYPE, "application/nostr+json"), (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"), (header::ACCESS_CONTROL_ALLOW_HEADERS, "*")], relay.info_document()).into_response(),
        Err(_) => (StatusCode::OK, "astroportd relay\n").into_response(),
    }
}

/// Sert le relai sur `listener` (l'adresse cliente alimente la limitation de débit).
pub async fn serve(listener: tokio::net::TcpListener, relay: Arc<Relay>) -> std::io::Result<()> {
    axum::serve(listener, relay.router().into_make_service_with_connect_info::<SocketAddr>()).await
}
