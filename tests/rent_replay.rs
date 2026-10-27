//! Rectification Sprint 8 : le statut d'un MULTIPASS se reconstruit au replay d'après le dernier loyer constaté
//! (kind 30852 vers le NODE, ou kind 30851 `type=rent` attesté par la station).

#![allow(clippy::unwrap_used)]

use astro_db::{Db, events};
use astro_lifecycle::rebuild::rebuild_all;
use astro_nostr_proto::{Event, UnsignedEvent, kind, pubkey_of};
use astro_sync::replay_local;

const T0: u64 = 1_790_000_000;
const DAY: u64 = 86_400;

fn n2(secret: &[u8; 32], to: &str, amount: &str, d: &str, at: u64) -> Event {
    let tags = vec![vec!["d".into(), d.into()], vec!["p".into(), to.into()], vec!["amount".into(), amount.into()], vec!["prev".into(), "genesis".into()], vec!["t".into(), "g1-n2".into()]];
    UnsignedEvent { created_at: at, kind: kind::LEDGER_N2, tags, content: String::new() }.sign(secret).unwrap()
}

async fn store(db: &Db, e: Event) {
    db.writer().transaction(move |tx| events::insert_event(tx, &e).map(|_| ())).await.unwrap();
}

async fn status_of(db: &Db, hex: String) -> Option<String> {
    db.readers()
        .read(move |c| {
            let v: Option<String> = c.query_row("SELECT value FROM kv WHERE scope = 'lifecycle' AND key = ?1", [hex], |r| r.get(0)).ok();
            Ok(v.map(|j| serde_json::from_str::<serde_json::Value>(&j).unwrap()["status"].as_str().unwrap().to_owned()))
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_rebuilds_the_account_status_from_the_last_rent() {
    let dir = std::env::temp_dir().join(format!("astroportd-rent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("uplanet.db"), 2).unwrap();
    let (node, bob, eve) = (pubkey_of(&[1; 32]).unwrap(), pubkey_of(&[4; 32]).unwrap(), pubkey_of(&[5; 32]).unwrap());
    use rusqlite::types::Value as V;
    let text = |s: &str| V::Text(s.to_owned());
    for (hex, role, email, rental, created) in [(&node, "node", None, false, None), (&bob, "multipass", Some("bob@example.org"), true, Some(T0)), (&eve, "multipass", Some("eve@example.org"), true, Some(T0))] {
        db.writer()
            .execute(
                "INSERT INTO identities(hex, role, email, contract, created_at, hosted) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
                vec![text(hex), text(role), email.map_or(V::Null, text), if rental { text("active_rental") } else { V::Null }, created.map_or(V::Null, |t| V::Integer(t as i64))],
            )
            .await
            .unwrap();
    }
    // Bob paie à J+7 ; Eve ne paie jamais ; un virement vers un tiers non hébergé n'est pas un loyer.
    store(&db, n2(&[4; 32], &node, "50.00", "rent-1", T0 + 7 * DAY)).await;
    store(&db, n2(&[4; 32], &pubkey_of(&[9; 32]).unwrap(), "10.00", "gift", T0 + 20 * DAY)).await;

    let stats = replay_local(&db, 100).await.unwrap();
    assert_eq!(stats.skipped, 0, "{:?}", stats.errors);
    let paid: (i64, i64) = db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*), COALESCE(MAX(amount_cents), 0) FROM rent_payments", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
    assert_eq!(paid, (1, 5_000), "seul le virement vers le NODE est un loyer");

    // À J+13 : Bob (payé à J+7) est à jour, Eve (jamais payé, créée à J0) est en grâce depuis J+7.
    assert_eq!(rebuild_all(&db, T0 + 13 * DAY).await.unwrap(), 2);
    assert_eq!(status_of(&db, bob.clone()).await.as_deref(), Some("active_rental"));
    assert_eq!(status_of(&db, eve.clone()).await.as_deref(), Some("grace_period"));

    // Rejouer reconstruit à l'identique (l'état est une projection) ; plus tard, Bob bascule en grâce puis suspension.
    replay_local(&db, 100).await.unwrap();
    assert_eq!(rebuild_all(&db, T0 + 13 * DAY).await.unwrap(), 2);
    replay_local(&db, 100).await.unwrap();
    rebuild_all(&db, T0 + 30 * DAY).await.unwrap();
    assert_eq!(status_of(&db, bob).await.as_deref(), Some("suspended"));
    assert_eq!(status_of(&db, eve).await.as_deref(), Some("suspended"));

    db.writer().shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
