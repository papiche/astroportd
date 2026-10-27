//! Reverse proxy intégré : préfixes, flux, garde d'accès, en-têtes, erreurs amont.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use astro_ui::{Gate, ProxyRoute, proxy_router};
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::any;
use tower::ServiceExt;

/// Service amont : renvoie ce qu'il reçoit (méthode, chemin+requête, en-têtes utiles, corps).
async fn upstream() -> String {
    let app = Router::new()
        .route(
            "/slow",
            any(|| async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                "trop tard"
            }),
        )
        .route("/missing", any(|| async { (StatusCode::NOT_FOUND, "absent côté amont").into_response() }))
        .fallback(any(|req: Request| async move {
            let (parts, body) = req.into_parts();
            let bytes = axum::body::to_bytes(body, 8 << 20).await.unwrap();
            let h = |n: &str| parts.headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or("-").to_owned();
            let head = format!(
                "{} {} host={} xff={} xfh={} keep={} auth={} len={}\n",
                parts.method,
                parts.uri.path_and_query().unwrap(),
                h("host"),
                h("x-forwarded-for"),
                h("x-forwarded-host"),
                h("keep-alive"),
                h("authorization"),
                bytes.len()
            );
            ([("connection", "x-hop"), ("x-hop", "secret"), ("x-custom", "gardé")], [head.into_bytes(), bytes.to_vec()].concat()).into_response()
        }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

fn gate(allow: bool) -> Gate {
    Arc::new(move |headers, _method, _uri| Box::pin(async move { allow && headers.get(header::AUTHORIZATION).is_some() }))
}

async fn call(app: &Router, method: &str, path: &str, body: Vec<u8>, extra: &[(&str, &str)]) -> (u16, axum::http::HeaderMap, Vec<u8>) {
    let mut b = Request::builder().method(method).uri(path).header(header::HOST, "station.example");
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let mut req = b.body(Body::from(body)).unwrap();
    req.extensions_mut().insert(ConnectInfo("203.0.113.7:4444".parse::<SocketAddr>().unwrap()));
    let r = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = r.into_parts();
    (parts.status.as_u16(), parts.headers, axum::body::to_bytes(body, 8 << 20).await.unwrap().to_vec())
}

#[tokio::test]
async fn prefixes_are_stripped_headers_adjusted_and_bodies_streamed() {
    let up = upstream().await;
    let route = ProxyRoute::parse(&format!("/brain/faces={up}"), None).unwrap();
    let app = proxy_router(vec![route], Duration::from_secs(5));
    let payload = vec![7u8; 1 << 20];
    let (status, headers, body) = call(&app, "POST", "/brain/faces/v1/detect?model=buffalo_l&x=1", payload.clone(), &[("keep-alive", "timeout=5"), ("connection", "keep-alive")]).await;
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&body[..body.iter().position(|b| *b == b'\n').unwrap()]).into_owned();
    let upstream_host = up.trim_start_matches("http://");
    assert_eq!(text, format!("POST /v1/detect?model=buffalo_l&x=1 host={upstream_host} xff=203.0.113.7 xfh=station.example keep=- auth=- len=1048576"), "{text}");
    assert_eq!(&body[text.len() + 1..], payload.as_slice(), "corps de 1 Mo relayé intact");
    assert_eq!(headers["x-custom"], "gardé");
    assert!(headers.get("x-hop").is_none() && headers.get("connection").is_none(), "en-têtes hop-by-hop retirés");
    // La racine du préfixe et le préfixe suivi de « / » joignent l'amont sur « / ».
    for p in ["/brain/faces", "/brain/faces/"] {
        let (s, _, b) = call(&app, "GET", p, vec![], &[]).await;
        assert_eq!(s, 200);
        assert!(String::from_utf8_lossy(&b).starts_with("GET / host="), "{p}");
    }
    // Statuts de l'amont conservés ; chemins hors préfixe : non routés.
    assert_eq!(call(&app, "GET", "/brain/faces/missing", vec![], &[]).await.0, 404);
    assert_eq!(call(&app, "GET", "/autre", vec![], &[]).await.0, 404);
    assert_eq!(call(&app, "GET", "/brain/facesX", vec![], &[]).await.0, 404, "préfixe voisin non capturé");
}

#[tokio::test]
async fn access_gate_upstream_failures_and_timeouts() {
    let up = upstream().await;
    let guarded = ProxyRoute::parse(&format!("/ollama={up}"), Some(gate(true))).unwrap();
    let denied = ProxyRoute::parse(&format!("/comfy={up}"), Some(gate(false))).unwrap();
    let dead = ProxyRoute::parse("/dead=http://127.0.0.1:1", None).unwrap();
    let app = proxy_router(vec![guarded, denied, dead], Duration::from_millis(500));

    let (s, h, _) = call(&app, "GET", "/ollama/api/tags", vec![], &[]).await;
    assert_eq!(s, 401);
    assert_eq!(h.get_all(header::WWW_AUTHENTICATE).iter().count(), 2, "Basic et Nostr annoncés");
    let (s, _, b) = call(&app, "GET", "/ollama/api/tags", vec![], &[("authorization", "Basic abc")]).await;
    assert_eq!(s, 200);
    assert!(String::from_utf8_lossy(&b).contains("auth=Basic abc"), "l'authentification est transmise à l'amont");
    assert_eq!(call(&app, "GET", "/comfy/x", vec![], &[("authorization", "Basic abc")]).await.0, 401, "garde qui refuse");
    assert_eq!(call(&app, "GET", "/dead/x", vec![], &[]).await.0, 502, "amont injoignable");
    let slow = ProxyRoute::parse(&format!("/slow={up}"), None).unwrap();
    let app2 = proxy_router(vec![slow], Duration::from_millis(300));
    assert_eq!(call(&app2, "GET", "/slow/slow", vec![], &[]).await.0, 504, "délai dépassé");
}

mod websocket {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use astro_ui::{Gate, ProxyRoute, proxy_router};
    use axum::Router;
    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::routing::get;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as Tm;

    /// Amont WebSocket façon ComfyUI : renvoie chaque message, et annonce le premier en-tête de sous-protocole reçu.
    async fn ws_upstream() -> String {
        let app = Router::new().route(
            "/ws",
            get(|ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut socket| async move {
                    while let Some(Ok(msg)) = socket.recv().await {
                        match msg {
                            Message::Text(t) => {
                                let _ = socket.send(Message::Text(format!("écho:{t}").into())).await;
                            }
                            Message::Binary(b) => {
                                let _ = socket.send(Message::Binary(b)).await;
                            }
                            Message::Close(_) => return,
                            _ => {}
                        }
                    }
                })
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        url
    }

    async fn serve(router: Router) -> SocketAddr {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, router.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
        addr
    }

    #[tokio::test]
    async fn websockets_are_relayed_both_ways_through_the_proxy() {
        let up = ws_upstream().await;
        let app = proxy_router(vec![ProxyRoute::parse(&format!("/comfy={up}"), None).unwrap()], Duration::from_secs(5));
        let addr = serve(app).await;
        let (mut ws, resp) = connect_async(format!("ws://{addr}/comfy/ws")).await.unwrap();
        assert_eq!(resp.status(), 101);
        ws.send(Tm::text("bonjour")).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap().as_str(), "écho:bonjour");
        // Gros message binaire : plusieurs trames TCP, intégrité conservée.
        let big: Vec<u8> = (0..1_500_000u32).map(|i| (i % 251) as u8).collect();
        ws.send(Tm::binary(big.clone())).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap().into_data().to_vec(), big);
        for i in 0..20 {
            ws.send(Tm::text(format!("m{i}"))).await.unwrap();
        }
        for i in 0..20 {
            assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap().as_str(), format!("écho:m{i}"));
        }
        ws.close(None).await.unwrap();
    }

    #[tokio::test]
    async fn the_access_gate_applies_to_the_websocket_handshake() {
        let up = ws_upstream().await;
        let gate: Gate = Arc::new(|headers, _, _| Box::pin(async move { headers.get("authorization").is_some() }));
        let app = proxy_router(vec![ProxyRoute::parse(&format!("/comfy={up}"), Some(gate)).unwrap()], Duration::from_secs(5));
        let addr = serve(app).await;
        let err = connect_async(format!("ws://{addr}/comfy/ws")).await.unwrap_err().to_string();
        assert!(err.contains("401"), "{err}");
        let mut req = format!("ws://{addr}/comfy/ws").into_client_request().unwrap();
        req.headers_mut().insert("authorization", "Basic abc".parse().unwrap());
        let (mut ws, resp) = connect_async(req).await.unwrap();
        assert_eq!(resp.status(), 101);
        ws.send(Tm::text("ok")).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap().as_str(), "écho:ok");
    }

    #[tokio::test]
    async fn a_dead_upstream_answers_502_instead_of_hanging_the_handshake() {
        let app = proxy_router(vec![ProxyRoute::parse("/comfy=http://127.0.0.1:1", None).unwrap()], Duration::from_secs(2));
        let addr = serve(app).await;
        let err = connect_async(format!("ws://{addr}/comfy/ws")).await.unwrap_err().to_string();
        assert!(err.contains("502"), "{err}");
    }
}
