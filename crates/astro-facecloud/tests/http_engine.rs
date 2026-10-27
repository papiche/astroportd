//! Moteur d'inférence déporté (Brain GPU) : contrat JSON, erreurs, délai.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use astro_facecloud::{FaceEngine, HttpFaceEngine};
use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn parses_faces_and_forwards_the_raw_image() {
    let app = Router::new().route(
        "/faces",
        post(|req: Request| async move {
            let body = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap();
            assert_eq!(&body[..], b"octets-de-l-image");
            let emb: Vec<f32> = (0..512).map(|i| i as f32).collect();
            let json = serde_json::json!({ "faces": [{ "bbox": { "x1": 1.0, "y1": 2.0, "x2": 3.0, "y2": 4.0 }, "det_score": 0.97, "embedding": emb }] });
            ([("content-type", "application/json")], json.to_string())
        }),
    );
    let url = serve(app).await;
    let engine = HttpFaceEngine::new(&format!("{url}/faces"), Duration::from_secs(5));
    let faces = engine.detect(b"octets-de-l-image").await.unwrap();
    assert_eq!(faces.len(), 1);
    assert_eq!((faces[0].bbox.x2, faces[0].det_score, faces[0].embedding.len()), (3.0, 0.97, 512));
}

#[tokio::test]
async fn backend_failures_are_reported() {
    let app = Router::new()
        .route("/err", post(|| async { (StatusCode::SERVICE_UNAVAILABLE, "GPU occupé").into_response() }))
        .route("/bad", post(|| async { "pas du json" }))
        .route("/slow", post(|| async { tokio::time::sleep(Duration::from_secs(5)).await; "{\"faces\":[]}" }));
    let url = serve(app).await;
    let e = |p: &str, t: u64| HttpFaceEngine::new(&format!("{url}{p}"), Duration::from_millis(t));
    assert!(e("/err", 2000).detect(b"x").await.unwrap_err().to_string().contains("503"));
    assert!(e("/bad", 2000).detect(b"x").await.unwrap_err().to_string().contains("illisible"));
    assert!(e("/slow", 100).detect(b"x").await.unwrap_err().to_string().contains("délai"));
    assert!(HttpFaceEngine::new("http://127.0.0.1:1/faces", Duration::from_secs(1)).detect(b"x").await.is_err());
}
