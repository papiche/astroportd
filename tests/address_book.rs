//! Carnet d'adresses : recherche, fiche détaillée, groupes, import, doublons et fusion, comparaison avec le profil Nostr de l'ami, « Ma fiche ».

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use astro_crypto::StaticKeySource;
use astro_dav::vcard::{self, Fields, Typed};
use astro_dav::{DavConfig, DavState, FsBlobStore, ManifestPublisher, carddav, ingest};
use astro_db::{Db, SqliteReplayStore};
use astroportd::address_book::{AddressBook, BookError};
use astroportd::identity_links::IdentityLinks;
use rusqlite::types::Value as V;
use serde_json::{Value, json};

const ALICE: [u8; 32] = [0xA1; 32];
const BEA: [u8; 32] = [0xB2; 32];

fn hex(s: &[u8; 32]) -> String {
    astro_nostr_proto::pubkey_of(s).unwrap()
}

struct Bench {
    db: Arc<Db>,
    book: AddressBook<FsBlobStore>,
    dav: Arc<DavState<FsBlobStore>>,
    dir: std::path::PathBuf,
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn bench(tag: &str) -> Bench {
    let dir = std::env::temp_dir().join(format!("astroportd-book-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    for (secret, email) in [(&ALICE, "alice@example.org"), (&BEA, "bea@example.org")] {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![V::Text(hex(secret)), V::Text(email.into())]).await.unwrap();
    }
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let keys: Arc<dyn astro_crypto::KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BEA));
    let publisher = ManifestPublisher::spawn(db.clone(), blobs.clone(), keys, Duration::from_secs(3600));
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: Some(publisher), config: DavConfig::new(b"secret".to_vec()), on_upload: None });
    let links = Arc::new(IdentityLinks::new(db.clone(), dav.clone(), None, None));
    Bench { book: AddressBook::new(db.clone(), dav.clone(), links, None), db, dav, dir }
}

fn fields(name: &str) -> Fields {
    Fields { name: name.into(), ..Fields::default() }
}

fn with_phone(mut f: Fields, p: &str) -> Fields {
    f.phones.push(Typed { kind: "mobile".into(), value: p.into() });
    f
}

fn names(v: &Value) -> Vec<String> {
    v["contacts"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_owned()).collect()
}

async fn card_text(b: &Bench, path: &str) -> String {
    // Les lignes longues sont repliées (RFC 6350) : on les recolle pour chercher une propriété.
    String::from_utf8(carddav::read_file(&b.dav, &hex(&ALICE), path).await.unwrap()).unwrap().replace("\r\n ", "")
}

#[tokio::test]
async fn cards_are_created_found_by_name_phone_or_group_and_edited_without_losing_what_the_book_does_not_manage() {
    let b = bench("search").await;
    let me = hex(&ALICE);
    let mut lea = with_phone(fields("Léa Martin"), "+33 6 12 34 56 78");
    lea.org = "Atelier du Sud".into();
    lea.groups = vec!["Famille".into(), "Voisins".into()];
    lea.emails.push(Typed { kind: "home".into(), value: "lea@example.org".into() });
    let path = b.book.save(&me, None, &lea).await.unwrap()["path"].as_str().unwrap().to_owned();
    let mut paul = fields("Paul Durand");
    paul.groups = vec!["Équipe".into()];
    b.book.save(&me, None, &paul).await.unwrap();
    b.book.save(&me, None, &fields("Zoé Petit")).await.unwrap();

    let all = b.book.list(&me, "", None).await.unwrap();
    assert_eq!(names(&all), ["Léa Martin", "Paul Durand", "Zoé Petit"], "ordre alphabétique");
    assert_eq!(all["letters"], json!(["L", "P", "Z"]));
    assert_eq!(all["groups"], json!([{ "name": "Famille", "count": 1 }, { "name": "Voisins", "count": 1 }, { "name": "Équipe", "count": 1 }]));
    // Recherche sans accents ni casse, par morceaux de nom, société, téléphone (chiffres) et par groupe.
    for (q, want) in [("lea", vec!["Léa Martin"]), ("MARTIN léa", vec!["Léa Martin"]), ("atelier", vec!["Léa Martin"]), ("0612", vec![]), ("06 12 34", vec![]), ("12 34 56", vec!["Léa Martin"]), ("zoe", vec!["Zoé Petit"]), ("example.org", vec!["Léa Martin"]), ("inconnu", vec![])] {
        assert_eq!(names(&b.book.list(&me, q, None).await.unwrap()), want, "recherche « {q} »");
    }
    assert_eq!(names(&b.book.list(&me, "", Some("Équipe")).await.unwrap()), ["Paul Durand"]);

    // Une fiche venue d'un téléphone garde sa photo et sa clé Nostr quand on la modifie ici.
    let raw = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:tel-1\r\nFN:Bea\r\nPHOTO;ENCODING=b;TYPE=JPEG:AAAA\r\nX-NOSTR-PUBKEY:{}\r\nEND:VCARD\r\n", hex(&BEA));
    ingest::ingest(&b.dav, &me, "/Contacts", "Bea.vcf", Some("text/vcard"), std::io::Cursor::new(raw.into_bytes())).await.unwrap();
    let mut f = vcard::read_fields(&card_text(&b, "/Contacts/Bea.vcf").await);
    f.title = "Boulangère".into();
    f.phones.push(Typed { kind: "work".into(), value: "05 61 00 00 00".into() });
    b.book.save(&me, Some("/Contacts/Bea.vcf"), &f).await.unwrap();
    let text = card_text(&b, "/Contacts/Bea.vcf").await;
    assert!(text.contains("PHOTO;ENCODING=b;TYPE=JPEG:AAAA") && text.contains(&format!("X-NOSTR-PUBKEY:{}", hex(&BEA))) && text.contains("UID:tel-1"), "{text}");
    let d = b.book.get(&me, "/Contacts/Bea.vcf").await.unwrap();
    assert_eq!((d["fields"]["title"].as_str(), d["pubkey"].as_str()), (Some("Boulangère"), Some(hex(&BEA).as_str())), "le lien avec le MULTIPASS survit à la modification");
    // La photo incorporée à la fiche est montrée (lecture seule) ; une fiche sans photo répond `null`.
    assert_eq!(b.book.photo(&me, "/Contacts/Bea.vcf").await.unwrap()["uri"], "data:image/jpeg;base64,AAAA");
    assert!(b.book.photo(&me, &path).await.unwrap()["uri"].is_null());
    // Changer la photo : image réduite et ré-encodée en JPEG dans la fiche ; une donnée qui n'est pas une image est refusée.
    {
        use base64::Engine;
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::new(600, 400)).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let uri = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png));
        b.book.set_photo(&me, "/Contacts/Bea.vcf", Some(&uri)).await.unwrap();
        let shown = b.book.photo(&me, "/Contacts/Bea.vcf").await.unwrap()["uri"].as_str().unwrap().to_owned();
        assert!(shown.starts_with("data:image/jpeg;base64,") && shown != "data:image/jpeg;base64,AAAA", "{shown}");
        let text = card_text(&b, "/Contacts/Bea.vcf").await;
        assert_eq!(text.matches("PHOTO").count(), 1, "une seule photo dans la fiche");
        for bad in ["data:image/png;base64,AAAA", "data:text/html;base64,AAAA", "https://x/y.png", "data:image/png,AAAA"] {
            assert!(matches!(b.book.set_photo(&me, "/Contacts/Bea.vcf", Some(bad)).await, Err(BookError::Invalid(_))), "{bad}");
        }
        assert_eq!(b.book.photo_choices(&me, "/Contacts/Bea.vcf").await.unwrap()["faces"], json!([]));
        b.book.set_photo(&me, "/Contacts/Bea.vcf", None).await.unwrap();
        assert!(b.book.photo(&me, "/Contacts/Bea.vcf").await.unwrap()["uri"].is_null());
    }
    // Suppression en lot : chaque échec est dit sans arrêter les autres.
    let (x, y) = (b.book.save(&me, None, &fields("Lot Un")).await.unwrap()["path"].as_str().unwrap().to_owned(), b.book.save(&me, None, &fields("Lot Deux")).await.unwrap()["path"].as_str().unwrap().to_owned());
    let r = b.book.delete_many(&me, &[x, "/Contacts/absente.vcf".into(), y]).await.unwrap();
    assert_eq!((r["deleted"].as_i64(), r["failed"].as_array().map(Vec::len)), (Some(2), Some(1)));
    assert!(matches!(b.book.delete_many(&me, &[]).await, Err(BookError::Invalid(_))));
    // Chemins hostiles et fiche sans nom refusés.
    for bad in ["/Contacts/../x.vcf", "/etc/passwd", "/Contacts/a/b.vcf", "/Contacts/x.txt"] {
        assert!(matches!(b.book.get(&me, bad).await, Err(BookError::Invalid(_))), "{bad}");
    }
    assert!(matches!(b.book.save(&me, Some(&path), &fields("  ")).await, Err(BookError::Invalid(_))));
    // Suppression.
    b.book.delete(&me, &path).await.unwrap();
    assert!(!names(&b.book.list(&me, "", None).await.unwrap()).contains(&"Léa Martin".to_owned()));
}

#[tokio::test]
async fn groups_are_renamed_or_removed_on_every_card_and_import_skips_duplicates_and_says_why_a_card_is_refused() {
    let b = bench("groups").await;
    let me = hex(&ALICE);
    for (n, g) in [("Ana", "Famille"), ("Bob", "Famille"), ("Cléo", "Amis")] {
        let mut f = fields(n);
        f.groups = vec![g.into()];
        b.book.save(&me, None, &f).await.unwrap();
    }
    assert_eq!(b.book.set_group(&me, "Famille", Some("Proches")).await.unwrap()["changed"], 2);
    assert_eq!(names(&b.book.list(&me, "", Some("Proches")).await.unwrap()), ["Ana", "Bob"]);
    assert_eq!(b.book.set_group(&me, "Amis", None).await.unwrap()["changed"], 1);
    assert!(b.book.list(&me, "", None).await.unwrap()["groups"].as_array().unwrap().iter().all(|g| g["name"] != "Amis"));

    // Import : une fiche nouvelle, une déjà là (même nom), une invalide, une du même UID qu'une fiche importée juste avant.
    let vcf = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:n1\r\nFN:Nina Roy\r\nTEL:+33 7 00 00 00 01\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ana\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:3.0\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:3.0\r\nUID:n1\r\nFN:Nina R.\r\nEND:VCARD\r\n";
    let r = b.book.import(&me, vcf).await.unwrap();
    assert_eq!((r["imported"].as_i64(), r["duplicates"].as_i64(), r["rejected"].as_array().unwrap().len()), (Some(1), Some(2), 1), "{r}");
    assert!(r["rejected"][0].as_str().unwrap().contains("nom"), "{r}");
    assert_eq!(names(&b.book.list(&me, "nina", None).await.unwrap()), ["Nina Roy"]);
    assert_eq!(b.book.import(&me, &"x".repeat(5 << 20)).await.unwrap_err(), BookError::Invalid("fichier trop gros (4 Mo au plus)".into()));
}

#[tokio::test]
async fn duplicates_are_found_by_email_phone_or_name_and_merged_without_losing_anything() {
    let b = bench("merge").await;
    let me = hex(&ALICE);
    let mut a = with_phone(fields("Marie Curie"), "+33 6 11 22 33 44");
    a.emails.push(Typed { kind: "home".into(), value: "marie@example.org".into() });
    a.note = "Rencontrée à Paris".into();
    let pa = b.book.save(&me, None, &a).await.unwrap()["path"].as_str().unwrap().to_owned();
    let mut c = with_phone(fields("M. Curie"), "06 11 22 33 44"); // même téléphone, nom différent
    c.groups = vec!["Science".into()];
    c.org = "Institut".into();
    c.note = "Prix Nobel".into();
    let pc = b.book.save(&me, None, &c).await.unwrap()["path"].as_str().unwrap().to_owned();
    let mut d = fields("Marie Curie"); // même nom
    d.emails.push(Typed { kind: "work".into(), value: "mc@example.org".into() });
    let pd = b.book.save(&me, None, &d).await.unwrap()["path"].as_str().unwrap().to_owned();
    b.book.save(&me, None, &fields("Autre Personne")).await.unwrap();

    let dups = b.book.duplicates(&me).await.unwrap();
    let groups = dups["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{dups}");
    assert_eq!(groups[0]["cards"].as_array().unwrap().len(), 3);
    let reasons = groups[0]["reasons"].to_string();
    assert!(reasons.contains("même nom") && reasons.contains("même téléphone"), "{reasons}");

    let r = b.book.merge(&me, &pa, &[pc.clone(), pd.clone()]).await.unwrap();
    assert_eq!(r["removed"], 2);
    let merged = b.book.get(&me, &pa).await.unwrap()["fields"].clone();
    let f: Fields = serde_json::from_value(merged).unwrap();
    assert_eq!(f.phones.len(), 1, "le même numéro écrit autrement n'est gardé qu'une fois");
    assert_eq!(f.emails.iter().map(|e| e.value.as_str()).collect::<Vec<_>>(), ["marie@example.org", "mc@example.org"]);
    assert_eq!((f.org.as_str(), f.groups.as_slice()), ("Institut", ["Science".to_owned()].as_slice()));
    assert_eq!(f.note, "Rencontrée à Paris\nPrix Nobel");
    assert_eq!(names(&b.book.list(&me, "", None).await.unwrap()), ["Autre Personne", "Marie Curie"]);
    assert!(matches!(b.book.merge(&me, &pa, std::slice::from_ref(&pa)).await, Err(BookError::Invalid(_))));
    assert!(b.book.duplicates(&me).await.unwrap()["groups"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_friends_card_is_compared_with_his_nostr_profile_and_each_value_is_taken_on_its_own() {
    let b = bench("profile").await;
    let me = hex(&ALICE);
    // La fiche de Bea (email d'un MULTIPASS hébergé : lien automatique), un peu différente de son profil public.
    let mut f = fields("Bea");
    f.emails.push(Typed { kind: "home".into(), value: "bea@example.org".into() });
    f.about = "ancienne présentation".into();
    let path = b.book.save(&me, None, &f).await.unwrap()["path"].as_str().unwrap().to_owned();
    let profile = json!({ "name": "bea", "display_name": "Béa Lambert", "about": "Boulangère à Toulouse", "picture": "https://img.example/bea.jpg", "website": "https://bea.example", "nip05": "bea@bea.example" });
    b.db.writer().execute("INSERT INTO nostr_events(id, pubkey, kind, created_at, content, sig, tags) VALUES ('e1', ?1, 0, 1700000000, ?2, 'sig', '[]')", vec![V::Text(hex(&BEA)), V::Text(profile.to_string())]).await.unwrap();

    let d = b.book.get(&me, &path).await.unwrap();
    assert_eq!(d["pubkey"].as_str(), Some(hex(&BEA).as_str()));
    let diff: std::collections::BTreeMap<String, (String, String, bool)> = d["diff"].as_array().unwrap().iter().map(|r| (r["key"].as_str().unwrap().into(), (r["mine"].as_str().unwrap().into(), r["theirs"].as_str().unwrap().into(), r["can_take"].as_bool().unwrap()))).collect();
    assert_eq!(diff["name"], ("Bea".into(), "Béa Lambert".into(), true), "le nom affiché du profil l'emporte sur `name`");
    assert_eq!(diff["about"], ("ancienne présentation".into(), "Boulangère à Toulouse".into(), true));
    assert_eq!(diff["website"], (String::new(), "https://bea.example".into(), true));
    // On prend le nom et le site, PAS la présentation : chaque valeur se choisit.
    b.book.take(&me, &path, &["name".into(), "website".into(), "picture".into()]).await.unwrap();
    let f: Fields = serde_json::from_value(b.book.get(&me, &path).await.unwrap()["fields"].clone()).unwrap();
    assert_eq!((f.name.as_str(), f.given.as_str(), f.family.as_str(), f.urls.first().map(String::as_str), f.picture.as_str(), f.about.as_str()), ("Béa Lambert", "Béa", "Lambert", Some("https://bea.example"), "https://img.example/bea.jpg", "ancienne présentation"));
    assert!(matches!(b.book.take(&me, &path, &["phones".into()]).await, Err(BookError::Invalid(_))), "seuls les champs du profil se prennent");
    let again = b.book.get(&me, &path).await.unwrap();
    let still: Vec<&str> = again["diff"].as_array().unwrap().iter().filter(|r| r["can_take"] == true).map(|r| r["key"].as_str().unwrap()).collect();
    assert_eq!(still, ["about", "nip05"], "ce qui n'a pas été pris reste proposé");
    // Une fiche sans lien n'a rien à prendre.
    let lone = b.book.save(&me, None, &fields("Inconnu Total")).await.unwrap()["path"].as_str().unwrap().to_owned();
    assert!(matches!(b.book.take(&me, &lone, &["name".into()]).await, Err(BookError::Invalid(_))));
    assert!(b.book.get(&me, &lone).await.unwrap()["diff"].is_null());
}

#[tokio::test]
async fn my_own_card_is_created_from_my_profile_cannot_be_deleted_or_merged_and_stays_first() {
    let b = bench("me").await;
    let me = hex(&ALICE);
    let profile = json!({ "name": "alice", "about": "Capitaine", "picture": "https://img.example/a.jpg", "nip05": "alice@alice.example" });
    b.db.writer().execute("INSERT INTO nostr_events(id, pubkey, kind, created_at, content, sig, tags) VALUES ('p1', ?1, 0, 1700000000, ?2, 'sig', '[]')", vec![V::Text(me.clone()), V::Text(profile.to_string())]).await.unwrap();
    b.book.save(&me, None, &fields("Zoé")).await.unwrap();
    let v = b.book.me(&me).await.unwrap();
    assert_eq!(v["me"], true);
    assert_eq!(v["fields"]["name"], "alice");
    assert_eq!((v["fields"]["about"].as_str(), v["fields"]["nip05"].as_str()), (Some("Capitaine"), Some("alice@alice.example")), "le profil Nostr fait foi");
    assert_eq!(v["fields"]["emails"][0]["value"], "alice@example.org", "l'email du compte est dans la fiche");
    let path = v["path"].as_str().unwrap().to_owned();
    let text = card_text(&b, &path).await;
    assert!(text.contains("X-ASTRO-ME:1") && text.contains(&format!("X-NOSTR-PUBKEY:{me}")));
    assert_eq!(b.book.me(&me).await.unwrap()["path"], path, "une seule fiche « moi », pas une de plus à chaque visite");
    let list = b.book.list(&me, "", None).await.unwrap();
    assert_eq!(names(&list)[0], "alice", "ma fiche est toujours en tête");
    assert_eq!(list["contacts"][0]["me"], true);
    assert!(matches!(b.book.delete(&me, &path).await, Err(BookError::Invalid(m)) if m.contains("propre fiche")));
    let other = b.book.save(&me, None, &fields("Autre")).await.unwrap()["path"].as_str().unwrap().to_owned();
    assert!(matches!(b.book.merge(&me, &other, &[path]).await, Err(BookError::Invalid(_))));
    assert!(b.book.duplicates(&me).await.unwrap()["groups"].as_array().unwrap().is_empty(), "ma fiche n'est jamais proposée comme doublon");
    // Sans service de profil, l'enregistrement de « ma fiche » dit pourquoi il ne peut pas publier.
    assert!(matches!(b.book.save_me(&me, &fields("x")).await, Err(BookError::Internal(_))));
}

#[tokio::test]
async fn a_person_gets_a_card_by_explicit_choice_never_by_guessing_between_homonyms() {
    let b = bench("person").await;
    let me = hex(&ALICE);
    let first = b.book.person_card(&me, "Léa Martin", None, false).await.unwrap();
    assert_eq!(first["created"], true);
    let uid = first["uid"].as_str().unwrap().to_owned();
    let second = b.book.person_card(&me, "lea  MARTIN", None, false).await.unwrap();
    assert_eq!((second["created"].as_bool(), &second["uid"]), (Some(false), &first["uid"]), "une seule fiche de ce nom : c'est elle");
    assert_eq!(second["name"], "Léa Martin", "le nom de la fiche fait foi");
    // « Une autre Léa Martin » : demandée explicitement, elle a sa propre fiche ; ensuite, le nom seul est ambigu.
    let other = b.book.person_card(&me, "Léa Martin", None, true).await.unwrap();
    assert!(other["created"] == true && other["uid"] != first["uid"]);
    assert!(matches!(b.book.person_card(&me, "Léa Martin", None, false).await, Err(BookError::Conflict(m)) if m.contains("2 fiches")));
    // Le choix d'une fiche par son UID tranche ; un UID inconnu est dit.
    assert_eq!(b.book.person_card(&me, "peu importe", Some(&uid), false).await.unwrap()["path"], first["path"]);
    assert!(matches!(b.book.person_card(&me, "x", Some("uid-inconnu"), false).await, Err(BookError::NotFound(_))));
    assert_eq!(names(&b.book.list(&me, "", None).await.unwrap()), ["Léa Martin", "Léa Martin"]);
}

#[tokio::test]
async fn exact_duplicates_merge_in_batches_keep_the_card_with_faces_and_leave_near_duplicates_alone() {
    let b = bench("exact").await;
    let me = hex(&ALICE);
    let email = |mut f: Fields, e: &str| {
        f.emails.push(Typed { kind: "home".into(), value: e.into() });
        f
    };
    let path = |v: Value| v["path"].as_str().unwrap().to_owned();
    // Deux paires et un triplet exacts (le carnet importé deux ou trois fois) ; la casse de l'email ne compte pas.
    let lea1 = path(b.book.save(&me, None, &email(fields("Léa Martin"), "lea@example.org")).await.unwrap());
    let lea2 = path(b.book.save(&me, None, &email(fields("Léa Martin"), "LEA@example.org")).await.unwrap());
    for _ in 0..3 {
        b.book.save(&me, None, &with_phone(fields("Paul"), "06 12 34 56 78")).await.unwrap();
    }
    b.book.save(&me, None, &email(fields("Zoé"), "zoe@example.org")).await.unwrap();
    let mut zoe = email(fields("Zoé"), "zoe@example.org");
    zoe.note = "à garder".into();
    b.book.save(&me, None, &zoe).await.unwrap();
    // Jamais réunis : homonymes sans coordonnées, même nom mais autre email, même nom et email mais autre société.
    b.book.save(&me, None, &fields("Marie")).await.unwrap();
    b.book.save(&me, None, &fields("Marie")).await.unwrap();
    b.book.save(&me, None, &email(fields("Nina"), "nina@a.org")).await.unwrap();
    b.book.save(&me, None, &email(fields("Nina"), "nina@b.org")).await.unwrap();
    let mut org = email(fields("Hugo"), "hugo@example.org");
    org.org = "Boulangerie".into();
    b.book.save(&me, None, &org).await.unwrap();
    b.book.save(&me, None, &email(fields("Hugo"), "hugo@example.org")).await.unwrap();
    // La seconde Léa a des visages : c'est elle qu'on garde, et ses visages restent à elle.
    let uid = |p: String| {
        let db = b.db.clone();
        async move { db.readers().read(move |c| Ok(c.query_row("SELECT uid FROM identity_links WHERE path = ?1", [p], |r| r.get::<_, String>(0))?)).await.unwrap() }
    };
    let (uid1, uid2) = (uid(lea1.clone()).await, uid(lea2.clone()).await);
    b.db.writer().execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, contact_uid) VALUES (?1, 'Léa Martin', 'h1', 1, ?2), (?1, 'Léa Martin', 'h2', 1, ?3)", vec![V::Text(me.clone()), V::Text(uid2.clone()), V::Text(uid1.clone())]).await.unwrap();
    b.db.writer().execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, contact_uid) VALUES (?1, 'Léa Martin', 'h3', 1, ?2)", vec![V::Text(me.clone()), V::Text(uid2.clone())]).await.unwrap();

    let preview = b.book.merge_exact(&me, false, 50).await.unwrap();
    assert_eq!((preview["groups"].as_u64(), preview["cards"].as_u64()), (Some(3), Some(4)), "{preview}");
    assert_eq!(names(&b.book.list(&me, "", None).await.unwrap()).len(), 13, "l'aperçu ne touche à rien");

    // Par lots : un groupe, puis le reste.
    let first = b.book.merge_exact(&me, true, 1).await.unwrap();
    assert_eq!((first["merged"].as_u64(), first["remaining"].as_u64()), (Some(1), Some(2)), "{first}");
    let rest = b.book.merge_exact(&me, true, 50).await.unwrap();
    assert_eq!((rest["merged"].as_u64(), rest["remaining"].as_u64(), rest["failed"].as_array().map(Vec::len)), (Some(2), Some(0), Some(0)), "{rest}");
    let mut left = names(&b.book.list(&me, "", None).await.unwrap());
    left.sort();
    assert_eq!(left, ["Hugo", "Hugo", "Léa Martin", "Marie", "Marie", "Nina", "Nina", "Paul", "Zoé"]);
    assert_eq!(b.book.merge_exact(&me, false, 50).await.unwrap()["groups"], 0, "plus rien d'exact");

    // La Léa gardée est celle qui avait le plus de visages ; ceux de l'autre l'ont rejointe.
    let kept: Vec<String> = b.db.readers().read(|c| Ok(c.prepare("SELECT DISTINCT contact_uid FROM faces")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
    assert_eq!(kept, [uid2]);
    // Rien n'est perdu : la note de la copie fusionnée de Zoé est dans la fiche gardée.
    let zoe_path = b.book.list(&me, "Zoé", None).await.unwrap()["contacts"][0]["path"].as_str().unwrap().to_owned();
    assert!(card_text(&b, &zoe_path).await.contains("à garder"));
}
