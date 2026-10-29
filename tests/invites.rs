//! Invitations par lien, de bout en bout sur une vraie base : création, aperçu public, réclamation (compte, double suivi,
//! photos rangées, fiche reliée), usage unique, retrait, expiration.

#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_crypto::FileKeySource;
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher, ingest};
use astro_db::{Db, SqliteReplayStore};
use astro_nostr_proto::Event;
use astroportd::identity_links::IdentityLinks;
use astroportd::invites::{CreateRequest, InviteError, Invites};
use astroportd::register::{Limits, PublishFn, Registrar};
use rusqlite::types::Value as V;
use serde_json::Value;

const ALICE: [u8; 32] = [0xA1; 32];

fn hex(s: &[u8; 32]) -> String {
    astro_nostr_proto::pubkey_of(s).unwrap()
}

struct Bench {
    db: Arc<Db>,
    invites: Invites<FsBlobStore>,
    registrar: Arc<Registrar>,
    links: Arc<IdentityLinks<FsBlobStore>>,
    dav: Arc<DavState<FsBlobStore>>,
    published: Arc<Mutex<Vec<Event>>>,
    dir: std::path::PathBuf,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn png() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(64, 48, |x, y| image::Rgba([(x * 4) as u8, (y * 5) as u8, 90, 255]));
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

async fn bench(tag: &str) -> Bench {
    let dir = std::env::temp_dir().join(format!("astroportd-invites-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("nostr");
    std::fs::create_dir_all(root.join("alice@example.org")).unwrap();
    let nsec = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), &ALICE).unwrap();
    std::fs::write(root.join("alice@example.org/.secret.nostr"), format!("NSEC={nsec}; NPUB=x; HEX={}\n", hex(&ALICE))).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'alice@example.org', 1)", vec![V::Text(hex(&ALICE))]).await.unwrap();
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let keys: Arc<dyn astro_crypto::KeySource> = Arc::new(FileKeySource::new(&root));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys.clone(), Duration::from_secs(3600));
    let dav = Arc::new(DavState { db: db.clone(), blobs: blobs.clone(), replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher.clone()), config: DavConfig::new(b"secret".to_vec()), on_upload: None });
    let links = Arc::new(IdentityLinks::new(db.clone(), dav.clone(), None, None));
    let registrar = Arc::new(Registrar::new(db.clone(), root, "0".repeat(64), Arc::new(|hex| format!("jeton-{}", &hex[..8])), Limits::default()));
    let published: Arc<Mutex<Vec<Event>>> = Arc::default();
    let sink = published.clone();
    let publish: PublishFn = Arc::new(move |e| {
        let sink = sink.clone();
        Box::pin(async move {
            sink.lock().unwrap().push(e);
            Ok(())
        })
    });
    let invites = Invites::new(db.clone(), blobs, keys, registrar.clone(), publish).with_links(links.clone()).with_publisher(publisher);
    Bench { db, invites, registrar, links, dav, published, dir }
}

/// Deux photos de `Léa` dans l'uCloud d'Alice, avec leurs visages ; une fiche de contact « Léa ».
async fn alice_has_lea(b: &Bench) -> String {
    let alice = hex(&ALICE);
    for (i, name) in ["a.png", "b.png"].iter().enumerate() {
        ingest::ingest(&b.dav, &alice, "/Photos", name, Some("image/png"), Cursor::new(png())).await.unwrap();
        b.db.writer()
            .execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, ucloud_path) VALUES (?1, 'Léa', ?2, ?3, ?4)", vec![V::Text(alice.clone()), V::Text(format!("h{i}")), V::Integer(i as i64 + 1), V::Text(format!("/Photos/{name}"))])
            .await
            .unwrap();
    }
    let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Léa\r\nEMAIL:lea@example.org\r\nEND:VCARD\r\n";
    let done = ingest::ingest(&b.dav, &alice, "/Contacts", "lea.vcf", Some("text/vcard"), Cursor::new(card.as_bytes().to_vec())).await.unwrap();
    b.links.sync_owner(&alice).await.unwrap();
    done.path
}

fn req(contact: &str) -> CreateRequest {
    CreateRequest { person: Some("Léa".into()), label: Some("Léa".into()), email: Some("lea@example.org".into()), contact_path: Some(contact.into()), ..Default::default() }
}

async fn count(b: &Bench, sql: &str, params: Vec<V>) -> i64 {
    let sql = sql.to_owned();
    b.db.readers().read(move |c| Ok(c.query_row(&sql, rusqlite::params_from_iter(params), |r| r.get::<_, i64>(0))?)).await.unwrap()
}

#[tokio::test]
async fn a_link_creates_the_account_befriends_both_ways_shares_the_photos_and_links_the_card() {
    let b = bench("happy").await;
    let (alice, contact) = (hex(&ALICE), String::new());
    let contact = alice_has_lea(&b).await + &contact;
    let created = b.invites.create(&alice, req(&contact)).await.unwrap();
    let token = created["token"].as_str().unwrap().to_owned();
    assert_eq!((token.len(), created["count"].as_u64()), (32, Some(2)), "toutes les photos de Léa, pas une de plus");

    // Ce que voit le porteur du lien : qui invite et combien, jamais un chemin ni une clé ; première ouverture notée.
    let p = b.invites.preview(&token, false).await.unwrap();
    assert_eq!((p["from"].as_str(), p["count"].as_u64(), p["email_hint"].as_str()), (Some("alice"), Some(2), Some("l**@example.org")));
    assert!(!p.to_string().contains("/Photos") && !p.to_string().contains(&alice));
    assert_eq!(b.invites.list(&alice).await.unwrap()["invites"][0]["state"], "opened");

    let claimed = b.invites.claim(&token, None, "Lea@Example.org", "fr").await.unwrap();
    let lea = claimed.registered.account.hex.clone();
    assert_eq!((claimed.shared, claimed.from.as_str(), claimed.warnings.is_empty()), (2, "alice", true), "{:?}", claimed.warnings);

    // Double suivi : chacun suit l'autre, dans la base (projeté) et sur les relais (publié).
    assert_eq!(count(&b, "SELECT COUNT(*) FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", vec![V::Text(alice.clone()), V::Text(lea.clone())]).await, 1);
    assert_eq!(count(&b, "SELECT COUNT(*) FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", vec![V::Text(lea.clone()), V::Text(alice.clone())]).await, 1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let kind3: Vec<_> = b.published.lock().unwrap().iter().filter(|e| e.kind == 3).map(|e| e.pubkey.clone()).collect();
    assert!(kind3.contains(&alice) && kind3.contains(&lea), "{kind3:?}");
    // Les photos sont rangées chez Léa : même blob, même clé.
    assert_eq!(count(&b, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Partages/De_alice/%' AND type = 'file'", vec![V::Text(lea.clone())]).await, 2);
    // La fiche d'Alice porte maintenant la clé de Léa : elle est « reliée » et Proches réciproques.
    let v = b.links.list(&alice).await.unwrap();
    assert_eq!((v["contacts"][0]["pubkey"].as_str(), v["contacts"][0]["friends"].as_bool()), (Some(lea.as_str()), Some(true)), "{v}");
    // État et prévenance de l'expéditeur.
    assert_eq!(b.invites.list(&alice).await.unwrap()["invites"][0]["state"], "claimed");
}

#[tokio::test]
async fn a_link_works_once_and_a_failed_claim_leaves_it_usable() {
    let b = bench("once").await;
    let alice = hex(&ALICE);
    let contact = alice_has_lea(&b).await;
    let token = b.invites.create(&alice, req(&contact)).await.unwrap()["token"].as_str().unwrap().to_owned();

    // Adresse invalide, puis adresse qui a déjà un MULTIPASS ici : refusées, le lien reste valable.
    assert!(matches!(b.invites.claim(&token, None, "pas-un-email", "fr").await, Err(InviteError::Invalid(_))));
    assert!(matches!(b.invites.claim(&token, None, "alice@example.org", "fr").await, Err(InviteError::EmailTaken)));
    assert_eq!(b.invites.list(&alice).await.unwrap()["invites"][0]["state"], "pending");
    assert_eq!(count(&b, "SELECT COUNT(*) FROM identities WHERE hosted = 1", vec![]).await, 1, "aucun compte créé par un essai refusé");

    b.invites.claim(&token, None, "lea@example.org", "fr").await.unwrap();
    // Réutilisation : refusée, aussi bien pour l'aperçu que pour une nouvelle réclamation.
    assert!(matches!(b.invites.claim(&token, None, "autre@example.org", "fr").await, Err(InviteError::Gone(_))));
    assert!(matches!(b.invites.preview(&token, false).await, Err(InviteError::Gone(_))));
    assert_eq!(count(&b, "SELECT COUNT(*) FROM identities WHERE hosted = 1", vec![]).await, 2);
}

#[tokio::test]
async fn revoked_expired_unknown_and_foreign_requests_are_refused() {
    let b = bench("refused").await;
    let alice = hex(&ALICE);
    let contact = alice_has_lea(&b).await;
    let token = b.invites.create(&alice, req(&contact)).await.unwrap()["token"].as_str().unwrap().to_owned();

    // Un autre compte ne retire pas le lien d'Alice ; Alice, si.
    assert!(matches!(b.invites.revoke(&"cd".repeat(32), &token).await, Err(InviteError::NotFound(_))));
    b.invites.revoke(&alice, &token).await.unwrap();
    assert!(matches!(b.invites.preview(&token, false).await, Err(InviteError::Gone(_))));
    assert!(matches!(b.invites.claim(&token, None, "lea@example.org", "fr").await, Err(InviteError::Gone(_))));

    // Expirée.
    let t2 = b.invites.create(&alice, req(&contact)).await.unwrap()["token"].as_str().unwrap().to_owned();
    b.db.writer().execute("UPDATE invites SET expires_at = 1 WHERE token = ?1", vec![V::Text(t2.clone())]).await.unwrap();
    assert!(matches!(b.invites.preview(&t2, false).await, Err(InviteError::Gone(_))));
    assert!(matches!(b.invites.claim(&t2, None, "lea@example.org", "fr").await, Err(InviteError::Gone(_))));

    // Jeton inventé ou mal formé : introuvable, sans rien révéler.
    for bad in ["0".repeat(32), "x".into(), String::new(), "../etc/passwd".into()] {
        assert!(matches!(b.invites.preview(&bad, false).await, Err(InviteError::NotFound(_))), "{bad}");
    }
    // Rien à envoyer, chemins d'un autre compte, validité absurde.
    assert!(matches!(b.invites.create(&alice, CreateRequest::default()).await, Err(InviteError::Invalid(_))));
    assert!(matches!(b.invites.create(&"cd".repeat(32), CreateRequest { paths: vec!["/Photos".into()], ..Default::default() }).await, Err(InviteError::Invalid(_))));
    assert!(matches!(b.invites.create(&alice, CreateRequest { paths: vec!["/Photos".into()], days: Some(0), ..Default::default() }).await, Err(InviteError::Invalid(_))));
    assert!(matches!(b.invites.create(&alice, CreateRequest { paths: vec!["/Photos".into()], contact_path: Some("/Photos/a.png".into()), ..Default::default() }).await, Err(InviteError::Invalid(_))));
}

#[tokio::test]
async fn the_public_thumbnail_is_a_few_pixels_wide() {
    let b = bench("thumb").await;
    let alice = hex(&ALICE);
    alice_has_lea(&b).await;
    let token = b.invites.create(&alice, CreateRequest { paths: vec!["/Photos".into()], ..Default::default() }).await.unwrap()["token"].as_str().unwrap().to_owned();
    // La miniature de l'uCloud est faite en tâche de fond : on l'attend.
    let mut png = None;
    for _ in 0..100 {
        if let Ok(p) = b.invites.thumb(&token, 0).await {
            png = Some(p);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let img = image::load_from_memory(&png.unwrap()).unwrap();
    assert!(img.width().max(img.height()) <= 16, "{}x{}", img.width(), img.height());
    let p: Value = b.invites.preview(&token, false).await.unwrap();
    assert_eq!(p["thumbs"].as_array().unwrap().len(), 2);
    assert!(matches!(b.invites.thumb(&token, 7).await, Err(InviteError::NotFound(_))));
}

#[tokio::test]
async fn an_existing_account_accepts_without_a_form_and_gets_the_photos_added_since_the_link_was_made() {
    let b = bench("accept").await;
    let alice = hex(&ALICE);
    let contact = alice_has_lea(&b).await;
    let token = b.invites.create(&alice, req(&contact)).await.unwrap()["token"].as_str().unwrap().to_owned();
    assert_eq!(b.invites.preview(&token, false).await.unwrap()["count"], 2);

    // Une troisième photo de Léa arrive après la création du lien : l'aperçu et l'acceptation la comptent.
    ingest::ingest(&b.dav, &alice, "/Photos", "c.png", Some("image/png"), Cursor::new(png())).await.unwrap();
    b.db.writer().execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, ucloud_path) VALUES (?1, 'Léa', 'h9', 9, '/Photos/c.png')", vec![V::Text(alice.clone())]).await.unwrap();
    assert_eq!(b.invites.preview(&token, false).await.unwrap()["count"], 3);

    // Léa a déjà un compte ici. Alice ne peut pas accepter sa propre invitation.
    let lea = b.registrar.register(None, "lea@example.org", "fr").await.unwrap().account.hex;
    assert!(matches!(b.invites.accept(&token, &alice).await, Err(InviteError::Invalid(_))));
    // Elle accepte avec son compte : aucune création, double suivi, photos (3), fiche reliée, expéditeur prévenu.
    let accepted = b.invites.accept(&token, &lea).await.unwrap();
    assert_eq!((accepted.shared, accepted.from.as_str(), accepted.warnings.is_empty()), (3, "alice", true), "{:?}", accepted.warnings);
    assert_eq!(count(&b, "SELECT COUNT(*) FROM identities WHERE hosted = 1 AND role = 'multipass'", vec![]).await, 2, "pas de nouveau compte");
    assert_eq!(count(&b, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Partages/De_alice/%' AND type = 'file'", vec![V::Text(lea.clone())]).await, 3);
    for (a, c) in [(&alice, &lea), (&lea, &alice)] {
        assert_eq!(count(&b, "SELECT COUNT(*) FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", vec![V::Text(a.clone()), V::Text(c.clone())]).await, 1);
    }
    let v = b.links.list(&alice).await.unwrap();
    assert_eq!((v["contacts"][0]["pubkey"].as_str(), v["contacts"][0]["friends"].as_bool()), (Some(lea.as_str()), Some(true)), "{v}");
    assert_eq!(b.invites.list(&alice).await.unwrap()["invites"][0]["state"], "claimed");
    // Usage unique : une seconde acceptation, par n'importe qui, est refusée avec son code stable.
    let again = b.invites.accept(&token, &lea).await.err().unwrap();
    assert_eq!((again.code(), &again), ("invite_used", &InviteError::Gone("invite_used")));
    assert_eq!(InviteError::Gone("invite_expired").code(), "invite_expired");
}

#[tokio::test]
async fn photos_deleted_after_the_link_was_made_are_not_shared() {
    let b = bench("deleted").await;
    let alice = hex(&ALICE);
    let contact = alice_has_lea(&b).await;
    let token = b.invites.create(&alice, req(&contact)).await.unwrap()["token"].as_str().unwrap().to_owned();
    b.db.writer().execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND path = '/Photos/b.png'", vec![V::Text(alice.clone())]).await.unwrap();
    let lea = b.registrar.register(None, "lea@example.org", "fr").await.unwrap().account.hex;
    assert_eq!(b.invites.accept(&token, &lea).await.unwrap().shared, 1);
}
