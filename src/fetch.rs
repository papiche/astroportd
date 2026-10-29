//! Téléchargement HTTP(S) d'un document demandé par un titulaire (`#upload <URL>`).
//!
//! La station va chercher une adresse choisie par un tiers : c'est le scénario classique de **SSRF** (atteindre
//! `127.0.0.1`, le réseau local, les métadonnées d'un hébergeur). Garde-fous :
//! - seules les adresses **publiques** sont jointes ; le nom est résolu ici, la connexion se fait sur l'adresse
//!   vérifiée (pas de seconde résolution qu'un DNS malveillant pourrait détourner) ;
//! - chaque redirection est revérifiée de la même façon, 3 au plus, jamais de HTTPS vers HTTP ;
//! - HTTPS est validé (racines Mozilla) ; taille bornée ; pas de compression (le flux est copié tel quel).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::header::{self, HeaderValue};
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpStream;
use tokio_util::io::StreamReader;
use url::Url;

/// Réglages d'un téléchargement.
#[derive(Clone)]
pub struct FetchLimits {
    /// Octets de corps au plus.
    pub max_bytes: u64,
    /// Délai de connexion et d'obtention des en-têtes, par saut.
    pub timeout: Duration,
    /// Redirections suivies au plus.
    pub max_redirects: u8,
    /// Autorise les adresses privées et locales. **Tests seulement** : aucune option de ligne de commande n'y mène.
    pub allow_private: bool,
    /// Racines de confiance supplémentaires (autorité privée d'une constellation, tests).
    pub extra_roots: Vec<CertificateDer<'static>>,
}

impl FetchLimits {
    /// Réglages de production pour un fichier de `max_bytes` au plus.
    pub fn new(max_bytes: u64) -> Self {
        Self { max_bytes, timeout: Duration::from_secs(20), max_redirects: 3, allow_private: false, extra_roots: Vec::new() }
    }
}

/// Erreurs de téléchargement.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// URL inutilisable.
    #[error("URL invalide : {0}")]
    BadUrl(String),
    /// Adresse refusée (réseau privé, local…).
    #[error("adresse refusée : {0}")]
    Blocked(String),
    /// Résolution du nom.
    #[error("résolution du nom impossible : {0}")]
    Dns(String),
    /// Connexion ou échange réseau.
    #[error("connexion impossible : {0}")]
    Connect(String),
    /// Certificat ou négociation TLS.
    #[error("TLS : {0}")]
    Tls(String),
    /// Réponse HTTP d'erreur.
    #[error("le serveur répond {0}")]
    Status(u16),
    /// Trop de redirections ou redirection dangereuse.
    #[error("redirection refusée : {0}")]
    Redirect(&'static str),
    /// Fichier trop gros.
    #[error("fichier de {0} octets au-delà du plafond")]
    TooLarge(u64),
}

impl FetchError {
    /// Une nouvelle tentative plus tard a-t-elle une chance de réussir ?
    pub fn transient(&self) -> bool {
        matches!(self, Self::Dns(_) | Self::Connect(_)) || matches!(self, Self::Status(s) if *s >= 500 || *s == 429 || *s == 408)
    }
}

/// Document en cours de réception.
pub struct Fetched {
    /// Corps, au plus `max_bytes + 1` octets.
    pub body: Pin<Box<dyn AsyncRead + Send>>,
    /// `Content-Type` annoncé.
    pub content_type: Option<String>,
    /// `Content-Length` annoncé.
    pub content_length: Option<u64>,
    /// URL finale, après redirections.
    pub final_url: Url,
}

fn v4_public(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || (o[0] == 100 && (64..128).contains(&o[1])) // 100.64.0.0/10 : CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
        || (o[0] == 198 && (18..20).contains(&o[1])) // 198.18.0.0/15 : bancs d'essai
        || o[0] >= 240) // réservé
}

/// L'adresse est-elle joignable sur l'Internet public (ni locale, ni privée, ni réservée) ?
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_public(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4_public(v4);
            }
            let seg = v6.segments();
            let nat64 = seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0]; // 64:ff9b::/96 embarque une IPv4
            if nat64 {
                return v4_public(Ipv4Addr::new((seg[6] >> 8) as u8, seg[6] as u8, (seg[7] >> 8) as u8, seg[7] as u8));
            }
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || (seg[0] & 0xfe00) == 0xfc00 || (seg[0] & 0xffc0) == 0xfe80 || (seg[0] == 0x2001 && seg[1] == 0x0db8) || v6 == Ipv6Addr::LOCALHOST)
        }
    }
}

fn tls_config(extra: &[CertificateDer<'static>]) -> Result<Arc<rustls::ClientConfig>, FetchError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for c in extra {
        roots.add(c.clone()).map_err(|e| FetchError::Tls(e.to_string()))?;
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| FetchError::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

async fn resolve(host: &str, port: u16, allow_private: bool) -> Result<SocketAddr, FetchError> {
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return if allow_private || is_public(ip) { Ok(SocketAddr::new(ip, port)) } else { Err(FetchError::Blocked(ip.to_string())) };
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await.map_err(|e| FetchError::Dns(e.to_string()))?.collect();
    if addrs.is_empty() {
        return Err(FetchError::Dns(format!("{host} : aucune adresse")));
    }
    addrs.iter().copied().find(|a| allow_private || is_public(a.ip())).ok_or_else(|| FetchError::Blocked(format!("{host} → {}", addrs[0].ip())))
}

/// Télécharge `url`. Le corps est rendu en flux : rien n'est lu en mémoire ni écrit sur disque ici.
pub async fn fetch(url: &str, limits: &FetchLimits) -> Result<Fetched, FetchError> {
    fetch_inner(url, limits, None, &[]).await
}

/// Comme [`fetch`], avec des en-têtes en plus (`Authorization` d'un appel de station à station).
pub async fn fetch_with(url: &str, headers: &[(&str, String)], limits: &FetchLimits) -> Result<Fetched, FetchError> {
    fetch_inner(url, limits, None, headers).await
}

/// Envoie `json` en `POST` à `url` (service d'indexation configuré par l'opérateur : GraphQL) et rend la réponse,
/// bornée par `limits.max_bytes`. Mêmes garde-fous que [`fetch`] ; aucune redirection n'est suivie (un `POST` n'est
/// jamais rejoué ailleurs).
pub async fn post_json(url: &str, json: &str, limits: &FetchLimits) -> Result<Vec<u8>, FetchError> {
    post_json_with(url, json, &[], limits).await
}

/// Comme [`post_json`], avec des en-têtes en plus (`Authorization` d'un appel de station à station).
pub async fn post_json_with(url: &str, json: &str, headers: &[(&str, String)], limits: &FetchLimits) -> Result<Vec<u8>, FetchError> {
    let mut once = limits.clone();
    once.max_redirects = 0;
    let got = fetch_inner(url, &once, Some(Bytes::from(json.to_owned())), headers).await?;
    let mut out = Vec::new();
    let mut body = got.body;
    body.read_to_end(&mut out).await.map_err(|e| FetchError::Connect(e.to_string()))?;
    if out.len() as u64 > limits.max_bytes {
        return Err(FetchError::TooLarge(out.len() as u64));
    }
    Ok(out)
}

async fn fetch_inner(url: &str, limits: &FetchLimits, post: Option<Bytes>, extra: &[(&str, String)]) -> Result<Fetched, FetchError> {
    let mut current = Url::parse(url).map_err(|e| FetchError::BadUrl(e.to_string()))?;
    let tls = tls_config(&limits.extra_roots)?;
    for hop in 0..=limits.max_redirects {
        let https = match current.scheme() {
            "http" => false,
            "https" => true,
            other => return Err(FetchError::BadUrl(format!("schéma « {other} » non pris en charge"))),
        };
        if !current.username().is_empty() || current.password().is_some() {
            return Err(FetchError::BadUrl("identifiants dans l'URL refusés".into()));
        }
        let host = current.host_str().ok_or_else(|| FetchError::BadUrl("hôte manquant".into()))?.to_owned();
        let port = current.port_or_known_default().ok_or_else(|| FetchError::BadUrl("port inconnu".into()))?;
        let addr = resolve(&host, port, limits.allow_private).await?;

        let connect = async {
            let tcp = TcpStream::connect(addr).await.map_err(|e| FetchError::Connect(e.to_string()))?;
            let (mut sender, conn) = if https {
                let name = ServerName::try_from(host.clone()).map_err(|e| FetchError::Tls(e.to_string()))?;
                let stream = tokio_rustls::TlsConnector::from(tls.clone()).connect(name, tcp).await.map_err(|e| FetchError::Tls(e.to_string()))?;
                let (s, c) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.map_err(|e| FetchError::Connect(e.to_string()))?;
                (s, tokio::spawn(async move { let _ = c.await; }))
            } else {
                let (s, c) = hyper::client::conn::http1::handshake(TokioIo::new(tcp)).await.map_err(|e| FetchError::Connect(e.to_string()))?;
                (s, tokio::spawn(async move { let _ = c.await; }))
            };
            let target = match current.query() {
                Some(q) => format!("{}?{q}", current.path()),
                None => current.path().to_owned(),
            };
            let host_header = if current.port().is_some() { format!("{host}:{port}") } else { host.clone() };
            let builder = match &post {
                Some(_) => Request::post(target).header(header::CONTENT_TYPE, "application/json"),
                None => Request::get(target),
            };
            let builder = extra.iter().fold(builder, |b, (k, v)| b.header(*k, v.as_str()));
            let req = builder
                .header(header::HOST, host_header)
                .header(header::USER_AGENT, HeaderValue::from_static("astroportd"))
                .header(header::ACCEPT, if post.is_some() { "application/json" } else { "*/*" })
                .header(header::ACCEPT_ENCODING, "identity")
                .body(Full::new(post.clone().unwrap_or_default()))
                .map_err(|e| FetchError::BadUrl(e.to_string()))?;
            let resp = sender.send_request(req).await.map_err(|e| FetchError::Connect(e.to_string()))?;
            Ok::<_, FetchError>((resp, conn))
        };
        let (resp, _conn) = tokio::time::timeout(limits.timeout, connect).await.map_err(|_| FetchError::Connect("délai dépassé".into()))??;

        let status = resp.status();
        if status.is_redirection() {
            let location = resp.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).ok_or(FetchError::Redirect("sans Location"))?;
            let next = current.join(location).map_err(|_| FetchError::Redirect("Location illisible"))?;
            if https && next.scheme() == "http" {
                return Err(FetchError::Redirect("HTTPS vers HTTP"));
            }
            if hop == limits.max_redirects {
                return Err(FetchError::Redirect("trop de redirections"));
            }
            current = next;
            continue;
        }
        if !status.is_success() {
            return Err(FetchError::Status(status.as_u16()));
        }
        let content_length = resp.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        if let Some(n) = content_length.filter(|n| *n > limits.max_bytes) {
            return Err(FetchError::TooLarge(n));
        }
        let content_type = resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let stream = futures_util::TryStreamExt::map_err(resp.into_body().into_data_stream(), std::io::Error::other);
        // `max + 1` : un octet de plus suffit au dépôt pour constater le dépassement.
        let body = Box::pin(StreamReader::new(stream).take(limits.max_bytes.saturating_add(1)));
        return Ok(Fetched { body, content_type, content_length, final_url: current });
    }
    Err(FetchError::Redirect("trop de redirections"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_local_and_reserved_addresses_are_not_public() {
        for ip in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.254", "192.168.1.1", "169.254.169.254", "0.0.0.0", "100.64.0.1", "198.18.0.1", "224.0.0.1", "255.255.255.255", "240.0.0.1", "::1", "::", "fe80::1", "fc00::1", "fd12::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "64:ff9b::7f00:1", "2001:db8::1", "ff02::1"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip} doit être refusée");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "172.32.0.1", "100.128.0.1", "2606:4700:4700::1111", "::ffff:8.8.8.8", "64:ff9b::808:808"] {
            assert!(is_public(ip.parse().unwrap()), "{ip} doit être permise");
        }
    }

    #[test]
    fn only_server_side_errors_are_worth_retrying() {
        assert!(FetchError::Status(503).transient() && FetchError::Status(429).transient() && FetchError::Connect("x".into()).transient());
        assert!(!FetchError::Status(404).transient() && !FetchError::Blocked("x".into()).transient() && !FetchError::TooLarge(1).transient() && !FetchError::Tls("x".into()).transient());
    }
}
