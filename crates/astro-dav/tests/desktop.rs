//! Montage bureau (Explorateur Windows, Finder) : LOCK → PUT → UNLOCK → COPY, PROPPATCH, déduplication.

#![allow(clippy::unwrap_used)]

mod common;

use axum::body::Body;
use common::*;

const LOCKINFO: &str = r#"<?xml version="1.0" encoding="utf-8"?><D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><D:href>PC\fred</D:href></D:owner></D:lockinfo>"#;

fn token_of(r: &axum::http::Response<Body>) -> String {
    r.headers()["lock-token"].to_str().unwrap().trim_matches(|c| c == '<' || c == '>').to_owned()
}

async fn blob_count(b: &Bench) -> usize {
    std::fs::read_dir(b.blobs.root()).unwrap().filter(|e| e.as_ref().unwrap().file_name() != ".tmp").count()
}

#[tokio::test]
async fn windows_style_session_lock_put_unlock_copy() {
    let b = bench("desktop").await;
    let a = alice();
    // 1. LOCK sur une URL inexistante : ressource « lock-null » créée vide (201), jeton rendu.
    let r = call(&b.app, "LOCK", "/dav/rapport.docx", Some(&a), &[("timeout", "Second-300"), ("depth", "0")], Body::from(LOCKINFO)).await;
    assert_eq!(r.status(), 201);
    let token = token_of(&r);
    assert!(token.starts_with("opaquelocktoken:"));
    let (_, xml) = text(r).await;
    assert!(xml.contains("<D:lockdiscovery>") && xml.contains("Second-") && xml.contains(&token) && xml.contains("PC\\fred"), "{xml}");
    let (_, _, empty) = bytes(call(&b.app, "GET", "/dav/rapport.docx", Some(&a), &[], Body::empty()).await).await;
    assert!(empty.is_empty(), "le lock-null est un fichier vide");

    // 2. Autre client sans jeton : 423 sur PUT, DELETE, MOVE, PROPPATCH, nouveau LOCK.
    assert_eq!(call(&b.app, "PUT", "/dav/rapport.docx", Some(&a), &[], Body::from("intrus")).await.status(), 423);
    assert_eq!(call(&b.app, "DELETE", "/dav/rapport.docx", Some(&a), &[], Body::empty()).await.status(), 423);
    assert_eq!(call(&b.app, "MOVE", "/dav/rapport.docx", Some(&a), &[("destination", "/dav/autre.docx")], Body::empty()).await.status(), 423);
    assert_eq!(call(&b.app, "LOCK", "/dav/rapport.docx", Some(&a), &[], Body::from(LOCKINFO)).await.status(), 423);

    // 3. Avec le jeton dans `If` : l'écriture passe (le fichier existait : 204).
    let if_hdr = format!("(<{token}>)");
    assert_eq!(call(&b.app, "PUT", "/dav/rapport.docx", Some(&a), &[("if", &if_hdr)], streamed(sample(150_000, 1), 40_000)).await.status(), 204);

    // 4. PROPFIND expose lockdiscovery et supportedlock ; PROPPATCH pose les attributs Windows.
    let (_, xml) = text(call(&b.app, "PROPFIND", "/dav/rapport.docx", Some(&a), &[("depth", "0")], Body::empty()).await).await;
    assert!(xml.contains("<D:supportedlock>") && xml.contains(&token), "{xml}");
    let patch = r#"<?xml version="1.0"?><D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:schemas-microsoft-com:"><D:set><D:prop><Z:Win32FileAttributes>00000020</Z:Win32FileAttributes><Z:Win32LastModifiedTime>Thu, 05 Nov 2026 10:00:00 GMT</Z:Win32LastModifiedTime></D:prop></D:set></D:propertyupdate>"#;
    assert_eq!(call(&b.app, "PROPPATCH", "/dav/rapport.docx", Some(&a), &[], Body::from(patch)).await.status(), 423, "verrouillé : jeton requis");
    let r = call(&b.app, "PROPPATCH", "/dav/rapport.docx", Some(&a), &[("if", &if_hdr)], Body::from(patch)).await;
    let (status, xml) = text(r).await;
    assert_eq!(status, 207);
    assert_eq!(xml.matches("HTTP/1.1 200 OK").count(), 2, "{xml}");
    let (_, xml) = text(call(&b.app, "PROPFIND", "/dav/rapport.docx", Some(&a), &[("depth", "0")], Body::empty()).await).await;
    assert!(xml.contains(">00000020</p0:Win32FileAttributes>") || xml.contains(">00000020</p1:Win32FileAttributes>"), "{xml}");

    // 5. Prolonger le verrou (LOCK sans corps + If), puis UNLOCK.
    let r = call(&b.app, "LOCK", "/dav/rapport.docx", Some(&a), &[("if", &if_hdr), ("timeout", "Second-900")], Body::empty()).await;
    assert_eq!(r.status(), 200);
    assert_eq!(call(&b.app, "UNLOCK", "/dav/rapport.docx", Some(&a), &[("lock-token", "<opaquelocktoken:00000000-0000-4000-8000-000000000000>")], Body::empty()).await.status(), 409);
    assert_eq!(call(&b.app, "UNLOCK", "/dav/rapport.docx", Some(&a), &[], Body::empty()).await.status(), 400);
    assert_eq!(call(&b.app, "UNLOCK", "/dav/rapport.docx", Some(&a), &[("lock-token", &format!("<{token}>"))], Body::empty()).await.status(), 204);
    assert_eq!(call(&b.app, "UNLOCK", "/dav/rapport.docx", Some(&a), &[("lock-token", &format!("<{token}>"))], Body::empty()).await.status(), 409, "déjà levé");
    assert_eq!(call(&b.app, "PUT", "/dav/rapport.docx", Some(&a), &[], Body::from(sample(1000, 2))).await.status(), 204, "plus de verrou : libre");
    assert_eq!(call(&b.app, "PUT", "/dav/rapport.docx", Some(&a), &[], streamed(sample(150_000, 1), 40_000)).await.status(), 204);

    // 6. COPY : même CID, même clé, aucun blob de plus, aucun ré-encodage.
    let before = blob_count(&b).await;
    assert_eq!(call(&b.app, "COPY", "/dav/rapport.docx", Some(&a), &[("destination", "http://localhost/dav/copie.docx")], Body::empty()).await.status(), 201);
    assert_eq!(blob_count(&b).await, before, "déduplication instantanée");
    let (cids, keys): (Vec<String>, i64) = b
        .db
        .readers()
        .read(|c| Ok((c.prepare("SELECT cid FROM ucloud_entries WHERE path IN ('/rapport.docx','/copie.docx')")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?, c.query_row("SELECT count(*) FROM ucloud_keyring", [], |r| r.get(0))?)))
        .await
        .unwrap();
    assert_eq!(cids.len(), 2);
    assert_eq!(cids[0], cids[1]);
    assert_eq!(keys, 1, "une seule clé pour les deux entrées");
    let (_, _, original) = bytes(call(&b.app, "GET", "/dav/rapport.docx", Some(&a), &[], Body::empty()).await).await;
    let (_, _, copy) = bytes(call(&b.app, "GET", "/dav/copie.docx", Some(&a), &[], Body::empty()).await).await;
    assert_eq!(original, copy);
    assert_eq!(original, sample(150_000, 1));
    let (_, headers, part) = bytes(call(&b.app, "GET", "/dav/copie.docx", Some(&a), &[("range", "bytes=65530-65545")], Body::empty()).await).await;
    assert_eq!((part.as_slice(), headers["content-range"].to_str().unwrap()), (&original[65_530..65_546], "bytes 65530-65545/150000"));

    // 7. Supprimer l'original ne détruit pas le blob ni la clé tant que la copie existe.
    assert_eq!(call(&b.app, "DELETE", "/dav/rapport.docx", Some(&a), &[], Body::empty()).await.status(), 204);
    assert_eq!(blob_count(&b).await, before);
    let (_, _, still) = bytes(call(&b.app, "GET", "/dav/copie.docx", Some(&a), &[], Body::empty()).await).await;
    assert_eq!(still, original);
    assert_eq!(call(&b.app, "DELETE", "/dav/copie.docx", Some(&a), &[], Body::empty()).await.status(), 204);
    assert_eq!(blob_count(&b).await, 0, "dernier référent supprimé : blob et clé partent");
    let keys: i64 = b.db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM ucloud_keyring", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(keys, 0);
}

#[tokio::test]
async fn copy_semantics_overwrite_depth_and_quota() {
    let b = bench_with("copy", |c| c.quota_multipass = 1000).await;
    let a = alice();
    let go = |m: &'static str, p: &'static str, extra: &'static [(&'static str, &'static str)], body: Vec<u8>| {
        let (app, a) = (b.app.clone(), a.clone());
        async move { call(&app, m, p, Some(&a), extra, Body::from(body)).await.status().as_u16() }
    };
    assert_eq!(go("MKCOL", "/dav/d", &[], vec![]).await, 201);
    assert_eq!(go("MKCOL", "/dav/d/sub", &[], vec![]).await, 201);
    assert_eq!(go("PUT", "/dav/d/a.bin", &[], vec![1; 200]).await, 201);
    assert_eq!(go("PUT", "/dav/d/sub/b.bin", &[], vec![2; 100]).await, 201);
    // Copie récursive (Depth par défaut = infinity).
    assert_eq!(go("COPY", "/dav/d", &[("destination", "/dav/e")], vec![]).await, 201);
    let (_, xml) = text(call(&b.app, "PROPFIND", "/dav/e", Some(&a), &[("depth", "infinity")], Body::empty()).await).await;
    assert_eq!(xml.matches("<D:response>").count(), 4, "e, e/sub, e/a.bin, e/sub/b.bin : {xml}");
    // Depth: 0 → le dossier seul.
    assert_eq!(go("COPY", "/dav/d", &[("destination", "/dav/vide"), ("depth", "0")], vec![]).await, 201);
    let (_, xml) = text(call(&b.app, "PROPFIND", "/dav/vide", Some(&a), &[("depth", "infinity")], Body::empty()).await).await;
    assert_eq!(xml.matches("<D:response>").count(), 1);
    // Existe déjà : Overwrite F → 412 ; sinon remplacement (204).
    assert_eq!(go("COPY", "/dav/d/a.bin", &[("destination", "/dav/e/a.bin"), ("overwrite", "F")], vec![]).await, 412);
    assert_eq!(go("COPY", "/dav/d/a.bin", &[("destination", "/dav/e/a.bin")], vec![]).await, 204);
    // Cas interdits.
    assert_eq!(go("COPY", "/dav/d", &[("destination", "/dav/d/dedans")], vec![]).await, 403);
    assert_eq!(go("COPY", "/dav/d", &[("destination", "/dav/d")], vec![]).await, 403);
    assert_eq!(go("COPY", "/dav/absent", &[("destination", "/dav/x")], vec![]).await, 404);
    assert_eq!(go("COPY", "/dav/d/a.bin", &[("destination", "/dav/nope/a.bin")], vec![]).await, 409);
    assert_eq!(go("COPY", "/dav/d/a.bin", &[("destination", "/hors/montage")], vec![]).await, 502);
    // Quota logique (chaque entrée compte sa taille, même dédupliquée) : d (300) + e (300) = 600 ;
    // une troisième copie (900) passe, la suivante (1200 > 1000) échoue.
    assert_eq!(go("COPY", "/dav/d", &[("destination", "/dav/f")], vec![]).await, 201);
    assert_eq!(go("COPY", "/dav/d", &[("destination", "/dav/g")], vec![]).await, 507);
    assert_eq!(go("COPY", "/dav/d", &[("destination", "/dav/h")], vec![]).await, 507);
    // Supprimer une copie garde les blobs partagés.
    assert_eq!(go("DELETE", "/dav/e", &[], vec![]).await, 204);
    let (status, _, body) = bytes(call(&b.app, "GET", "/dav/d/a.bin", Some(&a), &[], Body::empty()).await).await;
    assert_eq!((status, body), (200, vec![1u8; 200]));
}

#[tokio::test]
async fn locks_on_collections_expiry_and_proppatch_rules() {
    let b = bench("locks").await;
    let a = alice();
    assert_eq!(call(&b.app, "MKCOL", "/dav/Projets", Some(&a), &[], Body::empty()).await.status(), 201);
    // Verrou de profondeur infinie sur un dossier : bloque toute la descendance.
    let r = call(&b.app, "LOCK", "/dav/Projets", Some(&a), &[("depth", "infinity")], Body::from(LOCKINFO)).await;
    assert_eq!(r.status(), 200);
    let token = token_of(&r);
    for (m, p, extra) in [("PUT", "/dav/Projets/x.txt", vec![]), ("MKCOL", "/dav/Projets/s", vec![]), ("DELETE", "/dav/Projets", vec![])] {
        let extra: Vec<(&str, &str)> = extra;
        assert_eq!(call(&b.app, m, p, Some(&a), &extra, Body::from("x")).await.status(), 423, "{m} {p}");
    }
    assert_eq!(call(&b.app, "PUT", "/dav/ailleurs.txt", Some(&a), &[], Body::from("ok")).await.status(), 201, "hors du verrou");
    assert_eq!(call(&b.app, "MOVE", "/dav/ailleurs.txt", Some(&a), &[("destination", "/dav/Projets/ailleurs.txt")], Body::empty()).await.status(), 423, "destination sous verrou");
    assert_eq!(call(&b.app, "PUT", "/dav/Projets/x.txt", Some(&a), &[("if", &format!("</dav/Projets> (<{token}>)"))], Body::from("x")).await.status(), 201);
    // Un verrou partagé n'est pas supporté ; un lockinfo invalide est refusé.
    let shared = r#"<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:shared/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockinfo>"#;
    assert_eq!(call(&b.app, "LOCK", "/dav/libre.txt", Some(&a), &[], Body::from(shared)).await.status(), 501);
    assert_eq!(call(&b.app, "LOCK", "/dav/libre.txt", Some(&a), &[], Body::from("<pas du xml")).await.status(), 400);
    assert_eq!(call(&b.app, "LOCK", "/dav/nope/libre.txt", Some(&a), &[], Body::from(LOCKINFO)).await.status(), 409, "dossier parent absent");
    // Expiration : un verrou périmé ne bloque plus.
    b.db.writer().execute("UPDATE kv SET value = json_set(value, '$.expires_at', 1) WHERE scope = 'dav_lock'", vec![]).await.unwrap();
    assert_eq!(call(&b.app, "PUT", "/dav/Projets/y.txt", Some(&a), &[], Body::from("y")).await.status(), 201);
    // Verrous isolés par compte.
    let r = call(&b.app, "LOCK", "/dav/priv.txt", Some(&bob()), &[], Body::from(LOCKINFO)).await;
    assert_eq!(r.status(), 201);
    assert_eq!(call(&b.app, "PUT", "/dav/priv.txt", Some(&a), &[], Body::from("alice")).await.status(), 201, "le verrou de Bob ne concerne pas l'espace d'Alice");

    // PROPPATCH : propriétés DAV: protégées, tout ou rien ; racine et inexistant.
    let live = r#"<D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:x:"><D:set><D:prop><D:getcontentlength>1</D:getcontentlength><Z:note>n</Z:note></D:prop></D:set></D:propertyupdate>"#;
    let (status, xml) = text(call(&b.app, "PROPPATCH", "/dav/priv.txt", Some(&a), &[], Body::from(live)).await).await;
    assert_eq!(status, 207);
    assert!(xml.contains("403 Forbidden") && xml.contains("424 Failed Dependency"), "{xml}");
    let (_, xml) = text(call(&b.app, "PROPFIND", "/dav/priv.txt", Some(&a), &[("depth", "0")], Body::empty()).await).await;
    assert!(!xml.contains(">n</p"), "rien n'est appliqué si une propriété est protégée : {xml}");
    assert_eq!(call(&b.app, "PROPPATCH", "/dav/absent.txt", Some(&a), &[], Body::from(live)).await.status(), 404);
    assert_eq!(call(&b.app, "PROPPATCH", "/dav/priv.txt", Some(&a), &[], Body::from("<x")).await.status(), 400);
    // Les propriétés mortes suivent le fichier lors d'un MOVE et sont retirables.
    let set = r#"<D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:x:"><D:set><D:prop><Z:note>vive les nuages</Z:note></D:prop></D:set></D:propertyupdate>"#;
    assert_eq!(call(&b.app, "PROPPATCH", "/dav/priv.txt", Some(&a), &[], Body::from(set)).await.status(), 207);
    assert_eq!(call(&b.app, "MOVE", "/dav/priv.txt", Some(&a), &[("destination", "/dav/renomme.txt")], Body::empty()).await.status(), 201);
    let (_, xml) = text(call(&b.app, "PROPFIND", "/dav/renomme.txt", Some(&a), &[("depth", "0")], Body::empty()).await).await;
    assert!(xml.contains(">vive les nuages</p"), "{xml}");
    let remove = r#"<D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:x:"><D:remove><D:prop><Z:note/></D:prop></D:remove></D:propertyupdate>"#;
    assert_eq!(call(&b.app, "PROPPATCH", "/dav/renomme.txt", Some(&a), &[], Body::from(remove)).await.status(), 207);
    let (_, xml) = text(call(&b.app, "PROPFIND", "/dav/renomme.txt", Some(&a), &[("depth", "0")], Body::empty()).await).await;
    assert!(!xml.contains("vive les nuages"));
}
