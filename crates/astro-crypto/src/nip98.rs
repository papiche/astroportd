//! Validation NIP-98 durcie (`Authorization: Nostr <base64(event)>`).
//!
//! Par rapport au legacy (`UPassport/services/nostr.py`) :
//! - les tags `u` et `method` sont **obligatoires** et comparés à la requête réelle ;
//! - le tag `payload` (sha256 du corps) est exigé dès que la requête a un corps ;
//! - tous les événements sont soumis à l'anti-rejeu, via un [`ReplayStore`] persistant ;
//! - l'écart d'horloge doit être strictement inférieur à [`MAX_SKEW_SECS`].

use std::collections::HashMap;
use std::sync::Mutex;

use astro_nostr_proto::{Event, EventError, UnsignedEvent, kind};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Écart maximal toléré entre `created_at` et l'horloge locale, en secondes (exclu).
pub const MAX_SKEW_SECS: u64 = 120;
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Requête HTTP à lier à l'événement signé.
#[derive(Debug, Clone, Copy)]
pub struct Nip98Request<'a> {
    /// Méthode HTTP.
    pub method: &'a str,
    /// URL absolue **telle que vue par le client** (à reconstruire derrière un reverse proxy).
    pub url: &'a str,
    /// Corps de la requête, si elle en a un.
    pub body: Option<&'a [u8]>,
}

/// Identité authentifiée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nip98Verified {
    /// Clé publique de l'auteur (64 hex).
    pub pubkey: String,
    /// Identifiant de l'événement d'authentification.
    pub event_id: String,
    /// Horodatage signé.
    pub created_at: u64,
    /// Empreinte du corps (tag `payload`) si l'événement en porte une : à comparer après coup
    /// pour les téléversements en flux dont le corps n'a pas pu être haché avant la vérification.
    pub payload: Option<String>,
}

/// Erreurs de validation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Nip98Error {
    /// En-tête absent ou schéma différent de `Nostr`.
    #[error("en-tête Authorization: Nostr manquant")]
    MissingScheme,
    /// En-tête démesuré.
    #[error("en-tête trop volumineux")]
    TooLarge,
    /// Base64 invalide.
    #[error("base64 invalide")]
    BadBase64,
    /// JSON d'événement invalide.
    #[error("événement JSON invalide")]
    BadJson,
    /// Kind différent de 27235.
    #[error("kind inattendu : {0}")]
    WrongKind(u32),
    /// Identifiant ou signature invalide.
    #[error("événement invalide : {0}")]
    Event(#[from] EventError),
    /// Horodatage hors fenêtre.
    #[error("horodatage hors fenêtre de {MAX_SKEW_SECS} s")]
    Skew,
    /// Tag obligatoire absent.
    #[error("tag obligatoire manquant : {0}")]
    MissingTag(&'static str),
    /// URL signée différente de l'URL demandée.
    #[error("URL signée différente de la requête")]
    UrlMismatch,
    /// Méthode signée différente de la requête.
    #[error("méthode signée différente de la requête")]
    MethodMismatch,
    /// Empreinte du corps absente ou différente.
    #[error("empreinte du corps incorrecte")]
    PayloadMismatch,
    /// Événement déjà présenté.
    #[error("événement déjà utilisé (rejeu)")]
    Replayed,
    /// Le magasin anti-rejeu est indisponible : on refuse plutôt que d'accepter un rejeu possible.
    #[error("magasin anti-rejeu indisponible : {0}")]
    Store(String),
}

/// Erreur d'un [`ReplayStore`].
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ReplayError(pub String);

/// Mémoire des événements NIP-98 déjà consommés.
pub trait ReplayStore: Send + Sync {
    /// Enregistre `event_id` jusqu'à `expires_at` (UNIX, s).
    /// Retourne `true` si l'identifiant est nouveau, `false` s'il a déjà été vu et n'est pas expiré.
    fn check_and_insert(&self, event_id: &str, expires_at: u64, now: u64) -> Result<bool, ReplayError>;
}

/// Magasin en mémoire (tests, mode dégradé). Perdu au redémarrage : préférer le magasin SQLite d'`astro-db`.
#[derive(Default)]
pub struct MemoryReplayStore {
    seen: Mutex<HashMap<String, u64>>,
}

impl MemoryReplayStore {
    /// Magasin vide.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ReplayStore for MemoryReplayStore {
    fn check_and_insert(&self, event_id: &str, expires_at: u64, now: u64) -> Result<bool, ReplayError> {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, exp| *exp > now);
        if seen.contains_key(event_id) {
            return Ok(false);
        }
        seen.insert(event_id.to_owned(), expires_at);
        Ok(true)
    }
}

/// Construit l'en-tête `Authorization: Nostr …` d'une requête que **cette station** adresse à une autre au nom d'un compte
/// qu'elle héberge. Même forme que celle exigée par [`verify_authorization`] : `u`, `method`, et `payload` si `body` est non vide.
pub fn authorization_header(secret: &[u8; 32], method: &str, url: &str, body: Option<&[u8]>, now: u64) -> Result<String, EventError> {
    let mut tags = vec![vec!["u".to_owned(), url.to_owned()], vec!["method".to_owned(), method.to_ascii_uppercase()]];
    if let Some(b) = body.filter(|b| !b.is_empty()) {
        tags.push(vec!["payload".to_owned(), hex::encode(Sha256::digest(b))]);
    }
    // Deux appels identiques dans la même seconde donneraient le même événement : le second serait pris pour un rejeu.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let nonce = hex::encode(Sha256::digest(format!("{}:{nanos}", COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))))[..16].to_owned();
    let event = UnsignedEvent { created_at: now, kind: kind::HTTP_AUTH, tags, content: nonce }.sign(secret)?;
    let json = serde_json::to_string(&event).map_err(|_| EventError::Serialize)?;
    Ok(format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(json)))
}

/// Valide l'en-tête `Authorization` d'une requête.
///
/// L'événement n'est enregistré dans `store` qu'une fois toutes les autres vérifications réussies.
pub fn verify_authorization(
    header: &str,
    req: &Nip98Request<'_>,
    now: u64,
    store: &dyn ReplayStore,
) -> Result<Nip98Verified, Nip98Error> {
    let event = decode_header(header)?;
    if event.kind != kind::HTTP_AUTH {
        return Err(Nip98Error::WrongKind(event.kind));
    }
    event.verify()?;
    if event.created_at.abs_diff(now) >= MAX_SKEW_SECS {
        return Err(Nip98Error::Skew);
    }

    let signed_url = event.tag_value("u").ok_or(Nip98Error::MissingTag("u"))?;
    if normalize_url(signed_url).as_deref() != normalize_url(req.url).as_deref() || normalize_url(req.url).is_none() {
        return Err(Nip98Error::UrlMismatch);
    }
    let signed_method = event.tag_value("method").ok_or(Nip98Error::MissingTag("method"))?;
    if !signed_method.eq_ignore_ascii_case(req.method) {
        return Err(Nip98Error::MethodMismatch);
    }
    match (req.body.filter(|b| !b.is_empty()), event.tag_value("payload")) {
        (Some(body), Some(tag)) => {
            if !tag.eq_ignore_ascii_case(&hex::encode(Sha256::digest(body))) {
                return Err(Nip98Error::PayloadMismatch);
            }
        }
        (Some(_), None) => return Err(Nip98Error::MissingTag("payload")),
        (None, Some(_)) | (None, None) => {}
    }

    let expires_at = event.created_at.saturating_add(2 * MAX_SKEW_SECS);
    let fresh = store
        .check_and_insert(&event.id, expires_at, now)
        .map_err(|e| Nip98Error::Store(e.0))?;
    if !fresh {
        return Err(Nip98Error::Replayed);
    }
    let payload = event.tag_value("payload").map(str::to_ascii_lowercase);
    Ok(Nip98Verified { pubkey: event.pubkey, event_id: event.id, created_at: event.created_at, payload })
}

fn decode_header(header: &str) -> Result<Event, Nip98Error> {
    let header = header.trim();
    let (scheme, token) = header.split_once(char::is_whitespace).ok_or(Nip98Error::MissingScheme)?;
    if !scheme.eq_ignore_ascii_case("nostr") {
        return Err(Nip98Error::MissingScheme);
    }
    let token = token.trim();
    if token.len() > MAX_HEADER_BYTES {
        return Err(Nip98Error::TooLarge);
    }
    // Accepte base64 standard ou URL-safe, avec ou sans padding.
    let normalized: String = token
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            other => other,
        })
        .collect();
    let raw = URL_SAFE_NO_PAD.decode(normalized).map_err(|_| Nip98Error::BadBase64)?;
    serde_json::from_slice(&raw).map_err(|_| Nip98Error::BadJson)
}

/// URL absolue canonique : schéma et hôte en minuscules, port par défaut retiré, sans fragment.
fn normalize_url(raw: &str) -> Option<String> {
    let mut u = url::Url::parse(raw).ok()?;
    u.host_str()?;
    u.set_fragment(None);
    Some(u.into())
}

#[cfg(test)]
mod tests {
    use secp256k1::{Keypair, Secp256k1};

    use super::*;

    const NOW: u64 = 1_800_000_000;
    const URL: &str = "https://relay.example/api/cloud/upload";

    fn tag(n: &str, v: &str) -> Vec<String> {
        vec![n.into(), v.into()]
    }

    fn header_with(kind: u32, tags: Vec<Vec<String>>, created_at: u64) -> String {
        let secp = Secp256k1::new();
        let kp = Keypair::from_seckey_slice(&secp, &[9u8; 32]).unwrap();
        let (xonly, _) = kp.x_only_public_key();
        let mut ev = Event {
            id: String::new(),
            pubkey: hex::encode(xonly.serialize()),
            created_at,
            kind,
            tags,
            content: String::new(),
            sig: String::new(),
        };
        let id = ev.compute_id().unwrap();
        ev.id = hex::encode(id);
        ev.sig = hex::encode(secp.sign_schnorr_no_aux_rand(&id, &kp).to_byte_array());
        let json = serde_json::to_vec(&ev).unwrap();
        format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(json))
    }

    #[test]
    fn a_header_built_here_is_accepted_by_the_verifier() {
        let secret = [9u8; 32];
        let body = br#"{"a":1}"#;
        let h = authorization_header(&secret, "post", URL, Some(body), NOW).unwrap();
        let s = MemoryReplayStore::new();
        let ok = verify_authorization(&h, &req(Some(body)), NOW, &s).unwrap();
        assert_eq!(ok.pubkey, astro_nostr_proto::pubkey_of(&secret).unwrap());
        assert_eq!(verify_authorization(&h, &req(Some(b"autre")), NOW, &MemoryReplayStore::new()), Err(Nip98Error::PayloadMismatch), "le corps est lié à la signature");
    }

    fn good(body: Option<&[u8]>) -> String {
        let mut tags = vec![tag("u", URL), tag("method", "POST")];
        if let Some(b) = body {
            tags.push(tag("payload", &hex::encode(Sha256::digest(b))));
        }
        header_with(kind::HTTP_AUTH, tags, NOW)
    }

    fn req<'a>(body: Option<&'a [u8]>) -> Nip98Request<'a> {
        Nip98Request { method: "POST", url: URL, body }
    }

    #[test]
    fn accepts_valid_request_with_body() {
        let body = b"{\"a\":1}";
        let ok = verify_authorization(&good(Some(body)), &req(Some(body)), NOW, &MemoryReplayStore::new()).unwrap();
        assert_eq!(ok.created_at, NOW);
        assert_eq!(ok.pubkey.len(), 64);
    }

    #[test]
    fn rejects_replay_even_on_get() {
        let store = MemoryReplayStore::new();
        let h = header_with(kind::HTTP_AUTH, vec![tag("u", URL), tag("method", "GET")], NOW);
        let r = Nip98Request { method: "GET", url: URL, body: None };
        verify_authorization(&h, &r, NOW, &store).unwrap();
        assert_eq!(verify_authorization(&h, &r, NOW + 1, &store), Err(Nip98Error::Replayed));
    }

    #[test]
    fn replay_entry_expires_with_the_window() {
        let store = MemoryReplayStore::new();
        let h = good(None);
        verify_authorization(&h, &req(None), NOW, &store).unwrap();
        // Après 2×fenêtre l'événement est de toute façon refusé pour âge ; l'entrée est purgée.
        assert_eq!(verify_authorization(&h, &req(None), NOW + 240, &store), Err(Nip98Error::Skew));
    }

    #[test]
    fn rejects_skew_at_the_limit_both_ways() {
        let s = MemoryReplayStore::new();
        assert_eq!(verify_authorization(&good(None), &req(None), NOW + 120, &s), Err(Nip98Error::Skew));
        assert!(verify_authorization(&good(None), &req(None), NOW + 119, &s).is_ok());
        let s = MemoryReplayStore::new();
        assert_eq!(verify_authorization(&good(None), &req(None), NOW - 120, &s), Err(Nip98Error::Skew));
    }

    #[test]
    fn rejects_url_and_method_mismatch() {
        let s = MemoryReplayStore::new();
        let r = Nip98Request { method: "POST", url: "https://relay.example/api/cloud/delete", body: None };
        assert_eq!(verify_authorization(&good(None), &r, NOW, &s), Err(Nip98Error::UrlMismatch));
        let r = Nip98Request { method: "DELETE", url: URL, body: None };
        assert_eq!(verify_authorization(&good(None), &r, NOW, &s), Err(Nip98Error::MethodMismatch));
    }

    #[test]
    fn url_comparison_is_canonical_but_strict_on_host_and_query() {
        let s = MemoryReplayStore::new();
        let r = Nip98Request { method: "POST", url: "HTTPS://Relay.Example:443/api/cloud/upload#frag", body: None };
        assert!(verify_authorization(&good(None), &r, NOW, &s).is_ok());
        let r = Nip98Request { method: "POST", url: "https://evil.example/api/cloud/upload", body: None };
        assert_eq!(verify_authorization(&good(None), &r, NOW, &MemoryReplayStore::new()), Err(Nip98Error::UrlMismatch));
        let r = Nip98Request { method: "POST", url: "https://relay.example/api/cloud/upload?x=1", body: None };
        assert_eq!(verify_authorization(&good(None), &r, NOW, &MemoryReplayStore::new()), Err(Nip98Error::UrlMismatch));
    }

    #[test]
    fn missing_tags_are_refused() {
        let s = MemoryReplayStore::new();
        let h = header_with(kind::HTTP_AUTH, vec![tag("method", "POST")], NOW);
        assert_eq!(verify_authorization(&h, &req(None), NOW, &s), Err(Nip98Error::MissingTag("u")));
        let h = header_with(kind::HTTP_AUTH, vec![tag("u", URL)], NOW);
        assert_eq!(verify_authorization(&h, &req(None), NOW, &s), Err(Nip98Error::MissingTag("method")));
    }

    #[test]
    fn body_requires_matching_payload_tag() {
        let s = MemoryReplayStore::new();
        let body = b"data";
        assert_eq!(verify_authorization(&good(None), &req(Some(body)), NOW, &s), Err(Nip98Error::MissingTag("payload")));
        assert_eq!(verify_authorization(&good(Some(b"autre")), &req(Some(body)), NOW, &s), Err(Nip98Error::PayloadMismatch));
    }

    #[test]
    fn invalid_attempts_do_not_burn_the_event() {
        let store = MemoryReplayStore::new();
        let h = good(None);
        let bad = Nip98Request { method: "DELETE", url: URL, body: None };
        assert_eq!(verify_authorization(&h, &bad, NOW, &store), Err(Nip98Error::MethodMismatch));
        assert!(verify_authorization(&h, &req(None), NOW, &store).is_ok());
    }

    #[test]
    fn rejects_wrong_kind_and_garbage() {
        let s = MemoryReplayStore::new();
        let h = header_with(1, vec![tag("u", URL), tag("method", "POST")], NOW);
        assert_eq!(verify_authorization(&h, &req(None), NOW, &s), Err(Nip98Error::WrongKind(1)));
        assert_eq!(verify_authorization("Bearer abc", &req(None), NOW, &s), Err(Nip98Error::MissingScheme));
        assert_eq!(verify_authorization("Nostr !!!", &req(None), NOW, &s), Err(Nip98Error::BadBase64));
        assert_eq!(verify_authorization("Nostr e30", &req(None), NOW, &s), Err(Nip98Error::BadJson));
    }

    #[test]
    fn rejects_tampered_signature() {
        let h = good(None);
        let json = base64::engine::general_purpose::STANDARD.decode(h.trim_start_matches("Nostr ")).unwrap();
        let mut ev: Event = serde_json::from_slice(&json).unwrap();
        ev.content = "x".into();
        let forged = format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&ev).unwrap()));
        let r = verify_authorization(&forged, &req(None), NOW, &MemoryReplayStore::new());
        assert_eq!(r, Err(Nip98Error::Event(EventError::IdMismatch)));
    }

    #[test]
    fn accepts_urlsafe_unpadded_base64() {
        let h = good(None);
        let std_b64 = h.trim_start_matches("Nostr ");
        let url_b64 = std_b64.trim_end_matches('=').replace('+', "-").replace('/', "_");
        let r = verify_authorization(&format!("nostr {url_b64}"), &req(None), NOW, &MemoryReplayStore::new());
        assert!(r.is_ok());
    }

    struct Broken;
    impl ReplayStore for Broken {
        fn check_and_insert(&self, _: &str, _: u64, _: u64) -> Result<bool, ReplayError> {
            Err(ReplayError("disque plein".into()))
        }
    }

    #[test]
    fn store_failure_refuses_the_request() {
        let r = verify_authorization(&good(None), &req(None), NOW, &Broken);
        assert_eq!(r, Err(Nip98Error::Store("disque plein".into())));
    }
}
