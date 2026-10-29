//! Réglages du Capitaine : matrice des kinds appliquée à chaud et conservée, moteur FaceID basculé sans redémarrage.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use astro_db::Db;
use astro_facecloud::{FaceEngine, SwitchableEngine};
use astro_relay::{DbCircle, Relay, RelayConfig};
use astroportd::station_admin::{FaceControl, FaceMode, KindsControl, build_policy};
use serde_json::json;

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("astroportd-admin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn relay(db: &Arc<Db>, policy: astro_relay::Policy) -> Arc<Relay> {
    Relay::new(db.clone(), Arc::new(DbCircle::new()), RelayConfig { policy, ..RelayConfig::default() })
}

#[tokio::test]
async fn the_kind_matrix_applies_live_persists_and_rejects_bad_rules() {
    let d = dir("kinds");
    let db = Arc::new(Db::open(&d.join("u.db"), 2).unwrap());
    std::fs::write(d.join("station.conf"), "# station\nkind.30078 = write=open\nkind.1 = read=deny\n").unwrap();
    let initial = KindsControl::initial_policy(&db, &d.join("station.conf")).await;
    assert_eq!(initial.rule(30_078).write.name(), "open", "station.conf lu au démarrage");
    let relay = relay(&db, initial);
    let kc = KindsControl::new(db.clone(), &d.join("station.conf"), relay.clone());

    let view = kc.view().await;
    let row = |v: &serde_json::Value, k: u64| v["kinds"].as_array().unwrap().iter().find(|r| r["kind"] == k).unwrap().clone();
    assert_eq!((row(&view, 1)["read"].as_str(), row(&view, 1)["source"].as_str()), (Some("deny"), Some("station.conf")));
    assert_eq!(row(&view, 7)["source"], "défaut");

    // Le Capitaine ferme l'écriture du kind 7 : appliqué sans redémarrage, conservé en base.
    let view = kc.set(&json!({ "rules": { "7": "read=circle write=deny project=on" } })).await.unwrap();
    assert_eq!((row(&view, 7)["write"].as_str(), row(&view, 7)["source"].as_str()), (Some("deny"), Some("capitaine")));
    assert_eq!(relay.policy().rule(7).write.name(), "deny");
    let again = KindsControl::initial_policy(&db, &d.join("station.conf")).await;
    assert_eq!(again.rule(7).write.name(), "deny", "le réglage survit au redémarrage");
    assert_eq!(again.rule(30_078).write.name(), "open", "les couches se cumulent");

    // Une règle identique à la couche inférieure n'est pas conservée ; « reset » rend la main aux défauts.
    kc.set(&json!({ "rules": { "7": "read=circle write=circle project=on" } })).await.unwrap();
    assert_eq!(KindsControl::initial_policy(&db, &d.join("station.conf")).await.rule(7).write.name(), "circle");
    kc.set(&json!({ "rules": { "7": "write=deny" } })).await.unwrap();
    kc.set(&json!({ "reset": [7] })).await.unwrap();
    assert_eq!(relay.policy().rule(7).write.name(), "circle");

    // Une règle invalide ne change rien (ni la politique ni la base).
    for bad in [json!({ "rules": { "7": "write=tous" } }), json!({ "rules": { "abc": "read=public" } }), json!({ "rules": { "22242": "read=public" } }), json!({ "rules": { "1059": "write=open" } })] {
        assert!(kc.set(&bad).await.is_err(), "{bad}");
    }
    assert_eq!(relay.policy().rule(7).write.name(), "circle");
    assert!(build_policy(&Default::default(), &Default::default()).is_ok());
    let _ = std::fs::remove_dir_all(d);
}

#[tokio::test]
async fn the_face_engine_switches_live_and_the_choice_is_remembered() {
    let d = dir("face");
    let db = Arc::new(Db::open(&d.join("u.db"), 2).unwrap());
    let engine = Arc::new(SwitchableEngine::new(None));
    let fc = FaceControl::new(db.clone(), engine.clone(), 0, d.clone(), None, FaceMode::Off);
    let st = fc.status().await;
    assert_eq!((st["mode"].as_str(), st["label"].is_null(), st["locked"].is_null(), st["pending"].as_i64()), (Some("off"), true, true, Some(0)));

    // Un Brain injoignable est refusé, avec la raison : l'état ne bouge pas.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let err = fc.set(&json!({ "mode": "remote", "url": format!("http://127.0.0.1:{port}/faces") })).await.unwrap_err();
    assert!(err.contains("injoignable"), "{err}");
    assert!(!engine.available());
    assert!(FaceControl::stored(&db).await.is_none(), "un échec n'est pas enregistré");

    // Un Brain joignable est mis en service à chaud et le choix est conservé.
    let alive = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/faces", alive.local_addr().unwrap());
    let st = fc.set(&json!({ "mode": "remote", "url": url })).await.unwrap();
    assert_eq!((st["mode"].as_str(), st["url"].as_str()), (Some("remote"), Some(url.as_str())));
    assert!(engine.available() && engine.label().unwrap().contains(&url));
    assert_eq!(FaceControl::stored(&db).await, Some(FaceMode::Remote(url.clone())));

    // Arrêt : les photos attendent.
    fc.set(&json!({ "mode": "off" })).await.unwrap();
    assert!(!engine.available());
    assert_eq!(FaceControl::stored(&db).await, Some(FaceMode::Off));

    // Mode local sans modèles (ou sans la fonction onnx) : refus explicite.
    let err = fc.set(&json!({ "mode": "local" })).await.unwrap_err();
    assert!(err.contains("modèles") || err.contains("onnx"), "{err}");
    assert!(fc.set(&json!({ "mode": "gpu" })).await.is_err());
    drop(alive);
    let _ = std::fs::remove_dir_all(d);
}

#[tokio::test]
async fn a_command_line_engine_locks_the_setting() {
    let d = dir("locked");
    let db = Arc::new(Db::open(&d.join("u.db"), 2).unwrap());
    let fc = FaceControl::new(db.clone(), Arc::new(SwitchableEngine::new(None)), 0, d.clone(), Some("--face-engine".into()), FaceMode::Remote("http://x.example:1/faces".into()));
    assert_eq!(fc.status().await["locked"], "--face-engine");
    let err = fc.set(&json!({ "mode": "off" })).await.unwrap_err();
    assert!(err.contains("--face-engine"), "{err}");
    assert!(FaceControl::stored(&db).await.is_none());
    let _ = std::fs::remove_dir_all(d);
}
