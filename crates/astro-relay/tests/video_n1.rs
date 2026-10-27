//! N¹ universel des vidéos : la liste de contacts (kind 3) de l'auteur, hébergé ou distant, décide.

#![allow(clippy::unwrap_used)]

use astro_db::Db;
use astro_nostr_proto::{Event, kind};
use astro_relay::{AuthState, CircleProvider, DbCircle, Policy};
use rusqlite::types::Value;

fn key(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn ev(id: u8, author: u8, k: u32, at: u64, tags: Vec<Vec<String>>) -> Event {
    Event { id: key(id), pubkey: key(author), created_at: at, kind: k, tags, content: String::new(), sig: "0".repeat(128) }
}

async fn store(db: &Db, e: &Event) {
    db.writer()
        .execute(
            "INSERT INTO nostr_events(id, pubkey, kind, created_at, content, sig, tags) VALUES (?1, ?2, ?3, ?4, '', ?5, ?6)",
            vec![Value::Text(e.id.clone()), Value::Text(e.pubkey.clone()), Value::Integer(i64::from(e.kind)), Value::Integer(e.created_at as i64), Value::Text(e.sig.clone()), Value::Text(serde_json::to_string(&e.tags).unwrap())],
        )
        .await
        .unwrap();
}

fn follows(id: u8, author: u8, at: u64, who: &[u8]) -> Event {
    ev(id, author, kind::CONTACTS, at, who.iter().map(|w| vec!["p".to_owned(), key(*w)]).collect())
}

#[tokio::test]
async fn a_remote_authors_kind3_decides_who_reads_the_video() {
    let dir = std::env::temp_dir().join(format!("astroportd-video-n1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("u.db"), 2).unwrap();
    // L'auteur distant (50) n'est hébergé nulle part ; il suit 51 (ami) ; 52 le suit mais n'est pas suivi.
    let video = ev(1, 50, kind::VIDEO, 100, vec![]);
    store(&db, &video).await;
    store(&db, &follows(2, 50, 90, &[53])).await; // ancienne liste : 53
    store(&db, &follows(3, 50, 200, &[51, 50])).await; // liste courante : 51 (et lui-même, ignoré)
    store(&db, &follows(4, 52, 200, &[50])).await; // 52 suit l'auteur : sans effet sur le N¹ de l'auteur
    // Un autre auteur sans vidéo : son kind 3 n'est pas chargé.
    store(&db, &follows(5, 60, 200, &[51])).await;

    let circle = DbCircle::new();
    circle.refresh(&db).await.unwrap();
    assert!(circle.is_friend_of(&key(50), &key(51)));
    assert!(!circle.is_friend_of(&key(50), &key(53)), "seule la dernière liste compte");
    assert!(!circle.is_friend_of(&key(50), &key(52)), "suivre l'auteur ne donne pas accès");
    assert!(!circle.is_friend_of(&key(50), &key(50)));
    assert!(!circle.is_friend_of(&key(60), &key(51)), "pas de vidéo, pas de N¹ chargé");

    let p = Policy::default();
    let auth = |k: u8| {
        let mut a = AuthState::default();
        a.add(key(k));
        a
    };
    assert!(p.can_read(&video, &auth(50), &circle), "l'auteur relit sa vidéo");
    assert!(p.can_read(&video, &auth(51), &circle), "ami de l'auteur distant");
    assert!(!p.can_read(&video, &auth(52), &circle));
    assert!(!p.can_read(&video, &auth(53), &circle));
    assert!(!p.can_read(&video, &AuthState::default(), &circle));

    // La liste change : l'ami retiré perd l'accès après rafraîchissement.
    store(&db, &follows(6, 50, 300, &[53])).await;
    db.writer().execute("DELETE FROM nostr_events WHERE id = ?1", vec![Value::Text(key(3))]).await.unwrap();
    circle.refresh(&db).await.unwrap();
    assert!(!p.can_read(&video, &auth(51), &circle));
    assert!(p.can_read(&video, &auth(53), &circle));
    let _ = std::fs::remove_dir_all(dir);
}
