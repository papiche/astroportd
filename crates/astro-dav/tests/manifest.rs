//! Manifeste continu : chaque modification du VFS est ancrée dans un kind 30078 `ucloud:manifest` chiffré en NIP-44.

#![allow(clippy::unwrap_used)]

mod common;

use astro_crypto::{KeySource, nip44};
use astro_dav::manifest::{self, D_TAG, ManifestError, is_manifest_event};
use axum::body::Body;
use common::*;

async fn manifest_events(b: &Bench, owner: &str) -> Vec<astro_nostr_proto::Event> {
    let owner = owner.to_owned();
    b.db
        .readers()
        .read(move |c| {
            let f = astro_nostr_proto::Filter { kinds: Some(vec![30_078]), authors: Some(vec![owner]), ..Default::default() };
            astro_db::events::query_events(c, &f, u64::MAX / 2, 100)
        })
        .await
        .unwrap()
}

async fn dump_vfs(b: &Bench) -> (Vec<String>, Vec<String>) {
    b.db
        .readers()
        .read(|c| {
            let rows = |sql: &str| -> Result<Vec<String>, astro_db::DbError> {
                Ok(c.prepare(sql)?.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?)
            };
            Ok((
                rows("SELECT owner_hex||'|'||path||'|'||parent||'|'||type||'|'||coalesce(cid,'')||'|'||coalesce(mime,'')||'|'||coalesce(size,'')||'|'||coalesce(sha_plain,'')||'|'||data FROM ucloud_entries ORDER BY 1")?,
                rows("SELECT owner_hex||'|'||cid||'|'||key_hex||'|'||enc FROM ucloud_keyring ORDER BY 1")?,
            ))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn every_change_is_anchored_and_the_pointer_is_self_encrypted() {
    let b = bench("anchor").await;
    let (a, owner) = (alice(), hex(&ALICE));
    assert_eq!(call(&b.app, "MKCOL", "/dav/Docs", Some(&a), &[], Body::empty()).await.status(), 201);
    assert_eq!(call(&b.app, "PUT", "/dav/Docs/note.txt", Some(&a), &[], Body::from("bonjour")).await.status(), 201);
    assert!(b.publisher.flush().await.is_empty());

    let events = manifest_events(&b, &owner).await;
    assert_eq!(events.len(), 1, "remplaçable : un seul événement par compte");
    let ev = &events[0];
    assert!(is_manifest_event(ev) && ev.tag_value("d") == Some(D_TAG));
    ev.verify().unwrap();
    assert!(!ev.content.contains("note.txt") && !ev.content.contains("bafkrei"), "le contenu n'est pas lisible en clair");
    // Seule la clé du propriétaire déchiffre ; une autre clé échoue.
    assert!(nip44::decrypt_from_self(&BOB, &hex(&BOB), &ev.content).is_err());
    let pointer = manifest::open_pointer(ev, keys().as_ref()).unwrap();
    assert_eq!((pointer.rev, pointer.entries, pointer.files), (1, 2, 1));
    let m = manifest::fetch_manifest(b.blobs.as_ref(), &pointer, &owner).await.unwrap();
    assert_eq!(m.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["/Docs", "/Docs/note.txt"]);
    assert_eq!(m.keyring.len(), 1);
    // Le blob de manifeste est chiffré : sans la clé du pointeur, il n'est pas exploitable.
    let blob = std::fs::read(b.blobs.root().join(&pointer.manifest_cid)).unwrap();
    assert_eq!(&blob[..5], b"UENC\x02");
    assert!(!String::from_utf8_lossy(&blob).contains("note.txt"));
    // Le compte de Bob n'a pas de manifeste : isolation.
    assert!(manifest_events(&b, &hex(&BOB)).await.is_empty());
}

#[tokio::test]
async fn republishing_replaces_the_event_with_a_strictly_newer_one_and_drops_the_old_blob() {
    let b = bench("rev").await;
    let (a, owner) = (alice(), hex(&ALICE));
    let mut created = Vec::new();
    let mut cids = Vec::new();
    for i in 0..3 {
        assert_eq!(call(&b.app, "PUT", &format!("/dav/f{i}.txt"), Some(&a), &[], Body::from(format!("contenu {i}"))).await.status(), 201);
        assert!(b.publisher.flush().await.is_empty());
        let ev = manifest_events(&b, &owner).await.pop().unwrap();
        let p = manifest::open_pointer(&ev, keys().as_ref()).unwrap();
        created.push(ev.created_at);
        cids.push(p.manifest_cid);
        assert_eq!(p.rev, i + 1);
        assert_eq!(p.files, i + 1);
    }
    assert!(created.windows(2).all(|w| w[0] < w[1]), "created_at strictement croissant même dans la même seconde : {created:?}");
    assert_eq!(manifest_events(&b, &owner).await.len(), 1);
    assert!(b.blobs.root().join(&cids[2]).exists());
    assert!(!b.blobs.root().join(&cids[0]).exists() && !b.blobs.root().join(&cids[1]).exists(), "anciens manifestes supprimés");
    // Une rafale de modifications regroupées : une seule publication (debounce d'1 h : seul `flush` publie).
    for i in 0..5 {
        call(&b.app, "PUT", &format!("/dav/rafale{i}.txt"), Some(&a), &[], Body::from("x")).await;
    }
    b.publisher.flush().await;
    let p = manifest::open_pointer(&manifest_events(&b, &owner).await[0], keys().as_ref()).unwrap();
    assert_eq!((p.rev, p.files), (4, 8));
}

#[tokio::test]
async fn publishing_requires_the_account_key() {
    let b = bench("nokey").await;
    // Un compte hébergé dont la station n'a pas la clé : l'échec est rapporté, rien n'est publié.
    let empty_keys = astro_crypto::StaticKeySource::new();
    let r = manifest::publish(&b.db, b.blobs.as_ref(), &empty_keys, &hex(&ALICE)).await;
    assert!(matches!(r, Err(ManifestError::NoKey)));
    assert!(manifest_events(&b, &hex(&ALICE)).await.is_empty());
}

#[tokio::test]
async fn the_vfs_and_keyring_are_restored_exactly_after_the_local_tables_are_purged() {
    let b = bench("restore").await;
    let (a, z) = (alice(), bob());
    call(&b.app, "MKCOL", "/dav/Photos", Some(&a), &[], Body::empty()).await;
    call(&b.app, "MKCOL", "/dav/Photos/%C3%A9t%C3%A9", Some(&a), &[], Body::empty()).await;
    call(&b.app, "PUT", "/dav/Photos/%C3%A9t%C3%A9/a.jpg", Some(&a), &[("content-type", "image/jpeg")], streamed(sample(150_000, 5), 33_333)).await;
    call(&b.app, "PUT", "/dav/lisez-moi.txt", Some(&a), &[], Body::from("texte")).await;
    call(&b.app, "PUT", "/dav/bob.bin", Some(&z), &[], Body::from(vec![9u8; 5000])).await;
    assert!(b.publisher.flush().await.is_empty());

    let before = dump_vfs(&b).await;
    assert_eq!((before.0.len(), before.1.len()), (5, 3));
    let original = {
        let (_, _, body) = bytes(call(&b.app, "GET", "/dav/Photos/%C3%A9t%C3%A9/a.jpg", Some(&a), &[], Body::empty()).await).await;
        body
    };

    // Purge de la base locale : seuls les événements Nostr (et les blobs) survivent.
    b.db.writer().batch(vec![astro_db::Statement::new("DELETE FROM ucloud_entries", vec![]), astro_db::Statement::new("DELETE FROM ucloud_keyring", vec![])]).await.unwrap();
    assert!(dump_vfs(&b).await.0.is_empty());
    assert_eq!(call(&b.app, "GET", "/dav/lisez-moi.txt", Some(&a), &[], Body::empty()).await.status(), 404);

    for owner in [hex(&ALICE), hex(&BOB)] {
        let ev = manifest_events(&b, &owner).await.pop().unwrap();
        manifest::restore_from_event(&b.db, b.blobs.as_ref(), keys().as_ref(), &ev).await.unwrap();
    }
    let after = dump_vfs(&b).await;
    assert_eq!(after.0, before.0, "VFS identique, ligne pour ligne");
    assert_eq!(after.1, before.1, "trousseau identique");
    // Et les fichiers sont de nouveau lisibles, octet pour octet.
    let (_, _, body) = bytes(call(&b.app, "GET", "/dav/Photos/%C3%A9t%C3%A9/a.jpg", Some(&a), &[], Body::empty()).await).await;
    assert_eq!(body, original);
    let (status, headers, part) = bytes(call(&b.app, "GET", "/dav/Photos/%C3%A9t%C3%A9/a.jpg", Some(&a), &[("range", "bytes=100000-100009")], Body::empty()).await).await;
    assert_eq!((status, part.as_slice(), headers["content-range"].to_str().unwrap()), (206, &original[100_000..100_010], "bytes 100000-100009/150000"));
}

#[tokio::test]
async fn hostile_manifests_are_refused() {
    let b = bench("hostile").await;
    call(&b.app, "PUT", "/dav/a.txt", Some(&alice()), &[], Body::from("a")).await;
    b.publisher.flush().await;
    let ev = manifest_events(&b, &hex(&ALICE)).await.pop().unwrap();
    let pointer = manifest::open_pointer(&ev, keys().as_ref()).unwrap();
    let genuine = manifest::fetch_manifest(b.blobs.as_ref(), &pointer, &hex(&ALICE)).await.unwrap();
    // Manifeste d'un autre compte.
    assert!(matches!(manifest::fetch_manifest(b.blobs.as_ref(), &pointer, &hex(&BOB)).await, Err(ManifestError::Invalid(_))));
    // Contenu malveillant : chemin hors normes, doublon, clé invalide, dossier avec CID.
    let tweak = |f: &dyn Fn(&mut astro_dav::manifest::Manifest)| {
        let mut m = genuine.clone();
        f(&mut m);
        manifest::restore_statements(&m, "bafkreicid").err().map(|e| e.to_string())
    };
    assert!(tweak(&|m| m.entries[0].path = "/a/../etc".into()).is_some());
    assert!(tweak(&|m| m.entries[0].path = "/".into()).is_some());
    assert!(tweak(&|m| m.entries.push(m.entries[0].clone())).unwrap().contains("double"));
    assert!(tweak(&|m| m.keyring[0].key_hex = "zz".into()).unwrap().contains("clé"));
    assert!(tweak(&|m| { m.entries[0].kind = "dir".into(); m.entries[0].cid = Some("x".into()); }).is_some());
    assert!(tweak(&|_| {}).is_none(), "le manifeste authentique passe");
    // Événement qui n'est pas un manifeste, ou clé absente.
    let mut other = ev.clone();
    other.tags = vec![vec!["d".into(), "autre".into()]];
    assert!(matches!(manifest::open_pointer(&other, keys().as_ref()), Err(ManifestError::NotManifest)));
    assert!(matches!(manifest::open_pointer(&ev, &astro_crypto::StaticKeySource::new()), Err(ManifestError::NoKey)));
    let none: Option<astro_crypto::SecretBytes> = astro_crypto::StaticKeySource::new().secret(&hex(&ALICE));
    assert!(none.is_none());
}
