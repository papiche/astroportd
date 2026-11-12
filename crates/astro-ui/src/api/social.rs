//! Social : carnet de contacts, messagerie, présence et visio, agents, BRO, serveur MCP.
//!
//! Routes et gestionnaires de ce domaine ; le trait [`Backend`] et les types partagés restent dans `api/mod.rs`.

use super::*;

/// Social : carnet de contacts, messagerie, présence et visio, agents, BRO, serveur MCP.
pub trait SocialApi: Auth {
    /// Paramètres IA du MULTIPASS, sans la clé d'API : `{ ollama_url, provider, has_key, model, declared }`.
    fn ai_params(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("paramètres IA non disponibles sur cette station")) }
    }
    /// Change les paramètres IA (champ absent : inchangé ; `null` ou vide : effacé).
    fn set_ai_params(&self, _who: &Principal, _patch: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("paramètres IA non disponibles sur cette station")) }
    }
    /// Carnet d'adresses : une opération ([`BookOp`]) sur les fiches du titulaire.
    fn book(&self, _who: &Principal, _op: BookOp) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("le carnet d'adresses n'est pas disponible sur cette station")) }
    }
    /// Présence visio : `{ reachable, friends: { hex: statut } }` ; `body` (POST) change le choix `reachable` ou l'état `in_call`.
    fn presence(&self, _who: &Principal, _body: Option<Value>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("la présence visio n'est pas disponible sur cette station")) }
    }
    /// Visio : serveurs de traversée à donner au navigateur (`{ stun_port, servers }`).
    fn call_ice(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("la visio n'est pas disponible sur cette station")) }
    }
    /// Chat privé : une opération ([`ChatOp`]) pour le titulaire (dont les signaux d'appel visio).
    fn chat(&self, _who: &Principal, _op: ChatOp) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("le chat privé n'est pas disponible sur cette station")) }
    }
    /// Mes clés dans les UPlanet de cette station et les liens prouvés : `{ plane, keys: [{plane, hex, via}] }`.
    fn my_links(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("annuaire des UPlanet non disponible sur cette station")) }
    }
    /// Annonce, sur demande, que mes comptes des autres UPlanet de la station sont les miens (lien signé des deux clés, public).
    fn announce_links(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("annuaire des UPlanet non disponible sur cette station")) }
    }
    /// Dialogue avec BRO.
    fn bro_say(&self, who: &Principal, text: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Agents déclarés par le membre (sans jeton ni empreinte).
    fn agents_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Déclare un agent ; rend son jeton, montré une seule fois.
    fn agents_enroll(&self, _who: &Principal, _name: String, _group: String, _max_secs: u64, _totp: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Retire un agent (son jeton cesse de fonctionner).
    fn agents_revoke(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Demandes d'accès en cours : `{ requests: [{ id, agent, site, reason, state, until }] }`.
    fn agents_requests(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Le membre accorde (`secs` facultatif) ou refuse une demande.
    fn agents_decide(&self, _who: &Principal, _id: String, _approve: bool, _secs: Option<u64>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Serveur MCP (JSON-RPC 2.0) : `bearer` = jeton d'agent. `Ok(None)` : notification, rien à répondre ;
    /// jeton absent ou faux : `Err` en 401.
    fn mcp(&self, _bearer: Option<String>, _body: Value) -> impl Future<Output = Result<Option<Value>, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Annuaire : fiches de contact, état de leur liaison à un MULTIPASS, personnes reconnues sans fiche.
    fn contacts(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// Relit le carnet d'adresses du compte et rapproche les visages.
    fn contacts_sync(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// Crée la fiche d'une personne reconnue.
    fn contacts_create(&self, _who: &Principal, _name: String, _email: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// Relie (ou délie) une fiche à un MULTIPASS.
    fn contacts_link(&self, _who: &Principal, _path: String, _target: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
}

pub(super) fn routes<B: SocialApi>() -> Router<Arc<UiState<B>>> {
    Router::new()
        .route("/api/v1/activity", get(activity_stream::<B>))
        .route("/api/v1/me/ai", get(ai_params::<B>).post(set_ai_params::<B>))
        .route("/api/v1/book", get(book_list::<B>))
        .route("/api/v1/book/card", get(book_get::<B>).post(book_save::<B>))
        .route("/api/v1/book/delete", post(book_delete::<B>))
        .route("/api/v1/book/delete-many", post(book_delete_many::<B>))
        .route("/api/v1/book/photo", get(book_photo::<B>).post(book_set_photo::<B>))
        .route("/api/v1/book/photo-choices", get(book_photo_choices::<B>))
        .route("/api/v1/book/take", post(book_take::<B>))
        .route("/api/v1/book/group", post(book_group::<B>))
        .route("/api/v1/book/import", post(book_import::<B>))
        .route("/api/v1/book/duplicates", get(book_duplicates::<B>))
        .route("/api/v1/book/merge", post(book_merge::<B>))
        .route("/api/v1/book/merge-exact", post(book_merge_exact::<B>))
        .route("/api/v1/book/me", get(book_me::<B>).post(book_save_me::<B>))
        .route("/api/v1/me/links", get(my_links::<B>).post(announce_links::<B>))
        .route("/api/v1/chat", get(chat_list::<B>))
        .route("/api/v1/chat/messages", get(chat_messages::<B>))
        .route("/api/v1/chat/send", post(chat_send::<B>))
        .route("/api/v1/chat/read", post(chat_read::<B>))
        .route("/api/v1/call/signal", post(call_signal::<B>))
        .route("/api/v1/call/ice", get(call_ice::<B>))
        .route("/api/v1/presence", get(presence_get::<B>).post(presence_set::<B>))
        .route("/api/v1/bro/say", post(bro_say::<B>))
        .route("/api/v1/agents", get(agents_list::<B>).post(agents_enroll::<B>))
        .route("/api/v1/agents/revoke", post(agents_revoke::<B>))
        .route("/api/v1/agents/requests", get(agents_requests::<B>))
        .route("/api/v1/agents/decide", post(agents_decide::<B>))
        .route("/mcp", post(mcp::<B>))
        .route("/api/v1/contacts", get(contacts::<B>))
        .route("/api/v1/contacts/sync", post(contacts_sync::<B>))
        .route("/api/v1/contacts/from-person", post(contacts_create::<B>))
        .route("/api/v1/contacts/link", post(contacts_link::<B>))
}

#[derive(Deserialize)]
struct BookListQuery {
    q: Option<String>,
    group: Option<String>,
}

#[derive(Deserialize)]
struct BookPathQuery {
    path: String,
}

#[derive(Deserialize)]
struct BookSaveBody {
    path: Option<String>,
    fields: Value,
}

#[derive(Deserialize)]
struct BookPathBody {
    path: String,
}

#[derive(Deserialize)]
struct BookTakeBody {
    path: String,
    keys: Vec<String>,
}

#[derive(Deserialize)]
struct BookGroupBody {
    from: String,
    to: Option<String>,
}

#[derive(Deserialize)]
struct BookImportBody {
    vcf: String,
}

#[derive(Deserialize)]
struct BookMergeBody {
    keep: String,
    drop: Vec<String>,
}

#[derive(Deserialize)]
struct BookMergeExactBody {
    #[serde(default)]
    apply: bool,
    #[serde(default)]
    limit: Option<usize>,
}

async fn book_list<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookListQuery>) -> Response {
    reply(st.backend.book(&p, BookOp::List(q.q.unwrap_or_default().chars().take(200).collect(), q.group.filter(|g| g.len() <= 80))).await)
}

async fn book_get<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookPathQuery>) -> Response {
    reply(st.backend.book(&p, BookOp::Get(q.path)).await)
}

async fn book_save<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookSaveBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Save(b.path, b.fields)).await)
}

#[derive(Deserialize)]
struct BookPathsBody {
paths: Vec<String>,
}

async fn book_delete_many<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookPathsBody>) -> Response {
reply(st.backend.book(&p, BookOp::DeleteMany(b.paths)).await)
}

async fn book_photo<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookPathQuery>) -> Response {
reply(st.backend.book(&p, BookOp::Photo(q.path)).await)
}

#[derive(Deserialize)]
struct BookPhotoBody {
path: String,
uri: Option<String>,
}

async fn book_set_photo<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookPhotoBody>) -> Response {
reply(st.backend.book(&p, BookOp::SetPhoto(b.path, b.uri)).await)
}

async fn book_photo_choices<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookPathQuery>) -> Response {
reply(st.backend.book(&p, BookOp::PhotoChoices(q.path)).await)
}

async fn book_delete<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookPathBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Delete(b.path)).await)
}

async fn book_take<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookTakeBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Take(b.path, b.keys.into_iter().take(10).collect())).await)
}

async fn book_group<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookGroupBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Group(b.from, b.to)).await)
}

async fn book_import<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookImportBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Import(b.vcf)).await)
}

async fn book_duplicates<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.book(&p, BookOp::Duplicates).await)
}

async fn book_merge<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookMergeBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Merge(b.keep, b.drop)).await)
}

async fn book_merge_exact<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookMergeExactBody>) -> Response {
    reply(st.backend.book(&p, BookOp::MergeExact(b.apply, b.limit.unwrap_or(50).clamp(1, 200))).await)
}

async fn book_me<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.book(&p, BookOp::Me).await)
}

async fn book_save_me<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(fields): Json<Value>) -> Response {
    reply(st.backend.book(&p, BookOp::SaveMe(fields)).await)
}

#[derive(Deserialize)]
struct ChatPeerQuery {
    peer: String,
    before: Option<i64>,
}

#[derive(Deserialize)]
struct ChatSendBody {
    peer: String,
    text: String,
}

#[derive(Deserialize)]
struct ChatPeerBody {
    peer: String,
}

async fn my_links<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.my_links(&p).await)
}

async fn announce_links<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.announce_links(&p).await)
}

async fn chat_list<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.chat(&p, ChatOp::List).await)
}

async fn chat_messages<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<ChatPeerQuery>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Messages(q.peer.chars().take(80).collect(), q.before)).await)
}

async fn call_signal<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Signal(b)).await)
}

async fn presence_get<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.presence(&p, None).await)
}

async fn presence_set<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.presence(&p, Some(b)).await)
}

async fn call_ice<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.call_ice(&p).await)
}

async fn chat_send<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<ChatSendBody>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Send(b.peer.chars().take(80).collect(), b.text.chars().take(20_000).collect())).await)
}

async fn chat_read<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<ChatPeerBody>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Read(b.peer.chars().take(80).collect())).await)
}

async fn ai_params<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.ai_params(&p).await)
}

async fn set_ai_params<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(patch): Json<Value>) -> Response {
    if !patch.is_object() {
        return reply(Err(ApiError::bad_request("objet JSON attendu")));
    }
    reply(st.backend.set_ai_params(&p, patch).await)
}

#[derive(Deserialize)]
struct SayBody {
    text: String,
}

async fn agents_list<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.agents_list(&p).await)
}

#[derive(Deserialize)]
struct EnrollBody {
    name: String,
    group: String,
    #[serde(default = "default_grant_secs")]
    max_secs: u64,
    #[serde(default)]
    totp: bool,
}

fn default_grant_secs() -> u64 {
    600
}

async fn agents_enroll<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<EnrollBody>) -> Response {
    reply(st.backend.agents_enroll(&p, body.name, body.group, body.max_secs, body.totp).await)
}

async fn agents_revoke<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.agents_revoke(&p, body.id).await)
}

async fn agents_requests<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.agents_requests(&p).await)
}

#[derive(Deserialize)]
struct DecideBody {
    id: String,
    approve: bool,
    secs: Option<u64>,
}

async fn agents_decide<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<DecideBody>) -> Response {
    reply(st.backend.agents_decide(&p, body.id, body.approve, body.secs).await)
}

/// Serveur MCP : le jeton d'agent (`Authorization: Bearer`) remplace la connexion d'un compte.
async fn mcp<B: SocialApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let bearer = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(|t| t.trim().to_owned());
    match st.backend.mcp(bearer, body).await {
        Ok(Some(v)) => private(Json(v).into_response()),
        Ok(None) => private(StatusCode::ACCEPTED.into_response()),
        Err(e) => {
            // Un jeton refusé annonce le schéma attendu, comme le veut HTTP.
            let mut r = e.into_response();
            if r.status() == StatusCode::UNAUTHORIZED {
                r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            private(r)
        }
    }
}

async fn bro_say<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<SayBody>) -> Response {
    let text = body.text.trim().to_owned();
    if text.is_empty() || text.chars().count() > 2000 {
        return reply(Err(ApiError::bad_request("message de 1 à 2000 caractères attendu")));
    }
    reply(st.backend.bro_say(&p, text).await)
}

async fn contacts<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.contacts(&p).await)
}

async fn contacts_sync<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.contacts_sync(&p).await)
}

#[derive(serde::Deserialize)]
struct PersonBody {
    name: String,
    #[serde(default)]
    email: Option<String>,
}

async fn contacts_create<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<PersonBody>) -> Response {
    reply(st.backend.contacts_create(&p, body.name, body.email).await)
}

#[derive(serde::Deserialize)]
struct LinkBody {
    path: String,
    #[serde(default)]
    target: Option<String>,
}

async fn contacts_link<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<LinkBody>) -> Response {
    reply(st.backend.contacts_link(&p, body.path, body.target).await)
}

/// Flux SSE : d'abord ce que le client a manqué (`Last-Event-ID`), puis le direct.
async fn activity_stream<B: SocialApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    let principal = match who(&st, &headers, &method, &uri).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let after = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let (backlog, rx) = st.activity.subscribe(after);
    // Tant que ce flux est ouvert, le compte est « là » (présence visio, pour ses amis qui l'ont choisie).
    let here = st.activity.listen(&principal.hex);
    // Un événement personnel n'est ni rejoué ni diffusé à un autre compte (virement, e-mail d'un nouveau membre…).
    let backlog: Vec<_> = backlog.into_iter().filter(|a| a.audience.admits(&principal.hex, principal.captain)).collect();
    let sse = Sse::new(event_stream(backlog, rx, principal, here)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"));
    private(sse.into_response())
}

fn to_event(a: &astro_core::Activity) -> Event {
    Event::default().id(a.id.to_string()).event(a.kind.clone()).data(serde_json::to_string(a).unwrap_or_default())
}

type Live = tokio::sync::broadcast::Receiver<Arc<astro_core::Activity>>;

fn event_stream(backlog: Vec<Arc<astro_core::Activity>>, rx: Live, who: Principal, here: astro_core::activity::ListenGuard) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold((backlog.into_iter(), rx, who, here), |(mut backlog, mut rx, who, here)| async move {
        if let Some(a) = backlog.next() {
            return Some((Ok(to_event(&a)), (backlog, rx, who, here)));
        }
        loop {
            match rx.recv().await {
                Ok(a) if a.audience.admits(&who.hex, who.captain) => return Some((Ok(to_event(&a)), (backlog, rx, who, here))),
                Ok(_) => {}
                // Le client lisait trop lentement : on le dit plutôt que de laisser croire à un flux complet.
                Err(RecvError::Lagged(n)) => return Some((Ok(Event::default().event("lag").data(n.to_string())), (backlog, rx, who, here))),
                Err(RecvError::Closed) => return None,
            }
        }
    })
}
