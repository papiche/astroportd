//! Façade : port 80 (défi ACME HTTP-01 + redirection 301 vers HTTPS) et port 443 (HTTPS), routeur applicatif commun.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::acme::{AcmeManager, ChallengeStore};
use crate::tls::TlsListener;

#[derive(Clone)]
struct HttpState {
    challenges: ChallengeStore,
    domains: Arc<Vec<String>>,
    https_port: u16,
}

async fn acme_challenge(State(st): State<HttpState>, Path(token): Path<String>) -> Response {
    match st.challenges.get(&token) {
        Some(answer) => ([(header::CONTENT_TYPE, "application/octet-stream")], answer).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn redirect_to_https(State(st): State<HttpState>, headers: HeaderMap, uri: axum::http::Uri) -> Response {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase());
    // Seuls nos propres noms sont redirigés : pas de redirection ouverte vers un hôte choisi par le client.
    let Some(host) = host.filter(|h| st.domains.iter().any(|d| d.eq_ignore_ascii_case(h))) else {
        return (StatusCode::MISDIRECTED_REQUEST, "nom d'hôte inconnu").into_response();
    };
    let port = if st.https_port == 443 { String::new() } else { format!(":{}", st.https_port) };
    let target = format!("https://{host}{port}{}", uri.path_and_query().map_or("/", |p| p.as_str()));
    (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, target)]).into_response()
}

/// Routeur du port 80 : défis HTTP-01 puis redirection 301 vers HTTPS.
pub fn http_router(challenges: ChallengeStore, domains: Vec<String>, https_port: u16) -> Router {
    Router::new()
        .route("/.well-known/acme-challenge/{token}", get(acme_challenge))
        .fallback(redirect_to_https)
        .with_state(HttpState { challenges, domains: Arc::new(domains), https_port })
}

/// Façade démarrée.
pub struct Frontend {
    /// Adresse du port HTTP.
    pub http_addr: SocketAddr,
    /// Adresse du port HTTPS.
    pub https_addr: SocketAddr,
    tasks: Vec<JoinHandle<()>>,
    stop: watch::Sender<bool>,
}

impl Frontend {
    /// Ouvre les ports et lance la gestion automatique des certificats.
    pub async fn start(acme: Arc<AcmeManager>, app: Router, http_listen: SocketAddr, https_listen: SocketAddr) -> std::io::Result<Self> {
        let http = tokio::net::TcpListener::bind(http_listen).await?;
        let http_addr = http.local_addr()?;
        let https = TlsListener::bind(https_listen, acme.resolver.clone()).await?;
        let https_addr = axum::serve::Listener::local_addr(&https)?;
        let (stop, stop_rx) = watch::channel(false);
        let http_app = http_router(acme.challenges.clone(), acme.config.domains.clone(), https_addr.port());
        let mut tasks = vec![
            tokio::spawn(async move {
                let _ = axum::serve(http, http_app.into_make_service_with_connect_info::<SocketAddr>()).await;
            }),
            tokio::spawn(async move {
                let app = app.layer(axum::middleware::from_fn(crate::tls::promote_peer));
                let _ = axum::serve(https, app.into_make_service_with_connect_info::<crate::tls::TlsPeer>()).await;
            }),
        ];
        tasks.push(tokio::spawn(acme.run(stop_rx)));
        Ok(Self { http_addr, https_addr, tasks, stop })
    }

    /// Arrête les écouteurs et la tâche de renouvellement.
    pub fn shutdown(self) {
        let _ = self.stop.send(true);
        for t in self.tasks {
            t.abort();
        }
    }
}
