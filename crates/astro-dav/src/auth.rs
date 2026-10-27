//! Comptes WebDAV : jeton Basic dérivé du compte, recherche de l'identité hébergée.

use astro_db::Db;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Formule d'abonnement : détermine les plafonds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// MULTIPASS standard.
    Multipass,
    /// ZenCard (sociétaire).
    ZenCard,
}

/// Compte authentifié.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Clé Nostr du propriétaire (64 hex).
    pub owner: String,
    /// Formule.
    pub tier: Tier,
}

/// Jeton Basic d'un compte : `base64url(HMAC-SHA256(secret_station, "ucloud-dav:" ‖ hex))[..32]`.
///
/// Dérivé, donc jamais stocké ; il change si le secret de la station change.
pub fn derive_dav_token(station_secret: &[u8], owner_hex: &str) -> String {
    // HMAC accepte toute longueur de clé : `new_from_slice` ne peut échouer, la valeur vide en dernier recours.
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(station_secret) else { return String::new() };
    mac.update(b"ucloud-dav:");
    mac.update(owner_hex.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())[..32].to_owned()
}

/// Comparaison en temps constant.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

/// `Authorization: Basic …` → `(utilisateur, mot de passe)`.
pub fn parse_basic(header: &str) -> Option<(String, String)> {
    let (scheme, rest) = header.trim().split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = String::from_utf8(STANDARD.decode(rest.trim()).ok()?).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_owned(), pass.to_owned()))
}

fn account_from_row(owner: String, role: &str, contract: Option<&str>) -> Account {
    let zen = role == "zencard" || contract == Some("cooperative_member");
    Account { owner, tier: if zen { Tier::ZenCard } else { Tier::Multipass } }
}

/// Compte hébergé ici pour une clé publique.
pub async fn lookup_by_hex(db: &Db, hex: &str) -> Option<Account> {
    let hex = hex.to_owned();
    db.readers()
        .read(move |c| {
            let row: Option<(String, Option<String>)> = rusqlite::OptionalExtension::optional(
                c.query_row("SELECT role, contract FROM identities WHERE hex = ?1 AND hosted = 1", [&hex], |r| Ok((r.get(0)?, r.get(1)?))),
            )?;
            Ok(row.map(|(role, contract)| account_from_row(hex, &role, contract.as_deref())))
        })
        .await
        .ok()
        .flatten()
}

/// Compte hébergé ici pour un identifiant de connexion : clé publique (64 hex) ou email.
pub async fn lookup_by_login(db: &Db, login: &str) -> Option<Account> {
    let login = login.to_owned();
    db.readers()
        .read(move |c| {
            let row: Option<(String, String, Option<String>)> = rusqlite::OptionalExtension::optional(c.query_row(
                "SELECT hex, role, contract FROM identities WHERE hosted = 1 AND (hex = ?1 OR email = ?1) ORDER BY hex LIMIT 1",
                [&login],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ))?;
            Ok(row.map(|(hex, role, contract)| account_from_row(hex, &role, contract.as_deref())))
        })
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_deterministic_per_account_and_secret() {
        let (s1, s2) = (b"secret-station-1", b"secret-station-2");
        let t = derive_dav_token(s1, "aa");
        assert_eq!(t.len(), 32);
        assert_eq!(t, derive_dav_token(s1, "aa"));
        assert_ne!(t, derive_dav_token(s1, "bb"));
        assert_ne!(t, derive_dav_token(s2, "aa"));
    }

    #[test]
    fn basic_header_parsing_and_constant_time_compare() {
        let h = format!("Basic {}", STANDARD.encode("alice@example.org:p:ass"));
        assert_eq!(parse_basic(&h), Some(("alice@example.org".into(), "p:ass".into())));
        assert_eq!(parse_basic("Bearer abc"), None);
        assert_eq!(parse_basic("Basic !!!"), None);
        assert_eq!(parse_basic(&format!("Basic {}", STANDARD.encode("sans-deux-points"))), None);
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"abc", b"abcd") && !ct_eq(b"", b"a"));
    }

    #[tokio::test]
    async fn only_hosted_identities_can_log_in_and_tier_follows_the_contract() {
        let dir = std::env::temp_dir().join(format!("astro-dav-auth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("u.db"), 1).unwrap();
        for (hex, role, email, contract, hosted) in [
            ("aa", "multipass", "a@x.org", None, 1),
            ("bb", "multipass", "b@x.org", Some("cooperative_member"), 1),
            ("cc", "zencard", "c@x.org", None, 1),
            ("dd", "multipass", "d@x.org", None, 0),
        ] {
            db.writer()
                .execute(
                    "INSERT INTO identities(hex, role, email, contract, hosted) VALUES (?1, ?2, ?3, ?4, ?5)",
                    vec![hex.to_owned().into(), role.to_owned().into(), email.to_owned().into(), contract.map_or(rusqlite::types::Value::Null, |c: &str| c.to_owned().into()), i64::from(hosted).into()],
                )
                .await
                .unwrap();
        }
        assert_eq!(lookup_by_hex(&db, "aa").await, Some(Account { owner: "aa".into(), tier: Tier::Multipass }));
        assert_eq!(lookup_by_hex(&db, "bb").await.unwrap().tier, Tier::ZenCard);
        assert_eq!(lookup_by_login(&db, "c@x.org").await.unwrap().tier, Tier::ZenCard);
        assert_eq!(lookup_by_login(&db, "a@x.org").await.unwrap().owner, "aa");
        assert!(lookup_by_hex(&db, "dd").await.is_none(), "identité non hébergée");
        assert!(lookup_by_login(&db, "inconnu@x.org").await.is_none());
        drop(db);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
