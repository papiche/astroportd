//! CalDAV (RFC 4791) : l'agenda de chaque compte, synchronisable avec « Agenda » d'Android (DAVx⁵), d'iOS et Thunderbird.
//!
//! Même principe que CardDAV : les événements sont de simples fichiers `.ics` du dossier `/Calendrier` du uCloud (chiffrés
//! UENC, répliqués chez les amis avec le manifeste, comptés dans le quota, visibles aussi d'un client WebDAV ordinaire).
//! Ils restent **privés** : rien n'est publié sur Nostr. Ce module n'est qu'une façade de protocole.
//!
//! | URL (sous le montage) | rôle |
//! |-----------------------|------|
//! | `/principals/<login>/` | principal : `calendar-home-set` (voir `carddav`) |
//! | `/calendars/<login>/` | dossier d'accueil des agendas |
//! | `/calendars/<login>/default/` | l'agenda (`getctag`, `sync-token`, rapports `calendar-multiget`, `calendar-query`, `sync-collection`) |
//! | `/calendars/<login>/default/<nom>.ics` | un objet ↔ `/Calendrier/<nom>.ics` |
//!
//! Limites assumées : un seul agenda par compte ; `calendar-query` renvoie tout l'agenda sans appliquer le filtre de dates
//! ni développer les récurrences (le client filtre et calcule) ; pas d'invitations (`schedule-*`) ; pas de `MKCALENDAR`.

use std::collections::BTreeMap;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;

use crate::blob::BlobStore;
use crate::carddav::{self, XML, login_of, response};
use crate::ical;
use crate::server::{Auth, DavState, now_secs, reply};
use crate::vfs::{self, Entry, EntryKind};
use crate::xml::{self, escape};

/// Dossier du uCloud qui porte l'agenda.
pub const CALENDAR_DIR: &str = "/Calendrier";
const NS: &str = "xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\" xmlns:CS=\"http://calendarserver.org/ns/\"";

/// Le chemin (hors montage) relève-t-il de CalDAV ?
pub(crate) fn is_caldav_path(path: &str) -> bool {
    path == "/calendars" || path.strip_prefix("/calendars").is_some_and(|r| r.starts_with('/'))
}

enum Target {
    Root,
    Home(String),
    Calendar(String),
    Object(String, String),
}

fn target(path: &str) -> Option<Target> {
    let seg: Vec<&str> = path.trim_matches('/').split('/').collect();
    Some(match seg.as_slice() {
        ["calendars"] => Target::Root,
        ["calendars", login] => Target::Home((*login).to_owned()),
        ["calendars", login, "default"] => Target::Calendar((*login).to_owned()),
        ["calendars", login, "default", file] => Target::Object((*login).to_owned(), (*file).to_owned()),
        _ => return None,
    })
}

fn is_object(e: &Entry) -> bool {
    e.kind == EntryKind::File && e.path.to_ascii_lowercase().ends_with(".ics")
}

/// Objets `.ics` du dossier `/Calendrier` d'un compte.
pub async fn list_objects<B: BlobStore>(st: &DavState<B>, owner: &str) -> Vec<Entry> {
    let o = owner.to_owned();
    let listed = st.db.readers().read(move |c| Ok(vfs::list_children(c, &o, CALENDAR_DIR).unwrap_or_default())).await.unwrap_or_default();
    listed.into_iter().filter(is_object).collect()
}

fn etag(e: &Entry) -> String {
    format!("\"{}\"", e.cid.as_deref().unwrap_or(""))
}

pub(crate) fn home_href(mount: &str, login: &str) -> String {
    xml::href(mount, &format!("/calendars/{login}"), true)
}

fn href(mount: &str, login: &str, file: Option<&str>) -> String {
    match file {
        Some(f) => xml::href(mount, &format!("/calendars/{login}/default/{f}"), false),
        None => xml::href(mount, &format!("/calendars/{login}/default"), true),
    }
}

fn multistatus(parts: &[String]) -> Response {
    (StatusCode::MULTI_STATUS, XML, format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus {NS}>\n{}</D:multistatus>\n", parts.concat())).into_response()
}

fn not_found(h: &str) -> String {
    format!("<D:response><D:href>{}</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>\n", escape(h))
}

fn object_props(e: &Entry, data: Option<&str>) -> String {
    let mut p = format!(
        "<D:resourcetype/><D:displayname>{}</D:displayname><D:getetag>{}</D:getetag><D:getcontenttype>text/calendar; charset=utf-8</D:getcontenttype><D:getcontentlength>{}</D:getcontentlength><D:getlastmodified>{}</D:getlastmodified>",
        escape(vfs::name_of(&e.path)),
        etag(e),
        e.size,
        xml::http_date(e.mtime)
    );
    if let Some(d) = data {
        // `\r` doit survivre : `escape` retire les caractères de contrôle.
        p.push_str(&format!("<C:calendar-data>{}</C:calendar-data>", escape(&d.replace('\r', "\u{e000}")).replace('\u{e000}', "&#13;")));
    }
    p
}

fn home(mount: &str, login: &str) -> String {
    response(
        &home_href(mount, login),
        &format!("<D:resourcetype><D:collection/></D:resourcetype><D:displayname>{}</D:displayname><D:current-user-principal><D:href>{}</D:href></D:current-user-principal>", escape(login), escape(&carddav::principal_href(mount, login))),
    )
}

fn calendar(mount: &str, login: &str, ctag: &str, token: i64) -> String {
    response(
        &href(mount, login, None),
        &format!(
            "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype><D:displayname>Agenda</D:displayname><CS:getctag>{ctag}</CS:getctag><D:sync-token>{prefix}{token}</D:sync-token>\
<D:owner><D:href>{p}</D:href></D:owner><C:max-resource-size>{max}</C:max-resource-size>\
<C:supported-calendar-component-set><C:comp name=\"VEVENT\"/><C:comp name=\"VTODO\"/><C:comp name=\"VJOURNAL\"/></C:supported-calendar-component-set>\
<C:supported-calendar-data><C:calendar-data content-type=\"text/calendar\" version=\"2.0\"/></C:supported-calendar-data>\
<D:supported-report-set><D:supported-report><D:report><C:calendar-multiget/></D:report></D:supported-report><D:supported-report><D:report><C:calendar-query/></D:report></D:supported-report><D:supported-report><D:report><D:sync-collection/></D:report></D:supported-report></D:supported-report-set>\
<D:current-user-privilege-set><D:privilege><D:all/></D:privilege></D:current-user-privilege-set>",
            prefix = carddav::TOKEN_PREFIX,
            p = escape(&carddav::principal_href(mount, login)),
            max = ical::MAX_OBJECT
        ),
    )
}

async fn find<B: BlobStore>(st: &DavState<B>, owner: &str, file: &str) -> Option<Entry> {
    let (o, p) = (owner.to_owned(), format!("{CALENDAR_DIR}/{file}"));
    st.db.readers().read(move |c| Ok(vfs::get_entry(c, &o, &p).ok().flatten())).await.ok().flatten().filter(is_object)
}

/// Point d'entrée : une requête sous `/calendars`, déjà authentifiée.
pub(crate) async fn handle<B: BlobStore>(st: &DavState<B>, auth: &Auth, method: &str, path: &str, req: Request) -> Response {
    let owner = auth.account.owner.clone();
    let login = login_of(&st.db, &owner).await;
    let Some(t) = target(path) else { return reply(StatusCode::NOT_FOUND, "introuvable") };
    // Jamais l'agenda d'un autre : le segment d'URL doit être l'identifiant du compte connecté.
    if let Target::Home(l) | Target::Calendar(l) | Target::Object(l, _) = &t {
        if !percent_decode_str(l).decode_utf8_lossy().eq_ignore_ascii_case(&login) {
            return reply(StatusCode::FORBIDDEN, "cet agenda n'est pas le vôtre");
        }
    }
    let mount = st.config.mount.clone();
    let depth = req.headers().get("depth").and_then(|v| v.to_str().ok()).map_or("1", str::trim).to_owned();
    match (method, &t) {
        ("PROPFIND", Target::Root) => {
            let mut parts = vec![response(&xml::href(&mount, "/calendars", true), "<D:resourcetype><D:collection/></D:resourcetype>")];
            if depth != "0" {
                parts.push(home(&mount, &login));
            }
            multistatus(&parts)
        }
        ("PROPFIND", Target::Home(_)) => {
            let mut parts = vec![home(&mount, &login)];
            if depth != "0" {
                let head = carddav::journal_head(st).await;
                parts.push(calendar(&mount, &login, &carddav::ctag(&list_objects(st, &owner).await), head));
            }
            multistatus(&parts)
        }
        ("PROPFIND", Target::Calendar(_)) => {
            let head = carddav::journal_head(st).await;
            let all = list_objects(st, &owner).await;
            let mut parts = vec![calendar(&mount, &login, &carddav::ctag(&all), head)];
            if depth != "0" {
                parts.extend(all.iter().map(|e| response(&href(&mount, &login, Some(vfs::name_of(&e.path))), &object_props(e, None))));
            }
            multistatus(&parts)
        }
        ("PROPFIND", Target::Object(_, file)) => {
            let Some(e) = find(st, &owner, file).await else { return reply(StatusCode::NOT_FOUND, "objet introuvable") };
            multistatus(&[response(&href(&mount, &login, Some(file)), &object_props(&e, None))])
        }
        ("REPORT", Target::Calendar(_)) => report(st, &owner, &login, req).await,
        ("GET" | "HEAD", Target::Object(_, file)) => crate::server::get(st, auth, &format!("{CALENDAR_DIR}/{file}"), req.headers(), method == "HEAD").await,
        ("PUT", Target::Object(_, file)) => put_object(st, auth, file, req).await,
        ("DELETE", Target::Object(_, file)) => {
            let response = crate::server::delete(st, auth, &format!("{CALENDAR_DIR}/{file}")).await;
            if response.status().is_success() {
                if let Some(p) = &st.publisher {
                    p.notify(&owner);
                }
            }
            response
        }
        ("PROPPATCH" | "MKCOL" | "MKCALENDAR" | "MOVE" | "COPY" | "DELETE", _) => reply(StatusCode::FORBIDDEN, "l'agenda est géré par la station"),
        _ => reply(StatusCode::METHOD_NOT_ALLOWED, "méthode non supportée ici"),
    }
}

async fn report<B: BlobStore>(st: &DavState<B>, owner: &str, login: &str, req: Request) -> Response {
    let Ok(body) = axum::body::to_bytes(req.into_body(), crate::parse::MAX_XML).await else { return reply(StatusCode::PAYLOAD_TOO_LARGE, "corps trop volumineux") };
    let Ok((kind, hrefs, token)) = carddav::parse_report(&body) else { return reply(StatusCode::BAD_REQUEST, "XML mal formé") };
    if kind == "sync-collection" {
        return sync_collection(st, owner, login, &token).await;
    }
    let mount = st.config.mount.clone();
    let all = list_objects(st, owner).await;
    let by_name: BTreeMap<&str, &Entry> = all.iter().map(|e| (vfs::name_of(&e.path), e)).collect();
    let mut parts = Vec::new();
    match kind.as_str() {
        // Le filtre (`comp-filter`, `time-range`) n'est pas appliqué : le client trie et développe les récurrences.
        "calendar-query" => {
            for e in &all {
                let data = carddav::read_file(st, owner, &e.path).await.and_then(|b| String::from_utf8(b).ok());
                parts.push(response(&href(&mount, login, Some(vfs::name_of(&e.path))), &object_props(e, data.as_deref())));
            }
        }
        "calendar-multiget" => {
            for h in hrefs.iter().take(1000) {
                let decoded = percent_decode_str(h.rsplit('/').next().unwrap_or("")).decode_utf8_lossy().into_owned();
                match by_name.get(decoded.as_str()) {
                    Some(e) => {
                        let data = carddav::read_file(st, owner, &e.path).await.and_then(|b| String::from_utf8(b).ok());
                        parts.push(response(&href(&mount, login, Some(&decoded)), &object_props(e, data.as_deref())));
                    }
                    None => parts.push(not_found(h)),
                }
            }
        }
        _ => return reply(StatusCode::FORBIDDEN, "rapport non supporté"),
    }
    multistatus(&parts)
}

/// `sync-collection` : même journal et même jeton que CardDAV, filtré sur `/Calendrier`.
async fn sync_collection<B: BlobStore>(st: &DavState<B>, owner: &str, login: &str, token: &str) -> Response {
    let head = carddav::journal_head(st).await;
    let since = if token.is_empty() {
        None
    } else {
        let Some(n) = token.strip_prefix(carddav::TOKEN_PREFIX).and_then(|n| n.parse::<i64>().ok()) else { return carddav::invalid_token() };
        if !carddav::token_is_valid(st, n, head).await {
            return carddav::invalid_token();
        }
        Some(n)
    };
    let mount = st.config.mount.clone();
    let all = list_objects(st, owner).await;
    let mut parts = Vec::new();
    match since {
        None => parts.extend(all.iter().map(|e| response(&href(&mount, login, Some(vfs::name_of(&e.path))), &object_props(e, None)))),
        Some(n) => {
            let by_path: BTreeMap<&str, &Entry> = all.iter().map(|e| (e.path.as_str(), e)).collect();
            for path in carddav::changed_paths(st, owner, n, head, CALENDAR_DIR).await {
                let h = href(&mount, login, Some(vfs::name_of(&path)));
                parts.push(match by_path.get(path.as_str()) {
                    Some(e) => response(&h, &object_props(e, None)),
                    None => not_found(&h),
                });
            }
        }
    }
    parts.push(format!("<D:sync-token>{}{head}</D:sync-token>\n", carddav::TOKEN_PREFIX));
    multistatus(&parts)
}

async fn put_object<B: BlobStore>(st: &DavState<B>, auth: &Auth, file: &str, req: Request) -> Response {
    let owner = auth.account.owner.clone();
    let decoded = percent_decode_str(file).decode_utf8_lossy().into_owned();
    if !decoded.to_ascii_lowercase().ends_with(".ics") || crate::ingest::sanitize_name(&decoded).as_deref() != Some(decoded.as_str()) {
        return reply(StatusCode::BAD_REQUEST, "nom d'objet attendu : <nom>.ics, sans séparateur ni caractère de contrôle");
    }
    let headers = req.headers().clone();
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), ical::MAX_OBJECT + 1).await else { return reply(StatusCode::PAYLOAD_TOO_LARGE, "objet trop volumineux (512 Ko au plus)") };
    if let Err(e) = ical::parse(&bytes) {
        let body = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:error xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\"><C:valid-calendar-data/><D:responsedescription>{}</D:responsedescription></D:error>\n", escape(&e.to_string()));
        return (StatusCode::FORBIDDEN, XML, body).into_response();
    }
    let existing = find(st, &owner, &decoded).await;
    let cond = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).map(str::to_owned);
    if cond("if-none-match").as_deref() == Some("*") && existing.is_some() {
        return reply(StatusCode::PRECONDITION_FAILED, "l'objet existe déjà");
    }
    if let Some(wanted) = cond("if-match") {
        if wanted != "*" && existing.as_ref().is_none_or(|e| etag(e) != wanted) {
            return reply(StatusCode::PRECONDITION_FAILED, "l'objet a changé depuis votre lecture");
        }
    }
    let o = owner.clone();
    if let Err(e) = vfs::vfs_tx(&st.db, move |tx| match vfs::mkdir_tx(tx, &o, CALENDAR_DIR, now_secs()) {
        Ok(()) | Err(vfs::VfsError::Exists) => Ok(()),
        Err(e) => Err(e),
    })
    .await
    {
        return reply(StatusCode::INTERNAL_SERVER_ERROR, &format!("dossier de l'agenda : {e}"));
    }
    let Ok(request) = Request::builder().method("PUT").uri("/").header(header::CONTENT_TYPE, "text/calendar; charset=utf-8").header(header::CONTENT_LENGTH, bytes.len()).body(Body::from(bytes)) else {
        return reply(StatusCode::INTERNAL_SERVER_ERROR, "requête interne invalide");
    };
    let mut response = crate::server::put(st, auth, &format!("{CALENDAR_DIR}/{decoded}"), request).await;
    if response.status().is_success() {
        if let Some(e) = find(st, &owner, &decoded).await {
            if let Ok(v) = HeaderValue::from_str(&etag(&e)) {
                response.headers_mut().insert(header::ETAG, v);
            }
        }
        if let Some(p) = &st.publisher {
            p.notify(&owner);
        }
    }
    response
}
