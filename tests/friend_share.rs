//! Un fichier envoyé à un ami **d'une autre station**, de bout en bout et sans kubo : la station de l'expéditeur emballe
//! l'envoi (NIP-59), la station de l'ami l'ouvre, rapatrie le blob par le maillage P2P (empreinte vérifiée) et le range.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_core::ActivityBus;
use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::blob::{BlobStore, BlobWriter, FsBlobStore};
use astro_db::Db;
use astro_db::events::insert_event;
use astro_facecloud::{Outbox, OutboxError};
use astro_nostr_proto::{Event, UnsignedEvent, pubkey_of};
use astro_p2p::blob::{P2pBlobStore, ServeOpts, fetch_blob, serve_blobs};
use astro_p2p::{Multiaddr, P2pConfig, P2pNode};
use astroportd::share_inbox::ShareInbox;
use astroportd::sharing::Sharing;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ALICE: [u8; 32] = [0xA1; 32]; // station A
const CAROL: [u8; 32] = [0xC3; 32]; // station B

fn hex(s: &[u8; 32]) -> String {
    pubkey_of(s).unwrap()
}

#[derive(Default)]
struct Capture(Mutex<Vec<Event>>);

impl Outbox for Capture {
    async fn send(&self, _: &str, wrap: Event) -> Result<(), OutboxError> {
        self.0.lock().unwrap().push(wrap);
        Ok(())
    }
}

async fn station(tag: &str) -> (std::path::PathBuf, Arc<Db>, Arc<FsBlobStore>) {
    let dir = std::env::temp_dir().join(format!("astroportd-friendshare-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), Arc::new(Db::open(&dir.join("u.db"), 2).unwrap()), Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap()))
}

async fn node(seed: u8) -> (P2pNode, Multiaddr) {
    let n = P2pNode::spawn(P2pConfig::local([seed; 32])).await.unwrap();
    let tcp = n.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let full: Multiaddr = format!("{tcp}/p2p/{}", n.handle.peer_id()).parse().unwrap();
    (n, full)
}

async fn follow(db: &Db, who: &[u8; 32], target: &[u8; 32]) {
    let ev = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), hex(target)]], content: String::new() }.sign(who).unwrap();
    db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_sent_to_a_friend_on_another_station_arrives_through_the_mesh_without_kubo() {
    let (dir_a, db_a, blobs_a) = station("a").await;
    let (dir_b, db_b, blobs_b) = station("b").await;
    // Un blob « chiffré » (le contenu n'a pas d'importance pour le transport : seule l'empreinte compte).
    let content: Vec<u8> = (0..700_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    let mut w = blobs_a.begin().await.unwrap();
    w.write_all(&content).await.unwrap();
    let cid = w.commit().await.unwrap().cid;

    // Station A : Alice (hébergée) et Carol (ailleurs) se suivent ; le fichier est dans l'uCloud d'Alice.
    for (s, email, hosted) in [(&ALICE, "alice@example.org", 1), (&CAROL, "carol@example.org", 0)] {
        db_a.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, ?3)", vec![hex(s).into(), email.to_owned().into(), i64::from(hosted).into()]).await.unwrap();
    }
    db_a.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ALICE).into(), hex(&CAROL).into()]).await.unwrap();
    follow(&db_a, &CAROL, &ALICE).await;
    db_a.writer()
        .execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, sha_plain, data) VALUES (?1, '/Videos/fete.mp4', '/Videos', 'file', ?2, 'video/mp4', 700000, 1, ?3, '{}')", vec![hex(&ALICE).into(), cid.clone().into(), "ee".repeat(32).into()])
        .await
        .unwrap();
    db_a.writer().execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", vec![hex(&ALICE).into(), cid.clone().into(), "ab".repeat(32).into()]).await.unwrap();

    // Le maillage : A sert ses blobs ; B se connecte à A.
    let (a, a_addr) = node(31).await;
    let (b, _) = node(32).await;
    let _serve = serve_blobs(&a.handle, blobs_a.clone(), ServeOpts::default()).unwrap();
    b.handle.dial(a_addr).await.unwrap();

    // 1. Alice envoie à Carol (l'enveloppe est capturée à la place du relai).
    let outbox = Arc::new(Capture::default());
    let keys_a: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
    let sharing = Sharing::new(db_a.clone(), keys_a, outbox.clone(), None, Some(a.handle.peer_id().to_string()));
    let report = sharing.send(&hex(&ALICE), &["/Videos".to_owned()], &[hex(&CAROL)]).await.unwrap();
    assert_eq!(report["results"][0]["sent"], 1, "{report}");
    let wrap = outbox.0.lock().unwrap()[0].clone();

    // 2. La station B reçoit l'enveloppe (par son relai) : Carol est hébergée là, suit Alice, qui la suit.
    db_b.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'carol@example.org', 1)", vec![hex(&CAROL).into()]).await.unwrap();
    db_b.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&CAROL).into(), hex(&ALICE).into()]).await.unwrap();
    follow(&db_b, &ALICE, &CAROL).await;
    // Le profil d'Alice, répliqué par le crawl N¹/N², donne son nom au dossier ; sans lui, ce serait le début de sa clé.
    let profile = UnsignedEvent { created_at: 101, kind: 0, tags: vec![], content: r#"{"name":"Alice"}"#.into() }.sign(&ALICE).unwrap();
    db_b.writer().transaction(move |tx| insert_event(tx, &profile).map(|_| ())).await.unwrap();
    let w2 = wrap.clone();
    db_b.writer().transaction(move |tx| insert_event(tx, &w2).map(|_| ())).await.unwrap();
    assert!(blobs_b.open(&cid).await.is_err(), "le blob n'est pas encore sur la station B");

    let p2p_blobs = Arc::new(P2pBlobStore::new(blobs_b.clone(), b.handle.clone(), vec![]));
    let keys_b: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(CAROL));
    let told = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = told.clone();
    let notify: astroportd::upload_worker::Notify = Arc::new(move |_, text| {
        let log = log.clone();
        Box::pin(async move { log.lock().unwrap().push(text) })
    });
    let (handle, local) = (b.handle.clone(), blobs_b.clone());
    let inbox = ShareInbox::new(db_b.clone(), p2p_blobs, keys_b, None, ActivityBus::new(), Some(notify)).with_prefetch(Arc::new(move |cid: String, peer: String| {
        let (handle, local) = (handle.clone(), local.clone());
        Box::pin(async move { fetch_blob(&handle, &[peer.parse().unwrap()], &cid, local.as_ref(), 1 << 30, Duration::from_secs(30)).await.map(|_| ()).map_err(|e| e.to_string()) })
    }));
    let stats = inbox.poll_once(i64::try_from(wrap.created_at).unwrap() + 60).await;
    assert_eq!((stats.received, stats.deferred, stats.failed), (1, 0, 0), "{stats:?}");

    // 3. Carol a le fichier : même CID, même clé, octets identiques ; Alice est nommée dans le message de BRO.
    let (path, key): (String, String) = db_b
        .readers()
        .read(|c| Ok(c.query_row("SELECT e.path, k.key_hex FROM ucloud_entries e JOIN ucloud_keyring k ON k.cid = e.cid AND k.owner_hex = e.owner_hex WHERE e.owner_hex = ?1", [hex(&CAROL)], |r| Ok((r.get(0)?, r.get(1)?)))?))
        .await
        .unwrap();
    assert_eq!((path.as_str(), key.as_str()), ("/Partages/De_Alice/fete.mp4", "ab".repeat(32).as_str()));
    let (mut r, len) = blobs_b.open(&cid).await.unwrap();
    let mut got = Vec::new();
    r.read_to_end(&mut got).await.unwrap();
    assert!(len == 700_000 && got == content, "le blob est identique octet pour octet");
    assert!(told.lock().unwrap()[0].contains("fete.mp4"), "{:?}", told.lock().unwrap());
    let _ = (std::fs::remove_dir_all(dir_a), std::fs::remove_dir_all(dir_b));
}
