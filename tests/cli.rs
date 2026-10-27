//! Test de bout en bout du binaire : `astroportd replay` sur une vraie base.

#![allow(clippy::unwrap_used)]

use std::process::Command;

use astro_db::Db;
use astro_nostr_proto::UnsignedEvent;

fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_astroportd")).args(args).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_rebuilds_projections_from_stored_events() {
    let dir = std::env::temp_dir().join(format!("astroportd-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("uplanet.db");
    {
        let db = Db::open(&path, 1).unwrap();
        let alice = astro_nostr_proto::pubkey_of(&[1; 32]).unwrap();
        let bob = astro_nostr_proto::pubkey_of(&[2; 32]).unwrap();
        db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'multipass', 1)", vec![alice.into()]).await.unwrap();
        let follow = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), bob]], content: String::new() }.sign(&[1; 32]).unwrap();
        db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &follow)).await.unwrap();
        db.writer().shutdown().await.unwrap();
    }
    let (ok, stdout, stderr) = run(&["replay", "--db", path.to_str().unwrap()]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("replay : 1 examinés, 1 projetés, 0 ignorés"), "{stdout}");
    let db = Db::open(&path, 1).unwrap();
    let n: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM social_graph WHERE level = 1", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(n, 1);
    drop(db);

    let (ok, _, stderr) = run(&["replay", "--db", path.to_str().unwrap(), "--hosted", &"ab".repeat(32)]);
    assert!(!ok && stderr.contains("--from"), "{stderr}");
    std::fs::remove_dir_all(&dir).unwrap();
}
