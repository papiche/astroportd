//! Moteur interchangeable à chaud : arrêté, il refuse proprement ; remis en marche, il répond.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use astro_facecloud::{AnyFaceEngine, EngineError, FaceEngine, HttpFaceEngine, SwitchableEngine};

#[tokio::test]
async fn a_stopped_engine_refuses_and_reports_unavailable() {
    let e = SwitchableEngine::new(None);
    assert!(!e.available());
    assert_eq!(e.label(), None);
    assert!(matches!(e.detect(b"x").await, Err(EngineError::Backend(m)) if m.contains("arrêté")));
}

#[tokio::test]
async fn switching_changes_the_engine_without_rebuilding_the_holder() {
    let e = SwitchableEngine::new(None);
    e.set(Some(AnyFaceEngine::Http(Box::new(HttpFaceEngine::new("http://127.0.0.1:9/faces", Duration::from_millis(300))))), "Brain test".into());
    assert!(e.available());
    assert_eq!(e.label().as_deref(), Some("Brain test"));
    // Le port 9 refuse : l'erreur vient du moteur distant, pas de l'arrêt.
    assert!(matches!(e.detect(b"x").await, Err(EngineError::Backend(m)) if !m.contains("arrêté")));
    e.set(None, String::new());
    assert!(!e.available());
    assert!(matches!(e.detect(b"x").await, Err(EngineError::Backend(m)) if m.contains("arrêté")));
}
