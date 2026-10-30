//! Application web embarquée (`rust-embed`) : HTML5 sémantique, CSS moderne, JavaScript sans framework.
//!
//! - `/` est négocié : un navigateur (`Accept: text/html`) reçoit l'application ; `application/nostr+json` (NIP-11)
//!   et `Upgrade: websocket` continuent d'aller au relais, comme avant ;
//! - `/ui/…` sert les fichiers statiques (ETag, revalidation, `nosniff`, CSP stricte).

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

use axum::Router;
use axum::extract::{Path, Request};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/"]
struct Assets;

/// Politique de sécurité du contenu : tout vient de la station, rien d'inline ni d'externe.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' blob: data:; media-src 'self' blob:; \
                   connect-src 'self'; font-src 'self'; manifest-src 'self'; worker-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Page de la console : xterm.js crée des éléments `<style>` à l'exécution (couleurs, dimensions des cellules), ce que
/// `style-src 'self'` interdit. Seuls les **styles** sont assouplis, pour cette page-là ; les scripts restent `'self'`.
const CSP_CONSOLE: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self'; \
                           base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("LICENSE") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Taille cumulée des fichiers embarqués, en octets (budget : 48 Ko bruts, 15 Ko transférés compressés).
pub fn embedded_bytes() -> usize {
    shell_files().map(|(_, f)| f.data.len()).sum()
}

/// Fichiers chargés à la demande : la page d'inscription (`register.*`, pour les seuls nouveaux venus) et les écrans
/// lourds (`m-*.js` / `m-*.css`, tri des visages, réplication) n'entrent pas dans le budget de la coque ; ils ont le leur.
fn is_lazy(path: &str) -> bool {
    path.starts_with("register.") || path.starts_with("invite.") || path.starts_with("claim.") || path.starts_with("m-") || path.starts_with("console.") || path.starts_with("xterm")
}

/// Fichiers de la coque de l'application (tout sauf les fichiers chargés à la demande).
fn shell_files() -> impl Iterator<Item = (String, rust_embed::EmbeddedFile)> {
    Assets::iter().filter(|p| !is_lazy(p)).filter_map(|p| Assets::get(&p).map(|f| (p.into_owned(), f)))
}

/// Écrans chargés à la demande (`m-*`).
fn module_files() -> impl Iterator<Item = (String, rust_embed::EmbeddedFile)> {
    Assets::iter().filter(|p| p.starts_with("m-")).filter_map(|p| Assets::get(&p).map(|f| (p.into_owned(), f)))
}

/// Taille cumulée des écrans chargés à la demande (octets bruts, Gzip) : chacun ne se télécharge qu'à sa première visite.
/// Un visiteur ne charge qu'**un** dictionnaire de langue (`m-lang-*`) : on compte le plus lourd, pas leur somme.
pub fn module_bytes() -> (usize, usize) {
    let sizes = |(p, f): (String, rust_embed::EmbeddedFile)| (p.starts_with("m-lang-"), f.data.len(), compressed(&p, Encoding::Gzip, &f.data).len());
    let (mut raw, mut gz, mut lang) = (0, 0, (0, 0));
    for (is_lang, r, g) in module_files().map(sizes) {
        if is_lang {
            lang = (lang.0.max(r), lang.1.max(g));
        } else {
            (raw, gz) = (raw + r, gz + g);
        }
    }
    (raw + lang.0, gz + lang.1)
}

/// Compression négociée par `Accept-Encoding` : Brotli de préférence, puis Gzip, sinon octets bruts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Encoding {
    Brotli,
    Gzip,
}

impl Encoding {
    fn token(self) -> &'static str {
        match self {
            Self::Brotli => "br",
            Self::Gzip => "gzip",
        }
    }
}

/// Meilleur encodage accepté (`q=0` refuse ; Brotli l'emporte à qualité égale).
fn negotiate_encoding(accept: Option<&HeaderValue>) -> Option<Encoding> {
    let accept = accept?.to_str().ok()?.to_ascii_lowercase();
    let (mut br, mut gz, mut star) = (None, None, None);
    for part in accept.split(',') {
        let mut it = part.split(';');
        let name = it.next().unwrap_or("").trim();
        let q = it.find_map(|p| p.trim().strip_prefix("q=").and_then(|v| v.trim().parse::<f32>().ok())).unwrap_or(1.0);
        match name {
            "br" => br = Some(q),
            "gzip" | "x-gzip" => gz = Some(q),
            "*" => star = Some(q),
            _ => {}
        }
    }
    let (br, gz) = (br.or(star).unwrap_or(0.0), gz.or(star).unwrap_or(0.0));
    if br > 0.0 && br >= gz {
        Some(Encoding::Brotli)
    } else if gz > 0.0 {
        Some(Encoding::Gzip)
    } else {
        None
    }
}

fn compress(enc: Encoding, data: &[u8]) -> Vec<u8> {
    match enc {
        Encoding::Brotli => {
            let mut out = Vec::new();
            {
                let mut w = brotli::CompressorWriter::new(&mut out, 4096, 11, 22);
                let _ = w.write_all(data);
            }
            out
        }
        Encoding::Gzip => {
            let mut w = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            let _ = w.write_all(data);
            w.finish().unwrap_or_default()
        }
    }
}

/// Les fichiers sont immuables (embarqués dans le binaire) : chacun n'est compressé qu'**une fois par encodage**, à la
/// première requête, puis servi depuis la mémoire. Aucun coût par requête, aucun fichier sur disque.
fn compressed(path: &str, enc: Encoding, data: &[u8]) -> Vec<u8> {
    type Cache = Mutex<HashMap<(String, Encoding), Vec<u8>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&(path.to_owned(), enc)) {
        return hit.clone();
    }
    let out = compress(enc, data);
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert((path.to_owned(), enc), out.clone());
    out
}

/// Octets réellement transférés pour toute l'application vers un client qui accepte `enc`.
pub fn transfer_bytes(enc: Option<&str>) -> usize {
    let accept = enc.and_then(|e| HeaderValue::from_str(e).ok());
    let chosen = negotiate_encoding(accept.as_ref());
    shell_files()
        .map(|(p, f)| chosen.map_or(f.data.len(), |e| compressed(&p, e, &f.data).len()))
        .sum()
}

fn asset(path: &str, headers_in: &HeaderMap) -> Response {
    let Some(file) = Assets::get(path) else { return (StatusCode::NOT_FOUND, "introuvable").into_response() };
    let if_none_match = headers_in.get(header::IF_NONE_MATCH);
    let encoding = negotiate_encoding(headers_in.get(header::ACCEPT_ENCODING));
    let base = file.metadata.sha256_hash().iter().take(8).map(|b| format!("{b:02x}")).collect::<String>();
    let etag = match encoding {
        Some(e) => format!("\"{base}-{}\"", e.token()),
        None => format!("\"{base}\""),
    };
    let mut headers = HeaderMap::new();
    let put = |h: &mut HeaderMap, name, value: &str| {
        if let Ok(v) = HeaderValue::from_str(value) {
            h.insert(name, v);
        }
    };
    put(&mut headers, header::ETAG, &etag);
    put(&mut headers, header::CACHE_CONTROL, "no-cache");
    put(&mut headers, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    put(&mut headers, header::REFERRER_POLICY, "no-referrer");
    put(&mut headers, header::CONTENT_SECURITY_POLICY, if path == "console.html" { CSP_CONSOLE } else { CSP });
    // Le service worker pilote tout le site (scope `/`) bien qu'il vive sous `/ui/` ; la page `/` varie selon `Accept`.
    if path == "sw.js" {
        headers.insert(header::HeaderName::from_static("service-worker-allowed"), HeaderValue::from_static("/"));
    }
    put(&mut headers, header::VARY, if path == "index.html" { "Accept, Accept-Encoding" } else { "Accept-Encoding" });
    if let Some(e) = encoding {
        put(&mut headers, header::CONTENT_ENCODING, e.token());
    }
    if if_none_match.and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return (StatusCode::NOT_MODIFIED, headers).into_response();
    }
    put(&mut headers, header::CONTENT_TYPE, mime_of(path));
    let body = match encoding {
        Some(e) => compressed(path, e, &file.data),
        None => file.data.into_owned(),
    };
    (headers, body).into_response()
}

/// Le client veut-il la page web ? Oui seulement s'il le dit (`text/html`), qu'il ne réclame pas NIP-11 et qu'il
/// ne demande pas une mise à niveau WebSocket.
pub fn wants_html(h: &HeaderMap) -> bool {
    let upgrade = h.get(header::UPGRADE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let accept = h.get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    !upgrade && accept.contains("text/html") && !accept.contains("application/nostr+json")
}

/// Intergiciel de négociation de contenu sur `/` : à poser sur le routeur complet de la station.
pub async fn negotiate(req: Request, next: Next) -> Response {
    if req.method() == Method::GET && req.uri().path() == "/" && wants_html(req.headers()) {
        return asset("index.html", req.headers());
    }
    next.run(req).await
}

async fn static_file(Path(path): Path<String>, headers: HeaderMap) -> Response {
    asset(&path, &headers)
}

async fn register_page(headers: HeaderMap) -> Response {
    asset("register.html", &headers)
}

/// Page d'un lien d'invitation (`/i/<jeton>`) : la même pour tous les jetons, qui se lit dans l'adresse. Jamais indexée.
async fn invite_page(headers: HeaderMap) -> Response {
    let mut r = asset("invite.html", &headers);
    r.headers_mut().insert(header::HeaderName::from_static("x-robots-tag"), HeaderValue::from_static("noindex, nofollow, noarchive"));
    r
}

/// Page où l'on accepte l'invitation d'une autre station avec son compte d'ici (`/claim#origin=…&token=…`). Jamais indexée.
async fn claim_page(headers: HeaderMap) -> Response {
    let mut r = asset("claim.html", &headers);
    r.headers_mut().insert(header::HeaderName::from_static("x-robots-tag"), HeaderValue::from_static("noindex, nofollow, noarchive"));
    r
}

/// Routes `/ui/{fichier}`, `/register` (inscription d'un MULTIPASS) et `/i/{jeton}` (invitation à recevoir des photos).
pub fn static_router() -> Router {
    Router::new().route("/ui/{*path}", get(static_file)).route("/register", get(register_page)).route("/i/{token}", get(invite_page)).route("/claim", get(claim_page))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use tower::ServiceExt;

    use super::*;

    fn app() -> Router {
        Router::new().route("/", get(|| async { "relais" })).merge(static_router()).layer(axum::middleware::from_fn(negotiate))
    }

    async fn call(accept: Option<&str>, upgrade: bool, uri: &str) -> (StatusCode, String, HeaderMap) {
        let mut req = Request::builder().uri(uri);
        if let Some(a) = accept {
            req = req.header(header::ACCEPT, a);
        }
        if upgrade {
            req = req.header(header::UPGRADE, "websocket");
        }
        let resp = app().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8_lossy(&body).into_owned(), headers)
    }

    #[tokio::test]
    async fn root_is_negotiated_between_the_app_nip11_and_websocket() {
        let browser = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
        let (s, body, h) = call(Some(browser), false, "/").await;
        assert_eq!(s, StatusCode::OK);
        assert!(body.contains("<!doctype html>") && body.contains("/ui/app.js"));
        assert!(h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("script-src 'self'"));
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        // NIP-11, curl, WebSocket : toujours le relais.
        assert_eq!(call(Some("application/nostr+json"), false, "/").await.1, "relais");
        assert_eq!(call(Some("text/html, application/nostr+json"), false, "/").await.1, "relais");
        assert_eq!(call(Some("*/*"), false, "/").await.1, "relais");
        assert_eq!(call(None, false, "/").await.1, "relais");
        assert_eq!(call(Some(browser), true, "/").await.1, "relais");
    }

    #[tokio::test]
    async fn assets_have_etags_and_revalidate_with_304() {
        let (s, css, h) = call(None, false, "/ui/app.css").await;
        assert_eq!((s, h[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "text/css; charset=utf-8"));
        assert!(css.contains(":root"));
        let etag = h[header::ETAG].to_str().unwrap().to_owned();
        let resp = app().oneshot(Request::builder().uri("/ui/app.css").header(header::IF_NONE_MATCH, etag).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(call(None, false, "/ui/../Cargo.toml").await.0, StatusCode::NOT_FOUND);
        assert_eq!(call(None, false, "/ui/inexistant.js").await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_pwa_files_are_served_with_the_right_types_and_scope() {
        let (s, body, h) = call(None, false, "/ui/manifest.webmanifest").await;
        assert_eq!((s, h[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "application/manifest+json"));
        let m: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!((m["display"].as_str(), m["start_url"].as_str(), m["scope"].as_str()), (Some("standalone"), Some("/"), Some("/")));
        assert!(m["icons"][0]["src"].as_str().unwrap().starts_with("/ui/") && m["theme_color"].is_string() && m["name"] == "Astroport");
        let (s, js, h) = call(None, false, "/ui/sw.js").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(h["service-worker-allowed"], "/", "le worker sous /ui/ peut contrôler toute l'application");
        // Données privées : jamais en cache.
        assert!(js.contains("/ui/") && !js.contains("/api/") && !js.contains("/dav"), "le worker ne met en cache que la coque");
        assert!(js.contains("ignoreVary"), "`/` varie selon Accept");
        let (_, html, h) = call(Some("text/html"), false, "/").await;
        assert!(html.contains("rel=\"manifest\"") && html.contains("theme-color") && html.contains("id=\"toasts\""));
        assert_eq!(h[header::VARY], "Accept, Accept-Encoding");
        assert!(h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("worker-src 'self'"));
    }

    #[test]
    fn the_whole_app_stays_under_its_budgets_raw_and_on_the_wire() {
        let size = embedded_bytes();
        assert!(size > 5_000, "l'application est-elle vraiment embarquée ? {size}");
        assert!(size < 48 * 1024, "{size} octets bruts : budget de 48 Ko dépassé");
        let (br, gz) = (transfer_bytes(Some("br, gzip")), transfer_bytes(Some("gzip")));
        assert!(br < 15 * 1024, "{br} octets en Brotli : budget réseau de 15 Ko dépassé");
        assert!(gz < 15 * 1024, "{gz} octets en Gzip : budget réseau de 15 Ko dépassé");
        eprintln!("poids de l'application : {size} o bruts, {gz} o en gzip, {br} o en brotli");
        let (mraw, mgz) = module_bytes();
        eprintln!("écrans à la demande : {mraw} o bruts, {mgz} o en gzip");
        // Le cumul des écrans (qu'un visiteur ne télécharge qu'à la demande) est borné, et chaque écran garde sa propre limite.
        assert!(mraw > 0 && mraw < 128 * 1024 && mgz < 48 * 1024, "écrans à la demande : {mraw} o bruts (budget 128 Ko), {mgz} o en gzip (budget 48 Ko)");
        for (name, f) in module_files().filter(|(p, _)| !p.starts_with("m-lang-")) {
            let gz = compressed(&name, Encoding::Gzip, &f.data).len();
            assert!(gz < 12 * 1024, "{name} : {gz} o en gzip, au-delà des 12 Ko par écran");
        }
        assert!(br <= gz && gz < size / 2, "la compression doit diviser par plus de deux : brut {size}, gzip {gz}, br {br}");
    }

    #[test]
    fn encodings_are_negotiated_with_quality_values() {
        let n = |v: &str| negotiate_encoding(Some(&HeaderValue::from_str(v).unwrap()));
        assert_eq!(n("gzip, deflate, br"), Some(Encoding::Brotli));
        assert_eq!(n("gzip"), Some(Encoding::Gzip));
        assert_eq!(n("br;q=0, gzip"), Some(Encoding::Gzip), "q=0 refuse");
        assert_eq!(n("gzip;q=1.0, br;q=0.5"), Some(Encoding::Gzip));
        assert_eq!(n("*"), Some(Encoding::Brotli));
        assert_eq!(n("identity"), None);
        assert_eq!(n("br;q=0, gzip;q=0"), None);
        assert_eq!(negotiate_encoding(None), None);
    }

    #[tokio::test]
    async fn assets_are_compressed_on_demand_and_decode_to_the_original() {
        let raw = call(None, false, "/ui/app.js").await.1;
        for (accept, token) in [("br", "br"), ("gzip", "gzip"), ("gzip, br", "br")] {
            let req = Request::builder().uri("/ui/app.js").header(header::ACCEPT_ENCODING, accept).body(Body::empty()).unwrap();
            let resp = app().oneshot(req).await.unwrap();
            let h = resp.headers().clone();
            assert_eq!(h[header::CONTENT_ENCODING], token);
            assert_eq!(h[header::VARY], "Accept-Encoding");
            assert!(h[header::ETAG].to_str().unwrap().ends_with(&format!("-{token}\"")), "l'ETag distingue les encodages");
            let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
            assert!(body.len() < raw.len() * 2 / 5, "{token} : {} octets pour {}", body.len(), raw.len());
            let mut decoded = Vec::new();
            match token {
                "br" => {
                    std::io::Read::read_to_end(&mut brotli::Decompressor::new(&body[..], 4096), &mut decoded).unwrap();
                }
                _ => {
                    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&body[..]), &mut decoded).unwrap();
                }
            }
            assert_eq!(String::from_utf8(decoded).unwrap(), raw, "{token} : aller-retour exact");
            // Revalidation : 304 avec l'ETag de CET encodage.
            let req = Request::builder().uri("/ui/app.js").header(header::ACCEPT_ENCODING, accept).header(header::IF_NONE_MATCH, h[header::ETAG].clone()).body(Body::empty()).unwrap();
            assert_eq!(app().oneshot(req).await.unwrap().status(), StatusCode::NOT_MODIFIED);
        }
        // Sans Accept-Encoding : octets bruts, sans Content-Encoding.
        let (_, h) = { let r = app().oneshot(Request::builder().uri("/ui/app.js").body(Body::empty()).unwrap()).await.unwrap(); (r.status(), r.headers().clone()) };
        assert!(h.get(header::CONTENT_ENCODING).is_none());
        // La page `/` est négociée de la même façon.
        let req = Request::builder().uri("/").header(header::ACCEPT, "text/html").header(header::ACCEPT_ENCODING, "br").body(Body::empty()).unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!((resp.headers()[header::CONTENT_ENCODING].to_str().unwrap(), resp.headers()[header::VARY].to_str().unwrap()), ("br", "Accept, Accept-Encoding"));
    }
}
