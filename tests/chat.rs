//! Conversations privées : envoi chiffré, réception filtrée par le suivi, non-lus, état d'échec.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use astro_core::ActivityBus;
use astro_crypto::{KeySource, StaticKeySource};
use astro_db::Db;
use astro_db::events::insert_event;
use astro_nostr_proto::pubkey_of;
use astroportd::chat::{Chat, DeliverFn};

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
