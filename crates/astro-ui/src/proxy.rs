//! Reverse proxy intégré : redirige des préfixes d'URL vers des services locaux (Brain GPU, ComfyUI, Ollama…).
//!
//! Corps de requête et de réponse en flux. Les en-têtes « hop-by-hop » sont retirés, `X-Forwarded-*` ajoutés.
//! Une route peut exiger une authentification (`gate`) : un service d'inférence ne doit jamais être exposé sans garde.
//! Les WebSockets (`Upgrade: websocket`) sont relayés dans les deux sens : ComfyUI et les interfaces réactives fonctionnent derrière la façade.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

/// Garde d'authentification d'une route : `(en-têtes, méthode, URI) → autorisé ?`.
pub type Gate = Arc<dyn Fn(HeaderMap, Method, Uri) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Route de proxy.
#[derive(Clone)]
pub struct ProxyRoute {
    /// Préfixe public (`/brain/faces`).
    pub prefix: String,
    /// Service amont (`http://127.0.0.1:8190`).
    pub upstream: String,
    /// Garde d'accès ; `None` = ouvert (à réserver aux services déjà publics).
    pub gate: Option<Gate>,
}

impl ProxyRoute {
    /// Analyse `/préfixe=http://hôte:port` (forme de la ligne de commande).
    pub fn parse(spec: &str, gate: Option<Gate>) -> Result<Self, String> {
        let (prefix, upstream) = spec.split_once('=').ok_or("format attendu : /préfixe=http://hôte:port")?;
        if !prefix.starts_with('/') || prefix.len() < 2 || prefix.ends_with('/') || prefix.contains("//") || prefix.contains("..") {
            return Err(format!("préfixe invalide : {prefix}"));
        }
        let uri: Uri = upstream.parse().map_err(|e| format!("amont invalide : {e}"))?;
        if uri.scheme_str() != Some("http") || uri.authority().is_none() {
            return Err("l'amont doit être http://hôte:port (le TLS s'arrête sur astroportd)".into());
        }
        Ok(Self { prefix: prefix.to_owned(), upstream: upstream.trim_end_matches('/').to_owned(), gate })
    }
}

#[derive(Clone)]
struct ProxyState {
    route: ProxyRoute,
    client: Client<HttpConnector, Body>,
    timeout: Duration,
}

const HOP_BY_HOP: [&str; 9] = ["connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "te", "trailer", "transfer-encoding", "upgrade", "host"];

fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let extra: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|n| HeaderName::from_bytes(n.trim().as_bytes()).ok())
        .collect();
    for name in HOP_BY_HOP.iter().map(|n| HeaderName::from_static(n)).chain(extra) {
        headers.remove(name);
    }
}

fn is_websocket(headers: &HeaderMap) -> bool {
    let has = |name: HeaderName, needle: &str| headers.get_all(name).iter().filter_map(|v| v.to_str().ok()).any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(needle)));
    has(header::UPGRADE, "websocket") && has(header::CONNECTION, "upgrade")
}

async fn forward(State(st): State<Arc<ProxyState>>, mut req: Request) -> Response {
    let peer = req.extensions().get::<ConnectInfo<std::net::SocketAddr>>().map(|c| c.0);
    let websocket = is_websocket(req.headers());
    let client_upgrade = websocket.then(|| req.extensions_mut().remove::<OnUpgrade>()).flatten();
    let (mut parts, body) = req.into_parts();
    if let Some(gate) = &st.route.gate {
        if !gate(parts.headers.clone(), parts.method.clone(), parts.uri.clone()).await {
            let mut r = (StatusCode::UNAUTHORIZED, "authentification requise").into_response();
            r.headers_mut().append(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"astroportd\", charset=\"UTF-8\""));
            r.headers_mut().append(header::WWW_AUTHENTICATE, HeaderValue::from_static("Nostr realm=\"astroportd\""));
            return r;
        }
    }
    let path = parts.uri.path();
    let rest = path.strip_prefix(st.route.prefix.as_str()).unwrap_or(path);
    let rest = if rest.is_empty() { "/" } else { rest };
    let target = match parts.uri.query() {
        Some(q) => format!("{}{rest}?{q}", st.route.upstream),
        None => format!("{}{rest}", st.route.upstream),
    };
    let Ok(uri) = target.parse::<Uri>() else { return (StatusCode::BAD_REQUEST, "URL invalide").into_response() };
    let original_host = parts.headers.get(header::HOST).cloned();
    strip_hop_by_hop(&mut parts.headers);
    if websocket {
        // L'établissement du tunnel exige de conserver ces deux en-têtes de bout en bout.
        parts.headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        parts.headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    }
    if let Some(a) = uri.authority().and_then(|a| HeaderValue::from_str(a.as_str()).ok()) {
        parts.headers.insert(header::HOST, a);
    }
    if let Some(addr) = peer {
        if let Ok(v) = HeaderValue::from_str(&addr.ip().to_string()) {
            parts.headers.insert("x-forwarded-for", v);
        }
    }
    if let Some(h) = original_host {
        parts.headers.insert("x-forwarded-host", h);
    }
    let mut out = Request::new(body);
    *out.method_mut() = parts.method;
    *out.uri_mut() = uri;
    *out.headers_mut() = parts.headers;
    match tokio::time::timeout(st.timeout, st.client.request(out)).await {
        Err(_) => (StatusCode::GATEWAY_TIMEOUT, "le service amont ne répond pas").into_response(),
        Ok(Err(_)) => (StatusCode::BAD_GATEWAY, "service amont injoignable").into_response(),
        Ok(Ok(mut resp)) => {
            if websocket && resp.status() == StatusCode::SWITCHING_PROTOCOLS {
                if let (Some(client), upstream) = (client_upgrade, hyper::upgrade::on(&mut resp)) {
                    // Les deux extrémités sont « mises à niveau » : on relaie les octets (trames WebSocket comprises) tels quels.
                    tokio::spawn(async move {
                        if let (Ok(client), Ok(upstream)) = tokio::join!(client, upstream) {
                            let (mut client, mut upstream) = (TokioIo::new(client), TokioIo::new(upstream));
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                        }
                    });
                }
                let (parts, _) = resp.into_parts();
                return Response::from_parts(parts, Body::empty()); // en-têtes 101 de l'amont conservés (Sec-WebSocket-Accept…)
            }
            let (mut parts, body) = resp.into_parts();
            strip_hop_by_hop(&mut parts.headers);
            Response::from_parts(parts, Body::new(body))
        }
    }
}

/// Routeur de proxy : un préfixe, un amont. `X-Forwarded-Proto` est posé par l'appelant selon son schéma.
pub fn proxy_router(routes: Vec<ProxyRoute>, timeout: Duration) -> Router {
    let client = Client::builder(TokioExecutor::new()).build_http();
    let mut router = Router::new();
    for route in routes {
        let prefix = route.prefix.clone();
        let state = Arc::new(ProxyState { route, client: client.clone(), timeout });
        let handler = any(forward).with_state(state);
        router = router.route(&prefix, handler.clone()).route(&format!("{prefix}/"), handler.clone()).route(&format!("{prefix}/{{*rest}}"), handler);
    }
    router
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_specs_are_validated() {
        assert!(ProxyRoute::parse("/brain/faces=http://127.0.0.1:8190", None).is_ok());
        assert_eq!(ProxyRoute::parse("/x=http://h:1/", None).unwrap().upstream, "http://h:1");
        for bad in ["brain=http://h:1", "/=http://h:1", "/a/=http://h:1", "/a/../b=http://h:1", "/a=https://h:1", "/a=pas-une-url", "/a", "/a//b=http://h:1"] {
            assert!(ProxyRoute::parse(bad, None).is_err(), "{bad}");
        }
    }
}
