//! ACME de bout en bout contre une autorité simulée (RFC 8555) : défi HTTP-01 réellement validé sur notre port 80,
//! émission, persistance, TLS, redirection 301 et renouvellement à chaud.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_db::Db;
use astro_ui::{AcmeConfig, AcmeManager, Frontend, KvCertStore};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use rcgen::{BasicConstraints, CertificateParams, CertificateSigningRequestParams, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, ServerName};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const DOMAIN: &str = "station.example";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("astro-ui-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Autorité ACME minimale : ne vérifie pas les signatures JWS, mais valide VRAIMENT le défi HTTP-01
/// (elle va chercher `/.well-known/acme-challenge/<jeton>` sur l'adresse de validation) et signe le CSR.
struct Mock {
    base: String,
    validate_addr: SocketAddr,
    ca_cert: rcgen::Certificate,
    ca_key: KeyPair,
    orders: AtomicUsize,
    state: Mutex<MockState>,
}

#[derive(Default)]
struct MockState {
    thumbprint: String,
    valid: std::collections::HashSet<usize>,
    certs: std::collections::HashMap<usize, String>,
    requests: Vec<String>,
}

fn json(code: StatusCode, nonce: &str, location: Option<String>, body: serde_json::Value) -> Response {
    let mut r = (code, [(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response();
    r.headers_mut().insert("Replay-Nonce", nonce.parse().unwrap());
    if let Some(l) = location {
        r.headers_mut().insert(header::LOCATION, l.parse().unwrap());
    }
    r
}

async fn http_get(addr: SocketAddr, host: &str, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

async fn acme(State(m): State<Arc<Mock>>, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    let nonce = format!("nonce-{}", m.orders.load(Ordering::SeqCst));
    if path == "/directory" {
        let b = &m.base;
        return json(StatusCode::OK, &nonce, None, serde_json::json!({ "newNonce": format!("{b}/nonce"), "newAccount": format!("{b}/acct"), "newOrder": format!("{b}/order"), "revokeCert": format!("{b}/revoke"), "keyChange": format!("{b}/kc") }));
    }
    if path == "/nonce" {
        return json(StatusCode::OK, &nonce, None, serde_json::json!({}));
    }
    let raw = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap();
    let jws: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let protected: serde_json::Value = serde_json::from_slice(&B64.decode(jws["protected"].as_str().unwrap()).unwrap()).unwrap();
    let payload_raw = B64.decode(jws["payload"].as_str().unwrap()).unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&payload_raw).unwrap_or(serde_json::Value::Null);
    m.state.lock().unwrap().requests.push(path.clone());
    let b = m.base.clone();
    let n: usize = path.rsplit('/').next().and_then(|s| s.parse().ok()).unwrap_or(0);
    match path.as_str() {
        "/acct" => {
            let jwk = &protected["jwk"];
            let canonical = format!("{{\"crv\":\"{}\",\"kty\":\"{}\",\"x\":\"{}\",\"y\":\"{}\"}}", jwk["crv"].as_str().unwrap(), jwk["kty"].as_str().unwrap(), jwk["x"].as_str().unwrap(), jwk["y"].as_str().unwrap());
            m.state.lock().unwrap().thumbprint = B64.encode(Sha256::digest(canonical.as_bytes()));
            json(StatusCode::CREATED, &nonce, Some(format!("{b}/acct/1")), serde_json::json!({ "status": "valid", "orders": format!("{b}/orders") }))
        }
        "/order" => {
            let id = m.orders.fetch_add(1, Ordering::SeqCst) + 1;
            let ids = payload["identifiers"].clone();
            json(StatusCode::CREATED, &nonce, Some(format!("{b}/order/{id}")), serde_json::json!({ "status": "pending", "identifiers": ids, "authorizations": [format!("{b}/authz/{id}")], "finalize": format!("{b}/finalize/{id}") }))
        }
        p if p.starts_with("/authz/") => {
            let valid = m.state.lock().unwrap().valid.contains(&n);
            let status = if valid { "valid" } else { "pending" };
            json(StatusCode::OK, &nonce, None, serde_json::json!({ "status": status, "identifier": { "type": "dns", "value": DOMAIN }, "challenges": [{ "type": "http-01", "url": format!("{b}/chall/{n}"), "status": status, "token": format!("token{n}") }] }))
        }
        p if p.starts_with("/chall/") => {
            // Validation réelle : la réponse doit être « jeton.empreinte-de-clé-du-compte », servie par notre port 80.
            let body = http_get(m.validate_addr, DOMAIN, &format!("/.well-known/acme-challenge/token{n}")).await;
            let expected = format!("token{n}.{}", m.state.lock().unwrap().thumbprint);
            let ok = body.contains("200 OK") && body.ends_with(&expected);
            if ok {
                m.state.lock().unwrap().valid.insert(n);
            }
            json(StatusCode::OK, &nonce, None, serde_json::json!({ "type": "http-01", "url": format!("{b}/chall/{n}"), "status": if ok { "valid" } else { "invalid" }, "token": format!("token{n}") }))
        }
        p if p.starts_with("/finalize/") => {
            let csr_der = B64.decode(payload["csr"].as_str().unwrap()).unwrap();
            let mut csr = CertificateSigningRequestParams::from_der(&csr_der.into()).unwrap();
            let now = time::OffsetDateTime::now_utc();
            csr.params.not_before = now - time::Duration::days(1);
            csr.params.not_after = now + time::Duration::days(60);
            let cert = csr.signed_by(&m.ca_cert, &m.ca_key).unwrap();
            m.state.lock().unwrap().certs.insert(n, format!("{}{}", cert.pem(), m.ca_cert.pem()));
            json(StatusCode::OK, &nonce, None, serde_json::json!({ "status": "valid", "identifiers": [{ "type": "dns", "value": DOMAIN }], "authorizations": [format!("{b}/authz/{n}")], "finalize": format!("{b}/finalize/{n}"), "certificate": format!("{b}/cert/{n}") }))
        }
        p if p.starts_with("/order/") => {
            let (valid, has_cert) = {
                let g = m.state.lock().unwrap();
                (g.valid.contains(&n), g.certs.contains_key(&n))
            };
            let status = if has_cert { "valid" } else if valid { "ready" } else { "pending" };
            let mut body = serde_json::json!({ "status": status, "identifiers": [{ "type": "dns", "value": DOMAIN }], "authorizations": [format!("{b}/authz/{n}")], "finalize": format!("{b}/finalize/{n}") });
            if has_cert {
                body["certificate"] = format!("{b}/cert/{n}").into();
            }
            json(StatusCode::OK, &nonce, None, body)
        }
        p if p.starts_with("/cert/") => {
            let pem = m.state.lock().unwrap().certs.get(&n).cloned().unwrap();
            let mut r = (StatusCode::OK, [(header::CONTENT_TYPE, "application/pem-certificate-chain")], pem).into_response();
            r.headers_mut().insert("Replay-Nonce", nonce.parse().unwrap());
            r
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn start_mock(validate_addr: SocketAddr) -> (Arc<Mock>, String) {
    let ca_key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name.push(rcgen::DnType::CommonName, "CA de test");
    let ca_cert = params.self_signed(&ca_key).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mock = Arc::new(Mock { base: base.clone(), validate_addr, ca_cert, ca_key, orders: AtomicUsize::new(0), state: Mutex::default() });
    let app = Router::new().fallback(axum::routing::any(acme)).with_state(mock.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (mock, base)
}

fn plain_http_client() -> Box<dyn instant_acme::HttpClient> {
    Box::new(hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build_http::<instant_acme::BodyWrapper<bytes::Bytes>>())
}

async fn https_get(addr: SocketAddr, ca_der: &CertificateDer<'static>, path: &str) -> (String, Vec<u8>) {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.clone()).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(ServerName::try_from(DOMAIN).unwrap(), TcpStream::connect(addr).await.unwrap()).await.unwrap();
    let leaf = tls.get_ref().1.peer_certificates().unwrap()[0].as_ref().to_vec();
    let mut tls = tls;
    tls.write_all(format!("GET {path} HTTP/1.1\r\nHost: {DOMAIN}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tls.read_to_end(&mut out).await;
    (String::from_utf8_lossy(&out).into_owned(), leaf)
}

fn app() -> Router {
    Router::new().route("/hello", axum::routing::get(|| async { "bonjour https" }))
}

#[tokio::test]
async fn issues_a_certificate_over_http01_serves_tls_redirects_and_renews_without_restart() {
    let dir = TempDir::new("acme");
    let db = Arc::new(Db::open(&dir.0.join("uplanet.db"), 2).unwrap());
    // Notre façade ouvre les ports (éphémères) ; l'autorité simulée valide sur notre port HTTP.
    // Le port HTTP n'est connu qu'après le démarrage de la façade : l'autorité le lit via un relais statique.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = probe.local_addr().unwrap();
    drop(probe);
    let (mock, base) = start_mock(http_addr).await;

    let mut config = AcmeConfig::new(vec![DOMAIN.to_owned()], Some("admin@example.org".into()));
    config.directory_url = format!("{base}/directory");
    let manager = AcmeManager::new(config, KvCertStore::new(db.clone())).with_http_client(plain_http_client());
    let front = Frontend::start(manager.clone(), app(), http_addr, "127.0.0.1:0".parse().unwrap()).await.unwrap();
    assert_eq!(front.http_addr, http_addr);

    // Émission automatique en tâche de fond (le défi est réellement servi et validé).
    for _ in 0..300 {
        if manager.resolver.has_certificate() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(manager.resolver.has_certificate(), "{:?}", manager.status.lock().unwrap());
    let ca_der = mock.ca_cert.der().clone();
    assert_eq!(mock.orders.load(Ordering::SeqCst), 1);
    let first_not_after = manager.status.lock().unwrap().not_after.unwrap();
    assert!(first_not_after > 0);

    // HTTPS : certificat signé par l'autorité, validé pour le nom, contenu servi.
    let (resp, leaf1) = https_get(front.https_addr, &ca_der, "/hello").await;
    assert!(resp.starts_with("HTTP/1.1 200") && resp.ends_with("bonjour https"), "{resp}");

    // HTTP : défi inconnu → 404 ; autre chemin → 301 vers HTTPS (port non standard conservé) ; hôte étranger → 421.
    let r = http_get(front.http_addr, DOMAIN, "/.well-known/acme-challenge/inconnu").await;
    assert!(r.starts_with("HTTP/1.1 404"), "{r}");
    let r = http_get(front.http_addr, DOMAIN, "/dav/photo.jpg?x=1").await;
    assert!(r.starts_with("HTTP/1.1 301"), "{r}");
    assert!(r.to_lowercase().contains(&format!("location: https://{DOMAIN}:{}/dav/photo.jpg?x=1", front.https_addr.port())), "{r}");
    let r = http_get(front.http_addr, "evil.example", "/").await;
    assert!(r.starts_with("HTTP/1.1 421"), "pas de redirection ouverte : {r}");

    // Persistance : un nouveau gestionnaire sur la même base installe le certificat en cache sans nouvelle commande.
    let again = AcmeManager::new(manager.config.clone(), KvCertStore::new(db.clone())).with_http_client(plain_http_client());
    assert!(!again.ensure().await.unwrap(), "certificat encore valable : aucune émission");
    assert!(again.resolver.has_certificate());
    assert_eq!(mock.orders.load(Ordering::SeqCst), 1);

    // Règle de renouvellement : certificat de 60 jours, seuil à 90 jours → échu, nouvelle émission (défi servi par la même façade).
    let mut due = manager.config.clone();
    due.renew_before = Duration::from_secs(90 * 24 * 3600);
    let due = AcmeManager::with_challenges(due, KvCertStore::new(db.clone()), manager.challenges.clone()).with_http_client(plain_http_client());
    assert!(due.ensure().await.unwrap(), "renouvellement dû");
    assert_eq!(mock.orders.load(Ordering::SeqCst), 2);
    assert_eq!(due.status.lock().unwrap().issued, 1);

    // Renouvellement à chaud sur le gestionnaire en service : le certificat change, l'écouteur HTTPS n'est jamais relancé.
    let mut in_flight = TcpStream::connect(front.https_addr).await.unwrap(); // connexion ouverte avant le renouvellement
    manager.clone().with_http_client(plain_http_client()).renew_now().await.unwrap();
    assert_eq!(mock.orders.load(Ordering::SeqCst), 3);
    let (resp, leaf2) = https_get(front.https_addr, &ca_der, "/hello").await;
    assert!(resp.ends_with("bonjour https"), "{resp}");
    assert_ne!(leaf1, leaf2, "nouveau certificat servi sans redémarrage");
    assert!(manager.status.lock().unwrap().not_after.unwrap() >= first_not_after);
    // La connexion ouverte avant n'a pas été coupée par le rechargement : elle peut encore négocier (poignée de main sur l'ancienne socket).
    let _ = in_flight.write_all(b"\x16\x03\x01").await;
    front.shutdown();
}
