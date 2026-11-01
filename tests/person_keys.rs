//! Une personne, plusieurs UPlanet : annuaire par email entre les plans de la station, liens prouvés par les deux clés.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use astro_crypto::{KeySource, StaticKeySource};
use astro_db::Db;
use astro_nostr_proto::{UnsignedEvent, pubkey_of};
use astroportd::person_keys::{PersonKeys, Sibling};
use astroportd::register::PublishFn;

const LEA_ORIGIN: [u8; 32] = [0x11; 32];
const LEA_ZEN: [u8; 32] = [0x22; 32];
const MAL: [u8; 32] = [0x33; 32];

fn hex(s: &[u8; 32]) -> String {
    pubkey_of(s).unwrap()
}

async fn plane_db(dir: &std::path::Path, name: &str, accounts: &[(&[u8; 32], &str)]) -> Arc<Db> {
    std::fs::create_dir_all(dir.join(name)).unwrap();
    let db = Arc::new(Db::open(&dir.join(name).join("uplanet.db"), 2).unwrap());
    for (s, email) in accounts {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex(s).into(), (*email).to_owned().into()]).await.unwrap();
    }
    db
}

fn into_db(db: &Arc<Db>) -> PublishFn {
    let db = db.clone();
    Arc::new(move |ev| {
        let db = db.clone();
        Box::pin(async move { astro_sync::ingest_event(db.writer(), ev, "test").await.map(|_| ()).map_err(|e| e.to_string()) })
    })
}

#[tokio::test]
async fn the_email_finds_the_key_in_every_plane_of_the_station() {
    let dir = std::env::temp_dir().join(format!("astroportd-pk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let origin = plane_db(&dir, "origin", &[(&LEA_ORIGIN, "lea@example.org")]).await;
    let _zen = plane_db(&dir, "zen1", &[(&LEA_ZEN, "Lea@Example.org")]).await;
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new());
    let pk = PersonKeys::new(origin.clone(), "origin".into(), vec![Sibling { name: "zen1".into(), db: dir.join("zen1/uplanet.db"), game: None }], keys, into_db(&origin));
    let found = pk.keys_for(&["lea@example.org".to_owned()], None).await;
    let got: Vec<(&str, &str)> = found.iter().map(|k| (k.plane.as_str(), k.hex.as_str())).collect();
    assert_eq!(got, [("origin", hex(&LEA_ORIGIN).as_str()), ("zen1", hex(&LEA_ZEN).as_str())], "ce plan d'abord, puis les voisins : {found:?}");
    assert!(pk.by_email("inconnu@example.org").await.is_empty());
    assert!(pk.by_email("pas-un-email").await.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_link_counts_only_when_both_keys_signed_it() {
    let dir = std::env::temp_dir().join(format!("astroportd-pk-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let origin = plane_db(&dir, "origin", &[(&LEA_ORIGIN, "lea@example.org")]).await;
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new());
    let pk = PersonKeys::new(origin.clone(), "origin".into(), vec![], keys, into_db(&origin));
    let (a, b, m) = (hex(&LEA_ORIGIN), hex(&LEA_ZEN), hex(&MAL));
    let proof = |signer: &[u8; 32], other: &str| UnsignedEvent { created_at: 100, kind: 31903, tags: vec![vec!["d".into(), "plane-link-proof".into()], vec!["p".into(), other.to_owned()]], content: String::new() }.sign(signer).unwrap();
    let link = |signer: &[u8; 32], other: &str, proof: &astro_nostr_proto::Event| UnsignedEvent { created_at: 100, kind: 31903, tags: vec![vec!["d".into(), "plane-link".into()], vec!["p".into(), other.to_owned()], vec!["plane".into(), "zen1".into()]], content: serde_json::to_string(proof).unwrap() }.sign(signer).unwrap();
    let publish = into_db(&origin);

    // Mal prétend que la clé de Léa (zen1) est la sienne, sans preuve de Léa : sa propre signature sert de « preuve » → refusé.
    publish(link(&MAL, &b, &proof(&MAL, &m))).await.unwrap();
    // Mal fabrique la preuve avec la mauvaise cible : refusé.
    publish(link(&MAL, &b, &proof(&LEA_ZEN, &a))).await.unwrap();
    assert!(pk.linked(&b).await.is_empty() && pk.linked(&m).await.is_empty(), "aucun faux lien retenu");

    // Léa signe des deux côtés : le lien existe, dans les deux sens.
    publish(link(&LEA_ORIGIN, &b, &proof(&LEA_ZEN, &a))).await.unwrap();
    assert_eq!(pk.linked(&a).await.iter().map(|k| (k.hex.as_str(), k.plane.as_str(), k.via.as_str())).collect::<Vec<_>>(), [(b.as_str(), "zen1", "lien")]);
    assert_eq!(pk.linked(&b).await.iter().map(|k| k.hex.as_str()).collect::<Vec<_>>(), [a.as_str()]);
    assert!(pk.same_person(&a, &b).await && pk.same_person(&b, &a).await && !pk.same_person(&a, &m).await);
    // Une fiche qui ne connaît que la clé de zen1 retrouve donc aussi celle d'origin.
    let all = pk.keys_for(&[], Some(&b)).await;
    assert_eq!(all.len(), 2, "{all:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn announcing_links_the_accounts_the_station_holds_in_other_planes() {
    let dir = std::env::temp_dir().join(format!("astroportd-pk-ann-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let origin = plane_db(&dir, "origin", &[(&LEA_ORIGIN, "lea@example.org")]).await;
    let _zen = plane_db(&dir, "zen1", &[(&LEA_ZEN, "lea@example.org")]).await;
    // Dossier des comptes du plan zen1 : `nostr/<email>/.secret.nostr`.
    let game = dir.join("game-zen1");
    std::fs::create_dir_all(game.join("nostr/lea@example.org")).unwrap();
    let nsec = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), &LEA_ZEN).unwrap();
    std::fs::write(game.join("nostr/lea@example.org/.secret.nostr"), format!("NSEC={nsec}; NPUB=x; HEX={}\n", hex(&LEA_ZEN))).unwrap();
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(LEA_ORIGIN));
    let sibs = vec![Sibling { name: "zen1".into(), db: dir.join("zen1/uplanet.db"), game: Some(game) }, Sibling { name: "zen2".into(), db: dir.join("absent.db"), game: None }];
    let pk = PersonKeys::new(origin.clone(), "origin".into(), sibs, keys, into_db(&origin));
    let r = pk.announce(&hex(&LEA_ORIGIN), "lea@example.org").await.unwrap();
    assert_eq!(r["linked"][0]["plane"], "zen1", "{r}");
    assert_eq!(r["skipped"][0]["plane"], "zen2");
    assert_eq!(pk.linked(&hex(&LEA_ORIGIN)).await[0].hex, hex(&LEA_ZEN), "le lien publié est accepté par la projection");
    assert!(PersonKeys::new(origin.clone(), "origin".into(), vec![], Arc::new(StaticKeySource::new()), into_db(&origin)).announce(&hex(&LEA_ORIGIN), "x@y.z").await.is_err(), "sans la clé du compte, rien n'est signé");
    let _ = std::fs::remove_dir_all(dir);
}
