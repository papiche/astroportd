//! Verrous WebDAV exclusifs (RFC 4918 §6–7), persistés dans `kv` (scope `dav_lock`).
//!
//! Un verrou protège une ressource (et, en profondeur infinie, toute sa descendance) : toute écriture doit
//! présenter son jeton dans l'en-tête `If`, sinon `423 Locked`. Les verrous expirent seuls.

use std::collections::HashSet;

use astro_db::{Db, DbError};
use serde::{Deserialize, Serialize};

/// Durée par défaut d'un verrou et plafond, en secondes.
pub const DEFAULT_TIMEOUT: i64 = 3600;
/// Plafond de durée.
pub const MAX_TIMEOUT: i64 = 86_400;
/// Verrous actifs au plus par compte.
pub const MAX_LOCKS: usize = 256;

const SCOPE: &str = "dav_lock";

/// Verrou actif.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    /// Jeton `opaquelocktoken:<uuid>`.
    pub token: String,
    /// Ressource verrouillée.
    pub path: String,
    /// Profondeur infinie (sinon 0).
    pub infinity: bool,
    /// Expiration, secondes UNIX.
    pub expires_at: i64,
    /// Texte de l'élément `owner` fourni par le client.
    pub holder: String,
}

/// Erreurs de verrouillage.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Un verrou incompatible existe déjà.
    #[error("ressource verrouillée")]
    Locked,
    /// Trop de verrous.
    #[error("trop de verrous")]
    TooMany,
    /// Base de données.
    #[error("base : {0}")]
    Db(#[from] DbError),
}

fn under(path: &str, ancestor: &str) -> bool {
    path.len() > ancestor.len() && path.starts_with(ancestor) && (ancestor == "/" || path.as_bytes()[ancestor.len()] == b'/')
}

impl Lock {
    /// Ce verrou protège-t-il `path` (et `descendants` si l'opération touche toute une arborescence) ?
    pub fn covers(&self, path: &str, descendants: bool) -> bool {
        self.path == path || (self.infinity && under(path, &self.path)) || (descendants && under(&self.path, path))
    }
}

/// Jeton UUID v4 : `opaquelocktoken:xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`.
pub fn new_token() -> Result<String, getrandom::Error> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    Ok(format!("opaquelocktoken:{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

/// Jetons présentés dans un en-tête `If` (`(<opaquelocktoken:…>)`, éventuellement précédés d'une ressource).
pub fn tokens_in_if(header: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut rest = header;
    while let Some(i) = rest.find("<opaquelocktoken:") {
        let tail = &rest[i + 1..];
        let Some(end) = tail.find('>') else { break };
        out.insert(tail[..end].to_owned());
        rest = &tail[end..];
    }
    out
}

/// Jeton de l'en-tête `Lock-Token: <opaquelocktoken:…>`.
pub fn token_of_header(header: &str) -> Option<String> {
    let t = header.trim().strip_prefix('<')?.strip_suffix('>')?;
    t.starts_with("opaquelocktoken:").then(|| t.to_owned())
}

/// Durée demandée par `Timeout: Second-N | Infinite`, bornée.
pub fn parse_timeout(header: Option<&str>) -> i64 {
    let secs = header
        .and_then(|h| h.split(',').map(str::trim).find_map(|t| t.strip_prefix("Second-").and_then(|n| n.parse::<i64>().ok())))
        .unwrap_or(DEFAULT_TIMEOUT);
    secs.clamp(1, MAX_TIMEOUT)
}

/// Verrous actifs du compte ; les expirés sont purgés au passage.
pub async fn active(db: &Db, owner: &str, now: i64) -> Result<Vec<Lock>, LockError> {
    let prefix = format!("{owner}|");
    let rows: Vec<(String, String)> = db
        .readers()
        .read(move |c| {
            let mut s = c.prepare("SELECT key, value FROM kv WHERE scope = ?1 AND substr(key, 1, ?2) = ?3")?;
            let rows = s.query_map(rusqlite::params![SCOPE, i64::try_from(prefix.len()).unwrap_or(0), prefix], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let (mut live, mut dead) = (Vec::new(), Vec::new());
    for (key, value) in rows {
        match serde_json::from_str::<Lock>(&value) {
            Ok(l) if l.expires_at > now => live.push(l),
            _ => dead.push(key),
        }
    }
    for key in dead {
        db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SCOPE.to_owned().into(), key.into()]).await?;
    }
    Ok(live)
}

async fn store(db: &Db, owner: &str, lock: &Lock, now: i64) -> Result<(), LockError> {
    let value = serde_json::to_string(lock).map_err(|e| DbError::Corrupt(e.to_string()))?;
    db.writer()
        .execute(
            "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            vec![SCOPE.to_owned().into(), format!("{owner}|{}", lock.token).into(), value.into(), now.into()],
        )
        .await?;
    Ok(())
}

/// Pose un verrou exclusif ; `Locked` si un verrou incompatible est déjà actif.
pub async fn create(db: &Db, owner: &str, path: &str, infinity: bool, timeout: i64, holder: &str, now: i64) -> Result<Lock, LockError> {
    let existing = active(db, owner, now).await?;
    if existing.len() >= MAX_LOCKS {
        return Err(LockError::TooMany);
    }
    let candidate = Lock { token: String::new(), path: path.to_owned(), infinity, expires_at: 0, holder: String::new() };
    if existing.iter().any(|l| l.covers(path, infinity) || candidate.covers(&l.path, false) && infinity) {
        return Err(LockError::Locked);
    }
    let token = new_token().map_err(|e| DbError::Corrupt(e.to_string()))?;
    let lock = Lock { token, path: path.to_owned(), infinity, expires_at: now + timeout, holder: holder.to_owned() };
    store(db, owner, &lock, now).await?;
    Ok(lock)
}

/// Prolonge un verrou existant.
pub async fn refresh(db: &Db, owner: &str, token: &str, timeout: i64, now: i64) -> Result<Option<Lock>, LockError> {
    let Some(mut lock) = active(db, owner, now).await?.into_iter().find(|l| l.token == token) else { return Ok(None) };
    lock.expires_at = now + timeout;
    store(db, owner, &lock, now).await?;
    Ok(Some(lock))
}

/// Lève un verrou ; `false` si le jeton est inconnu.
pub async fn release(db: &Db, owner: &str, token: &str, now: i64) -> Result<bool, LockError> {
    if !active(db, owner, now).await?.iter().any(|l| l.token == token) {
        return Ok(false);
    }
    db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SCOPE.to_owned().into(), format!("{owner}|{token}").into()]).await?;
    Ok(true)
}

/// Un verrou non présenté bloque-t-il l'écriture sur `path` ?
pub fn blocked(locks: &[Lock], path: &str, descendants: bool, presented: &HashSet<String>) -> bool {
    locks.iter().any(|l| l.covers(path, descendants) && !presented.contains(&l.token))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(path: &str, infinity: bool) -> Lock {
        Lock { token: format!("opaquelocktoken:{path}"), path: path.into(), infinity, expires_at: 100, holder: String::new() }
    }

    #[test]
    fn coverage_rules() {
        let (file, tree) = (lock("/a/f.txt", false), lock("/a", true));
        assert!(file.covers("/a/f.txt", false));
        assert!(!file.covers("/a", false) && !file.covers("/a/g.txt", false));
        assert!(file.covers("/a", true), "supprimer /a toucherait le fichier verrouillé");
        assert!(tree.covers("/a/b/c", false) && tree.covers("/a", false));
        assert!(!tree.covers("/ab", false), "préfixe voisin");
        assert!(!lock("/a", false).covers("/a/f", false), "profondeur 0 : le dossier seul");
        assert!(lock("/", true).covers("/x", false));
    }

    #[test]
    fn header_parsing() {
        let t = tokens_in_if("</dav/a.txt> (<opaquelocktoken:1111> [\"etag\"]) (<opaquelocktoken:2222>)");
        assert_eq!(t, HashSet::from(["opaquelocktoken:1111".to_owned(), "opaquelocktoken:2222".to_owned()]));
        assert!(tokens_in_if("(Not <DAV:no-lock>)").is_empty());
        assert_eq!(token_of_header(" <opaquelocktoken:abc> "), Some("opaquelocktoken:abc".into()));
        assert_eq!(token_of_header("opaquelocktoken:abc"), None);
        assert_eq!(parse_timeout(Some("Second-600")), 600);
        assert_eq!(parse_timeout(Some("Infinite, Second-4100000000")), MAX_TIMEOUT);
        assert_eq!(parse_timeout(Some("Infinite")), DEFAULT_TIMEOUT);
        assert_eq!(parse_timeout(None), DEFAULT_TIMEOUT);
        let tok = new_token().unwrap();
        assert!(tok.starts_with("opaquelocktoken:") && tok.len() == 16 + 36 && &tok[16 + 14..16 + 15] == "4");
        assert_ne!(tok, new_token().unwrap());
    }

    #[test]
    fn blocking_requires_the_token() {
        let l = vec![lock("/a", true)];
        let none = HashSet::new();
        let with = HashSet::from([l[0].token.clone()]);
        assert!(blocked(&l, "/a/f", false, &none));
        assert!(!blocked(&l, "/a/f", false, &with));
        assert!(!blocked(&l, "/b", false, &none));
    }

    #[tokio::test]
    async fn lifecycle_conflicts_refresh_expiry() {
        let dir = std::env::temp_dir().join(format!("astro-locks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("u.db"), 1).unwrap();
        let l1 = create(&db, "aa", "/a", true, 100, "fred", 1000).await.unwrap();
        assert!(matches!(create(&db, "aa", "/a/f", false, 100, "", 1000).await, Err(LockError::Locked)), "sous un verrou infini");
        assert!(matches!(create(&db, "aa", "/", true, 100, "", 1000).await, Err(LockError::Locked)), "un verrou infini englobant un verrou existant");
        create(&db, "bb", "/a", true, 100, "", 1000).await.unwrap();
        create(&db, "aa", "/z", false, 100, "", 1000).await.unwrap();
        assert_eq!(active(&db, "aa", 1050).await.unwrap().len(), 2);
        let r = refresh(&db, "aa", &l1.token, 500, 1090).await.unwrap().unwrap();
        assert_eq!(r.expires_at, 1590);
        assert!(refresh(&db, "aa", "opaquelocktoken:inconnu", 5, 1090).await.unwrap().is_none());
        assert_eq!(active(&db, "aa", 1150).await.unwrap().len(), 1, "/z a expiré");
        assert!(release(&db, "aa", &l1.token, 1150).await.unwrap());
        assert!(!release(&db, "aa", &l1.token, 1150).await.unwrap());
        assert!(active(&db, "aa", 1150).await.unwrap().is_empty());
        assert_eq!(active(&db, "bb", 1050).await.unwrap().len(), 1, "isolation entre comptes");
        drop(db);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
