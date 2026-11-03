//! Conversations privées : envoi chiffré, réception filtrée par le suivi, non-lus, état d'échec.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use astro_core::ActivityBus;
use astro_crypto::{KeySource, StaticKeySource};
use astro_db::Db;
use astro_db::events::insert_event;
use astro_nostr_proto::pubkey_of;
use astroportd::chat::{Chat, DeliverFn};
use serde_json::json;

const ALICE: [u8; 32] = [0xA1; 32];
const BOB: [u8; 32] = [0xB2; 32];
const EVE: [u8; 32] = [0xE5; 32]; // inconnue : n'est ni suivie ni hébergée

fn hex(s: &[u8; 32]) -> String {
    pubkey_of(s).unwrap()
}

/// Relai local simulé : l'enveloppe entre dans `nostr_events` (ou échoue, pour tester l'état `failed`).
fn into_db(db: &Arc<Db>, fail: bool) -> DeliverFn {
    let db = db.clone();
    Arc::new(move |_to, ev| {
        let db = db.clone();
        Box::pin(async move {
            if fail {
                return Err("station injoignable".to_owned());
            }
            db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.map_err(|e| e.to_string())
        })
    })
}

async fn rig(tag: &str, fail: bool) -> (std::path::PathBuf, Arc<Db>, Chat) {
    let dir = std::env::temp_dir().join(format!("astroportd-chat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    for (s, email) in [(&ALICE, "alice@example.org"), (&BOB, "bob@example.org")] {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex(s).into(), email.to_owned().into()]).await.unwrap();
    }
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BOB));
    let chat = Chat::new(db.clone(), keys, into_db(&db, fail), into_db(&db, false), ActivityBus::default());
    (dir, db, chat)
}

#[tokio::test]
async fn a_message_reaches_the_friend_unread_and_the_sender_keeps_a_copy() {
    let (dir, _db, chat) = rig("roundtrip", false).await;
    let sent = chat.send(&hex(&ALICE), &hex(&BOB), "  Salut Bob 👋  ").await.unwrap();
    assert_eq!((sent["state"].as_str(), sent["text"].as_str()), (Some("ok"), Some("Salut Bob 👋")));
    assert_eq!(chat.poll_once().await, 1, "Bob reçoit ; la copie d'Alice est déjà rangée (aucun doublon)");
    assert_eq!(chat.poll_once().await, 0, "le repère avance : rien n'est relu");

    let bob = chat.conversations(&hex(&BOB)).await.unwrap();
    assert_eq!((bob["unread"].as_i64(), bob["conversations"][0]["peer"].as_str()), (Some(1), Some(hex(&ALICE).as_str())));
    let msgs = chat.messages(&hex(&BOB), &hex(&ALICE), None).await.unwrap();
    assert_eq!((msgs["messages"][0]["text"].as_str(), msgs["messages"][0]["mine"].as_bool()), (Some("Salut Bob 👋"), Some(false)));
    chat.mark_read(&hex(&BOB), &hex(&ALICE)).await.unwrap();
    assert_eq!(chat.conversations(&hex(&BOB)).await.unwrap()["unread"], 0);

    let alice = chat.conversations(&hex(&ALICE)).await.unwrap();
    assert_eq!((alice["unread"].as_i64(), alice["conversations"][0]["mine"].as_bool()), (Some(0), Some(true)), "ses propres messages ne sont jamais « non lus »");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_stranger_who_is_not_followed_is_ignored_but_a_followed_one_gets_through() {
    let (dir, db, chat) = rig("filter", false).await;
    // Eve écrit à Alice : ni suivie, ni hébergée ici.
    let eve = Chat::new(db.clone(), Arc::new(StaticKeySource::new().with(EVE)), into_db(&db, false), into_db(&db, false), ActivityBus::default());
    eve.send(&hex(&EVE), &hex(&ALICE), "Gagnez des ẑen !").await.unwrap();
    chat.poll_once().await;
    assert_eq!(chat.conversations(&hex(&ALICE)).await.unwrap()["conversations"].as_array().unwrap().len(), 0, "inconnue : rien n'apparaît");
    // Alice suit Eve : un nouveau message passe (l'ancien, lu une fois, n'est pas rejoué : le repère a avancé).
    db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ALICE).into(), hex(&EVE).into()]).await.unwrap();
    eve.send(&hex(&EVE), &hex(&ALICE), "Bonjour, c'est Eve").await.unwrap();
    chat.poll_once().await;
    let m = chat.messages(&hex(&ALICE), &hex(&EVE), None).await.unwrap();
    assert_eq!(m["messages"].as_array().unwrap().len(), 1);
    assert_eq!(m["messages"][0]["text"], "Bonjour, c'est Eve");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_member_of_the_n2_circle_filled_by_replication_gets_through() {
    let (dir, db, chat) = rig("n2", false).await;
    // Eve n'est pas suivie directement : elle est dans le cercle N² (niveau 2), alimenté par la réplication.
    db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 2, 0)", vec![hex(&ALICE).into(), hex(&EVE).into()]).await.unwrap();
    let eve = Chat::new(db.clone(), Arc::new(StaticKeySource::new().with(EVE)), into_db(&db, false), into_db(&db, false), ActivityBus::default());
    eve.send(&hex(&EVE), &hex(&ALICE), "Salut depuis N²").await.unwrap();
    chat.poll_once().await;
    let m = chat.messages(&hex(&ALICE), &hex(&EVE), None).await.unwrap();
    assert_eq!(m["messages"].as_array().unwrap().len(), 1);
    assert_eq!(m["messages"][0]["text"], "Salut depuis N²");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_failed_delivery_is_kept_and_said_and_bad_input_is_refused() {
    let (dir, _db, chat) = rig("fail", true).await;
    let sent = chat.send(&hex(&ALICE), &hex(&BOB), "Tu es là ?").await.unwrap();
    assert_eq!((sent["state"].as_str(), sent["error"].as_str()), (Some("failed"), Some("station injoignable")));
    let m = chat.messages(&hex(&ALICE), &hex(&BOB), None).await.unwrap();
    assert_eq!(m["messages"][0]["state"], "failed", "le message n'est pas perdu");
    assert!(chat.send(&hex(&ALICE), &hex(&BOB), "   ").await.is_err());
    assert!(chat.send(&hex(&ALICE), &hex(&BOB), &"x".repeat(4001)).await.is_err());
    assert!(chat.send(&hex(&ALICE), &hex(&ALICE), "moi à moi").await.is_err());
    assert!(chat.send(&hex(&ALICE), "pas-une-cle", "x").await.is_err());
    assert!(chat.messages(&hex(&ALICE), "../etc", None).await.is_err());
    // Un compte sans clé ici ne peut rien envoyer.
    let ghost = chat.send(&hex(&EVE), &hex(&BOB), "x").await.unwrap_err();
    assert!(ghost.to_string().contains("clé"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn history_pages_back_in_time() {
    let (dir, _db, chat) = rig("page", false).await;
    for i in 0..70 {
        // Horodatages distincts sans attendre : on range directement via l'envoi puis on décale la date.
        chat.send(&hex(&ALICE), &hex(&BOB), &format!("message {i}")).await.unwrap();
    }
    let first = chat.messages(&hex(&ALICE), &hex(&BOB), None).await.unwrap();
    assert_eq!(first["messages"].as_array().unwrap().len(), 60);
    assert_eq!(first["more"], true);
    let texts: Vec<&str> = first["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert_eq!((texts[0], texts[59]), ("message 10", "message 69"), "ordre d'envoi respecté même dans la même seconde");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn one_conversation_per_person_across_planes_and_the_channel_is_the_shared_uplanet() {
    use astroportd::person_keys::{PersonKeys, Sibling};
    const LEA_ORIGIN: [u8; 32] = [0x11; 32];
    const LEA_ZEN: [u8; 32] = [0x22; 32];
    let dir = std::env::temp_dir().join(format!("astroportd-chat-planes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("zen1")).unwrap();
    // Plan courant (origin) : Alice, et Léa qui y a une clé ; plan voisin zen1 : Léa y a une autre clé, Alice aussi.
    let origin = Arc::new(Db::open(&dir.join("o.db"), 2).unwrap());
    let zen = Arc::new(Db::open(&dir.join("zen1/uplanet.db"), 2).unwrap());
    for (db, s, email) in [(&origin, &ALICE, "alice@example.org"), (&origin, &LEA_ORIGIN, "lea@example.org"), (&zen, &LEA_ZEN, "lea@example.org"), (&zen, &BOB, "alice@example.org")] {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex(s).into(), email.to_owned().into()]).await.unwrap();
    }
    // Un message déjà reçu dans zen1 de la Léa de zen1 par l'Alice de zen1 (clé BOB ici : même email qu'Alice).
    zen.writer().execute("INSERT INTO chat_messages(owner_hex, id, peer, mine, at, text, state, read, n) VALUES (?1, 'z1', ?2, 0, 500, 'Coucou depuis zen1', 'ok', 0, 1)", vec![hex(&BOB).into(), hex(&LEA_ZEN).into()]).await.unwrap();
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(LEA_ORIGIN));
    let people = Arc::new(PersonKeys::new(origin.clone(), "origin".into(), vec![Sibling { name: "zen1".into(), db: dir.join("zen1/uplanet.db"), game: None }], keys.clone(), publish_one(&origin)));
    let chat = Chat::new(origin.clone(), keys, into_db(&origin, false), into_db(&origin, false), ActivityBus::default()).with_people(people);
    // Alice écrit à la Léa de zen1 : le canal devient la clé de Léa dans origin (l'UPlanet commune).
    let sent = chat.send(&hex(&ALICE), &hex(&LEA_ZEN), "Salut Léa").await.unwrap();
    assert_eq!(sent["peer"], hex(&LEA_ORIGIN).as_str(), "{sent}");
    let list = chat.conversations(&hex(&ALICE)).await.unwrap();
    let convs = list["conversations"].as_array().unwrap();
    assert_eq!(convs.len(), 1, "une seule conversation pour la personne : {list}");
    assert_eq!(convs[0]["channels"].as_array().unwrap().len(), 2, "origin + zen1 : {list}");
    assert_eq!(list["unread"], 0, "les non lus du plan voisin ne gonflent pas la pastille d'ici");
    // Léa sans clé dans le plan courant : l'erreur dit où écrire.
    let only_zen = hex(&[0x77; 32]);
    zen.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'zoe@example.org', 1)", vec![only_zen.clone().into()]).await.unwrap();
    let err = chat.send(&hex(&ALICE), &only_zen, "x").await.unwrap_err().to_string();
    assert!(err.contains("zen1"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

fn publish_one(db: &Arc<Db>) -> astroportd::register::PublishFn {
    let d = into_db(db, false);
    Arc::new(move |ev| d(String::new(), ev))
}

/// Le flux d'activité poussé au navigateur : les événements d'appel.
fn calls(bus: &ActivityBus) -> Vec<(String, serde_json::Value)> {
    bus.recent(100).iter().filter(|a| a.kind == "call").map(|a| (a.source.clone(), a.data.clone().unwrap_or_default())).collect()
}

#[tokio::test]
async fn a_call_rings_the_friend_live_carries_the_handshake_and_a_missed_call_stays_in_the_conversation() {
    let dir = std::env::temp_dir().join(format!("astroportd-chat-call-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    for (s, email) in [(&ALICE, "alice@example.org"), (&BOB, "bob@example.org")] {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex(s).into(), email.to_owned().into()]).await.unwrap();
    }
    let bus = ActivityBus::default();
    let chat = Chat::new(db.clone(), Arc::new(StaticKeySource::new().with(ALICE).with(BOB)), into_db(&db, false), into_db(&db, false), bus.clone());
    let (alice, bob) = (hex(&ALICE), hex(&BOB));
    let sdp = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n";

    // Alice appelle : la sonnerie arrive chez Bob avec l'offre, et rien n'est rangé dans la conversation.
    chat.signal(&alice, &bob, &json!({ "type": "offer", "call": "c0ffee01", "sdp": sdp, "video": true })).await.unwrap();
    assert_eq!(chat.poll_once().await, 0);
    let rung = calls(&bus);
    assert_eq!(rung.len(), 1);
    assert_eq!((rung[0].0.as_str(), rung[0].1["from"].as_str(), rung[0].1["sdp"].as_str(), rung[0].1["call"].as_str()), ("call:offer", Some(alice.as_str()), Some(sdp), Some("c0ffee01")));
    // L'enveloppe expire : le relai l'oubliera (elle contient des adresses réseau).
    let expiring: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM nostr_events WHERE kind = 1059 AND expires_at IS NOT NULL", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(expiring, 1);

    // Bob décroche : la réponse revient à Alice, et les autres onglets de Bob cessent de sonner.
    chat.signal(&bob, &alice, &json!({ "type": "answer", "call": "c0ffee01", "sdp": sdp })).await.unwrap();
    assert!(calls(&bus).iter().any(|(s, d)| s == "call:handled" && d["call"] == "c0ffee01" && d["from"] == alice.as_str()));
    chat.poll_once().await;
    assert!(calls(&bus).iter().any(|(s, d)| s == "call:answer" && d["from"] == bob.as_str()));
    assert!(chat.conversations(&alice).await.unwrap()["conversations"].as_array().unwrap().is_empty(), "un appel abouti ne laisse rien dans le fil");
    // Réseau faible : Bob passe en voix seule, Alice reçoit la demande de couper sa vidéo vers lui (sans SDP, sans trace).
    chat.signal(&bob, &alice, &json!({ "type": "media", "call": "c0ffee01", "video": false })).await.unwrap();
    chat.poll_once().await;
    assert!(calls(&bus).iter().any(|(s, d)| s == "call:media" && d["video"] == false && d["from"] == bob.as_str()), "{:?}", calls(&bus));
    assert!(chat.signal(&bob, &alice, &json!({ "type": "media", "call": "c0ffee01", "sdp": sdp })).await.is_err(), "pas de SDP hors offre et réponse");

    // Un autre appel, raccroché avant qu'on décroche : « Appel manqué » chez Bob (non lu), « Appel sans réponse » chez Alice.
    chat.signal(&alice, &bob, &json!({ "type": "offer", "call": "c0ffee02", "sdp": sdp })).await.unwrap();
    chat.signal(&alice, &bob, &json!({ "type": "hangup", "call": "c0ffee02", "missed": true })).await.unwrap();
    assert_eq!(chat.poll_once().await, 1);
    let b = chat.messages(&bob, &alice, None).await.unwrap();
    assert_eq!(b["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap()).collect::<Vec<_>>(), ["📹 Appel manqué"]);
    assert_eq!(chat.conversations(&bob).await.unwrap()["unread"], 1);
    assert_eq!(chat.messages(&alice, &bob, None).await.unwrap()["messages"][0]["text"], "📹 Appel sans réponse");
    assert!(calls(&bus).iter().any(|(s, _)| s == "call:hangup"), "la sonnerie s'arrête aussi en direct");

    // Une sonnerie vieille de plus d'une minute (station éteinte) ne sonne pas : elle devient un appel manqué.
    let old = astro_crypto::nip59::Rumor { pubkey: alice.clone(), created_at: 1_700_000_000, kind: astro_nostr_proto::kind::CALL_SIGNAL, tags: vec![vec!["p".into(), bob.clone()]], content: json!({ "type": "offer", "call": "c0ffee03", "sdp": sdp }).to_string() };
    let wrap = astro_crypto::nip59::gift_wrap(&ALICE, &bob, &old, 1_700_000_000).unwrap();
    db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
    let before = calls(&bus).len();
    assert_eq!(chat.poll_once().await, 1);
    assert_eq!(calls(&bus).len(), before, "pas de sonnerie pour un appel périmé");
    assert_eq!(chat.messages(&bob, &alice, None).await.unwrap()["messages"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn only_friends_can_ring_and_malformed_signals_are_refused() {
    let (dir, db, chat) = rig("call-guard", false).await;
    let sdp = "v=0\r\n";
    let bus = ActivityBus::default();
    let eve = Chat::new(db.clone(), Arc::new(StaticKeySource::new().with(EVE)), into_db(&db, false), into_db(&db, false), bus.clone());
    eve.signal(&hex(&EVE), &hex(&ALICE), &json!({ "type": "offer", "call": "deadbeef", "sdp": sdp })).await.unwrap();
    chat.poll_once().await;
    assert!(chat.conversations(&hex(&ALICE)).await.unwrap()["conversations"].as_array().unwrap().is_empty(), "une inconnue ne fait pas sonner, et ne laisse rien");
    let (a, b) = (hex(&ALICE), hex(&BOB));
    for bad in [
        json!({ "type": "sonne", "call": "deadbeef" }),
        json!({ "type": "offer", "call": "deadbeef" }),
        json!({ "type": "hangup", "call": "deadbeef", "sdp": sdp }),
        json!({ "type": "offer", "call": "pas-hex!", "sdp": sdp }),
        json!({ "type": "offer", "call": "abc", "sdp": sdp }),
        json!({ "type": "offer", "call": "deadbeef", "sdp": "x".repeat(70_000) }),
    ] {
        assert!(chat.signal(&a, &b, &bad).await.is_err(), "{bad}");
    }
    assert!(chat.signal(&a, &a, &json!({ "type": "offer", "call": "deadbeef", "sdp": sdp })).await.is_err(), "pas d'appel à soi-même");
    // Station de l'ami injoignable : l'échec est dit.
    let (dir2, _db2, down) = rig("call-down", true).await;
    assert!(matches!(down.signal(&a, &b, &json!({ "type": "offer", "call": "deadbeef", "sdp": sdp })).await, Err(astroportd::chat::ChatError::Unreachable(_))));
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}

#[tokio::test]
async fn the_call_relay_is_offered_only_to_station_members_and_mutual_friends() {
    let dir = std::env::temp_dir().join(format!("astroportd-chat-turn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    for (s, email) in [(&ALICE, "alice@example.org"), (&BOB, "bob@example.org")] {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex(s).into(), email.to_owned().into()]).await.unwrap();
    }
    let bus = ActivityBus::default();
    let keys = Arc::new(StaticKeySource::new().with(ALICE).with(BOB).with(EVE));
    let chat = Chat::new(db.clone(), keys, into_db(&db, false), into_db(&db, false), bus.clone()).with_turn([7; 32], 443);
    let (alice, bob, eve) = (hex(&ALICE), hex(&BOB), hex(&EVE));
    let offer = |call: &str| json!({ "type": "offer", "call": call, "sdp": "v=0\r\n", "turn_host": "station.example.org" });
    let ice_of = |call: &str| calls(&bus).into_iter().find(|(s, d)| s == "call:offer" && d["call"] == call).map(|(_, d)| d["ice"].clone());

    // Bob est membre de la station : son offre emporte un accès au relais, fabriqué pour lui seul.
    chat.signal(&alice, &bob, &offer("aaaa0001")).await.unwrap();
    chat.poll_once().await;
    let ice = ice_of("aaaa0001").unwrap();
    assert_eq!(ice["urls"][0], "turn:station.example.org:443?transport=udp");
    assert!(ice["username"].as_str().unwrap().ends_with(&format!("n1-{}", &bob[..16])) && ice["credential"].as_str().is_some_and(|c| !c.is_empty()));

    // Eve (autre station) : Bob la suit, mais elle ne le suit pas → pas de relais ; l'appel part quand même.
    db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0), (?2, ?1, 1, 0)", vec![bob.clone().into(), eve.clone().into()]).await.unwrap();
    let wraps = |db: &Arc<Db>, to: &str| {
        let (db, to) = (db.clone(), to.to_owned());
        async move { db.readers().read(move |c| Ok(c.query_row("SELECT count(*) FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'p' AND t.value = ?1 WHERE e.kind = 1059", [to], |r| r.get::<_, i64>(0))?)).await.unwrap() }
    };
    let before = wraps(&db, &eve).await;
    chat.signal(&bob, &eve, &offer("bbbb0001")).await.unwrap();
    assert_eq!(wraps(&db, &eve).await, before + 1, "l'appel est remis");
    let opened = |call: &'static str| {
        let db = db.clone();
        let eve = eve.clone();
        async move {
            let evs: Vec<astro_nostr_proto::Event> = db.readers().read(move |c| {
                let mut st = c.prepare("SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'p' AND t.value = ?1 WHERE e.kind = 1059")?;
                let mut rows = st.query([eve])?;
                let mut out = Vec::new();
                while let Some(r) = rows.next()? { out.push(astro_db::events::row_to_event(r)?); }
                Ok(out)
            }).await.unwrap();
            evs.iter().filter_map(|w| astro_crypto::nip59::unwrap(&EVE, w).ok()).map(|o| serde_json::from_str::<serde_json::Value>(&o.rumor.content).unwrap()).find(|v| v["call"] == call).unwrap()
        }
    };
    assert!(opened("bbbb0001").await.get("ice").is_none(), "suivie d'un seul côté : pas d'accès au relais");
    // Eve suit Bob en retour (sa liste kind 3) : amis réciproques, l'accès est joint.
    let follow = astro_nostr_proto::UnsignedEvent { created_at: 1_800_000_000, kind: 3, tags: vec![vec!["p".into(), bob.clone()]], content: String::new() }.sign(&EVE).unwrap();
    db.writer().transaction(move |tx| insert_event(tx, &follow).map(|_| ())).await.unwrap();
    chat.signal(&bob, &eve, &offer("bbbb0002")).await.unwrap();
    assert!(opened("bbbb0002").await["ice"]["username"].as_str().unwrap().ends_with(&format!("n1-{}", &eve[..16])));
    // Un nom d'hôte piégé n'est jamais recopié.
    chat.signal(&alice, &bob, &json!({ "type": "offer", "call": "aaaa0002", "sdp": "v=0\r\n", "turn_host": "x.org?transport=tcp&evil" })).await.unwrap();
    chat.poll_once().await;
    assert!(ice_of("aaaa0002").unwrap().is_null());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn in_a_group_call_a_friend_of_my_friend_reaches_me_only_inside_the_room_my_friend_opened() {
    let dir = std::env::temp_dir().join(format!("astroportd-chat-room-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    // Alice (hôte) et Bob vivent ici ; Bob ne suit pas Eve, mais suit Alice.
    for (s, email) in [(&ALICE, "alice@example.org"), (&BOB, "bob@example.org")] {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex(s).into(), email.to_owned().into()]).await.unwrap();
    }
    db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&BOB).into(), hex(&ALICE).into()]).await.unwrap();
    let bus = ActivityBus::default();
    let keys = Arc::new(StaticKeySource::new().with(ALICE).with(BOB).with(EVE));
    let chat = Chat::new(db.clone(), keys, into_db(&db, false), into_db(&db, false), bus.clone());
    let (alice, bob, eve) = (hex(&ALICE), hex(&BOB), hex(&EVE));
    let got = |src: &str, from: &str, room: &str| calls(&bus).iter().any(|(s, d)| s == src && d["from"] == from && d["call"] == room);

    // Eve, inconnue de Bob, ne peut ni le faire sonner ni rejoindre une salle qu'elle prétend ouvrir.
    chat.signal(&eve, &bob, &json!({ "type": "offer", "call": "f00d0001", "sdp": "v=0\r\n", "roster": [eve, bob] })).await.unwrap();
    chat.poll_once().await;
    assert!(!got("call:offer", &eve, "f00d0001"), "une inconnue ne fait pas sonner");

    // Alice invite Bob dans une salle où figure Eve : Eve peut alors joindre Bob, dans cette salle seulement.
    chat.signal(&alice, &bob, &json!({ "type": "offer", "call": "f00d0002", "sdp": "v=0\r\n", "roster": [alice, bob, eve] })).await.unwrap();
    chat.poll_once().await;
    let offer = calls(&bus).into_iter().find(|(s, d)| s == "call:offer" && d["call"] == "f00d0002").unwrap().1;
    assert_eq!((offer["friend"].as_bool(), offer["roster"].as_array().unwrap().len()), (Some(true), 3));
    chat.signal(&eve, &bob, &json!({ "type": "offer", "call": "f00d0002", "sdp": "v=0\r\n" })).await.unwrap();
    chat.poll_once().await;
    let from_eve = calls(&bus).into_iter().find(|(s, d)| s == "call:offer" && d["from"] == eve.as_str() && d["call"] == "f00d0002").unwrap().1;
    assert_eq!(from_eve["friend"], false, "le navigateur sait qu'il ne doit pas sonner pour elle : seulement l'accepter dans la salle en cours");
    chat.signal(&eve, &bob, &json!({ "type": "join", "call": "f00d0003" })).await.unwrap();
    chat.poll_once().await;
    assert!(!got("call:join", &eve, "f00d0003"), "une autre salle : refusée");

    // Seul l'hôte réécrit la liste : Eve ne peut pas y ajouter un complice.
    let mallory = hex(&[0x77; 32]);
    chat.signal(&eve, &bob, &json!({ "type": "roster", "call": "f00d0002", "roster": [alice, bob, eve, mallory] })).await.unwrap();
    chat.signal(&alice, &bob, &json!({ "type": "roster", "call": "f00d0002", "roster": [alice, bob] })).await.unwrap();
    chat.poll_once().await;
    chat.signal(&eve, &bob, &json!({ "type": "join", "call": "f00d0002" })).await.unwrap();
    chat.poll_once().await;
    assert!(!got("call:join", &eve, "f00d0002"), "retirée de la liste par l'hôte : plus admise");

    // Une liste invalide est refusée à l'envoi : plus de 4 participants, doublon, clé illisible.
    let five: Vec<String> = (1..=5u8).map(|i| hex(&[i; 32])).collect();
    for bad in [json!(five), json!([alice, alice]), json!(["pas-une-cle"]), json!([])] {
        assert!(chat.signal(&alice, &bob, &json!({ "type": "roster", "call": "f00d0002", "roster": bad })).await.is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn an_envelope_accepted_by_the_relay_wakes_the_reader_at_once() {
    let (dir, _db, chat) = rig("wake", false).await;
    let chat = Arc::new(chat);
    let (live, relay) = tokio::sync::broadcast::channel(16);
    let (stop_tx, stop) = tokio::sync::watch::channel(false);
    // Contrôle régulier toutes les 60 s : seul le réveil par le relai peut livrer le message dans le délai du test.
    let task = tokio::spawn(chat.clone().run(std::time::Duration::from_secs(60), relay, stop));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await; // premier passage (à vide)
    chat.send(&hex(&ALICE), &hex(&BOB), "Tu décroches ?").await.unwrap();
    let unread = || async { chat.conversations(&hex(&BOB)).await.unwrap()["unread"].as_i64() };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(unread().await, Some(0), "sans réveil, rien avant le prochain tour");
    // Un autre kind ne réveille pas ; une enveloppe NIP-59, si.
    let other = astro_nostr_proto::UnsignedEvent { created_at: 1_800_000_000, kind: 1, tags: vec![], content: String::new() }.sign(&EVE).unwrap();
    live.send(Arc::new(other)).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(unread().await, Some(0));
    let wrap = astro_nostr_proto::UnsignedEvent { created_at: 1_800_000_000, kind: 1059, tags: vec![vec!["p".into(), hex(&BOB)]], content: String::new() }.sign(&EVE).unwrap();
    live.send(Arc::new(wrap)).unwrap();
    let start = std::time::Instant::now();
    while unread().await != Some(1) {
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "le message aurait dû être lu tout de suite");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    stop_tx.send(true).unwrap();
    task.await.unwrap();
    let _ = std::fs::remove_dir_all(dir);
}
