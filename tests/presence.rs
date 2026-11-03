//! Présence visio : sur option, entre amis mutuels seulement ; en direct sur la station, par battement 24315 ailleurs.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use astro_core::ActivityBus;
use astro_crypto::{KeySource, StaticKeySource};
use astro_db::Db;
use astro_db::events::insert_event;
use astro_nostr_proto::{Event, UnsignedEvent, pubkey_of};
use astroportd::chat::DeliverFn;
use astroportd::presence::Presence;

const ALICE: [u8; 32] = [0xA1; 32];
const BOB: [u8; 32] = [0xB2; 32];
const CAROL: [u8; 32] = [0xC3; 32]; // une autre station
const EVE: [u8; 32] = [0xE5; 32]; // ni amie ni mutuelle

fn hex(s: &[u8; 32]) -> String {
    pubkey_of(s).unwrap()
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

struct Rig {
    db: Arc<Db>,
    bus: ActivityBus,
    sent: Arc<Mutex<Vec<(String, Event)>>>,
    p: Presence,
    dir: std::path::PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn rig(tag: &str) -> Rig {
    let dir = std::env::temp_dir().join(format!("astroportd-presence-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    for s in [&ALICE, &BOB] {
        db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'multipass', 1)", vec![hex(s).into()]).await.unwrap();
    }
    let sent: Arc<Mutex<Vec<(String, Event)>>> = Arc::default();
    let deliver: DeliverFn = {
        let sent = sent.clone();
        Arc::new(move |to, ev| {
            sent.lock().unwrap().push((to, ev));
            Box::pin(async { Ok(()) })
        })
    };
    let local: DeliverFn = {
        let db = db.clone();
        Arc::new(move |_to, ev| {
            let db = db.clone();
            Box::pin(async move { db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.map_err(|e| e.to_string()) })
        })
    };
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BOB));
    let bus = ActivityBus::default();
    Rig { p: Presence::new(db.clone(), keys, bus.clone(), deliver, local), db, bus, sent, dir }
}

/// `a` suit `b` (graphe social de la station) et `b` suit `a` (sa liste kind 3 signée).
async fn mutual(r: &Rig, a: &[u8; 32], b: &[u8; 32]) {
    r.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(a).into(), hex(b).into()]).await.unwrap();
    let back = UnsignedEvent { created_at: now(), kind: 3, tags: vec![vec!["p".into(), hex(a)]], content: String::new() }.sign(b).unwrap();
    r.db.writer().transaction(move |tx| insert_event(tx, &back).map(|_| ())).await.unwrap();
}

fn beat(from: &[u8; 32], to: &[u8; 32], status: &str, at: u64) -> Event {
    UnsignedEvent { created_at: at, kind: 24_315, tags: vec![vec!["p".into(), hex(to)], vec!["status".into(), status.into()]], content: String::new() }.sign(from).unwrap()
}

#[tokio::test]
async fn a_mutual_friend_who_chose_to_be_reachable_is_seen_online_in_a_call_then_offline() {
    let r = rig("local").await;
    let (alice, bob) = (hex(&ALICE), hex(&BOB));
    // Kind 3 de Bob : un seul suffit pour les deux sens ici (Alice et Bob sont sur la station) ; chacun suit l'autre.
    mutual(&r, &ALICE, &BOB).await;
    mutual(&r, &BOB, &ALICE).await;
    let tab = r.bus.listen(&bob);
    assert!(r.p.view(&alice).await["friends"].get(&bob).is_none(), "sans son accord, Bob n'apparaît pas");
    r.p.set_reachable(&bob, true).await.unwrap();
    assert_eq!(r.p.view(&alice).await["friends"][&bob], "online");
    assert!(r.bus.recent(20).iter().any(|a| a.kind == "presence" && a.data.as_ref().is_some_and(|d| d["hex"] == bob.as_str() && d["status"] == "online")), "Alice est prévenue en direct");
    r.p.set_in_call(&bob, true).await;
    assert_eq!(r.p.view(&alice).await["friends"][&bob], "visio");
    r.p.set_in_call(&bob, false).await;
    drop(tab);
    assert_eq!(r.p.view(&alice).await["friends"][&bob], "offline", "onglet fermé : hors ligne");
    assert_eq!(r.p.view(&bob).await["reachable"], true);
    assert!(r.sent.lock().unwrap().is_empty(), "rien ne sort de la station pour des amis qui y vivent");
}

#[tokio::test]
async fn a_remote_mutual_friend_receives_a_nip101_heartbeat_and_only_while_reachable() {
    let r = rig("remote").await;
    let (bob, carol, eve) = (hex(&BOB), hex(&CAROL), hex(&EVE));
    mutual(&r, &BOB, &CAROL).await;
    // Eve : Bob la suit, elle ne le suit pas → jamais prévenue.
    r.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![bob.clone().into(), eve.clone().into()]).await.unwrap();
    let _tab = r.bus.listen(&bob);
    r.p.announce(&bob, false).await;
    assert!(r.sent.lock().unwrap().is_empty(), "pas encore joignable : rien n'est annoncé");
    r.p.set_reachable(&bob, true).await.unwrap();
    let (to, ev) = r.sent.lock().unwrap().last().cloned().unwrap();
    assert_eq!(to, carol);
    assert_eq!((ev.kind, ev.content.as_str(), ev.pubkey.as_str()), (24_315, "", bob.as_str()));
    assert_eq!(ev.tags, vec![vec!["p".to_owned(), carol.clone()], vec!["status".to_owned(), "online".to_owned()]], "format NIP-101 : destinataires, puis un statut");
    assert!(ev.verify().is_ok());
    // Bob retire son choix : un dernier battement « offline », puis plus rien.
    r.p.set_reachable(&bob, false).await.unwrap();
    assert_eq!(r.sent.lock().unwrap().last().unwrap().1.tags.last().unwrap()[1], "offline");
    let n = r.sent.lock().unwrap().len();
    r.p.announce(&bob, false).await;
    assert_eq!(r.sent.lock().unwrap().len(), n);
    // Allers-retours rapides (dans la même seconde) : le dernier choix gagne toujours.
    for on in [true, false, true, false] {
        r.p.set_reachable(&bob, on).await.unwrap();
        assert_eq!(r.p.reachable(&bob).await, on);
    }
}

#[tokio::test]
async fn heartbeats_from_elsewhere_are_kept_only_from_mutual_friends_on_time_and_in_order() {
    let r = rig("incoming").await;
    let (alice, carol) = (hex(&ALICE), hex(&CAROL));
    mutual(&r, &ALICE, &CAROL).await;
    let t = now();
    assert_eq!(r.p.on_event(&beat(&CAROL, &ALICE, "online", t)).await, 1);
    assert_eq!(r.p.view(&alice).await["friends"][&carol], "online");
    assert_eq!(r.p.on_event(&beat(&CAROL, &ALICE, "visio", t + 1)).await, 1);
    assert_eq!(r.p.view(&alice).await["friends"][&carol], "visio");
    assert_eq!(r.p.on_event(&beat(&CAROL, &ALICE, "offline", t - 5)).await, 0, "plus ancien que le dernier connu : ignoré");
    assert_eq!(r.p.view(&alice).await["friends"][&carol], "visio");
    // Refus : horloge hors fenêtre, inconnue non mutuelle, statut inconnu, contenu non vide, deux statuts.
    assert_eq!(r.p.on_event(&beat(&CAROL, &ALICE, "online", t - 600)).await, 0);
    assert_eq!(r.p.on_event(&beat(&EVE, &ALICE, "online", t)).await, 0);
    assert_eq!(r.p.on_event(&beat(&CAROL, &ALICE, "danse", t)).await, 0);
    let chatty = UnsignedEvent { created_at: t, kind: 24_315, tags: vec![vec!["p".into(), alice.clone()], vec!["status".into(), "online".into()]], content: "coucou".into() }.sign(&CAROL).unwrap();
    assert_eq!(r.p.on_event(&chatty).await, 0);
    let twice = UnsignedEvent { created_at: t, kind: 24_315, tags: vec![vec!["p".into(), alice.clone()], vec!["status".into(), "online".into()], vec!["status".into(), "visio".into()]], content: String::new() }.sign(&CAROL).unwrap();
    assert_eq!(r.p.on_event(&twice).await, 0);
    assert!(r.p.view(&alice).await["friends"].get(hex(&EVE)).is_none());
}
