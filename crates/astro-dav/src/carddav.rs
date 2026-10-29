//! CardDAV (RFC 6352) : le carnet d'adresses de chaque compte, synchronisable avec « Contacts » d'Android (DAVx⁵) et d'iOS.
//!
//! Les fiches sont de simples fichiers `.vcf` du dossier `/Contacts` du uCloud : chiffrées (UENC), répliquées, comptées
//! dans le quota, et visibles aussi par un client WebDAV ordinaire. Ce module n'est qu'une façade de protocole :
//!
//! | URL (sous le montage) | rôle |
//! |-----------------------|------|
//! | `/principals/<login>/` | principal : `addressbook-home-set` |
//! | `/addressbooks/<login>/` | dossier d'accueil des carnets |
//! | `/addressbooks/<login>/contacts/` | le carnet (`getctag`, rapports `addressbook-multiget` / `addressbook-query`) |
//! | `/addressbooks/<login>/contacts/<nom>.vcf` | une fiche ↔ `/Contacts/<nom>.vcf` |
//!
//! `<login>` est l'email du compte (ou sa clé Nostr s'il n'en a pas) : on n'ouvre jamais le carnet d'un autre.
//! Limites assumées : pas de `sync-collection` (les clients retombent sur ctag + etags), `addressbook-query` renvoie
//! tout le carnet sans appliquer les filtres (le client filtre).

use std::collections::BTreeMap;

use astro_crypto::{UencKey, uenc};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;
use quick_xml::NsReader;
use quick_xml::events::Event;
use sha2::{Digest, Sha256};

use crate::blob::BlobStore;
use crate::server::{Auth, DavState, now_secs, reply};
use crate::vcard;
use crate::vfs::{self, Entry, EntryKind};
use crate::xml::{self, escape};

/// Dossier du uCloud qui porte le carnet.
pub const CONTACTS_DIR: &str = "/Contacts";
const NS: &str = "xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:carddav\" xmlns:CS=\"http://calendarserver.org/ns/\"";
const XML: [(header::HeaderName, &str); 1] = [(header::CONTENT_TYPE, "application/xml; charset=utf-8")];

/// Le chemin (hors montage) relève-t-il de CardDAV ?
pub(crate) fn is_carddav_path(path: &str) -> bool {
    ["/addressbooks", "/principals"].iter().any(|root| path == *root || path.strip_prefix(root).is_some_and(|r| r.starts_with('/')))
}

enum Target {
    PrincipalsRoot,
    Principal(String),
    AddressbooksRoot,
    Home(String),
    Book(String),
    Card(String, String),
}

fn target(path: &str) -> Option<Target> {
    let seg: Vec<&str> = path.trim_matches('/').split('/').collect();
    Some(match seg.as_slice() {
        ["principals"] => Target::PrincipalsRoot,
        ["principals", login] => Target::Principal((*login).to_owned()),
        ["addressbooks"] => Target::AddressbooksRoot,
        ["addressbooks", login] => Target::Home((*login).to_owned()),
        ["addressbooks", login, "contacts"] => Target::Book((*login).to_owned()),
        ["addressbooks", login, "contacts", file] => Target::Card((*login).to_owned(), (*file).to_owned()),
        _ => return None,
    })
}

/// Identifiant de connexion du compte : son email, à défaut sa clé.
pub async fn login_of(db: &astro_db::Db, owner: &str) -> String {
    let o = owner.to_owned();
    let email: Option<String> = db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT email FROM identities WHERE hex = ?1", [o], |r| r.get::<_, Option<String>>(0)))?.flatten())).await.ok().flatten();
    email.unwrap_or_else(|| owner.to_owned())
}

fn is_card(e: &Entry) -> bool {
    e.kind == EntryKind::File && e.path.to_ascii_lowercase().ends_with(".vcf")
}

/// Fiches `.vcf` du dossier `/Contacts` d'un compte.
pub async fn list_cards<B: BlobStore>(st: &DavState<B>, owner: &str) -> Vec<Entry> {
    let o = owner.to_owned();
    let listed = st.db.readers().read(move |c| Ok(vfs::list_children(c, &o, CONTACTS_DIR).unwrap_or_default())).await.unwrap_or_default();
    listed.into_iter().filter(is_card).collect()
}

/// Étiquette de l'état du carnet : change dès qu'une fiche est ajoutée, modifiée ou supprimée.
pub fn ctag(cards: &[Entry]) -> String {
    let mut h = Sha256::new();
    for e in cards {
        h.update(e.path.as_bytes());
        h.update([0]);
        h.update(e.cid.as_deref().unwrap_or("").as_bytes());
        h.update([b'\n']);
    }
    hex::encode(&h.finalize()[..12])
}

fn etag(e: &Entry) -> String {
    format!("\"{}\"", e.cid.as_deref().unwrap_or(""))
}

fn card_name(path: &str) -> &str {
    vfs::name_of(path)
}

fn href(st_mount: &str, login: &str, file: Option<&str>) -> String {
    let path = match file {
        Some(f) => format!("/addressbooks/{login}/contacts/{f}"),
        None => format!("/addressbooks/{login}/contacts"),
    };
    xml::href(st_mount, &path, file.is_none())
}

fn response(href: &str, props: &str) -> String {
    format!("<D:response><D:href>{}</D:href><D:propstat><D:prop>{props}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>\n", escape(href))
}

fn multistatus(parts: &[String]) -> Response {
    (StatusCode::MULTI_STATUS, XML, format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus {NS}>\n{}</D:multistatus>\n", parts.concat())).into_response()
}

fn principal_href(mount: &str, login: &str) -> String {
    xml::href(mount, &format!("/principals/{login}"), true)
}

fn home_href(mount: &str, login: &str) -> String {
    xml::href(mount, &format!("/addressbooks/{login}"), true)
}

fn card_props(e: &Entry, data: Option<&str>) -> String {
    let mut p = format!(
        "<D:resourcetype/><D:displayname>{}</D:displayname><D:getetag>{}</D:getetag><D:getcontenttype>text/vcard; charset=utf-8</D:getcontenttype><D:getcontentlength>{}</D:getcontentlength><D:getlastmodified>{}</D:getlastmodified>",
        escape(card_name(&e.path)),
        etag(e), // le cid ne porte que des caractères sûrs : les guillemets restent tels quels
        e.size,
        xml::http_date(e.mtime)
    );
    if let Some(d) = data {
        // `\r` doit survivre : `escape` retire les caractères de contrôle.
        p.push_str(&format!("<C:address-data>{}</C:address-data>", escape(&d.replace('\r', "\u{e000}")).replace('\u{e000}', "&#13;")));
    }
    p
}

/// Fait connaître le principal dans la réponse `PROPFIND /` : c'est la découverte CardDAV (`current-user-principal`).
pub(crate) async fn announce_principal<B: BlobStore>(st: &DavState<B>, owner: &str, body: String) -> String {
    let login = login_of(&st.db, owner).await;
    let extra = format!("<D:current-user-principal><D:href>{}</D:href></D:current-user-principal>", escape(&principal_href(&st.config.mount, &login)));
    body.replacen("</D:prop>", &format!("{extra}</D:prop>"), 1)
}

/// Contenu en clair d'un fichier du uCloud (déchiffré par la station). `None` : absent ou illisible.
pub async fn read_file<B: BlobStore>(st: &DavState<B>, owner: &str, path: &str) -> Option<Vec<u8>> {
    let (o, p) = (owner.to_owned(), path.to_owned());
    let (entry, key) = st
        .db
        .readers()
        .read(move |c| {
            let entry = vfs::get_entry(c, &o, &p).map_err(|_| astro_db::DbError::Aborted)?;
            let key: Option<String> = match entry.as_ref().and_then(|e| e.cid.clone()) {
                Some(cid) => rusqlite::OptionalExtension::optional(c.query_row("SELECT key_hex FROM ucloud_keyring WHERE owner_hex = ?1 AND cid = ?2", rusqlite::params![o, cid], |r| r.get(0)))?,
                None => None,
            };
            Ok((entry, key))
        })
        .await
        .ok()?;
    let (cid, key) = (entry?.cid?, UencKey::from_hex(&key?).ok()?);
    let (mut reader, _) = st.blobs.open(&cid).await.ok()?;
    let mut out = Vec::new();
    uenc::decrypt_stream(&key, &mut reader, &mut out).await.ok()?;
    Some(out)
}

fn forbidden(msg: &str) -> Response {
    reply(StatusCode::FORBIDDEN, msg)
}

/// Point d'entrée : une requête sous `/principals` ou `/addressbooks`, déjà authentifiée.
pub(crate) async fn handle<B: BlobStore>(st: &DavState<B>, auth: &Auth, method: &str, path: &str, req: Request) -> Response {
    let owner = auth.account.owner.clone();
    let login = login_of(&st.db, &owner).await;
    let Some(t) = target(path) else { return reply(StatusCode::NOT_FOUND, "introuvable") };
    // Jamais le carnet d'un autre : le segment d'URL doit être l'identifiant du compte connecté.
    let segment = match &t {
        Target::Principal(l) | Target::Home(l) | Target::Book(l) | Target::Card(l, _) => Some(l),
        Target::PrincipalsRoot | Target::AddressbooksRoot => None,
    };
    if let Some(l) = segment {
        let decoded = percent_decode_str(l).decode_utf8_lossy();
        if !decoded.eq_ignore_ascii_case(&login) {
            return forbidden("ce carnet d'adresses n'est pas le vôtre");
        }
    }
    let mount = st.config.mount.clone();
    let depth = req.headers().get("depth").and_then(|v| v.to_str().ok()).map_or("1", str::trim).to_owned();
    match (method, &t) {
        ("PROPFIND", Target::PrincipalsRoot) => multistatus(&[response(&xml::href(&mount, "/principals", true), "<D:resourcetype><D:collection/></D:resourcetype>"), if depth == "0" { String::new() } else { principal(&mount, &login) }]),
        ("PROPFIND", Target::Principal(_)) => multistatus(&[principal(&mount, &login)]),
        ("PROPFIND", Target::AddressbooksRoot) => {
            let mut parts = vec![response(&xml::href(&mount, "/addressbooks", true), "<D:resourcetype><D:collection/></D:resourcetype>")];
            if depth != "0" {
                parts.push(home(&mount, &login));
            }
            multistatus(&parts)
        }
        ("PROPFIND", Target::Home(_)) => {
            let mut parts = vec![home(&mount, &login)];
            if depth != "0" {
                parts.push(book(&mount, &login, &ctag(&list_cards(st, &owner).await)));
            }
            multistatus(&parts)
        }
        ("PROPFIND", Target::Book(_)) => {
            let all = list_cards(st, &owner).await;
            let mut parts = vec![book(&mount, &login, &ctag(&all))];
            if depth != "0" {
                parts.extend(all.iter().map(|e| response(&href(&mount, &login, Some(card_name(&e.path))), &card_props(e, None))));
            }
            multistatus(&parts)
        }
        ("PROPFIND", Target::Card(_, file)) => {
            let Some(e) = find(st, &owner, file).await else { return reply(StatusCode::NOT_FOUND, "fiche introuvable") };
            multistatus(&[response(&href(&mount, &login, Some(file)), &card_props(&e, None))])
        }
        ("REPORT", Target::Book(_)) => report(st, &owner, &login, req).await,
        ("GET" | "HEAD", Target::Card(_, file)) => crate::server::get(st, auth, &format!("{CONTACTS_DIR}/{file}"), req.headers(), method == "HEAD").await,
        ("PUT", Target::Card(_, file)) => put_card(st, auth, file, req).await,
        ("DELETE", Target::Card(_, file)) => {
            let response = crate::server::delete(st, auth, &format!("{CONTACTS_DIR}/{file}")).await;
            if response.status().is_success() {
                if let Some(p) = &st.publisher {
                    p.notify(&owner);
                }
            }
            response
        }
        ("PROPPATCH" | "MKCOL" | "MOVE" | "COPY" | "DELETE", _) => forbidden("le carnet d'adresses est géré par la station"),
        _ => reply(StatusCode::METHOD_NOT_ALLOWED, "méthode non supportée ici"),
    }
}

fn principal(mount: &str, login: &str) -> String {
    response(
        &principal_href(mount, login),
        &format!(
            "<D:resourcetype><D:collection/><D:principal/></D:resourcetype><D:displayname>{l}</D:displayname><D:current-user-principal><D:href>{p}</D:href></D:current-user-principal>\
<D:principal-URL><D:href>{p}</D:href></D:principal-URL><C:addressbook-home-set><D:href>{h}</D:href></C:addressbook-home-set>",
            l = escape(login),
            p = escape(&principal_href(mount, login)),
            h = escape(&home_href(mount, login))
        ),
    )
}

fn home(mount: &str, login: &str) -> String {
    response(&home_href(mount, login), &format!("<D:resourcetype><D:collection/></D:resourcetype><D:displayname>{}</D:displayname><D:current-user-principal><D:href>{}</D:href></D:current-user-principal>", escape(login), escape(&principal_href(mount, login))))
}

fn book(mount: &str, login: &str, ctag: &str) -> String {
    response(
        &href(mount, login, None),
        &format!(
            "<D:resourcetype><D:collection/><C:addressbook/></D:resourcetype><D:displayname>Contacts</D:displayname><CS:getctag>{ctag}</CS:getctag>\
<D:owner><D:href>{p}</D:href></D:owner><C:max-resource-size>{max}</C:max-resource-size>\
<C:supported-address-data><C:address-data-type content-type=\"text/vcard\" version=\"3.0\"/><C:address-data-type content-type=\"text/vcard\" version=\"4.0\"/></C:supported-address-data>\
<D:supported-report-set><D:supported-report><D:report><C:addressbook-multiget/></D:report></D:supported-report><D:supported-report><D:report><C:addressbook-query/></D:report></D:supported-report></D:supported-report-set>\
<D:current-user-privilege-set><D:privilege><D:all/></D:privilege></D:current-user-privilege-set>",
            p = escape(&principal_href(mount, login)),
            max = vcard::MAX_CARD
        ),
    )
}

async fn find<B: BlobStore>(st: &DavState<B>, owner: &str, file: &str) -> Option<Entry> {
    let (o, p) = (owner.to_owned(), format!("{CONTACTS_DIR}/{file}"));
    st.db.readers().read(move |c| Ok(vfs::get_entry(c, &o, &p).ok().flatten())).await.ok().flatten().filter(is_card)
}

/// Corps d'un `REPORT` : nature du rapport et `href` demandés (multiget).
fn parse_report(body: &[u8]) -> Result<(String, Vec<String>), ()> {
    let mut reader = NsReader::from_reader(body);
    let (mut root, mut hrefs, mut in_href, mut buf) = (None::<String>, Vec::new(), false, Vec::new());
    loop {
        match reader.read_event_into(&mut buf).map_err(|_| ())? {
            Event::Start(e) | Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if root.is_none() {
                    root = Some(name);
                } else if name == "href" {
                    in_href = true;
                }
            }
            Event::Text(t) if in_href => hrefs.push(t.unescape().map_err(|_| ())?.trim().to_owned()),
            Event::End(e) if e.local_name().as_ref() == b"href" => in_href = false,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok((root.ok_or(())?, hrefs))
}

async fn report<B: BlobStore>(st: &DavState<B>, owner: &str, login: &str, req: Request) -> Response {
    let Ok(body) = axum::body::to_bytes(req.into_body(), crate::parse::MAX_XML).await else { return reply(StatusCode::PAYLOAD_TOO_LARGE, "corps trop volumineux") };
    let Ok((kind, hrefs)) = parse_report(&body) else { return reply(StatusCode::BAD_REQUEST, "XML mal formé") };
    let mount = st.config.mount.clone();
    let all = list_cards(st, owner).await;
    let by_name: BTreeMap<&str, &Entry> = all.iter().map(|e| (card_name(&e.path), e)).collect();
    let mut parts = Vec::new();
    match kind.as_str() {
        "addressbook-query" => {
            for e in &all {
                let data = read_file(st, owner, &e.path).await.and_then(|b| String::from_utf8(b).ok());
                parts.push(response(&href(&mount, login, Some(card_name(&e.path))), &card_props(e, data.as_deref())));
            }
        }
        "addressbook-multiget" => {
            for h in hrefs.iter().take(1000) {
                let decoded = percent_decode_str(h.rsplit('/').next().unwrap_or("")).decode_utf8_lossy().into_owned();
                match by_name.get(decoded.as_str()) {
                    Some(e) => {
                        let data = read_file(st, owner, &e.path).await.and_then(|b| String::from_utf8(b).ok());
                        parts.push(response(&href(&mount, login, Some(&decoded)), &card_props(e, data.as_deref())));
                    }
                    None => parts.push(format!("<D:response><D:href>{}</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>\n", escape(h))),
                }
            }
        }
        // `sync-collection` n'est pas annoncé : le jeton n'est jamais valide, le client refait la synchronisation complète.
        "sync-collection" => return (StatusCode::FORBIDDEN, XML, "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:error xmlns:D=\"DAV:\"><D:valid-sync-token/></D:error>\n".to_owned()).into_response(),
        _ => return reply(StatusCode::FORBIDDEN, "rapport non supporté"),
    }
    multistatus(&parts)
}

async fn put_card<B: BlobStore>(st: &DavState<B>, auth: &Auth, file: &str, req: Request) -> Response {
    let owner = auth.account.owner.clone();
    let decoded = percent_decode_str(file).decode_utf8_lossy().into_owned();
    if !decoded.to_ascii_lowercase().ends_with(".vcf") || crate::ingest::sanitize_name(&decoded).as_deref() != Some(decoded.as_str()) {
        return reply(StatusCode::BAD_REQUEST, "nom de fiche attendu : <nom>.vcf, sans séparateur ni caractère de contrôle");
    }
    let headers = req.headers().clone();
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), vcard::MAX_CARD + 1).await else { return reply(StatusCode::PAYLOAD_TOO_LARGE, "fiche trop volumineuse (256 Ko au plus)") };
    if let Err(e) = vcard::parse(&bytes) {
        let body = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:error xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:carddav\"><C:valid-address-data/><D:responsedescription>{}</D:responsedescription></D:error>\n", escape(&e.to_string()));
        return (StatusCode::FORBIDDEN, XML, body).into_response();
    }
    let existing = find(st, &owner, &decoded).await;
    let cond = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).map(str::to_owned);
    if cond("if-none-match").as_deref() == Some("*") && existing.is_some() {
        return reply(StatusCode::PRECONDITION_FAILED, "la fiche existe déjà");
    }
    if let Some(wanted) = cond("if-match") {
        if wanted != "*" && existing.as_ref().is_none_or(|e| etag(e) != wanted) {
            return reply(StatusCode::PRECONDITION_FAILED, "la fiche a changé depuis votre lecture");
        }
    }
    let o = owner.clone();
    if let Err(e) = vfs::vfs_tx(&st.db, move |tx| match vfs::mkdir_tx(tx, &o, CONTACTS_DIR, now_secs()) {
        Ok(()) | Err(vfs::VfsError::Exists) => Ok(()),
        Err(e) => Err(e),
    })
    .await
    {
        return reply(StatusCode::INTERNAL_SERVER_ERROR, &format!("dossier des contacts : {e}"));
    }
    let Ok(request) = Request::builder().method("PUT").uri("/").header(header::CONTENT_TYPE, "text/vcard; charset=utf-8").header(header::CONTENT_LENGTH, bytes.len()).body(Body::from(bytes)) else {
        return reply(StatusCode::INTERNAL_SERVER_ERROR, "requête interne invalide");
    };
    let mut response = crate::server::put(st, auth, &format!("{CONTACTS_DIR}/{decoded}"), request).await;
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
