//! CardDAV : découverte, carnet, création/mise à jour/suppression conditionnelles, rapports, isolation entre comptes.

#![allow(clippy::unwrap_used)]

mod common;

use axum::body::Body;
use common::*;

const A: &str = "/dav/addressbooks/alice@example.org/contacts";
const CARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:lea-1\r\nFN:Léa Martin\r\nEMAIL:lea@example.org\r\nEND:VCARD\r\n";

async fn put(b: &Bench, path: &str, auth: &str, body: &str, extra: &[(&str, &str)]) -> (u16, axum::http::HeaderMap, Vec<u8>) {
    let mut headers = vec![("content-type", "text/vcard; charset=utf-8")];
    headers.extend_from_slice(extra);
    bytes(call(&b.app, "PUT", path, Some(auth), &headers, Body::from(body.to_owned())).await).await
}

#[tokio::test]
async fn discovery_leads_from_the_root_to_the_address_book() {
    let b = bench("carddav-discovery").await;
    let (s, body) = text(call(&b.app, "PROPFIND", "/dav/", Some(&alice()), &[("depth", "0")], Body::empty()).await).await;
    assert_eq!(s, 207);
    assert!(body.contains("<D:current-user-principal><D:href>/dav/principals/alice%40example.org/</D:href>"), "{body}");
    let (s, body) = text(call(&b.app, "PROPFIND", "/dav/principals/alice@example.org/", Some(&alice()), &[("depth", "0")], Body::empty()).await).await;
    assert_eq!(s, 207);
    assert!(body.contains("<C:addressbook-home-set><D:href>/dav/addressbooks/alice%40example.org/</D:href>"), "{body}");
    let (s, body) = text(call(&b.app, "PROPFIND", "/dav/addressbooks/alice@example.org/", Some(&alice()), &[("depth", "1")], Body::empty()).await).await;
    assert_eq!(s, 207);
    assert!(body.contains("<C:addressbook/>") && body.contains("<CS:getctag>") && body.contains("addressbook-multiget"), "{body}");
    // Le carnet vide existe virtuellement (le dossier /Contacts n'est créé qu'à la première fiche).
    let (s, body) = text(call(&b.app, "PROPFIND", &format!("{A}/"), Some(&alice()), &[("depth", "1")], Body::empty()).await).await;
    assert_eq!((s, body.matches("<D:response>").count()), (207, 1));
    let r = call(&b.app, "GET", "/.well-known/carddav", None, &[], Body::empty()).await;
    assert_eq!((r.status().as_u16(), r.headers()["location"].to_str().unwrap()), (301, "/dav/"));
    let r = call(&b.app, "OPTIONS", "/dav/", None, &[], Body::empty()).await;
    assert!(r.headers()["dav"].to_str().unwrap().contains("addressbook") && r.headers()["allow"].to_str().unwrap().contains("REPORT"));
}

#[tokio::test]
async fn cards_are_created_listed_read_updated_and_deleted_with_etags() {
    let b = bench("carddav-crud").await;
    let url = format!("{A}/lea.vcf");
    let (s, h, _) = put(&b, &url, &alice(), CARD, &[("if-none-match", "*")]).await;
    assert_eq!(s, 201);
    let etag1 = h["etag"].to_str().unwrap().to_owned();
    assert!(etag1.starts_with("\"baf"), "{etag1}");
    assert_eq!(put(&b, &url, &alice(), CARD, &[("if-none-match", "*")]).await.0, 412, "création seulement");

    let (s, body) = text(call(&b.app, "PROPFIND", &format!("{A}/"), Some(&alice()), &[("depth", "1")], Body::empty()).await).await;
    assert_eq!(s, 207);
    assert!(body.contains("lea.vcf") && body.contains(&etag1) && body.contains("text/vcard"), "{body}");
    let ctag1 = body.split("<CS:getctag>").nth(1).unwrap().split('<').next().unwrap().to_owned();

    // La fiche est un fichier chiffré du uCloud, lisible en clair par son propriétaire.
    let (s, _, data) = bytes(call(&b.app, "GET", &url, Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!((s, String::from_utf8(data).unwrap()), (200, CARD.to_owned()));
    let (s, body) = text(call(&b.app, "GET", "/dav/Contacts/lea.vcf", Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!((s, body), (200, CARD.to_owned()), "visible aussi comme fichier");

    // Mise à jour conditionnelle : If-Match périmé refusé, bon etag accepté, ctag change.
    let updated = CARD.replace("Léa Martin", "Léa Martin-Dupont");
    assert_eq!(put(&b, &url, &alice(), &updated, &[("if-match", "\"autre\"")]).await.0, 412);
    let (s, h, _) = put(&b, &url, &alice(), &updated, &[("if-match", &etag1)]).await;
    assert_eq!(s, 204);
    assert_ne!(h["etag"].to_str().unwrap(), etag1);
    let (_, body) = text(call(&b.app, "PROPFIND", &format!("{A}/"), Some(&alice()), &[("depth", "0")], Body::empty()).await).await;
    assert_ne!(body.split("<CS:getctag>").nth(1).unwrap().split('<').next().unwrap(), ctag1, "le ctag suit les changements");

    assert_eq!(call(&b.app, "DELETE", &url, Some(&alice()), &[], Body::empty()).await.status().as_u16(), 204);
    assert_eq!(call(&b.app, "GET", &url, Some(&alice()), &[], Body::empty()).await.status().as_u16(), 404);
}

#[tokio::test]
async fn reports_return_the_cards_with_their_data() {
    let b = bench("carddav-report").await;
    for (name, uid, fnn) in [("a.vcf", "u1", "Alice A"), ("b é.vcf", "u2", "Bob B")] {
        let card = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{fnn}\r\nEND:VCARD\r\n");
        assert_eq!(put(&b, &format!("{A}/{}", name.replace(' ', "%20").replace('é', "%C3%A9")), &alice(), &card, &[]).await.0, 201);
    }
    let query = "<?xml version=\"1.0\"?><C:addressbook-query xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:carddav\"><D:prop><D:getetag/><C:address-data/></D:prop></C:addressbook-query>";
    let (s, body) = text(call(&b.app, "REPORT", &format!("{A}/"), Some(&alice()), &[("depth", "1")], Body::from(query)).await).await;
    assert_eq!(s, 207);
    assert!(body.contains("FN:Alice A") && body.contains("FN:Bob B") && body.contains("&#13;"), "CRLF conservé : {body}");

    let multiget = format!("<?xml version=\"1.0\"?><C:addressbook-multiget xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:carddav\"><D:prop><D:getetag/><C:address-data/></D:prop><D:href>{A}/a.vcf</D:href><D:href>{A}/b%20%C3%A9.vcf</D:href><D:href>{A}/absent.vcf</D:href></C:addressbook-multiget>");
    let (s, body) = text(call(&b.app, "REPORT", &format!("{A}/"), Some(&alice()), &[], Body::from(multiget)).await).await;
    assert_eq!(s, 207);
    assert!(body.contains("FN:Alice A") && body.contains("FN:Bob B") && body.contains("404 Not Found") && body.contains("absent.vcf"), "{body}");

    assert_eq!(call(&b.app, "REPORT", &format!("{A}/"), Some(&alice()), &[], Body::from("pas du xml <")).await.status().as_u16(), 400);
}

#[tokio::test]
async fn invalid_cards_and_names_are_refused() {
    let b = bench("carddav-invalid").await;
    for (path, body, expect) in [(format!("{A}/x.vcf"), "ce n'est pas une carte", 403), (format!("{A}/x.txt"), CARD, 400), (format!("{A}/%2e%2e.vcf"), CARD, 400), (format!("{A}/sans-nom.vcf"), "BEGIN:VCARD\r\nVERSION:3.0\r\nEND:VCARD\r\n", 403)] {
        let (s, _, b_) = put(&b, &path, &alice(), body, &[]).await;
        assert_eq!(s, expect, "{path} : {}", String::from_utf8_lossy(&b_));
    }
    let (_, _, why) = put(&b, &format!("{A}/x.vcf"), &alice(), "BEGIN:VCARD\r\nVERSION:3.0\r\nEND:VCARD\r\n", &[]).await;
    assert!(String::from_utf8_lossy(&why).contains("valid-address-data"));
    let huge = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nNOTE:{}\r\nEND:VCARD\r\n", "a".repeat(300 * 1024));
    assert_eq!(put(&b, &format!("{A}/huge.vcf"), &alice(), &huge, &[]).await.0, 413);
}

#[tokio::test]
async fn nobody_reaches_someone_elses_address_book() {
    let b = bench("carddav-isolation").await;
    assert_eq!(put(&b, &format!("{A}/lea.vcf"), &alice(), CARD, &[]).await.0, 201);
    for (m, p) in [("PROPFIND", format!("{A}/")), ("GET", format!("{A}/lea.vcf")), ("PUT", format!("{A}/pirate.vcf")), ("DELETE", format!("{A}/lea.vcf")), ("PROPFIND", "/dav/principals/alice@example.org/".to_owned())] {
        let r = call(&b.app, m, &p, Some(&bob()), &[("depth", "1")], Body::from(CARD)).await;
        assert_eq!(r.status().as_u16(), 403, "{m} {p}");
    }
    assert_eq!(call(&b.app, "PROPFIND", &format!("{A}/"), None, &[], Body::empty()).await.status().as_u16(), 401);
    // Bob a son propre carnet, vide.
    let (s, body) = text(call(&b.app, "PROPFIND", "/dav/addressbooks/bob@example.org/contacts/", Some(&bob()), &[("depth", "1")], Body::empty()).await).await;
    assert_eq!((s, body.matches("<D:response>").count()), (207, 1));
    // Pas de gestion du carnet par MKCOL/PROPPATCH/MOVE.
    for m in ["MKCOL", "PROPPATCH", "MOVE", "COPY"] {
        assert_eq!(call(&b.app, m, &format!("{A}/lea.vcf"), Some(&alice()), &[("destination", "/dav/x")], Body::empty()).await.status().as_u16(), 403, "{m}");
    }
}

fn sync_body(token: &str) -> String {
    format!("<?xml version=\"1.0\"?><D:sync-collection xmlns:D=\"DAV:\"><D:sync-token>{token}</D:sync-token><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>")
}

async fn sync(b: &Bench, token: &str) -> (u16, String) {
    text(call(&b.app, "REPORT", &format!("{A}/"), Some(&alice()), &[], Body::from(sync_body(token))).await).await
}

fn token_of(body: &str) -> String {
    body.split("<D:sync-token>").nth(1).unwrap().split('<').next().unwrap().to_owned()
}

#[tokio::test]
async fn sync_collection_reports_only_what_changed_deletions_included() {
    let b = bench("carddav-sync").await;
    let card = |uid: &str, name: &str| format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nEND:VCARD\r\n");
    for (f, uid, n) in [("a.vcf", "u1", "Alice A"), ("b.vcf", "u2", "Bob B")] {
        assert_eq!(put(&b, &format!("{A}/{f}"), &alice(), &card(uid, n), &[]).await.0, 201);
    }
    // Annoncé par le carnet, avec son jeton courant.
    let (_, props) = text(call(&b.app, "PROPFIND", &format!("{A}/"), Some(&alice()), &[("depth", "0")], Body::empty()).await).await;
    assert!(props.contains("<D:sync-collection/>") && props.contains("<D:sync-token>urn:astro:sync:"), "{props}");

    // Première synchronisation : tout le carnet.
    let (s, first) = sync(&b, "").await;
    assert_eq!(s, 207);
    assert!(first.contains("a.vcf") && first.contains("b.vcf") && !first.contains("404"), "{first}");
    let t1 = token_of(&first);
    assert_eq!(props.split("<D:sync-token>").nth(1).unwrap().split('<').next().unwrap(), t1, "le jeton du carnet est celui du rapport");

    // Rien n'a changé : rapport vide, même jeton.
    let (s, none) = sync(&b, &t1).await;
    assert_eq!((s, none.matches("<D:response>").count(), token_of(&none)), (207, 0, t1.clone()));

    // Modification de a, suppression de b, création de c.
    assert_eq!(put(&b, &format!("{A}/a.vcf"), &alice(), &card("u1", "Alice Z"), &[]).await.0, 204);
    assert_eq!(call(&b.app, "DELETE", &format!("{A}/b.vcf"), Some(&alice()), &[], Body::empty()).await.status().as_u16(), 204);
    assert_eq!(put(&b, &format!("{A}/c.vcf"), &alice(), &card("u3", "Cléo C"), &[]).await.0, 201);
    let (s, delta) = sync(&b, &t1).await;
    assert_eq!(s, 207);
    assert_eq!(delta.matches("<D:response>").count(), 3, "{delta}");
    assert!(delta.contains("a.vcf") && delta.contains("c.vcf"), "{delta}");
    let gone = delta.split("<D:response>").find(|r| r.contains("b.vcf")).unwrap();
    assert!(gone.contains("404 Not Found") && !gone.contains("getetag"), "{gone}");
    let t2 = token_of(&delta);
    assert_ne!(t2, t1);
    let (_, after) = sync(&b, &t2).await;
    assert_eq!(after.matches("<D:response>").count(), 0);

    // Jeton illisible, du futur ou d'avant la purge : le client repart de zéro.
    for bad in ["n'importe quoi", "urn:astro:sync:abc", "urn:astro:sync:-1", "urn:astro:sync:99999999999", "urn:astro:sync:"] {
        let (s, body) = sync(&b, bad).await;
        assert_eq!(s, 403, "{bad}");
        assert!(body.contains("valid-sync-token"), "{bad}");
    }
    b.db.writer().execute("INSERT OR REPLACE INTO schema_info(key, value) VALUES ('dav_changes_floor', '3')", vec![]).await.unwrap();
    assert_eq!(sync(&b, "urn:astro:sync:1").await.0, 403, "jeton antérieur à la purge");
    assert_eq!(sync(&b, &t2).await.0, 207, "jeton postérieur au plancher");
}

#[tokio::test]
async fn sync_collection_never_leaks_another_account() {
    let b = bench("carddav-sync-iso").await;
    assert_eq!(put(&b, &format!("{A}/a.vcf"), &alice(), CARD, &[]).await.0, 201);
    let other = "/dav/addressbooks/bob@example.org/contacts/";
    let r = call(&b.app, "REPORT", other, Some(&alice()), &[], Body::from(sync_body(""))).await;
    assert_eq!(r.status().as_u16(), 403);
}
