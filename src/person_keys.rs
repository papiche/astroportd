//! Les clés Nostr d'une **même personne** dans plusieurs UPlanet.
//!
//! Une personne a un MULTIPASS par UPlanet, donc une clé Nostr par UPlanet (formule isolée) : la clé d'un ami dans une autre UPlanet
//! ne se déduit **pas** de son email (elle dépend de secrets qu'il est seul à connaître). Deux sources, jamais un email auto-déclaré :
//!
//! 1. **L'annuaire de la station** : les plans servis par cette station (`planes.json`) hébergent des comptes ; leurs bases sont
//!    lues en lecture seule et l'email d'un MULTIPASS hébergé donne sa clé dans chaque plan.
//! 2. **Les liens prouvés** (kind 31903 `d = plane-link`) : la personne annonce depuis une clé qu'une autre clé est la sienne, et
//!    cette autre clé le signe aussi. Sans les deux signatures, le lien n'existe pas (voir `astro-sync::projector`).
//!
//! Publier un lien révèle publiquement que deux clés vont ensemble : il ne part que sur demande du titulaire ([`PersonKeys::announce`]).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use astro_crypto::{FileKeySource, KeySource};
use astro_db::Db;
use astro_nostr_proto::{UnsignedEvent, is_hex64, kind};
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::{Value, json};

use crate::register::PublishFn;

/// Plan voisin servi par la même station.
#[derive(Debug, Clone)]
pub struct Sibling {
    /// Nom du plan (`origin`, `zen1`…).
    pub name: String,
    /// Sa base `uplanet.db` (lue en lecture seule).
    pub db: PathBuf,
    /// Son dossier `game` (comptes), pour annoncer un lien : la station y détient les clés des comptes qu'elle héberge.
    pub game: Option<PathBuf>,
}

/// Clé d'une personne dans une UPlanet.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlaneKey {
    /// UPlanet (vide si inconnue).
    pub plane: String,
    /// Clé Nostr (64 hex).
    pub hex: String,
    /// D'où vient la clé : `station` (compte hébergé par un plan de cette station) ou `lien` (lien signé des deux clés).
    pub via: String,
}

/// Plafond de clés par personne : une personne n'a pas des centaines d'UPlanet.
const MAX_KEYS: usize = 16;

/// Annuaire des clés d'une personne.
pub struct PersonKeys {
    db: Arc<Db>,
    plane: String,
    siblings: Vec<Sibling>,
    keys: Arc<dyn KeySource>,
    publish: PublishFn,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Email hébergé dans la base `path` (lecture seule, jamais d'écriture dans la base d'un autre plan).
fn hosted_in(path: &std::path::Path, email: &str) -> Option<String> {
    let c = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let _ = c.busy_timeout(std::time::Duration::from_millis(500));
    c.query_row("SELECT hex FROM identities WHERE lower(email) = ?1 AND hosted = 1 AND role IN ('multipass', 'captain')", [email], |r| r.get::<_, String>(0)).optional().ok().flatten().filter(|h| is_hex64(h))
}

impl PersonKeys {
    /// `plane` : nom du plan servi par ce processus ; `siblings` : les autres plans de la station.
    pub fn new(db: Arc<Db>, plane: String, siblings: Vec<Sibling>, keys: Arc<dyn KeySource>, publish: PublishFn) -> Self {
        Self { db, plane, siblings, keys, publish }
    }

    /// Nom du plan de ce processus.
    pub fn plane(&self) -> &str {
        &self.plane
    }

    /// Plans voisins.
    pub fn siblings(&self) -> &[Sibling] {
        &self.siblings
    }

    /// Clés hébergées par la station pour cet email : ce plan et les plans voisins.
    pub async fn by_email(&self, email: &str) -> Vec<PlaneKey> {
        let email = email.trim().to_ascii_lowercase();
        if !email.contains('@') {
            return Vec::new();
        }
        let mut out = Vec::new();
        let e = email.clone();
        if let Ok(Some(hex)) = self.db.readers().read(move |c| Ok(c.query_row("SELECT hex FROM identities WHERE lower(email) = ?1 AND hosted = 1 AND role IN ('multipass', 'captain')", [e], |r| r.get::<_, String>(0)).optional()?)).await {
            out.push(PlaneKey { plane: self.plane.clone(), hex, via: "station".into() });
        }
        for s in &self.siblings {
            let (path, e) = (s.db.clone(), email.clone());
            if let Ok(Some(hex)) = tokio::task::spawn_blocking(move || hosted_in(&path, &e)).await {
                out.push(PlaneKey { plane: s.name.clone(), hex, via: "station".into() });
            }
        }
        out
    }

    /// Toutes les clés liées à `hex` par des liens prouvés (fermeture, bornée), `hex` exclu.
    pub async fn linked(&self, hex: &str) -> Vec<PlaneKey> {
        let start = hex.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let mut seen: BTreeSet<String> = BTreeSet::from([start.clone()]);
                let mut todo = vec![start];
                let mut out = Vec::new();
                let mut stmt = c.prepare_cached("SELECT b, plane FROM key_links WHERE a = ?1 UNION SELECT a, '' FROM key_links WHERE b = ?1")?;
                while let Some(k) = todo.pop() {
                    if out.len() >= MAX_KEYS {
                        break;
                    }
                    let rows = stmt.query_map([&k], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                    for (other, plane) in rows {
                        if seen.insert(other.clone()) {
                            out.push(PlaneKey { plane, hex: other.clone(), via: "lien".into() });
                            todo.push(other);
                        }
                    }
                }
                Ok(out)
            })
            .await
            .unwrap_or_default()
    }

    /// Toutes les clés connues d'une personne : celles de ses emails, de la clé déclarée sur sa fiche et les clés liées aux précédentes.
    pub async fn keys_for(&self, emails: &[String], declared: Option<&str>) -> Vec<PlaneKey> {
        let mut all: Vec<PlaneKey> = Vec::new();
        let push = |all: &mut Vec<PlaneKey>, k: PlaneKey| {
            if let Some(known) = all.iter_mut().find(|x| x.hex == k.hex) {
                if known.plane.is_empty() {
                    known.plane = k.plane;
                }
            } else if all.len() < MAX_KEYS {
                all.push(k);
            }
        };
        for e in emails.iter().take(4) {
            for k in self.by_email(e).await {
                push(&mut all, k);
            }
        }
        if let Some(d) = declared.filter(|d| is_hex64(d)) {
            push(&mut all, PlaneKey { plane: String::new(), hex: d.to_owned(), via: "fiche".into() });
        }
        let seeds: Vec<String> = all.iter().map(|k| k.hex.clone()).collect();
        for s in seeds {
            for k in self.linked(&s).await {
                push(&mut all, k);
            }
        }
        // Le plan d'une clé reliée est celui de la station si on le connaît : on complète ce qu'un lien ne dit pas.
        for k in &mut all {
            if k.plane.is_empty() {
                k.plane = self.plane_of(&k.hex).await.unwrap_or_default();
            }
        }
        all.sort_by(|a, b| (a.plane != self.plane, &a.plane, &a.hex).cmp(&(b.plane != self.plane, &b.plane, &b.hex)));
        all
    }

    async fn plane_of(&self, hex: &str) -> Option<String> {
        let h = hex.to_owned();
        if self.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [h], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false) {
            return Some(self.plane.clone());
        }
        for s in &self.siblings {
            let (path, h) = (s.db.clone(), hex.to_owned());
            let found = tokio::task::spawn_blocking(move || {
                let c = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
                c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [h], |r| r.get::<_, i64>(0)).optional().ok().flatten()
            })
            .await
            .ok()
            .flatten();
            if found.is_some() {
                return Some(s.name.clone());
            }
        }
        None
    }

    /// Email d'un compte connu de ce plan (identités hébergées ou non).
    pub async fn email_of(&self, hex: &str) -> Option<String> {
        let h = hex.to_owned();
        let here = self.db.readers().read(move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1", [h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten().filter(|e| e.contains('@'));
        if here.is_some() {
            return here;
        }
        for s in &self.siblings {
            let (path, h) = (s.db.clone(), hex.to_owned());
            let found = tokio::task::spawn_blocking(move || {
                let c = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
                c.query_row("SELECT email FROM identities WHERE hex = ?1 AND hosted = 1", [h], |r| r.get::<_, Option<String>>(0)).optional().ok().flatten().flatten()
            })
            .await
            .ok()
            .flatten()
            .filter(|e| e.contains('@'));
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// La personne derrière `hex` : sa clé d'abord, puis ses autres clés (liens prouvés, et comptes de même email dans les plans de la station).
    pub async fn class_of(&self, hex: &str) -> Vec<PlaneKey> {
        let emails: Vec<String> = self.email_of(hex).await.into_iter().collect();
        let mut keys = self.keys_for(&emails, Some(hex)).await;
        if let Some(i) = keys.iter().position(|k| k.hex == hex) {
            let me = keys.remove(i);
            keys.insert(0, me);
        } else {
            keys.insert(0, PlaneKey { plane: String::new(), hex: hex.to_owned(), via: "fiche".into() });
        }
        keys
    }

    /// Conversations des **autres** plans pour le compte de même email (lecture seule) : `[{plane, peer, label, last, at, mine, unread}]`.
    pub async fn sibling_conversations(&self, email: &str) -> Vec<Value> {
        let email = email.trim().to_ascii_lowercase();
        let mut out = Vec::new();
        for s in &self.siblings {
            let (path, e, plane) = (s.db.clone(), email.clone(), s.name.clone());
            let rows = tokio::task::spawn_blocking(move || -> Option<Vec<Value>> {
                let c = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
                let _ = c.busy_timeout(std::time::Duration::from_millis(500));
                let owner: String = c.query_row("SELECT hex FROM identities WHERE lower(email) = ?1 AND hosted = 1 AND role IN ('multipass', 'captain')", [&e], |r| r.get(0)).ok()?;
                let mut stmt = c
                    .prepare(
                        "SELECT m.peer, m.text, m.at, m.mine, (SELECT COUNT(*) FROM chat_messages u WHERE u.owner_hex = m.owner_hex AND u.peer = m.peer AND u.mine = 0 AND u.read = 0), \
                                (SELECT json_extract(k.content, '$.name') FROM nostr_events k WHERE k.pubkey = m.peer AND k.kind = 0 ORDER BY k.created_at DESC LIMIT 1) \
                         FROM chat_messages m WHERE m.owner_hex = ?1 AND m.at = (SELECT MAX(x.at) FROM chat_messages x WHERE x.owner_hex = m.owner_hex AND x.peer = m.peer) GROUP BY m.peer ORDER BY m.at DESC LIMIT 100",
                    )
                    .ok()?;
                let rows = stmt
                    .query_map([&owner], |r| {
                        let peer: String = r.get(0)?;
                        let label: Option<String> = r.get(5)?;
                        Ok(json!({ "plane": plane, "peer": peer, "last": r.get::<_, String>(1)?.chars().take(120).collect::<String>(), "at": r.get::<_, i64>(2)?, "mine": r.get::<_, i64>(3)? == 1, "unread": r.get::<_, i64>(4)?,
                                   "label": label.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| peer.chars().take(8).collect()) }))
                    })
                    .ok()?
                    .filter_map(Result::ok)
                    .collect();
                Some(rows)
            })
            .await
            .ok()
            .flatten();
            out.extend(rows.unwrap_or_default());
        }
        out
    }

    /// Les clés de `hex` forment-elles une même personne avec `other` ? (même clé ou lien prouvé)
    pub async fn same_person(&self, hex: &str, other: &str) -> bool {
        hex == other || self.linked(hex).await.iter().any(|k| k.hex == other)
    }

    /// Annonce, sur demande du titulaire, que ses comptes des autres plans de cette station sont les siens : pour chacun, un lien signé
    /// des deux clés est publié sur le relai de ce plan. Rend `{ linked: [{plane, key}], skipped: [{plane, why}] }`.
    pub async fn announce(&self, owner: &str, email: &str) -> Result<Value, String> {
        let me = self.keys.secret(owner).ok_or("clé du compte indisponible sur cette station")?;
        let (mut linked, mut skipped) = (Vec::new(), Vec::new());
        for s in &self.siblings {
            let (path, e) = (s.db.clone(), email.trim().to_ascii_lowercase());
            let Some(other) = tokio::task::spawn_blocking(move || hosted_in(&path, &e)).await.ok().flatten() else {
                skipped.push(json!({ "plane": s.name, "why": "pas de MULTIPASS avec cet email dans ce plan" }));
                continue;
            };
            let Some(game) = &s.game else {
                skipped.push(json!({ "plane": s.name, "why": "dossier des comptes du plan inconnu" }));
                continue;
            };
            let Some(theirs) = FileKeySource::new(game.join("nostr")).secret(&other) else {
                skipped.push(json!({ "plane": s.name, "why": "clé du compte illisible dans ce plan" }));
                continue;
            };
            let at = now();
            let proof = UnsignedEvent { created_at: at, kind: kind::APP_PARAMS, tags: vec![vec!["d".into(), astro_sync::projector::PLANE_LINK_PROOF.into()], vec!["p".into(), owner.to_owned()]], content: String::new() }.sign(&theirs).map_err(|e| e.to_string())?;
            let content = serde_json::to_string(&proof).map_err(|e| e.to_string())?;
            let link = UnsignedEvent { created_at: at, kind: kind::APP_PARAMS, tags: vec![vec!["d".into(), astro_sync::projector::PLANE_LINK.into()], vec!["p".into(), other.clone()], vec!["plane".into(), s.name.clone()]], content }.sign(&me).map_err(|e| e.to_string())?;
            (self.publish)(link).await.map_err(|e| format!("publication du lien vers {} : {e}", s.name))?;
            linked.push(json!({ "plane": s.name, "key": other }));
        }
        Ok(json!({ "linked": linked, "skipped": skipped }))
    }
}
