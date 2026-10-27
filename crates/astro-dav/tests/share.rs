//! Liens d'invité `#hash` : le serveur ne voit jamais la clé ; la page déchiffre dans le navigateur (WebCrypto sous Node ici).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::process::Command;

use astro_crypto::{UencKey, uenc};
use astro_dav::share::{self, GUEST_LIB_JS, create_guest_link, purge_expired, revoke_guest_link};
use axum::body::Body;
use common::*;
use sha2::{Digest, Sha256};

async fn put(b: &Bench, path: &str, data: Vec<u8>) {
    assert_eq!(call(&b.app, "PUT", path, Some(&alice()), &[], streamed(data, 50_000)).await.status(), 201);
}

async fn anon(b: &Bench, path: &str, extra: &[(&str, &str)]) -> axum::http::Response<Body> {
    call(&b.app, "GET", path, None, extra, Body::empty()).await
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

#[tokio::test]
async fn the_link_serves_only_ciphertext_and_the_key_never_reaches_the_server() {
    let b = bench("share").await;
    let photo = sample(200_000, 3); // plusieurs chunks de 64 Kio
    put(&b, "/dav/vacances.jpg", photo.clone()).await;
    call(&b.app, "MKCOL", "/dav/Album", Some(&alice()), &[], Body::empty()).await;
    put(&b, "/dav/Album/a.png", sample(1000, 4)).await;
    put(&b, "/dav/Album/b.png", sample(2000, 5)).await;
    let owner = hex(&ALICE);
    let link = create_guest_link(&b.db, b.blobs.as_ref(), &owner, &["/vacances.jpg".into(), "/Album".into()], "Vacances 2026", 3600, now()).await.unwrap();
    assert_eq!(link.items, 3);
    assert_eq!(link.path, format!("/s/{}", link.slug));
    assert_eq!(link.key_hex.len(), 64);

    // La page est publique, sans authentification, et durcie.
    let r = anon(&b, &link.path, &[]).await;
    assert_eq!(r.status(), 200);
    for (h, v) in [("cache-control", "private, no-store"), ("referrer-policy", "no-referrer"), ("x-content-type-options", "nosniff")] {
        assert_eq!(r.headers()[h], v, "{h}");
    }
    assert!(r.headers()["content-security-policy"].to_str().unwrap().contains("default-src 'none'"));
    let (_, html) = text(r).await;
    assert!(html.contains("location.hash") && html.contains("crypto.subtle"));
    assert!(!html.contains(&link.key_hex) && !html.contains(&link.slug), "la page ne contient ni la clé ni le lien");

    // Le serveur ne détient pas la clé : ni dans l'enregistrement, ni dans les blobs.
    let record: String = b.db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'guest_share'", [], |r| r.get(0))?)).await.unwrap();
    assert!(!record.contains(&link.key_hex) && !record.contains("vacances"), "{record}");

    // Manifeste : chiffré (noms invisibles), déchiffrable seulement avec la clé du lien.
    let (status, _, manifest_blob) = bytes(anon(&b, &format!("{}/m", link.path), &[]).await).await;
    assert_eq!(status, 200);
    assert_eq!((&manifest_blob[..5], manifest_blob[5]), (b"UENC\x02".as_slice(), 1), "UENC v2, AES-256-GCM (seul chiffrement de WebCrypto)");
    assert!(!String::from_utf8_lossy(&manifest_blob).contains("vacances"));
    let key = UencKey::from_hex(&link.key_hex).unwrap();
    let m: serde_json::Value = serde_json::from_slice(&uenc::decrypt_to_vec(&key, &manifest_blob).await.unwrap()).unwrap();
    assert_eq!(m["title"], "Vacances 2026");
    let items = m["items"].as_array().unwrap();
    let names: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["vacances.jpg", "Album/a.png", "Album/b.png"]);
    let other = UencKey::generate().unwrap();
    assert!(uenc::decrypt_to_vec(&other, &manifest_blob).await.is_err(), "une autre clé ne déchiffre rien");

    // Blobs : copies rechiffrées avec la clé du lien (CID différents de ceux du propriétaire).
    let owner_cids: Vec<String> = b.db.readers().read(|c| Ok(c.prepare("SELECT cid FROM ucloud_entries WHERE cid IS NOT NULL")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
    let first_cid = items[0]["cid"].as_str().unwrap();
    assert!(!owner_cids.iter().any(|c| c == first_cid));
    let (status, headers, blob) = bytes(anon(&b, &format!("{}/b/{first_cid}", link.path), &[]).await).await;
    assert_eq!((status, headers["accept-ranges"].to_str().unwrap()), (200, "bytes"));
    assert_eq!(uenc::decrypt_to_vec(&key, &blob).await.unwrap(), photo);
    assert_eq!(items[0]["size"], 200_000);
    // Lecture partielle du blob chiffré (le navigateur peut reprendre un téléchargement).
    let (status, headers, part) = bytes(anon(&b, &format!("{}/b/{first_cid}", link.path), &[("range", "bytes=100-199")]).await).await;
    assert_eq!((status, part.as_slice(), headers["content-range"].to_str().unwrap()), (206, &blob[100..200], format!("bytes 100-199/{}", blob.len()).as_str()));
    assert_eq!(anon(&b, &format!("{}/b/{first_cid}", link.path), &[("range", "bytes=9999999-")]).await.status(), 416);

    // Seuls les blobs du lien sont servis : ni ceux du propriétaire, ni un CID inventé, ni un lien inconnu.
    assert_eq!(anon(&b, &format!("{}/b/{}", link.path, owner_cids[0]), &[]).await.status(), 404);
    assert_eq!(anon(&b, &format!("{}/b/../../x", link.path), &[]).await.status(), 404);
    assert_eq!(anon(&b, "/s/00000000000000000000000000000000", &[]).await.status(), 404);
    assert_eq!(anon(&b, "/s/pas-un-slug", &[]).await.status(), 404);
}

#[tokio::test]
async fn revocation_expiry_and_ownership() {
    let b = bench("share-rev").await;
    put(&b, "/dav/a.txt", b"secret".to_vec()).await;
    let owner = hex(&ALICE);
    let paths = ["/a.txt".to_owned()];
    let link = create_guest_link(&b.db, b.blobs.as_ref(), &owner, &paths, "t", 3600, now()).await.unwrap();
    let blobs_before = share::link_cids(&b.db, &link.slug).await;
    assert_eq!(blobs_before.len(), 2, "un fichier + le manifeste");
    // Un autre compte ne peut pas révoquer.
    assert!(!revoke_guest_link(&b.db, b.blobs.as_ref(), &hex(&BOB), &link.slug).await.unwrap());
    assert_eq!(anon(&b, &link.path, &[]).await.status(), 200);
    // Le propriétaire révoque : lien mort, blobs supprimés, fichier d'origine intact.
    assert!(revoke_guest_link(&b.db, b.blobs.as_ref(), &owner, &link.slug).await.unwrap());
    assert!(!revoke_guest_link(&b.db, b.blobs.as_ref(), &owner, &link.slug).await.unwrap());
    assert_eq!(anon(&b, &link.path, &[]).await.status(), 404);
    for cid in &blobs_before {
        assert!(!b.blobs.root().join(cid).exists());
    }
    assert_eq!(call(&b.app, "GET", "/dav/a.txt", Some(&alice()), &[], Body::empty()).await.status(), 200);
    // Expiration : un lien expiré répond 404 et `purge_expired` le nettoie.
    let old = create_guest_link(&b.db, b.blobs.as_ref(), &owner, &paths, "t", -10, now()).await.unwrap();
    let live = create_guest_link(&b.db, b.blobs.as_ref(), &owner, &paths, "t", 3600, now()).await.unwrap();
    assert_eq!(anon(&b, &old.path, &[]).await.status(), 404);
    assert_eq!(anon(&b, &live.path, &[]).await.status(), 200);
    assert_eq!(purge_expired(&b.db, b.blobs.as_ref(), now()).await.unwrap(), 1);
    assert_eq!(anon(&b, &live.path, &[]).await.status(), 200);
    // Sélections invalides.
    assert!(create_guest_link(&b.db, b.blobs.as_ref(), &owner, &["/absent".to_owned()], "t", 60, now()).await.is_err());
    assert!(create_guest_link(&b.db, b.blobs.as_ref(), &owner, &[], "t", 60, now()).await.is_err());
    call(&b.app, "MKCOL", "/dav/vide", Some(&alice()), &[], Body::empty()).await;
    assert!(create_guest_link(&b.db, b.blobs.as_ref(), &owner, &["/vide".to_owned()], "t", 60, now()).await.is_err(), "dossier sans fichier");
}

fn node_available() -> bool {
    Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Déchiffre un blob avec le vrai code de la page (WebCrypto de Node) et rend `(succès, octets)`.
fn node_decrypt(dir: &std::path::Path, blob: &[u8], key_hex: &str) -> (bool, Vec<u8>) {
    let (script, file) = (dir.join("decrypt.js"), dir.join("blob.bin"));
    std::fs::write(&file, blob).unwrap();
    std::fs::write(
        &script,
        format!("{GUEST_LIB_JS}\nconst fs=require('fs');(async()=>{{const b=fs.readFileSync(process.argv[2]);const k=await importKey(process.argv[3]);const p=await decryptUenc2(k,b.buffer.slice(b.byteOffset,b.byteOffset+b.byteLength));process.stdout.write(Buffer.from(p));}})().catch(e=>{{console.error(e.message);process.exit(3)}});"),
    )
    .unwrap();
    let out = Command::new("node").arg(&script).arg(&file).arg(key_hex).output().unwrap();
    (out.status.success(), out.stdout)
}

#[tokio::test]
async fn the_browser_code_decrypts_what_the_server_wrote() {
    if !node_available() {
        eprintln!("node absent : test WebCrypto ignoré");
        return;
    }
    let b = bench("share-js").await;
    for (name, size) in [("petit.bin", 10usize), ("exact.bin", 65_536), ("deux_chunks.bin", 65_537), ("gros.bin", 400_000), ("vide.bin", 0)] {
        put(&b, &format!("/dav/{name}"), sample(size, size as u64 + 1)).await;
    }
    let owner = hex(&ALICE);
    let names = ["petit.bin", "exact.bin", "deux_chunks.bin", "gros.bin", "vide.bin"];
    let paths: Vec<String> = names.iter().map(|n| format!("/{n}")).collect();
    let link = create_guest_link(&b.db, b.blobs.as_ref(), &owner, &paths, "js", 3600, now()).await.unwrap();
    let key = UencKey::from_hex(&link.key_hex).unwrap();
    let (_, _, m) = bytes(anon(&b, &format!("{}/m", link.path), &[]).await).await;
    let dir = TempDir::new("node");

    // Le manifeste lui-même, déchiffré par le code de la page.
    let (ok, plain) = node_decrypt(&dir.0, &m, &link.key_hex);
    assert!(ok);
    assert_eq!(plain, uenc::decrypt_to_vec(&key, &m).await.unwrap());
    let manifest: serde_json::Value = serde_json::from_slice(&plain).unwrap();

    for (i, item) in manifest["items"].as_array().unwrap().iter().enumerate() {
        let (_, _, blob) = bytes(anon(&b, &format!("{}/b/{}", link.path, item["cid"].as_str().unwrap()), &[]).await).await;
        let (ok, plain) = node_decrypt(&dir.0, &blob, &link.key_hex);
        let size = [10usize, 65_536, 65_537, 400_000, 0][i];
        assert!(ok, "{}", item["name"]);
        assert_eq!(hex::encode(Sha256::digest(&plain)), hex::encode(Sha256::digest(sample(size, size as u64 + 1))), "{} : octets identiques", item["name"]);
        if i == 3 {
            // Altération d'un octet, mauvaise clé, troncature : refusés par le navigateur aussi.
            let mut bad = blob.clone();
            bad[100_000] ^= 1;
            assert!(!node_decrypt(&dir.0, &bad, &link.key_hex).0, "octet modifié");
            assert!(!node_decrypt(&dir.0, &blob, &"ab".repeat(32)).0, "mauvaise clé");
            assert!(!node_decrypt(&dir.0, &blob[..blob.len() - (65_536 + 16)], &link.key_hex).0, "dernier chunk retiré : flux tronqué détecté");
            assert!(!node_decrypt(&dir.0, &blob, "pas-une-cle").0);
        }
    }
}
