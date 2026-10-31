//! CalDAV : découverte, agenda, création/mise à jour/suppression conditionnelles, rapports, `sync-collection`, isolation.

#![allow(clippy::unwrap_used)]

mod common;

use axum::body::Body;
use common::*;

const C: &str = "/dav/calendars/alice@example.org/default";
const CARDS: &str = "/dav/addressbooks/alice@example.org/contacts";

fn event(uid: &str, summary: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261031T100000Z\r\nDTSTART:20261101T100000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
}

async fn put(b: &Bench, path: &str, body: &str, extra: &[(&str, &str)]) -> (u16, axum::http::HeaderMap, Vec<u8>) {
    let mut headers = vec![("content-type", "text/calendar; charset=utf-8")];
    headers.extend_from_slice(extra);
    bytes(call(&b.app, "PUT", path, Some(&alice()), &headers, Body::from(body.to_owned())).await).await
}

async fn report(b: &Bench, body: &str) -> (u16, String) {
    text(call(&b.app, "REPORT", &format!("{C}/"), Some(&alice()), &[], Body::from(body.to_owned())).await).await
}

#[tokio::test]
async fn discovery_leads_from_the_principal_to_the_calendar() {
    let b = bench("caldav-discovery").await;
    let (_, body) = text(call(&b.app, "PROPFIND", "/dav/principals/alice@example.org/", Some(&alice()), &[("depth", "0")], Body::empty()).await).await;
    assert!(body.contains("<A:calendar-home-set><D:href>/dav/calendars/alice%40example.org/</D:href>"), "{body}");
    let (s, body) = text(call(&b.app, "PROPFIND", "/dav/calendars/alice@example.org/", Some(&alice()), &[("depth", "1")], Body::empty()).await).await;
    assert_eq!(s, 207);
    for needle in ["<C:calendar/>", "<CS:getctag>", "<D:sync-token>urn:astro:sync:", "<C:comp name=\"VEVENT\"/>", "calendar-multiget", "<D:sync-collection/>"] {
        assert!(body.contains(needle), "{needle} absent : {body}");
    }
    let r = call(&b.app, "GET", "/.well-known/caldav", None, &[], Body::empty()).await;
    assert_eq!((r.status().as_u16(), r.headers()["location"].to_str().unwrap()), (301, "/dav/"));
    let r = call(&b.app, "OPTIONS", "/dav/", None, &[], Body::empty()).await;
    assert!(r.headers()["dav"].to_str().unwrap().contains("calendar-access"));
}

#[tokio::test]
async fn events_are_created_read_updated_and_deleted_with_etags() {
    let b = bench("caldav-crud").await;
    let url = format!("{C}/e1.ics");
    let (s, h, _) = put(&b, &url, &event("e1", "Café"), &[("if-none-match", "*")]).await;
    assert_eq!(s, 201);
    let etag1 = h["etag"].to_str().unwrap().to_owned();
    assert_eq!(put(&b, &url, &event("e1", "Café"), &[("if-none-match", "*")]).await.0, 412, "déjà là");
    let (s, _, body) = bytes(call(&b.app, "GET", &url, Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!((s, String::from_utf8(body).unwrap()), (200, event("e1", "Café")), "rendu déchiffré, identique");
    let (_, list) = text(call(&b.app, "PROPFIND", &format!("{C}/"), Some(&alice()), &[("depth", "1")], Body::empty()).await).await;
    assert!(list.contains("e1.ics") && list.contains("text/calendar"), "{list}");

    assert_eq!(put(&b, &url, &event("e1", "Café 2"), &[("if-match", "\"autre\"")]).await.0, 412);
    let (s, h, _) = put(&b, &url, &event("e1", "Café 2"), &[("if-match", &etag1)]).await;
    assert_eq!(s, 204);
    assert_ne!(h["etag"].to_str().unwrap(), etag1);

    assert_eq!(call(&b.app, "DELETE", &url, Some(&alice()), &[], Body::empty()).await.status().as_u16(), 204);
    assert_eq!(call(&b.app, "GET", &url, Some(&alice()), &[], Body::empty()).await.status().as_u16(), 404);
    // Le dossier de l'agenda est un dossier du uCloud : un client WebDAV ordinaire le voit.
    assert_eq!(call(&b.app, "PROPFIND", "/dav/Calendrier/", Some(&alice()), &[("depth", "1")], Body::empty()).await.status().as_u16(), 207);
}

#[tokio::test]
async fn reports_return_the_events_with_their_data() {
    let b = bench("caldav-report").await;
    for (n, uid) in [("a.ics", "u1"), ("b é.ics", "u2")] {
        let path = format!("{C}/{}", n.replace(' ', "%20").replace('é', "%C3%A9"));
        assert_eq!(put(&b, &path, &event(uid, &format!("Rendez-vous {uid}")), &[]).await.0, 201);
    }
    let query = "<?xml version=\"1.0\"?><C:calendar-query xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\"><D:prop><D:getetag/><C:calendar-data/></D:prop><C:filter><C:comp-filter name=\"VCALENDAR\"/></C:filter></C:calendar-query>";
    let (s, body) = report(&b, query).await;
    assert_eq!(s, 207);
    assert!(body.contains("SUMMARY:Rendez-vous u1") && body.contains("SUMMARY:Rendez-vous u2") && body.contains("&#13;"), "{body}");
    let multiget = format!("<?xml version=\"1.0\"?><C:calendar-multiget xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\"><D:prop><D:getetag/><C:calendar-data/></D:prop><D:href>{C}/a.ics</D:href><D:href>{C}/absent.ics</D:href></C:calendar-multiget>");
    let (s, body) = report(&b, &multiget).await;
    assert_eq!(s, 207);
    assert!(body.contains("SUMMARY:Rendez-vous u1") && body.contains("404 Not Found") && body.contains("absent.ics"), "{body}");
    assert_eq!(report(&b, "pas du xml <").await.0, 400);
    assert_eq!(report(&b, "<?xml version=\"1.0\"?><D:unknown xmlns:D=\"DAV:\"/>").await.0, 403);
}

fn sync_body(token: &str) -> String {
    format!("<?xml version=\"1.0\"?><D:sync-collection xmlns:D=\"DAV:\"><D:sync-token>{token}</D:sync-token><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>")
}

fn token_of(body: &str) -> String {
    body.split("<D:sync-token>").nth(1).unwrap().split('<').next().unwrap().to_owned()
}

#[tokio::test]
async fn sync_collection_follows_the_calendar_and_ignores_the_address_book() {
    let b = bench("caldav-sync").await;
    assert_eq!(put(&b, &format!("{C}/a.ics"), &event("u1", "A"), &[]).await.0, 201);
    assert_eq!(put(&b, &format!("{C}/b.ics"), &event("u2", "B"), &[]).await.0, 201);
    let (s, first) = report(&b, &sync_body("")).await;
    assert_eq!(s, 207);
    assert!(first.contains("a.ics") && first.contains("b.ics"));
    let t1 = token_of(&first);

    // Une fiche de contact n'apparaît pas dans l'agenda…
    let vcf = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c1\r\nFN:Léa\r\nEND:VCARD\r\n";
    let (s, ..) = bytes(call(&b.app, "PUT", &format!("{CARDS}/lea.vcf"), Some(&alice()), &[("content-type", "text/vcard")], Body::from(vcf)).await).await;
    assert_eq!(s, 201);
    let (_, none) = report(&b, &sync_body(&t1)).await;
    assert_eq!(none.matches("<D:response>").count(), 0, "{none}");
    // … et un événement n'apparaît pas dans le carnet.
    assert_eq!(put(&b, &format!("{C}/c.ics"), &event("u3", "C"), &[]).await.0, 201);
    let (_, cards) = text(call(&b.app, "REPORT", &format!("{CARDS}/"), Some(&alice()), &[], Body::from(sync_body(""))).await).await;
    assert!(cards.contains("lea.vcf") && !cards.contains("c.ics"));
    let book_token = token_of(&cards);
    assert_eq!(put(&b, &format!("{C}/d.ics"), &event("u4", "D"), &[]).await.0, 201);
    let (_, cards) = text(call(&b.app, "REPORT", &format!("{CARDS}/"), Some(&alice()), &[], Body::from(sync_body(&book_token))).await).await;
    assert_eq!(cards.matches("<D:response>").count(), 0, "un .ics ne sort pas dans le carnet : {cards}");

    // Modification de a, suppression de b, création de c et d déjà vues.
    assert_eq!(put(&b, &format!("{C}/a.ics"), &event("u1", "A2"), &[]).await.0, 204);
    assert_eq!(call(&b.app, "DELETE", &format!("{C}/b.ics"), Some(&alice()), &[], Body::empty()).await.status().as_u16(), 204);
    let (_, delta) = report(&b, &sync_body(&t1)).await;
    let gone = delta.split("<D:response>").find(|r| r.contains("b.ics")).unwrap();
    assert!(gone.contains("404 Not Found"), "{delta}");
    assert!(delta.contains("a.ics") && delta.contains("c.ics") && delta.contains("d.ics") && !delta.contains("lea.vcf"), "{delta}");
    let (_, after) = report(&b, &sync_body(&token_of(&delta))).await;
    assert_eq!(after.matches("<D:response>").count(), 0);

    for bad in ["n'importe quoi", "urn:astro:sync:abc", "urn:astro:sync:99999999999"] {
        let (s, body) = report(&b, &sync_body(bad)).await;
        assert_eq!((s, body.contains("valid-sync-token")), (403, true), "{bad}");
    }
}

#[tokio::test]
async fn invalid_objects_and_names_are_refused() {
    let b = bench("caldav-invalid").await;
    for (path, body, expect) in [
        (format!("{C}/x.ics"), "ce n'est pas un agenda".to_owned(), 403),
        (format!("{C}/x.txt"), event("x", "X"), 400),
        (format!("{C}/%2e%2e.ics"), event("x", "X"), 400),
        (format!("{C}/sans-uid.ics"), event("x", "X").replace("UID:x\r\n", ""), 403),
        (format!("{C}/version.ics"), event("x", "X").replace("2.0", "1.0"), 403),
    ] {
        let (s, _, b_) = put(&b, &path, &body, &[]).await;
        assert_eq!(s, expect, "{path} : {}", String::from_utf8_lossy(&b_));
    }
    let (_, _, why) = put(&b, &format!("{C}/x.ics"), "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n", &[]).await;
    assert!(String::from_utf8_lossy(&why).contains("valid-calendar-data"));
    let huge = event("h", &"a".repeat(600 * 1024));
    assert_eq!(put(&b, &format!("{C}/huge.ics"), &huge, &[]).await.0, 413);
    for m in ["MKCALENDAR", "MKCOL", "PROPPATCH"] {
        assert_eq!(call(&b.app, m, &format!("{C}/"), Some(&alice()), &[], Body::empty()).await.status().as_u16(), 403, "{m}");
    }
}

#[tokio::test]
async fn nobody_reaches_someone_elses_calendar() {
    let b = bench("caldav-iso").await;
    assert_eq!(put(&b, &format!("{C}/a.ics"), &event("u1", "A"), &[]).await.0, 201);
    for (m, p) in [("PROPFIND", "/dav/calendars/bob@example.org/default/"), ("REPORT", "/dav/calendars/bob@example.org/default/"), ("PUT", "/dav/calendars/bob@example.org/default/x.ics"), ("GET", "/dav/calendars/bob@example.org/default/a.ics")] {
        let r = call(&b.app, m, p, Some(&alice()), &[], Body::from(event("x", "X"))).await;
        assert_eq!(r.status().as_u16(), 403, "{m} {p}");
    }
    let r = call(&b.app, "PROPFIND", &format!("{C}/"), None, &[], Body::empty()).await;
    assert_eq!(r.status().as_u16(), 401, "sans identifiants");
    let r = call(&b.app, "PROPFIND", &format!("{C}/"), Some(&bob()), &[], Body::empty()).await;
    assert_eq!(r.status().as_u16(), 403, "le dossier d'Alice, avec les identifiants de Bob");
}
