//! Serveur WebDAV (RFC 4918, classe 1) sous axum : GET, HEAD, PUT, PROPFIND, MKCOL, DELETE, MOVE, OPTIONS.
//!
//! Tous les fichiers sont chiffrés en UENC v2 au vol, avec une clé aléatoire par fichier. LOCK, PROPPATCH et COPY
//! répondent `501`.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use astro_crypto::UencKey;
use astro_crypto::nip98::{Nip98Request, ReplayStore, verify_authorization};
use astro_crypto::uenc::{self, Cipher};
use astro_db::Db;
use axum::Router;
use axum::body::{Body, HttpBody};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::StreamExt;
use percent_encoding::percent_decode_str;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, ReadBuf};
use tokio_util::io::{ReaderStream, StreamReader};
use zeroize::Zeroizing;

use crate::auth::{Account, Tier, account_token, ct_eq, lookup_by_hex, lookup_by_login, parse_basic};
use crate::blob::{BlobStore, BlobWriter, is_cid};
use crate::locks::{self, blocked, parse_timeout, token_of_header, tokens_in_if};
use crate::parse::{MAX_XML, parse_lockinfo, parse_propertyupdate};
use crate::manifest::ManifestPublisher;
use crate::vfs::{self, EntryKind, Limits, NewFile, VfsError, normalize_path, vfs_tx};
use crate::xml;

const GIB: u64 = 1 << 30;

/// Réglages du serveur WebDAV.
pub struct DavConfig {
    /// Préfixe de montage (`/dav`).
    pub mount: String,
    /// URL publique de la station, pour la liaison NIP-98 derrière un reverse proxy (`https://station.example`).
    /// Sans elle, l'URL attendue est reconstruite avec l'en-tête `Host` en `http`.
    pub public_base_url: Option<String>,
    /// Plafond par fichier d'un MULTIPASS.
    pub max_file_multipass: u64,
    /// Plafond par fichier d'une ZenCard.
    pub max_file_zencard: u64,
    /// Quota total d'un MULTIPASS.
    pub quota_multipass: u64,
    /// Quota total d'une ZenCard.
    pub quota_zencard: u64,
    /// Nombre maximal d'entrées d'un `PROPFIND` de profondeur infinie.
    pub propfind_infinity_cap: usize,
    /// Secret de la station dont dérivent les jetons Basic.
    pub station_secret: Zeroizing<Vec<u8>>,
}

impl DavConfig {
    /// Réglages par défaut : 1 Go / 4 Go par fichier, 10 Go / 128 Go de quota (valeurs du legacy).
    pub fn new(station_secret: Vec<u8>) -> Self {
        Self {
            mount: "/dav".into(),
            public_base_url: None,
            max_file_multipass: GIB,
            max_file_zencard: 4 * GIB,
            quota_multipass: 10 * GIB,
            quota_zencard: 128 * GIB,
            propfind_infinity_cap: 10_000,
            station_secret: Zeroizing::new(station_secret),
        }
    }

    /// Limites (taille de fichier, quota total) de la formule d'un compte.
    pub fn limits(&self, tier: Tier) -> Limits {
        match tier {
            Tier::Multipass => Limits { max_file: self.max_file_multipass, quota_total: self.quota_multipass },
            Tier::ZenCard => Limits { max_file: self.max_file_zencard, quota_total: self.quota_zencard },
        }
    }
}

/// État partagé du serveur.
pub struct DavState<B: BlobStore> {
    /// Base `uplanet.db`.
    pub db: Arc<Db>,
    /// Blobs chiffrés.
    pub blobs: Arc<B>,
    /// Anti-rejeu NIP-98.
    pub replay: Arc<dyn ReplayStore>,
    /// Publication du manifeste après chaque modification.
    pub publisher: Option<ManifestPublisher>,
    /// Réglages.
    pub config: DavConfig,
    /// Appelé après chaque fichier déposé avec succès : `(propriétaire, chemin, type MIME)`.
    pub on_upload: Option<UploadHook>,
}

/// Rappel de dépôt (FaceCloud met la photo en file d'attente d'analyse).
pub type UploadHook = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;

impl<B: BlobStore> DavState<B> {
    /// Le compte hébergé ici qui s'authentifie (Basic ou NIP-98), s'il y en a un.
    pub async fn account_of(&self, headers: &HeaderMap, method: &str, uri: &axum::http::Uri) -> Option<crate::auth::Account> {
        authenticate(self, headers, method, uri).await.ok().map(|a| a.account)
    }

    /// La requête porte-t-elle une authentification valide (Basic ou NIP-98) d'un compte hébergé ici ?
    /// Sert de garde aux routes du reverse proxy.
    pub async fn is_authorized(&self, headers: &HeaderMap, method: &str, uri: &axum::http::Uri) -> bool {
        authenticate(self, headers, method, uri).await.is_ok()
    }
}

/// Routeur WebDAV : toutes les méthodes, sous `config.mount`.
pub fn router<B: BlobStore>(state: Arc<DavState<B>>) -> Router {
    Router::new()
        .route("/.well-known/carddav", any(wellknown_carddav::<B>))
        .route("/s/{slug}", axum::routing::get(crate::share::page::<B>))
        .route("/s/{slug}/m", axum::routing::get(crate::share::manifest::<B>))
        .route("/s/{slug}/b/{cid}", axum::routing::get(crate::share::blob::<B>))
        .fallback(any(dispatch::<B>))
        .with_state(state)
}

pub(crate) fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

pub(crate) fn reply(code: StatusCode, msg: &str) -> Response {
    (code, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], msg.to_owned()).into_response()
}

fn unauthorized() -> Response {
    let mut r = reply(StatusCode::UNAUTHORIZED, "authentification requise");
    r.headers_mut().append(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"uCloud\", charset=\"UTF-8\""));
    r.headers_mut().append(header::WWW_AUTHENTICATE, HeaderValue::from_static("Nostr realm=\"uCloud\""));
    r
}

fn vfs_error(e: &VfsError) -> Response {
    let code = match e {
        VfsError::BadPath => StatusCode::BAD_REQUEST,
        VfsError::NotFound => StatusCode::NOT_FOUND,
        VfsError::Exists | VfsError::IsCollection => StatusCode::METHOD_NOT_ALLOWED,
        VfsError::ParentMissing | VfsError::NotCollection => StatusCode::CONFLICT,
        VfsError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        VfsError::QuotaExceeded => StatusCode::INSUFFICIENT_STORAGE,
        VfsError::BadMove => StatusCode::FORBIDDEN,
        VfsError::Db(_) | VfsError::Sqlite(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    reply(code, &match e {
        VfsError::Db(_) | VfsError::Sqlite(_) => "erreur interne".to_owned(),
        other => other.to_string(),
    })
}

/// Authentification établie.
pub(crate) struct Auth {
    pub(crate) account: Account,
    /// Tag `payload` NIP-98 à confronter au corps après un téléversement en flux.
    pub(crate) payload: Option<String>,
}

// L'erreur est la réponse HTTP elle-même, renvoyée telle quelle : la boxer n'apporterait rien.
#[allow(clippy::result_large_err)]
async fn authenticate<B: BlobStore>(st: &DavState<B>, headers: &HeaderMap, method: &str, uri: &axum::http::Uri) -> Result<Auth, Response> {
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else { return Err(unauthorized()) };
    let scheme = value.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    match scheme.as_str() {
        "basic" => {
            let (user, pass) = parse_basic(value).ok_or_else(unauthorized)?;
            let account = lookup_by_login(&st.db, &user).await;
            // Le jeton est calculé même pour un compte inconnu : pas de différence de temps observable.
            let expected = account_token(&st.db, &st.config.station_secret, account.as_ref().map_or("-", |a| a.owner.as_str())).await;
            match account {
                Some(a) if ct_eq(expected.as_bytes(), pass.as_bytes()) => Ok(Auth { account: a, payload: None }),
                _ => Err(unauthorized()),
            }
        }
        "nostr" => {
            let base = match &st.config.public_base_url {
                Some(b) => b.trim_end_matches('/').to_owned(),
                None => format!("http://{}", headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost")),
            };
            let url = format!("{base}{}", uri.path_and_query().map_or("/", |p| p.as_str()));
            let (header_value, method, store) = (value.to_owned(), method.to_owned(), st.replay.clone());
            let verified = tokio::task::spawn_blocking(move || {
                verify_authorization(&header_value, &Nip98Request { method: &method, url: &url, body: None }, now_secs().unsigned_abs(), store.as_ref())
            })
            .await
            .map_err(|_| reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne"))?
            .map_err(|e| {
                let mut r = unauthorized();
                *r.body_mut() = Body::from(format!("NIP-98 refusé : {e}"));
                r
            })?;
            let account = lookup_by_hex(&st.db, &verified.pubkey).await.ok_or_else(|| reply(StatusCode::FORBIDDEN, "compte non hébergé ici"))?;
            Ok(Auth { account, payload: verified.payload })
        }
        _ => Err(unauthorized()),
    }
}

/// `/.well-known/carddav` (RFC 6764) : renvoie vers le montage WebDAV, où `PROPFIND /` annonce le principal.
async fn wellknown_carddav<B: BlobStore>(State(st): State<Arc<DavState<B>>>) -> Response {
    let mount = st.config.mount.trim_end_matches('/');
    let mut r = reply(StatusCode::MOVED_PERMANENTLY, "");
    if let Ok(v) = HeaderValue::from_str(&format!("{mount}/")) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

/// Chemin du VFS d'une URL de requête (hors préfixe de montage).
#[allow(clippy::result_large_err)]
fn vfs_path(mount: &str, url_path: &str) -> Result<String, Response> {
    let decoded = percent_decode_str(url_path).decode_utf8().map_err(|_| reply(StatusCode::BAD_REQUEST, "encodage invalide"))?;
    let mount = mount.trim_end_matches('/');
    let rest = if mount.is_empty() {
        decoded.as_ref()
    } else {
        match decoded.strip_prefix(mount) {
            Some(r) if r.is_empty() || r.starts_with('/') => r,
            _ => return Err(reply(StatusCode::NOT_FOUND, "hors du montage WebDAV")),
        }
    };
    normalize_path(rest).map_err(|e| vfs_error(&e))
}

async fn dispatch<B: BlobStore>(State(st): State<Arc<DavState<B>>>, req: Request) -> Response {
    let method = req.method().as_str().to_owned();
    if method == "OPTIONS" {
        let mut r = reply(StatusCode::OK, "");
        let h = r.headers_mut();
        h.insert("DAV", HeaderValue::from_static("1, 2, addressbook"));
        h.insert("MS-Author-Via", HeaderValue::from_static("DAV"));
        h.insert(header::ALLOW, HeaderValue::from_static("OPTIONS, GET, HEAD, PUT, DELETE, MKCOL, MOVE, COPY, PROPFIND, PROPPATCH, REPORT, LOCK, UNLOCK"));
        return r;
    }
    let path = match vfs_path(&st.config.mount, req.uri().path()) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let auth = match authenticate(&st, req.headers(), &method, req.uri()).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    if crate::carddav::is_carddav_path(&path) {
        return crate::carddav::handle(&st, &auth, &method, &path, req).await;
    }
    let owner = auth.account.owner.clone();
    let has_body = !HttpBody::is_end_stream(req.body());
    let headers = req.headers().clone();
    let dest = if matches!(method.as_str(), "MOVE" | "COPY") {
        match destination(&st, &headers) {
            Ok(d) => Some(d),
            Err(r) => return r,
        }
    } else {
        None
    };
    // Écritures : un verrou actif non présenté dans `If` bloque (423).
    let targets: Vec<(&str, bool)> = match (method.as_str(), dest.as_deref()) {
        ("PUT" | "MKCOL" | "PROPPATCH", _) => vec![(path.as_str(), false)],
        ("DELETE", _) => vec![(path.as_str(), true)],
        ("MOVE", Some(d)) => vec![(path.as_str(), true), (d, true)],
        ("COPY", Some(d)) => vec![(d, true)],
        _ => Vec::new(),
    };
    if !targets.is_empty() {
        if let Some(r) = locked(&st, &owner, &headers, &targets).await {
            return r;
        }
    }
    let response = match method.as_str() {
        "GET" if wants_thumb(req.uri()) => get_thumb(&st, &auth, &path, &headers).await,
        "GET" => get(&st, &auth, &path, &headers, false).await,
        "HEAD" => get(&st, &auth, &path, &headers, true).await,
        "PROPFIND" => propfind(&st, &auth, &path, &headers).await,
        "PUT" => put(&st, &auth, &path, req).await,
        "MKCOL" => mkcol(&st, &auth, &path, has_body).await,
        "DELETE" => delete(&st, &auth, &path).await,
        "MOVE" => move_(&st, &auth, &path, dest.as_deref().unwrap_or("/"), &headers).await,
        "COPY" => copy(&st, &auth, &path, dest.as_deref().unwrap_or("/"), &headers).await,
        "LOCK" => lock(&st, &auth, &path, req).await,
        "UNLOCK" => unlock(&st, &auth, &headers).await,
        "PROPPATCH" => proppatch(&st, &auth, &path, req).await,
        _ => reply(StatusCode::METHOD_NOT_ALLOWED, "méthode non supportée"),
    };
    // Miniature WebP en arrière-plan : le dépôt n'attend jamais le décodage, et un échec ne l'annule pas.
    if method == "PUT" && response.status().is_success() && crate::thumb::is_thumbable(vfs::guess_mime(&path)) {
        let (st, owner, path) = (st.clone(), owner.clone(), path.clone());
        tokio::spawn(async move {
            if let Err(e) = crate::thumb::generate(&st, &owner, &path).await {
                eprintln!("miniature de {path} impossible : {e}");
            }
        });
    }
    let created_by_lock = method == "LOCK" && response.status() == StatusCode::CREATED;
    let changed = matches!(method.as_str(), "PUT" | "MKCOL" | "DELETE" | "MOVE" | "COPY" | "PROPPATCH") || created_by_lock;
    if changed && response.status().is_success() {
        if let Some(p) = &st.publisher {
            p.notify(&owner);
        }
    }
    response
}

/// Un verrou actif que la requête ne présente pas bloque-t-il l'une des cibles ?
async fn locked<B: BlobStore>(st: &DavState<B>, owner: &str, headers: &HeaderMap, targets: &[(&str, bool)]) -> Option<Response> {
    let Ok(active) = locks::active(&st.db, owner, now_secs()).await else { return Some(reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne")) };
    if active.is_empty() {
        return None;
    }
    let presented = tokens_in_if(headers.get("if").and_then(|v| v.to_str().ok()).unwrap_or(""));
    targets.iter().any(|(p, d)| blocked(&active, p, *d, &presented)).then(|| reply(StatusCode::LOCKED, "ressource verrouillée"))
}

/// Chemin du VFS désigné par l'en-tête `Destination`.
#[allow(clippy::result_large_err)]
fn destination<B: BlobStore>(st: &DavState<B>, headers: &HeaderMap) -> Result<String, Response> {
    let Some(dest) = headers.get("destination").and_then(|v| v.to_str().ok()) else { return Err(reply(StatusCode::BAD_REQUEST, "en-tête Destination manquant")) };
    let dest_path = match url::Url::parse(dest) {
        Ok(u) => u.path().to_owned(),
        Err(_) => dest.split(['?', '#']).next().unwrap_or("").to_owned(),
    };
    vfs_path(&st.config.mount, &dest_path).map_err(|_| reply(StatusCode::BAD_GATEWAY, "destination hors du montage"))
}

/// Intervalle d'octets demandé : `Ok(None)` = servir tout, `Err(())` = insatisfaisable.
pub(crate) fn parse_range(value: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = value.trim().strip_prefix("bytes=") else { return Ok(None) };
    if spec.contains(',') || size == 0 {
        return Ok(None); // plusieurs plages : on sert tout, ce que la RFC autorise
    }
    let Some((a, b)) = spec.split_once('-') else { return Ok(None) };
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            (size.saturating_sub(n), size - 1)
        }
        (s, "") => (s.parse::<u64>().map_err(|_| ())?, size - 1),
        (s, e) => (s.parse::<u64>().map_err(|_| ())?, e.parse::<u64>().map_err(|_| ())?.min(size - 1)),
    };
    if start >= size || start > end { Err(()) } else { Ok(Some((start, end))) }
}

/// `?thumb` : la miniature WebP du fichier au lieu de l'original.
fn wants_thumb(uri: &axum::http::Uri) -> bool {
    uri.query().is_some_and(|q| q.split('&').any(|kv| kv == "thumb" || kv.starts_with("thumb=")))
}

/// Sert la miniature : déchiffrée par la station, mise en cache sans limite côté client (son CID change avec le contenu).
async fn get_thumb<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str, headers: &HeaderMap) -> Response {
    let (owner, p) = (auth.account.owner.clone(), path.to_owned());
    let found = st
        .db
        .readers()
        .read(move |c| {
            let row: Option<(Option<String>, Option<String>)> = rusqlite::OptionalExtension::optional(c.query_row(
                "SELECT json_extract(data, '$.thumb.cid'), (SELECT key_hex FROM ucloud_keyring k WHERE k.owner_hex = e.owner_hex AND k.cid = json_extract(e.data, '$.thumb.cid')) \
                 FROM ucloud_entries e WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'",
                rusqlite::params![owner, p],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ))?;
            Ok(row)
        })
        .await;
    let Ok(Some((Some(cid), key))) = found else { return reply(StatusCode::NOT_FOUND, "pas de miniature") };
    let etag = format!("\"{cid}\"");
    let cache = [(header::ETAG, etag.clone()), (header::CACHE_CONTROL, "private, max-age=31536000, immutable".to_owned())];
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == etag)) {
        return (StatusCode::NOT_MODIFIED, cache).into_response();
    }
    let (Some(key_hex), true) = (key, is_cid(&cid)) else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "clé de la miniature absente") };
    let Ok(key) = UencKey::from_hex(&key_hex) else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "clé de la miniature invalide") };
    let Ok((mut reader, _)) = st.blobs.open(&cid).await else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "blob indisponible") };
    let mut webp = Vec::new();
    if uenc::decrypt_stream(&key, &mut reader, &mut webp).await.is_err() {
        return reply(StatusCode::INTERNAL_SERVER_ERROR, "miniature illisible");
    }
    (StatusCode::OK, [(header::CONTENT_TYPE, "image/webp".to_owned()), (header::CONTENT_LENGTH, webp.len().to_string())], cache, webp).into_response()
}

pub(crate) async fn get<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str, headers: &HeaderMap, head_only: bool) -> Response {
    let owner = auth.account.owner.clone();
    let p = path.to_owned();
    let found = st
        .db
        .readers()
        .read(move |c| {
            let entry = vfs::get_entry(c, &owner, &p).map_err(|_| astro_db::DbError::Aborted)?;
            let key: Option<String> = match entry.as_ref().and_then(|e| e.cid.clone()) {
                Some(cid) => rusqlite::OptionalExtension::optional(c.query_row("SELECT key_hex FROM ucloud_keyring WHERE owner_hex = ?1 AND cid = ?2", rusqlite::params![owner, cid], |r| r.get(0)))?,
                None => None,
            };
            Ok((entry, key))
        })
        .await;
    let Ok((Some(entry), key)) = found else { return reply(StatusCode::NOT_FOUND, "introuvable") };
    if entry.kind == EntryKind::Dir {
        return reply(StatusCode::METHOD_NOT_ALLOWED, "collection : utiliser PROPFIND");
    }
    let (Some(cid), Some(key_hex)) = (entry.cid.clone(), key) else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "clé du fichier absente") };
    let etag = format!("\"{cid}\"");
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*")) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    let range = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()).map(|v| parse_range(v, entry.size)) {
        Some(Err(())) => {
            return (StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{}", entry.size))]).into_response();
        }
        Some(Ok(r)) => r,
        None => None,
    };
    let mime = entry.mime.clone().unwrap_or_else(|| vfs::guess_mime(path).to_owned());
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::ETAG, &etag)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::LAST_MODIFIED, xml::http_date(entry.mtime));
    let (status, length) = match range {
        Some((s, e)) => {
            builder = builder.header(header::CONTENT_RANGE, format!("bytes {s}-{e}/{}", entry.size));
            (StatusCode::PARTIAL_CONTENT, e - s + 1)
        }
        None => (StatusCode::OK, entry.size),
    };
    builder = builder.status(status).header(header::CONTENT_LENGTH, length);
    if head_only {
        return builder.body(Body::empty()).unwrap_or_else(|_| reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne"));
    }
    let Ok(key) = UencKey::from_hex(&key_hex) else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "clé du fichier invalide") };
    if !is_cid(&cid) {
        return reply(StatusCode::INTERNAL_SERVER_ERROR, "CID invalide");
    }
    let Ok((mut reader, blob_len)) = st.blobs.open(&cid).await else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "blob indisponible") };
    // Tuyau borné : seuls quelques chunks de 64 Kio sont en mémoire, quelle que soit la taille du fichier.
    let (mut writer, pipe) = tokio::io::duplex(128 * 1024);
    tokio::spawn(async move {
        // Une erreur ferme le tuyau : le client reçoit moins d'octets que `Content-Length` et détecte la coupure.
        let _ = match range {
            Some((s, e)) => uenc::decrypt_range(&key, &mut reader, blob_len, s, e, &mut writer).await,
            None => uenc::decrypt_stream(&key, &mut reader, &mut writer).await,
        };
    });
    builder.body(Body::from_stream(ReaderStream::new(pipe))).unwrap_or_else(|_| reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne"))
}

async fn propfind<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str, headers: &HeaderMap) -> Response {
    let depth = headers.get("depth").and_then(|v| v.to_str().ok()).map_or("1", str::trim).to_ascii_lowercase();
    let (owner, p, cap) = (auth.account.owner.clone(), path.to_owned(), st.config.propfind_infinity_cap);
    let active = locks::active(&st.db, &owner, now_secs()).await.unwrap_or_default();
    let listed = st
        .db
        .readers()
        .read(move |c| {
            let Some(root) = vfs::get_entry(c, &owner, &p).map_err(|_| astro_db::DbError::Aborted)? else { return Ok(None) };
            let mut all = vec![root.clone()];
            if root.kind == EntryKind::Dir {
                match depth.as_str() {
                    "0" => {}
                    "infinity" => all.extend(vfs::list_subtree(c, &owner, &p, cap).map_err(|_| astro_db::DbError::Aborted)?),
                    _ => all.extend(vfs::list_children(c, &owner, &p).map_err(|_| astro_db::DbError::Aborted)?),
                }
            }
            Ok(Some(all))
        })
        .await;
    match listed {
        Ok(Some(entries)) if entries.len() > st.config.propfind_infinity_cap + 1 => (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:error xmlns:D=\"DAV:\"><D:propfind-finite-depth/></D:error>\n".to_owned(),
        )
            .into_response(),
        Ok(Some(entries)) => {
            let mut body = xml::multistatus(&st.config.mount, &entries, &active, now_secs());
            if path == "/" {
                // Découverte CardDAV : où est mon principal, donc mon carnet d'adresses.
                body = crate::carddav::announce_principal(st, &auth.account.owner, body).await;
            }
            (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
        }
        Ok(None) => reply(StatusCode::NOT_FOUND, "introuvable"),
        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne"),
    }
}

/// Lecteur qui hache le clair, le compte et refuse de dépasser `max` octets.
pub(crate) struct Measure<R> {
    pub(crate) inner: R,
    pub(crate) hasher: Sha256,
    pub(crate) count: u64,
    pub(crate) max: u64,
}

impl<R: AsyncRead + Unpin> AsyncRead for Measure<R> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let poll = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            let fresh = &buf.filled()[before..];
            this.hasher.update(fresh);
            this.count += fresh.len() as u64;
            if this.count > this.max {
                return Poll::Ready(Err(io::Error::other("fichier trop volumineux")));
            }
        }
        poll
    }
}

pub(crate) async fn put<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str, req: Request) -> Response {
    let limits = st.config.limits(auth.account.tier);
    let owner = auth.account.owner.clone();
    if path == "/" {
        return reply(StatusCode::METHOD_NOT_ALLOWED, "la racine est une collection");
    }
    let declared = req.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > limits.max_file) {
        return reply(StatusCode::PAYLOAD_TOO_LARGE, "fichier trop volumineux");
    }
    if !crate::blob::disk_has_room(st.blobs.as_ref(), declared.unwrap_or(0)) {
        return reply(StatusCode::INSUFFICIENT_STORAGE, "espace disque de la station presque plein : dépôt refusé");
    }
    // Contrôles préalables, avant de lire le corps : parent, collection, quota annoncé.
    let (o, p) = (owner.clone(), path.to_owned());
    let pre = st
        .db
        .readers()
        .read(move |c| {
            let parent = vfs::get_entry(c, &o, &vfs::parent_of(&p)).map_err(|_| astro_db::DbError::Aborted)?;
            let existing = vfs::get_entry(c, &o, &p).map_err(|_| astro_db::DbError::Aborted)?;
            let used = vfs::used_bytes(c, &o).map_err(|_| astro_db::DbError::Aborted)?;
            let old_thumb: Option<String> = rusqlite::OptionalExtension::optional(c.query_row("SELECT json_extract(data, '$.thumb.cid') FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", rusqlite::params![o, p], |r| r.get(0)))?.flatten();
            Ok((parent, existing, used, old_thumb))
        })
        .await;
    let Ok((parent, existing, used, old_thumb)) = pre else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne") };
    match parent {
        None => return reply(StatusCode::CONFLICT, "dossier parent absent"),
        Some(e) if e.kind == EntryKind::File => return reply(StatusCode::CONFLICT, "le parent n'est pas un dossier"),
        Some(_) => {}
    }
    if existing.as_ref().is_some_and(|e| e.kind == EntryKind::Dir) {
        return reply(StatusCode::METHOD_NOT_ALLOWED, "c'est une collection");
    }
    let old_size = existing.as_ref().map_or(0, |e| e.size);
    if declared.is_some_and(|n| used.saturating_sub(old_size).saturating_add(n) > limits.quota_total) {
        return reply(StatusCode::INSUFFICIENT_STORAGE, "quota dépassé");
    }
    // Un type générique (`octet-stream`, formulaire) ne dit rien du fichier : l'extension fait foi (l'album et les
    // aperçus reposent sur le type MIME).
    let declared_mime = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(|m| m.split(';').next().unwrap_or("").trim().to_ascii_lowercase()).filter(|m| !m.is_empty() && !matches!(m.as_str(), "application/octet-stream" | "binary/octet-stream" | "application/x-www-form-urlencoded"));
    let mime = declared_mime.unwrap_or_else(|| vfs::guess_mime(path).to_owned());

    // Flux : corps HTTP → mesure/hachage du clair → UENC v2 par chunks de 64 Kio → blob (fichier temporaire).
    let Ok(key) = UencKey::generate() else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "aléa indisponible") };
    let Ok(mut blob) = st.blobs.begin().await else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible") };
    let body = req.into_body().into_data_stream().map(|r| r.map_err(io::Error::other));
    let mut measured = Measure { inner: StreamReader::new(body), hasher: Sha256::new(), count: 0, max: limits.max_file };
    let written = uenc::encrypt_stream(&key, Cipher::detect(), &mut measured, &mut blob).await;
    if measured.count > limits.max_file {
        return reply(StatusCode::PAYLOAD_TOO_LARGE, "fichier trop volumineux");
    }
    if written.is_err() {
        return reply(StatusCode::BAD_REQUEST, "corps de requête interrompu"); // le blob non validé est supprimé à sa destruction
    }
    let (size_plain, sha_plain) = (measured.count, hex::encode(measured.hasher.finalize()));
    if auth.payload.as_deref().is_some_and(|h| h != sha_plain) {
        return reply(StatusCode::UNAUTHORIZED, "l'empreinte du corps ne correspond pas au tag payload NIP-98");
    }
    let Ok(meta) = blob.commit().await else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible") };
    let file = NewFile { cid: meta.cid.clone(), size_cid: meta.size, size_plain, sha_plain, mime: mime.clone(), key_hex: key.to_hex(), now: now_secs() };
    let (o, p) = (owner.clone(), path.to_owned());
    match vfs_tx(&st.db, move |tx| vfs::commit_file_tx(tx, &o, &p, &file, limits)).await {
        Ok(old) => {
            if let Some(hook) = &st.on_upload {
                hook(&owner, path, &mime);
            }
            let released: Vec<String> = old.as_ref().and_then(|e| e.cid.clone()).into_iter().chain(old_thumb).collect();
            release_blobs(st, released).await;
            if old.is_some() { StatusCode::NO_CONTENT.into_response() } else { StatusCode::CREATED.into_response() }
        }
        Err(e) => {
            let _ = st.blobs.remove(&meta.cid).await;
            vfs_error(&e)
        }
    }
}

/// Supprime les blobs qu'aucune entrée ne référence plus.
pub(crate) async fn release_blobs<B: BlobStore>(st: &DavState<B>, cids: Vec<String>) {
    for cid in cids {
        let c = cid.clone();
        let in_use = st.db.readers().read(move |conn| vfs::cid_in_use(conn, &c).map_err(|_| astro_db::DbError::Aborted)).await.unwrap_or(true);
        if !in_use {
            let _ = st.blobs.remove(&cid).await;
        }
    }
}

async fn mkcol<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str, has_body: bool) -> Response {
    if has_body {
        return reply(StatusCode::UNSUPPORTED_MEDIA_TYPE, "corps non supporté pour MKCOL");
    }
    let (o, p) = (auth.account.owner.clone(), path.to_owned());
    match vfs_tx(&st.db, move |tx| vfs::mkdir_tx(tx, &o, &p, now_secs())).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => vfs_error(&e),
    }
}

pub(crate) async fn delete<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str) -> Response {
    let (o, p) = (auth.account.owner.clone(), path.to_owned());
    match vfs_tx(&st.db, move |tx| vfs::delete_tree_tx(tx, &o, &p)).await {
        Ok(cids) => {
            release_blobs(st, cids).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => vfs_error(&e),
    }
}

async fn move_<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str, to: &str, headers: &HeaderMap) -> Response {
    let overwrite = headers.get("overwrite").and_then(|v| v.to_str().ok()).is_none_or(|v| !v.trim().eq_ignore_ascii_case("f"));
    let (o, from, dst) = (auth.account.owner.clone(), path.to_owned(), to.to_owned());
    let existed = {
        let (o, d) = (o.clone(), dst.clone());
        st.db.readers().read(move |c| Ok(vfs::get_entry(c, &o, &d).map_err(|_| astro_db::DbError::Aborted)?.is_some())).await.unwrap_or(false)
    };
    match vfs_tx(&st.db, move |tx| vfs::move_tx(tx, &o, &from, &dst, overwrite, now_secs())).await {
        Ok(overwritten) => {
            release_blobs(st, overwritten).await;
            if existed { StatusCode::NO_CONTENT.into_response() } else { StatusCode::CREATED.into_response() }
        }
        Err(VfsError::Exists) => reply(StatusCode::PRECONDITION_FAILED, "la destination existe"),
        Err(e) => vfs_error(&e),
    }
}

/// `COPY` : duplique l'entrée (et, sauf `Depth: 0`, sa descendance) en partageant blobs et clés : aucun ré-encodage.
async fn copy<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str, to: &str, headers: &HeaderMap) -> Response {
    let overwrite = headers.get("overwrite").and_then(|v| v.to_str().ok()).is_none_or(|v| !v.trim().eq_ignore_ascii_case("f"));
    let recursive = headers.get("depth").and_then(|v| v.to_str().ok()).is_none_or(|v| v.trim() != "0");
    let quota = st.config.limits(auth.account.tier).quota_total;
    let (o, from, dst) = (auth.account.owner.clone(), path.to_owned(), to.to_owned());
    let existed = {
        let (o, d) = (o.clone(), dst.clone());
        st.db.readers().read(move |c| Ok(vfs::get_entry(c, &o, &d).map_err(|_| astro_db::DbError::Aborted)?.is_some())).await.unwrap_or(false)
    };
    match vfs_tx(&st.db, move |tx| vfs::copy_tx(tx, &o, &from, &dst, vfs::CopyOpts { overwrite, recursive, now: now_secs(), quota_total: quota })).await {
        Ok(overwritten) => {
            release_blobs(st, overwritten).await;
            if existed { StatusCode::NO_CONTENT.into_response() } else { StatusCode::CREATED.into_response() }
        }
        Err(VfsError::Exists) => reply(StatusCode::PRECONDITION_FAILED, "la destination existe"),
        Err(e) => vfs_error(&e),
    }
}

/// Crée un fichier vide chiffré (ressource « lock-null » : un `LOCK` sur une URL inexistante la réserve).
async fn create_empty<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str) -> Result<(), Response> {
    let key = UencKey::generate().map_err(|_| reply(StatusCode::INTERNAL_SERVER_ERROR, "aléa indisponible"))?;
    let mut blob = st.blobs.begin().await.map_err(|_| reply(StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible"))?;
    let mut empty: &[u8] = &[];
    uenc::encrypt_stream(&key, Cipher::detect(), &mut empty, &mut blob).await.map_err(|_| reply(StatusCode::INTERNAL_SERVER_ERROR, "chiffrement impossible"))?;
    let meta = blob.commit().await.map_err(|_| reply(StatusCode::INTERNAL_SERVER_ERROR, "stockage indisponible"))?;
    let file = NewFile { cid: meta.cid.clone(), size_cid: meta.size, size_plain: 0, sha_plain: hex::encode(Sha256::digest([])), mime: vfs::guess_mime(path).to_owned(), key_hex: key.to_hex(), now: now_secs() };
    let (o, p, limits) = (auth.account.owner.clone(), path.to_owned(), st.config.limits(auth.account.tier));
    vfs_tx(&st.db, move |tx| vfs::commit_file_tx(tx, &o, &p, &file, limits)).await.map(|_| ()).map_err(|e| vfs_error(&e))
}

fn lock_response(status: StatusCode, mount: &str, l: &locks::Lock) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:prop xmlns:D=\"DAV:\"><D:lockdiscovery>{}</D:lockdiscovery></D:prop>\n",
        xml::activelock(mount, l, now_secs())
    );
    let mut r = (status, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("<{}>", l.token)) {
        r.headers_mut().insert("Lock-Token", v);
    }
    r
}

/// `LOCK` : verrou exclusif d'écriture, création, ou prolongation (corps vide + `If`).
async fn lock<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str, req: Request) -> Response {
    let headers = req.headers().clone();
    let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_XML).await else { return reply(StatusCode::PAYLOAD_TOO_LARGE, "corps trop volumineux") };
    let (owner, now) = (auth.account.owner.as_str(), now_secs());
    let timeout = parse_timeout(headers.get("timeout").and_then(|v| v.to_str().ok()));
    if body.iter().all(u8::is_ascii_whitespace) {
        let presented = tokens_in_if(headers.get("if").and_then(|v| v.to_str().ok()).unwrap_or(""));
        for token in presented {
            if let Ok(Some(l)) = locks::refresh(&st.db, owner, &token, timeout, now).await {
                return lock_response(StatusCode::OK, &st.config.mount, &l);
            }
        }
        return reply(StatusCode::PRECONDITION_FAILED, "aucun verrou à prolonger");
    }
    let info = match parse_lockinfo(&body) {
        Ok(i) => i,
        Err(_) => return reply(StatusCode::BAD_REQUEST, "lockinfo invalide"),
    };
    if info.shared {
        return reply(StatusCode::NOT_IMPLEMENTED, "seuls les verrous exclusifs sont supportés");
    }
    let infinity = headers.get("depth").and_then(|v| v.to_str().ok()).is_none_or(|v| v.trim() != "0");
    let (o, p) = (owner.to_owned(), path.to_owned());
    let existing = st.db.readers().read(move |c| vfs::get_entry(c, &o, &p).map_err(|_| astro_db::DbError::Aborted)).await;
    let Ok(existing) = existing else { return reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne") };
    if existing.is_none() {
        let (o, parent) = (owner.to_owned(), vfs::parent_of(path));
        let parent_ok = st.db.readers().read(move |c| Ok(vfs::get_entry(c, &o, &parent).map_err(|_| astro_db::DbError::Aborted)?.is_some_and(|e| e.kind == EntryKind::Dir))).await.unwrap_or(false);
        if !parent_ok {
            return reply(StatusCode::CONFLICT, "dossier parent absent");
        }
    }
    let lock = match locks::create(&st.db, owner, path, infinity, timeout, &info.owner, now).await {
        Ok(l) => l,
        Err(locks::LockError::Locked) => return reply(StatusCode::LOCKED, "ressource verrouillée"),
        Err(locks::LockError::TooMany) => return reply(StatusCode::INSUFFICIENT_STORAGE, "trop de verrous"),
        Err(locks::LockError::Db(_)) => return reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne"),
    };
    if existing.is_some() {
        return lock_response(StatusCode::OK, &st.config.mount, &lock);
    }
    match create_empty(st, auth, path).await {
        Ok(()) => lock_response(StatusCode::CREATED, &st.config.mount, &lock),
        Err(r) => {
            let _ = locks::release(&st.db, owner, &lock.token, now).await;
            r
        }
    }
}

async fn unlock<B: BlobStore>(st: &DavState<B>, auth: &Auth, headers: &HeaderMap) -> Response {
    let Some(token) = headers.get("lock-token").and_then(|v| v.to_str().ok()).and_then(token_of_header) else { return reply(StatusCode::BAD_REQUEST, "en-tête Lock-Token manquant") };
    match locks::release(&st.db, &auth.account.owner, &token, now_secs()).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => reply(StatusCode::CONFLICT, "jeton de verrou inconnu"),
        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne"),
    }
}

/// `PROPPATCH` : les propriétés mortes (ex. attributs Windows) sont stockées dans l'entrée ; les propriétés
/// `DAV:` sont protégées (403) et rendent la requête atomiquement refusée (424 pour les autres).
async fn proppatch<B: BlobStore>(st: &DavState<B>, auth: &Auth, path: &str, req: Request) -> Response {
    let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_XML).await else { return reply(StatusCode::PAYLOAD_TOO_LARGE, "corps trop volumineux") };
    let Ok(changes) = parse_propertyupdate(&body) else { return reply(StatusCode::BAD_REQUEST, "propertyupdate invalide") };
    let (o, p) = (auth.account.owner.clone(), path.to_owned());
    let present = st.db.readers().read(move |c| vfs::get_entry(c, &o, &p).map_err(|_| astro_db::DbError::Aborted)).await;
    match present {
        Ok(Some(_)) => {}
        Ok(None) => return reply(StatusCode::NOT_FOUND, "introuvable"),
        Err(_) => return reply(StatusCode::INTERNAL_SERVER_ERROR, "erreur interne"),
    }
    let protected = changes.iter().any(|(ns, _, _)| ns == "DAV:");
    if !protected && path != "/" {
        let (o, p, c) = (auth.account.owner.clone(), path.to_owned(), changes.clone());
        if let Err(e) = vfs_tx(&st.db, move |tx| vfs::set_props_tx(tx, &o, &p, &c)).await {
            return vfs_error(&e);
        }
    }
    let status = |ns: &str| match (protected, ns == "DAV:", path == "/") {
        (true, true, _) | (false, _, true) => "HTTP/1.1 403 Forbidden",
        (true, false, _) => "HTTP/1.1 424 Failed Dependency",
        (false, _, false) => "HTTP/1.1 200 OK",
    };
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\"><D:response>");
    out.push_str(&format!("<D:href>{}</D:href>", xml::escape(&xml::href(&st.config.mount, path, false))));
    for (i, (ns, name, _)) in changes.iter().enumerate() {
        let tag = if ns == "DAV:" {
            format!("<D:{}/>", xml::escape(name))
        } else if ns.is_empty() {
            format!("<{} xmlns=\"\"/>", xml::escape(name))
        } else {
            format!("<p{i}:{n} xmlns:p{i}=\"{}\"/>", xml::escape(ns), n = xml::escape(name))
        };
        out.push_str(&format!("<D:propstat><D:prop>{tag}</D:prop><D:status>{}</D:status></D:propstat>", status(ns)));
    }
    out.push_str("</D:response></D:multistatus>\n");
    (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], out).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_header_parsing() {
        assert_eq!(parse_range("bytes=0-9", 100), Ok(Some((0, 9))));
        assert_eq!(parse_range("bytes=90-", 100), Ok(Some((90, 99))));
        assert_eq!(parse_range("bytes=-10", 100), Ok(Some((90, 99))));
        assert_eq!(parse_range("bytes=-500", 100), Ok(Some((0, 99))));
        assert_eq!(parse_range("bytes=5-500", 100), Ok(Some((5, 99))));
        assert_eq!(parse_range("bytes=100-", 100), Err(()));
        assert_eq!(parse_range("bytes=9-5", 100), Err(()));
        assert_eq!(parse_range("bytes=-0", 100), Err(()));
        assert_eq!(parse_range("bytes=0-1,5-9", 100), Ok(None), "plages multiples : réponse complète");
        assert_eq!(parse_range("octets=0-1", 100), Ok(None));
        assert_eq!(parse_range("bytes=0-5", 0), Ok(None));
    }

    #[test]
    fn mount_prefix_is_stripped_and_checked() {
        assert_eq!(vfs_path("/dav", "/dav").unwrap(), "/");
        assert_eq!(vfs_path("/dav", "/dav/Photos/%C3%A9t%C3%A9.jpg").unwrap(), "/Photos/été.jpg");
        assert!(vfs_path("/dav", "/davx/a").is_err());
        assert!(vfs_path("/dav", "/autre").is_err());
        assert!(vfs_path("/dav", "/dav/a/../b").is_err());
        assert_eq!(vfs_path("", "/a/b").unwrap(), "/a/b");
    }
}
