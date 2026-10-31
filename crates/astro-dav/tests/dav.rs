//! WebDAV de bout en bout : PUT en flux → PROPFIND → GET (partiel) → conteneur UENC v2 vérifié.

#![allow(clippy::unwrap_used)]

mod common;

use astro_crypto::{UencKey, uenc};
use axum::body::Body;
use common::*;
use sha2::{Digest, Sha256};

#[tokio::test]
async fn options_needs_no_auth_and_other_methods_demand_it() {
    let b = bench("auth").await;
    let r = call(&b.app, "OPTIONS", "/dav/", None, &[], Body::empty()).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["dav"], "1, 2, addressbook, calendar-access", "CardDAV et CalDAV annoncés");

    let r = call(&b.app, "PROPFIND", "/dav/", None, &[], Body::empty()).await;
    assert_eq!(r.status(), 401);
    let challenges: Vec<_> = r.headers().get_all("www-authenticate").iter().map(|v| v.to_str().unwrap().to_owned()).collect();
    assert!(challenges.iter().any(|c| c.starts_with("Basic")) && challenges.iter().any(|c| c.starts_with("Nostr")), "{challenges:?}");

    for bad in [basic("alice@example.org", "mauvais-compte"), "Basic !!!".to_owned(), basic("inconnu@example.org", &hex(&ALICE)), "Bearer x".to_owned()] {
        assert_eq!(call(&b.app, "PROPFIND", "/dav/", Some(&bad), &[], Body::empty()).await.status(), 401, "{bad}");
    }
    // Compte non hébergé : un jeton valide pour sa clé ne suffit pas.
    assert_eq!(call(&b.app, "PROPFIND", "/dav/", Some(&basic(&hex(&STRANGER), &hex(&STRANGER))), &[], Body::empty()).await.status(), 401);
    assert_eq!(call(&b.app, "PROPFIND", "/dav/", Some(&alice()), &[("depth", "0")], Body::empty()).await.status(), 207);
    // Hors du montage.
    assert_eq!(call(&b.app, "GET", "/autre/x", Some(&alice()), &[], Body::empty()).await.status(), 404);
}

#[tokio::test]
async fn streaming_put_then_propfind_range_get_and_container_check() {
    let b = bench("flow").await;
    let plain = sample(300_000, 7); // plus de 4 chunks de 64 Kio, livrés en morceaux de 70 000 octets
    let r = call(&b.app, "PUT", "/dav/rapport.bin", Some(&alice()), &[("content-type", "application/x-test")], streamed(plain.clone(), 70_000)).await;
    assert_eq!(r.status(), 201);

    // PROPFIND profondeur 1 et 0.
    let (status, xml) = text(call(&b.app, "PROPFIND", "/dav/", Some(&alice()), &[("depth", "1")], Body::empty()).await).await;
    assert_eq!(status, 207);
    for needle in ["<D:displayname>rapport.bin</D:displayname>", "<D:getcontentlength>300000</D:getcontentlength>", "<D:getcontenttype>application/x-test</D:getcontenttype>", "<D:collection/>", "<D:getlastmodified>"] {
        assert!(xml.contains(needle), "{needle} absent de {xml}");
    }
    let (_, one) = text(call(&b.app, "PROPFIND", "/dav/rapport.bin", Some(&alice()), &[("depth", "0")], Body::empty()).await).await;
    assert_eq!(one.matches("<D:response>").count(), 1);
    assert_eq!(call(&b.app, "PROPFIND", "/dav/absent", Some(&alice()), &[], Body::empty()).await.status(), 404);

    // GET complet, HEAD, ETag.
    let (status, headers, body) = bytes(call(&b.app, "GET", "/dav/rapport.bin", Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!((status, body == plain), (200, true));
    assert_eq!(headers["content-length"], "300000");
    assert_eq!(headers["accept-ranges"], "bytes");
    let etag = headers["etag"].to_str().unwrap().to_owned();
    let head = call(&b.app, "HEAD", "/dav/rapport.bin", Some(&alice()), &[], Body::empty()).await;
    assert_eq!((head.status().as_u16(), head.headers()["content-length"].to_str().unwrap()), (200, "300000"));
    assert_eq!(call(&b.app, "GET", "/dav/rapport.bin", Some(&alice()), &[("if-none-match", &etag)], Body::empty()).await.status(), 304);

    // GET partiel : à cheval sur les chunks 1 et 2, début, fin, suffixe, hors bornes.
    for (range, lo, hi) in [("bytes=65530-65545", 65_530, 65_545), ("bytes=0-0", 0, 0), ("bytes=299990-", 299_990, 299_999), ("bytes=-5", 299_995, 299_999), ("bytes=131072-196607", 131_072, 196_607)] {
        let (status, headers, body) = bytes(call(&b.app, "GET", "/dav/rapport.bin", Some(&alice()), &[("range", range)], Body::empty()).await).await;
        assert_eq!(status, 206, "{range}");
        assert_eq!(body, plain[lo..=hi], "{range}");
        assert_eq!(headers["content-range"], format!("bytes {lo}-{hi}/300000"));
        assert_eq!(headers["content-length"], (hi - lo + 1).to_string());
    }
    let r = call(&b.app, "GET", "/dav/rapport.bin", Some(&alice()), &[("range", "bytes=300000-")], Body::empty()).await;
    assert_eq!(r.status(), 416);
    assert_eq!(r.headers()["content-range"], "bytes */300000");

    // Conteneur écrit : UENC v2, chunks de 64 Kio, ni clair lisible, taille exacte, clé du trousseau valide.
    let (cid, key_hex, sha): (String, String, String) = b
        .db
        .readers()
        .read(|c| {
            Ok(c.query_row(
                "SELECT e.cid, k.key_hex, e.sha_plain FROM ucloud_entries e JOIN ucloud_keyring k ON k.cid = e.cid AND k.owner_hex = e.owner_hex WHERE e.path = '/rapport.bin'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap();
    let blob = std::fs::read(b.blobs.root().join(&cid)).unwrap();
    assert_eq!(&blob[..4], b"UENC");
    assert_eq!(blob[4], 2, "version 2");
    assert!(matches!(blob[5], 1 | 2), "AES-GCM ou ChaCha20-Poly1305");
    assert_eq!(blob[6], 16, "chunks de 2^16 = 64 Kio");
    assert_eq!(blob.len(), 14 + 300_000 + 5 * 16, "5 chunks (4 pleins + 1 partiel), 16 octets de tag chacun");
    assert!(!blob.windows(64).any(|w| plain.windows(64).next() == Some(w) || w == &plain[1000..1064]), "le clair ne doit pas apparaître dans le blob");
    let key = UencKey::from_hex(&key_hex).unwrap();
    assert_eq!(uenc::decrypt_to_vec(&key, &blob).await.unwrap(), plain);
    assert_eq!(sha, hex::encode(Sha256::digest(&plain)));
    assert_eq!(uenc::v2_plaintext_len(blob.len() as u64, 16).unwrap(), 300_000);
    assert_eq!(astro_dav::blob::cid_raw_sha256(&Sha256::digest(&blob).into()), cid, "le CID est l'empreinte du blob chiffré");
}

#[tokio::test]
async fn each_file_gets_its_own_key_and_empty_files_work() {
    let b = bench("keys").await;
    for name in ["a.txt", "b.txt", "vide.txt"] {
        let body = if name == "vide.txt" { Vec::new() } else { b"meme contenu".to_vec() };
        assert_eq!(call(&b.app, "PUT", &format!("/dav/{name}"), Some(&alice()), &[], Body::from(body)).await.status(), 201);
    }
    let keys: Vec<String> = b.db.readers().read(|c| Ok(c.prepare("SELECT key_hex FROM ucloud_keyring")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
    assert_eq!(keys.len(), 3);
    assert_eq!(keys.iter().collect::<std::collections::HashSet<_>>().len(), 3, "une clé distincte par fichier");
    let (status, _, body) = bytes(call(&b.app, "GET", "/dav/vide.txt", Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!((status, body.len()), (200, 0));
    let (_, headers, _) = bytes(call(&b.app, "GET", "/dav/a.txt", Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!(headers["content-type"], "text/plain; charset=utf-8", "type deviné d'après l'extension");
    // Remplacement : 204, ancienne clé et ancien blob disparaissent.
    let before = std::fs::read_dir(b.blobs.root()).unwrap().count();
    assert_eq!(call(&b.app, "PUT", "/dav/a.txt", Some(&alice()), &[], Body::from("nouveau")).await.status(), 204);
    assert_eq!(std::fs::read_dir(b.blobs.root()).unwrap().count(), before, "un blob remplacé, pas ajouté");
    let n: i64 = b.db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM ucloud_keyring", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(n, 3);
    let (_, _, body) = bytes(call(&b.app, "GET", "/dav/a.txt", Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!(body, b"nouveau");
}

#[tokio::test]
async fn collections_mkcol_propfind_infinity_move_and_delete() {
    let b = bench("tree").await;
    let a = alice();
    let go = |m: &'static str, p: &'static str, extra: &'static [(&'static str, &'static str)], body: &'static str| {
        let (app, a) = (b.app.clone(), a.clone());
        async move { call(&app, m, p, Some(&a), extra, Body::from(body)).await.status().as_u16() }
    };
    assert_eq!(go("MKCOL", "/dav/Photos", &[], "").await, 201);
    assert_eq!(go("MKCOL", "/dav/Photos", &[], "").await, 405, "existe déjà");
    assert_eq!(go("MKCOL", "/dav/a/b", &[], "").await, 409, "parent absent");
    assert_eq!(go("MKCOL", "/dav/Autre", &[], "<x/>").await, 415);
    assert_eq!(go("MKCOL", "/dav/Photos/%C3%A9t%C3%A9%202026", &[], "").await, 201);
    assert_eq!(go("PUT", "/dav/Photos/%C3%A9t%C3%A9%202026/a.jpg", &[], "jpeg").await, 201);
    assert_eq!(go("PUT", "/dav/Photos/nouveau/b.jpg", &[], "x").await, 409, "dossier parent absent");
    assert_eq!(go("PUT", "/dav/Photos", &[], "x").await, 405, "PUT sur une collection");
    assert_eq!(go("PUT", "/dav/Photos/%C3%A9t%C3%A9%202026/a.jpg/sous", &[], "x").await, 409, "le parent est un fichier");

    let (status, xml) = text(call(&b.app, "PROPFIND", "/dav/", Some(&a), &[("depth", "infinity")], Body::empty()).await).await;
    assert_eq!(status, 207);
    assert_eq!(xml.matches("<D:response>").count(), 4, "racine, Photos, été 2026, a.jpg : {xml}");
    assert!(xml.contains("<D:href>/dav/Photos/%C3%A9t%C3%A9%202026/a.jpg</D:href>"), "{xml}");
    let (_, depth1) = text(call(&b.app, "PROPFIND", "/dav/Photos", Some(&a), &[("depth", "1")], Body::empty()).await).await;
    assert_eq!(depth1.matches("<D:response>").count(), 2, "Photos + son enfant direct");

    // MOVE : renommage de dossier, Overwrite et Destination absolue.
    assert_eq!(go("MOVE", "/dav/Photos", &[("destination", "http://localhost/dav/Images")], "").await, 201);
    assert_eq!(go("GET", "/dav/Images/%C3%A9t%C3%A9%202026/a.jpg", &[], "").await, 200);
    assert_eq!(go("GET", "/dav/Photos/%C3%A9t%C3%A9%202026/a.jpg", &[], "").await, 404);
    assert_eq!(go("PUT", "/dav/x.txt", &[], "x").await, 201);
    assert_eq!(go("MOVE", "/dav/x.txt", &[("destination", "/dav/Images"), ("overwrite", "F")], "").await, 412);
    assert_eq!(go("MOVE", "/dav/Images", &[("destination", "/dav/Images/dedans")], "").await, 403);
    assert_eq!(go("MOVE", "/dav/x.txt", &[("destination", "http://autre-hote/hors/montage")], "").await, 502);
    assert_eq!(go("MOVE", "/dav/x.txt", &[], "").await, 400, "Destination manquant");
    assert_eq!(go("MOVE", "/dav/x.txt", &[("destination", "/dav/Images/x.txt")], "").await, 201);
    assert_eq!(go("MOVE", "/dav/Images/x.txt", &[("destination", "/dav/Images/x.txt")], "").await, 403);

    // DELETE récursif : entrées, clés et blobs partent ensemble.
    assert!(std::fs::read_dir(b.blobs.root()).unwrap().count() > 1);
    assert_eq!(go("DELETE", "/dav/Images", &[], "").await, 204);
    assert_eq!(go("DELETE", "/dav/Images", &[], "").await, 404);
    assert_eq!(go("DELETE", "/dav/", &[], "").await, 403);
    let (entries, keys): (i64, i64) = b.db.readers().read(|c| Ok((c.query_row("SELECT count(*) FROM ucloud_entries", [], |r| r.get(0))?, c.query_row("SELECT count(*) FROM ucloud_keyring", [], |r| r.get(0))?))).await.unwrap();
    assert_eq!((entries, keys), (0, 0));
    assert_eq!(std::fs::read_dir(b.blobs.root()).unwrap().filter(|e| e.as_ref().unwrap().file_name() != ".tmp").count(), 0, "plus aucun blob");
    assert_eq!(go("LOCK", "/dav/x", &[], "").await, 412, "LOCK sans corps ni jeton : rien à prolonger");
    assert_eq!(go("PATCH", "/dav/x", &[], "").await, 405);
}

#[tokio::test]
async fn accounts_are_isolated_from_each_other() {
    let b = bench("iso").await;
    assert_eq!(call(&b.app, "PUT", "/dav/secret.txt", Some(&alice()), &[], Body::from("pour alice")).await.status(), 201);
    assert_eq!(call(&b.app, "GET", "/dav/secret.txt", Some(&bob()), &[], Body::empty()).await.status(), 404);
    assert_eq!(call(&b.app, "PUT", "/dav/secret.txt", Some(&bob()), &[], Body::from("pour bob")).await.status(), 201);
    let (_, _, a) = bytes(call(&b.app, "GET", "/dav/secret.txt", Some(&alice()), &[], Body::empty()).await).await;
    let (_, _, bo) = bytes(call(&b.app, "GET", "/dav/secret.txt", Some(&bob()), &[], Body::empty()).await).await;
    assert_eq!((a.as_slice(), bo.as_slice()), (b"pour alice".as_slice(), b"pour bob".as_slice()));
    assert_eq!(call(&b.app, "DELETE", "/dav/secret.txt", Some(&bob()), &[], Body::empty()).await.status(), 204);
    assert_eq!(call(&b.app, "GET", "/dav/secret.txt", Some(&alice()), &[], Body::empty()).await.status(), 200);
}

#[tokio::test]
async fn size_caps_and_quota_by_tier() {
    let b = bench_with("limits", |c| {
        c.max_file_multipass = 1000;
        c.max_file_zencard = 4000;
        c.quota_multipass = 1500;
        c.quota_zencard = 100_000;
    })
    .await;
    let (a, z) = (alice(), bob());
    // Plafond unitaire annoncé par Content-Length : refusé avant lecture du corps.
    let r = call(&b.app, "PUT", "/dav/gros.bin", Some(&a), &[("content-length", "2000")], Body::from(vec![0u8; 2000])).await;
    assert_eq!(r.status(), 413);
    // Sans longueur annoncée (flux), le plafond est imposé pendant le téléversement, sans rien laisser.
    let r = call(&b.app, "PUT", "/dav/gros.bin", Some(&a), &[], streamed(sample(2500, 1), 300)).await;
    assert_eq!(r.status(), 413);
    assert_eq!(std::fs::read_dir(b.blobs.root()).unwrap().filter(|e| e.as_ref().unwrap().file_name() != ".tmp").count(), 0);
    assert_eq!(std::fs::read_dir(b.blobs.root().join(".tmp")).unwrap().count(), 0, "aucun fichier temporaire résiduel");
    // La ZenCard a un plafond plus haut.
    assert_eq!(call(&b.app, "PUT", "/dav/gros.bin", Some(&z), &[], streamed(sample(2500, 1), 300)).await.status(), 201);
    // Quota total : 900 + 700 > 1500, mais remplacer le fichier de 900 par 1000 reste permis.
    assert_eq!(call(&b.app, "PUT", "/dav/a.bin", Some(&a), &[], Body::from(vec![1u8; 900])).await.status(), 201);
    assert_eq!(call(&b.app, "PUT", "/dav/b.bin", Some(&a), &[], Body::from(vec![2u8; 700])).await.status(), 507);
    assert_eq!(call(&b.app, "PUT", "/dav/b.bin", Some(&a), &[], streamed(sample(700, 2), 100)).await.status(), 507, "quota dépassé constaté après le flux : blob nettoyé");
    assert_eq!(call(&b.app, "PUT", "/dav/a.bin", Some(&a), &[], Body::from(vec![3u8; 1000])).await.status(), 204);
    let blobs = std::fs::read_dir(b.blobs.root()).unwrap().filter(|e| e.as_ref().unwrap().file_name() != ".tmp").count();
    assert_eq!(blobs, 2, "gros.bin (bob) + a.bin (alice) : pas de blob orphelin");
}

#[tokio::test]
async fn nip98_authentication_binding_payload_and_replay() {
    let b = bench("nip98").await;
    let body = b"contenu signe".to_vec();
    let sha = hex::encode(Sha256::digest(&body));
    let h = nip98(&ALICE, "PUT", "/dav/n.txt", Some(&sha));
    assert_eq!(call(&b.app, "PUT", "/dav/n.txt", Some(&h), &[], Body::from(body.clone())).await.status(), 201);
    // Rejeu du même en-tête : refusé.
    assert_eq!(call(&b.app, "PUT", "/dav/n.txt", Some(&h), &[], Body::from(body.clone())).await.status(), 401);
    // Mauvaise méthode / mauvais chemin : la liaison échoue.
    let h = nip98(&ALICE, "GET", "/dav/n.txt", None);
    assert_eq!(call(&b.app, "PUT", "/dav/n.txt", Some(&h), &[], Body::from("x")).await.status(), 401);
    let h = nip98(&ALICE, "PUT", "/dav/autre.txt", None);
    assert_eq!(call(&b.app, "PUT", "/dav/n.txt", Some(&h), &[], Body::from("x")).await.status(), 401);
    // Empreinte du corps différente du tag payload : le téléversement est refusé et rien n'est stocké.
    let h = nip98(&ALICE, "PUT", "/dav/faux.txt", Some(&sha));
    assert_eq!(call(&b.app, "PUT", "/dav/faux.txt", Some(&h), &[], Body::from("autre contenu")).await.status(), 401);
    assert_eq!(call(&b.app, "GET", "/dav/faux.txt", Some(&alice()), &[], Body::empty()).await.status(), 404);
    // Clé non hébergée : 403 ; lecture avec NIP-98 valide : 200.
    let h = nip98(&STRANGER, "GET", "/dav/n.txt", None);
    assert_eq!(call(&b.app, "GET", "/dav/n.txt", Some(&h), &[], Body::empty()).await.status(), 403);
    let h = nip98(&ALICE, "GET", "/dav/n.txt", None);
    let (status, _, got) = bytes(call(&b.app, "GET", "/dav/n.txt", Some(&h), &[], Body::empty()).await).await;
    assert_eq!((status, got), (200, body));
}

#[tokio::test]
async fn a_dropped_upload_leaves_no_trace() {
    let b = bench("abort").await;
    // Flux qui échoue au milieu.
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![Ok(sample(100_000, 3)), Err(std::io::Error::other("connexion coupée"))];
    let r = call(&b.app, "PUT", "/dav/coupe.bin", Some(&alice()), &[], Body::from_stream(futures_util::stream::iter(chunks))).await;
    assert_eq!(r.status(), 400);
    assert_eq!(call(&b.app, "GET", "/dav/coupe.bin", Some(&alice()), &[], Body::empty()).await.status(), 404);
    assert_eq!(std::fs::read_dir(b.blobs.root().join(".tmp")).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(b.blobs.root()).unwrap().count(), 1, "seul le dossier .tmp");
}

#[tokio::test]
async fn a_generic_content_type_falls_back_to_the_extension() {
    let b = bench("generic-mime").await;
    for (ct, expected) in [("application/x-www-form-urlencoded", "image/png"), ("application/octet-stream", "image/png"), ("image/webp; charset=x", "image/webp")] {
        let r = call(&b.app, "PUT", "/dav/p.png", Some(&alice()), &[("content-type", ct)], Body::from("x")).await;
        assert!(r.status().is_success(), "{ct} : {}", r.status());
        let owner = hex(&ALICE);
        let mime: String = b.db.readers().read(move |c| Ok(c.query_row("SELECT mime FROM ucloud_entries WHERE owner_hex = ?1 AND path = '/p.png'", [owner], |r| r.get(0))?)).await.unwrap();
        assert_eq!(mime, expected, "{ct}");
    }
}
