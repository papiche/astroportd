//! **Station marraine** : une station en place aide une nouvelle à rejoindre sa constellation, sans recopie de fichiers.
//!
//! 1. Sur la marraine, `astroportd parrainer` prépare une **offre** : PeerID et adresse P2P de la marraine (amorçage),
//!    son relai, et en ẐEN la `swarm.key`. L'offre est chiffrée (NIP-44 v2) avec une clé tirée pour l'occasion, puis
//!    rangée dans la base de la marraine sous un identifiant aléatoire. La commande affiche le **jeton**
//!    `astro-join:v1|<adresse de la marraine>|<identifiant>|<clé>`.
//! 2. Sur la nouvelle station, `astroportd init --join <jeton>` demande l'offre à la marraine
//!    (`GET /api/v1/join/<identifiant>`), la déchiffre, écrit la `swarm.key` et s'amorce sur la marraine.
//!
//! Garde-fous :
//! - la clé ne voyage **que dans le jeton**, jamais vers la marraine : la base de la marraine ne contient qu'un chiffré ;
//! - l'offre ne sert **qu'une fois** : la marraine l'efface en la rendant (un jeton photographié après usage ne vaut rien) ;
//! - elle expire (24 h par défaut) ; une offre périmée est effacée au passage, jamais rendue ;
//! - la `swarm.key` (le secret de toute une constellation ẐEN) ne circule donc jamais en clair, ni sur le réseau ni dans la base.
//!
//! Stockage : état local, `kv(scope = 'join', key = <identifiant>)`.

use astro_db::Db;
use base64::Engine;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroize::Zeroizing;

const SCOPE: &str = "join";
/// Préfixe et version du jeton.
pub const TOKEN_PREFIX: &str = "astro-join:v1";
/// Durée de validité par défaut d'une offre (heures).
pub const DEFAULT_HOURS: u32 = 24;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Ce que la marraine transmet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Payload {
    /// Nom lisible de la marraine (son domaine).
    pub from: String,
    /// `swarm.key` de la constellation (ẐEN) ; absente en ORIGIN.
    pub swarm_key: Option<String>,
    /// Adresses d'amorçage P2P (`…/p2p/<PeerID>`).
    pub bootstrap: Vec<String>,
    /// Relai Nostr de la marraine (`wss://…`).
    pub relay: Option<String>,
}

/// Jeton décodé.
pub struct Token {
    /// Adresse de la marraine (`https://station.exemple.org`).
    pub base_url: String,
    /// Identifiant de l'offre.
    pub id: String,
    /// Clé de déchiffrement.
    pub key: Zeroizing<[u8; 32]>,
}

/// Jeton à transmettre à la nouvelle station.
pub fn encode_token(base_url: &str, id: &str, key: &[u8; 32]) -> String {
    format!("{TOKEN_PREFIX}|{}|{id}|{}", base_url.trim_end_matches('/'), base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key))
}

/// Décode un jeton (espaces et retours à la ligne d'un copier-coller tolérés).
pub fn parse_token(text: &str) -> Result<Token, String> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let bad = || "jeton de parrainage illisible (astro-join:v1|<adresse>|<identifiant>|<clé> attendu)".to_owned();
    let mut parts = text.split('|');
    if parts.next() != Some(TOKEN_PREFIX) {
        return Err(bad());
    }
    let (Some(base), Some(id), Some(key), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return Err(bad()) };
    let url = url::Url::parse(base).map_err(|_| bad())?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        return Err(bad());
    }
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let raw = Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(key).map_err(|_| bad())?);
    let key: [u8; 32] = raw.as_slice().try_into().map_err(|_| bad())?;
    Ok(Token { base_url: base.trim_end_matches('/').to_owned(), id: id.to_ascii_lowercase(), key: Zeroizing::new(key) })
}

/// Prépare une offre sur la marraine. Rend le jeton.
pub async fn create(db: &Db, base_url: &str, payload: &Payload, hours: u32) -> Result<String, String> {
    let mut key = Zeroizing::new([0u8; 32]);
    getrandom::fill(key.as_mut()).map_err(|e| format!("aléa du système indisponible : {e}"))?;
    let id = crate::invites::new_token().map_err(|e| format!("{e:?}"))?;
    let plain = Zeroizing::new(serde_json::to_string(payload).map_err(|e| e.to_string())?);
    let sealed = astro_crypto::nip44::encrypt(&key, &plain).map_err(|e| format!("chiffrement : {e}"))?;
    let at = now();
    let row = json!({ "sealed": sealed, "expires_at": at + i64::from(hours.clamp(1, 24 * 7)) * 3600 });
    db.writer()
        .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![SCOPE.to_owned().into(), id.clone().into(), row.to_string().into(), at.into()])
        .await
        .map_err(|e| e.to_string())?;
    Ok(encode_token(base_url, &id, &key))
}

/// Rend **une fois** le chiffré d'une offre et l'efface (périmée : effacée, rien n'est rendu).
pub async fn take(db: &Db, id: &str) -> Result<Option<String>, String> {
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let (id, at) = (id.to_ascii_lowercase(), now());
    db.writer()
        .transaction(move |tx| {
            let Some(v) = tx.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, &id], |r| r.get::<_, String>(0)).optional()? else { return Ok(None) };
            tx.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, &id])?;
            let row: serde_json::Value = serde_json::from_str(&v).unwrap_or_default();
            Ok(row["sealed"].as_str().filter(|_| row["expires_at"].as_i64().unwrap_or(0) > at).map(str::to_owned))
        })
        .await
        .map_err(|e| e.to_string())
}

/// Ouvre le chiffré rendu par la marraine.
pub fn open(token: &Token, sealed: &str) -> Result<Payload, String> {
    let plain = Zeroizing::new(astro_crypto::nip44::decrypt(&token.key, sealed).map_err(|_| "offre indéchiffrable : le jeton ne correspond pas".to_owned())?);
    serde_json::from_str(&plain).map_err(|e| format!("offre illisible : {e}"))
}

/// Côté nouvelle station : demande l'offre à la marraine et la déchiffre.
pub async fn redeem(text: &str) -> Result<Payload, String> {
    let token = parse_token(text)?;
    let url = format!("{}/api/v1/join/{}", token.base_url, token.id);
    let mut limits = crate::fetch::FetchLimits::new(64 * 1024);
    // Adresse donnée par le Capitaine lui-même (marraine sur le réseau local possible) : pas la garde anti-SSRF d'un lien reçu.
    limits.allow_private = true;
    let fetched = crate::fetch::fetch(&url, &limits).await.map_err(|e| match e {
        crate::fetch::FetchError::Status(404) => "offre introuvable, déjà utilisée ou expirée : demandez un nouveau jeton à la station marraine".to_owned(),
        e => format!("marraine injoignable ({}) : {e:?}", token.base_url),
    })?;
    let mut body = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut tokio::io::AsyncReadExt::take(fetched.body, 64 * 1024), &mut body).await.map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|_| "réponse de la marraine illisible".to_owned())?;
    let sealed = v["sealed"].as_str().ok_or_else(|| v["error"].as_str().map_or_else(|| "offre introuvable".to_owned(), str::to_owned))?;
    open(&token, sealed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Payload {
        Payload { from: "marraine.exemple.org".into(), swarm_key: Some("/key/swarm/psk/1.0.0/\n/base16/\nabcdef\n".into()), bootstrap: vec!["/dns4/marraine.exemple.org/tcp/4101/p2p/12D3KooWTest".into()], relay: Some("wss://marraine.exemple.org".into()) }
    }

    #[test]
    fn a_token_round_trips_and_rejects_garbage() {
        let t = encode_token("https://marraine.exemple.org/", &"ab".repeat(16), &[7; 32]);
        assert!(t.starts_with("astro-join:v1|https://marraine.exemple.org|"));
        let p = parse_token(&format!(" {}\n", &t)).unwrap();
        assert_eq!((p.base_url.as_str(), p.id.len(), *p.key), ("https://marraine.exemple.org", 32, [7; 32]));
        for bad in ["", "astro-join:v2|https://a.b|00|AA", "astro-join:v1|ftp://a.b|abababababababababababababababab|AA", "astro-join:v1|https://a.b|zz|AA", &format!("{t}|x")] {
            assert!(parse_token(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn an_offer_is_sealed_given_once_and_only_the_token_opens_it() {
        let dir = std::env::temp_dir().join(format!("astroportd-sponsor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("u.db"), 2).unwrap();
        let token = create(&db, "https://marraine.exemple.org", &payload(), DEFAULT_HOURS).await.unwrap();
        let t = parse_token(&token).unwrap();
        // La base de la marraine ne contient pas la swarm.key en clair.
        let stored: String = db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'join'", [], |r| r.get(0))?)).await.unwrap();
        assert!(!stored.contains("abcdef") && !stored.contains("swarm"), "{stored}");
        let sealed = take(&db, &t.id).await.unwrap().unwrap();
        assert_eq!(open(&t, &sealed).unwrap(), payload());
        assert_eq!(take(&db, &t.id).await.unwrap(), None, "usage unique");
        // Un autre jeton (autre clé) n'ouvre rien.
        let other = Token { base_url: t.base_url.clone(), id: t.id.clone(), key: Zeroizing::new([1; 32]) };
        assert!(open(&other, &sealed).is_err());
        // Une offre périmée n'est jamais rendue, et disparaît.
        let token = create(&db, "https://marraine.exemple.org", &payload(), 1).await.unwrap();
        let id = parse_token(&token).unwrap().id;
        let i2 = id.clone();
        db.writer().execute("UPDATE kv SET value = json_set(value, '$.expires_at', 1) WHERE scope = 'join' AND key = ?1", vec![i2.into()]).await.unwrap();
        assert_eq!(take(&db, &id).await.unwrap(), None);
        let left: i64 = db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*) FROM kv WHERE scope = 'join'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(left, 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
