//! Photo ↔ fiche de contact ↔ MULTIPASS : le rapprochement des identités, de bout en bout sur une vraie base.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher, ingest, vcard};
use astro_db::{Db, SqliteReplayStore};
use astro_crypto::StaticKeySource;
use astroportd::identity_links::{IdentityLinks, LinkError};
use rusqlite::types::Value as V;

const ALICE: [u8; 32] = [0xA1; 32];
const BEA: [u8; 32] = [0xB2; 32];
const LEA: [u8; 32] = [0xC3; 32];
const STRANGER: [u8; 32] = [0xD4; 32];

fn hex(s: &[u8; 32]) -> String {
    astro_nostr_proto::pubkey_of(s).unwrap()
}

fn npub(s: &[u8; 32]) -> String {
    let bytes: Vec<u8> = (0..64).step_by(2).map(|i| u8::from_str_radix(&hex(s)[i..i + 2], 16).unwrap()).collect();
    bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("npub").unwrap(), &bytes).unwrap()
}

struct Bench {
    db: Arc<Db>,
    links: Arc<IdentityLinks<FsBlobStore>>,
    dav: Arc<DavState<FsBlobStore>>,
    shared: Arc<Mutex<Vec<(String, String)>>>,
    dir: std::path::PathBuf,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn bench(tag: &str) -> Bench {
    let dir = std::env::temp_dir().join(format!("astroportd-links-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    for (secret, role, email, hosted) in [(&ALICE, "multipass", "alice@example.org", 1), (&BEA, "multipass", "bea@example.org", 1)] {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, ?2, ?3, ?4)", vec![V::Text(hex(secret)), V::Text(role.into()), V::Text(email.into()), V::Integer(hosted)]).await.unwrap();
    }
    // Un identifiant distant qui REVENDIQUE l'adresse d'Alice… et une autre : jamais une preuve.
    db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'usurpateur@example.org', 0)", vec![V::Text(hex(&STRANGER))]).await.unwrap();
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let keys: Arc<dyn astro_crypto::KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BEA));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys, Duration::from_secs(3600));
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher), config: DavConfig::new(b"secret".to_vec()), on_upload: None });
    let shared: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let sink = shared.clone();
    let share: astroportd::identity_links::ShareFn = Arc::new(move |owner, friend| {
        let sink = sink.clone();
        Box::pin(async move { sink.lock().unwrap().push((owner, friend)) })
    });
    let links = Arc::new(IdentityLinks::new(db.clone(), dav.clone(), Some(share), None));
    Bench { db, links, dav, shared, dir }
}

async fn face(b: &Bench, n: i64, name: &str) {
    b.db.writer()
        .execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, ucloud_path) VALUES (?1, ?2, ?3, 1, ?4)", vec![V::Text(hex(&ALICE)), V::Text(name.into()), V::Text(format!("hash{n:04}")), V::Text(format!("/Photos/{n}.jpg"))])
        .await
        .unwrap();
}

async fn card(b: &Bench, file: &str, text: &str) {
    ingest::ingest(&b.dav, &hex(&ALICE), "/Contacts", file, Some("text/vcard"), std::io::Cursor::new(text.as_bytes().to_vec())).await.unwrap();
}

fn vc(name: &str, extra: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{name}\r\n{extra}END:VCARD\r\n")
}

async fn key_of(b: &Bench, name: &str) -> Option<String> {
    let n = name.to_owned();
    b.db.readers().read(move |c| Ok(c.query_row("SELECT pubkey FROM faces WHERE owner_hex = ?1 AND name = ?2 LIMIT 1", rusqlite::params![hex(&ALICE), n], |r| r.get::<_, Option<String>>(0))?)).await.unwrap()
}

#[tokio::test]
async fn a_card_with_a_key_or_a_hosted_email_gives_its_key_to_the_faces_of_the_same_name() {
    let b = bench("link").await;
    let alice = hex(&ALICE);
    for (i, name) in ["Léa Martin", "Léa Martin", "Bea", "Inconnu_deadbeef", "Paul", "Zoe"].iter().enumerate() {
        face(&b, i as i64, name).await;
    }
    // Léa : clé dans la fiche (npub) ; nom écrit sans accent ni casse dans la fiche.
    card(&b, "lea.vcf", &vc("LEA  martin", &format!("X-NOSTR-NPUB:{}\r\n", npub(&LEA)))).await;
    // Bea : aucune clé, mais l'email d'un MULTIPASS hébergé ici.
    card(&b, "bea.vcf", &vc("Bea", "EMAIL:Bea@Example.org\r\n")).await;
    // Paul : deux fiches de même nom, deux clés différentes → ambigu, jamais rapproché.
    card(&b, "paul1.vcf", &vc("Paul", &format!("X-NOSTR-PUBKEY:{}\r\n", hex(&LEA)))).await;
    card(&b, "paul2.vcf", &vc("Paul", &format!("X-NOSTR-PUBKEY:{}\r\n", hex(&BEA)))).await;
    // Zoe : l'email n'appartient à aucun compte hébergé (celui du DID distant ne compte pas).
    card(&b, "zoe.vcf", &vc("Zoe", "EMAIL:usurpateur@example.org\r\n")).await;

    let r = b.links.sync_owner(&alice).await.unwrap();
    assert_eq!((r.indexed, r.removed), (5, 0));
    let linked: std::collections::BTreeMap<_, _> = r.linked.iter().cloned().collect();
    assert_eq!(linked.get("Léa Martin"), Some(&hex(&LEA)));
    assert_eq!(linked.get("Bea"), Some(&hex(&BEA)));
    assert_eq!(r.linked.len(), 2, "{:?}", r.linked);
    assert_eq!(key_of(&b, "Léa Martin").await.as_deref(), Some(hex(&LEA).as_str()));
    assert_eq!(key_of(&b, "Paul").await, None, "nom ambigu : pas de rapprochement");
    assert_eq!(key_of(&b, "Zoe").await, None, "un email non hébergé ici n'est pas une preuve");
    assert_eq!(key_of(&b, "Inconnu_deadbeef").await, None);
    // Le partage des photos déjà analysées part pour chaque personne reliée, et seulement elles.
    let mut shared = b.shared.lock().unwrap().clone();
    shared.sort();
    let mut want = vec![(alice.clone(), hex(&LEA)), (alice.clone(), hex(&BEA))];
    want.sort();
    assert_eq!(shared, want);

    // Idempotent : rien à relire, rien à relier, aucun nouveau partage.
    let again = b.links.sync_owner(&alice).await.unwrap();
    assert_eq!((again.indexed, again.removed, again.linked.len()), (0, 0, 0));
    assert_eq!(b.shared.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn the_directory_follows_changes_and_deletions_and_reports_bad_cards() {
    let b = bench("follow").await;
    let alice = hex(&ALICE);
    card(&b, "a.vcf", &vc("Alice Amie", "")).await;
    card(&b, "casse.vcf", "ceci n'est pas une carte").await;
    card(&b, "cle.vcf", &vc("Clé fausse", "X-NOSTR-PUBKEY:pas-une-cle\r\n")).await;
    b.links.sync_owner(&alice).await.unwrap();
    let v = b.links.list(&alice).await.unwrap();
    let by = |name: &str| v["contacts"].as_array().unwrap().iter().find(|c| c["path"].as_str().unwrap().ends_with(name)).unwrap().clone();
    assert!(by("casse.vcf")["warning"].as_str().unwrap().contains("pas une vCard"), "{v}");
    assert!(by("cle.vcf")["warning"].as_str().unwrap().contains("illisible"), "une clé illisible est signalée, pas ignorée");
    assert!(by("a.vcf")["pubkey"].is_null());
    // Modification : la fiche reçoit une clé, l'index suit.
    card_replace(&b, "a.vcf", &vc("Alice Amie", &format!("X-NOSTR-PUBKEY:{}\r\n", hex(&LEA)))).await;
    let r = b.links.sync_owner(&alice).await.unwrap();
    assert_eq!(r.indexed, 1);
    assert_eq!(b.links.list(&alice).await.unwrap()["contacts"].as_array().unwrap().iter().find(|c| c["name"] == "Alice Amie").unwrap()["pubkey"], hex(&LEA));
    // Suppression de la fiche : l'index l'oublie.
    let (db, a) = (b.db.clone(), alice.clone());
    db.writer().transaction(move |tx| astro_dav::vfs::delete_tree_tx(tx, &a, "/Contacts/casse.vcf").map(|_| ()).map_err(|_| astro_db::DbError::Aborted)).await.unwrap();
    let r = b.links.sync_owner(&alice).await.unwrap();
    assert_eq!(r.removed, 1);
    assert_eq!(b.links.list(&alice).await.unwrap()["contacts"].as_array().unwrap().len(), 2);
}

async fn card_replace(b: &Bench, file: &str, text: &str) {
    ingest::replace(&b.dav, &hex(&ALICE), "/Contacts", file, Some("text/vcard"), std::io::Cursor::new(text.as_bytes().to_vec())).await.unwrap();
}

#[tokio::test]
async fn a_card_is_created_from_a_person_and_linked_by_hand() {
    let b = bench("create").await;
    let alice = hex(&ALICE);
    // Zoé est déjà reconnue avec une clé (ex. photo partagée) : sa fiche naît avec.
    b.db.writer().execute("INSERT INTO faces(owner_hex, name, pubkey, embed_hash, ts, ucloud_path) VALUES (?1, 'Zoé', ?2, 'h1', 1, '/Photos/z.jpg')", vec![V::Text(alice.clone()), V::Text(hex(&LEA))]).await.unwrap();
    face(&b, 1, "Marc").await;
    let created = b.links.create_from_person(&alice, "Zoé", Some("zoe@example.org")).await.unwrap();
    assert_eq!(created["path"], "/Contacts/Zoé.vcf");
    let text = String::from_utf8(astro_dav::carddav::read_file(&b.dav, &alice, "/Contacts/Zoé.vcf").await.unwrap()).unwrap();
    let c = vcard::parse(text.as_bytes()).unwrap();
    assert_eq!((c.name.as_str(), c.nostr.as_deref(), c.emails.as_slice()), ("Zoé", Some(hex(&LEA).as_str()), ["zoe@example.org".to_owned()].as_slice()));
    assert!(matches!(b.links.create_from_person(&alice, "zoé", None).await, Err(LinkError::Conflict(_))), "une seule fiche par nom");
    assert!(matches!(b.links.create_from_person(&alice, "  ", None).await, Err(LinkError::Invalid(_))));
    assert!(matches!(b.links.create_from_person(&alice, "X", Some("pas un email")).await, Err(LinkError::Invalid(_))));
    // Marc : fiche sans clé, puis reliée à la main par l'email d'un MULTIPASS hébergé.
    b.links.create_from_person(&alice, "Marc", None).await.unwrap();
    assert_eq!(key_of(&b, "Marc").await, None);
    let before = b.shared.lock().unwrap().len();
    b.links.link(&alice, "/Contacts/Marc.vcf", Some("bea@example.org")).await.unwrap();
    assert_eq!(key_of(&b, "Marc").await.as_deref(), Some(hex(&BEA).as_str()), "visage relié à la clé de Bea");
    assert_eq!(b.shared.lock().unwrap().len(), before + 1, "partage déclenché");
    let text = String::from_utf8(astro_dav::carddav::read_file(&b.dav, &alice, "/Contacts/Marc.vcf").await.unwrap()).unwrap();
    assert_eq!(vcard::parse(text.as_bytes()).unwrap().nostr.as_deref(), Some(hex(&BEA).as_str()), "la fiche garde la clé : elle suit le carnet sur le téléphone");
    // Refus explicites.
    assert!(matches!(b.links.link(&alice, "/Contacts/Marc.vcf", Some("inconnu@example.org")).await, Err(LinkError::NotFound(_))));
    assert!(matches!(b.links.link(&alice, "/Contacts/Marc.vcf", Some("alice@example.org")).await, Err(LinkError::Invalid(_))), "pas sa propre clé");
    assert!(matches!(b.links.link(&alice, "/Contacts/Marc.vcf", Some("pas-une-cle")).await, Err(LinkError::Invalid(_))));
    assert!(matches!(b.links.link(&alice, "/Photos/x.jpg", None).await, Err(LinkError::Invalid(_))));
    assert!(matches!(b.links.link(&alice, "/Contacts/absente.vcf", None).await, Err(LinkError::NotFound(_))));
    // Délier retire la clé de la fiche.
    b.links.link(&alice, "/Contacts/Marc.vcf", None).await.unwrap();
    let text = String::from_utf8(astro_dav::carddav::read_file(&b.dav, &alice, "/Contacts/Marc.vcf").await.unwrap()).unwrap();
    assert!(vcard::parse(text.as_bytes()).unwrap().nostr.is_none());
    // La vue liste les fiches et les personnes sans fiche.
    face(&b, 2, "Nina").await;
    let v = b.links.list(&alice).await.unwrap();
    assert_eq!(v["people_without_contact"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Nina"]);
}
