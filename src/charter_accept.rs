//! Acceptation de la charte de l'UPlanet par les Capitaines.
//!
//! Un Capitaine qui fonde ou rejoint une constellation **signe** la version courante de la charte : un événement kind 31903
//! `d = charter-accept` (paramètre d'application, remplaçable : la dernière acceptation du Capitaine fait foi), signé de sa clé,
//! portant l'empreinte SHA-256 du texte (`x`), son numéro de version (`v`) et la clé de la coopérative (`u`, qui désigne la
//! constellation). L'acceptation est publique : la signature en est la preuve, il n'y a rien à chiffrer.
//!
//! Une nouvelle version de la charte a une autre empreinte : les acceptations de l'ancienne ne comptent plus et chaque Capitaine
//! est invité à signer de nouveau. Seuls comptent les auteurs que cette station connaît comme Capitaines.

use std::sync::Arc;

use astro_crypto::KeySource;
use astro_db::Db;
use astro_nostr_proto::{UnsignedEvent, kind};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::coop_config;
use crate::register::PublishFn;

/// `d-tag` des acceptations.
pub const D_TAG: &str = "charter-accept";

/// Un Capitaine et sa dernière acceptation : clé, hébergé ici, empreinte signée, version, date.
type Row = (String, bool, Option<String>, Option<String>, Option<i64>);

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Le service d'acceptation, pour la constellation `uplanet_name`.
pub struct CharterAccept {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    publish: PublishFn,
    uplanet_name: String,
}

impl CharterAccept {
    /// Service signant avec `keys` et publiant par `publish`.
    #[must_use]
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, publish: PublishFn, uplanet_name: String) -> Self {
        Self { db, keys, publish, uplanet_name }
    }

    /// Charte courante : (clé de la coopérative, charte).
    async fn current(&self) -> Result<(String, coop_config::Charter), String> {
        let (_, coop_hex) = coop_config::coop_keys(&self.uplanet_name)?;
        let charter = coop_config::charter_of(coop_config::load(&self.db, &coop_hex).await.as_ref());
        Ok((coop_hex, charter))
    }

    /// Le Capitaine `captain` signe la charte d'empreinte `sha256`. Refusé si ce n'est plus la version courante (la charte a
    /// changé pendant que la page était ouverte) : on ne signe jamais ce qu'on n'a pas sous les yeux.
    pub async fn accept(&self, captain: &str, sha256: &str) -> Result<Value, String> {
        let (coop_hex, charter) = self.current().await?;
        if sha256 != charter.sha256 {
            return Err("la charte a changé depuis l'affichage : relisez la version courante avant de la signer".into());
        }
        let secret = self.keys.secret(captain).ok_or("clé du Capitaine absente de cette station")?;
        let previous = self.latest(&coop_hex, captain).await.map_or(0, |(_, _, at)| at);
        // L'horodatage dépasse toujours celui de l'acceptation précédente : un relai garde le plus récent d'un événement remplaçable.
        let at = now().max(previous + 1);
        let tags = vec![vec!["d".into(), D_TAG.into()], vec!["x".into(), charter.sha256.clone()], vec!["v".into(), charter.version.clone()], vec!["u".into(), coop_hex]];
        let event = UnsignedEvent { created_at: at, kind: kind::APP_PARAMS, tags, content: String::new() }.sign(&secret).map_err(|e| e.to_string())?;
        (self.publish)(event).await.map_err(|e| format!("acceptation non publiée : {e}"))?;
        Ok(json!({ "version": charter.version, "sha256": charter.sha256, "at": at }))
    }

    async fn latest(&self, coop_hex: &str, captain: &str) -> Option<(String, String, u64)> {
        let (u, c) = (coop_hex.to_owned(), captain.to_owned());
        self.db
            .readers()
            .read(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT (SELECT value FROM nostr_tags WHERE event_id = e.seq AND name = 'x' LIMIT 1), (SELECT value FROM nostr_tags WHERE event_id = e.seq AND name = 'v' LIMIT 1), e.created_at
                         FROM nostr_events e JOIN nostr_tags d ON d.event_id = e.seq AND d.name = 'd' AND d.value = ?3 JOIN nostr_tags u ON u.event_id = e.seq AND u.name = 'u' AND u.value = ?1
                         WHERE e.kind = ?4 AND e.pubkey = ?2 ORDER BY e.created_at DESC LIMIT 1",
                        rusqlite::params![u, c, D_TAG, kind::APP_PARAMS],
                        |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<String>>(1)?.unwrap_or_default(), r.get::<_, i64>(2)?.max(0) as u64)),
                    )
                    .optional()?)
            })
            .await
            .ok()
            .flatten()
    }

    /// Capitaines connus de cette station et état de leur acceptation de la charte courante : `current` = a signé cette version,
    /// sinon `version` donne la dernière qu'il a signée (ou `null`). `mine` : c'est le lecteur. Aucune adresse email n'est rendue.
    pub async fn signatories(&self, viewer: &str) -> Result<Value, String> {
        let (coop_hex, charter) = self.current().await?;
        let rows: Vec<Row> = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT i.hex, i.hosted,
                            (SELECT (SELECT value FROM nostr_tags WHERE event_id = e.seq AND name = 'x' LIMIT 1) FROM nostr_events e JOIN nostr_tags d ON d.event_id = e.seq AND d.name = 'd' AND d.value = ?1 JOIN nostr_tags u ON u.event_id = e.seq AND u.name = 'u' AND u.value = ?2 WHERE e.kind = ?3 AND e.pubkey = i.hex ORDER BY e.created_at DESC LIMIT 1),
                            (SELECT (SELECT value FROM nostr_tags WHERE event_id = e.seq AND name = 'v' LIMIT 1) FROM nostr_events e JOIN nostr_tags d ON d.event_id = e.seq AND d.name = 'd' AND d.value = ?1 JOIN nostr_tags u ON u.event_id = e.seq AND u.name = 'u' AND u.value = ?2 WHERE e.kind = ?3 AND e.pubkey = i.hex ORDER BY e.created_at DESC LIMIT 1),
                            (SELECT e.created_at FROM nostr_events e JOIN nostr_tags d ON d.event_id = e.seq AND d.name = 'd' AND d.value = ?1 JOIN nostr_tags u ON u.event_id = e.seq AND u.name = 'u' AND u.value = ?2 WHERE e.kind = ?3 AND e.pubkey = i.hex ORDER BY e.created_at DESC LIMIT 1)
                     FROM identities i WHERE i.role = 'captain' ORDER BY i.hosted DESC, i.hex",
                )?;
                let rows = st.query_map(rusqlite::params![D_TAG, coop_hex, kind::APP_PARAMS], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? == 1, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| e.to_string())?;
        let list: Vec<Value> = rows
            .into_iter()
            .map(|(hex, hosted, sha, version, at)| json!({ "captain": format!("{}…{}", &hex[..8.min(hex.len())], &hex[hex.len().saturating_sub(4)..]), "here": hosted, "mine": hex == viewer, "current": sha.as_deref() == Some(charter.sha256.as_str()), "version": version, "at": at }))
            .collect();
        Ok(json!({ "version": charter.version, "list": list }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_crypto::StaticKeySource;
    use astro_nostr_proto::Event;

    use super::*;

    const UP: &str = "UPlanetTestName";
    const SECRET: [u8; 32] = [7; 32];

    fn hex_of(secret: &[u8; 32]) -> String {
        astro_nostr_proto::pubkey_of(secret).unwrap()
    }

    async fn rig(tag: &str) -> (std::path::PathBuf, Arc<Db>, CharterAccept, Arc<Mutex<Vec<Event>>>, String) {
        let dir = std::env::temp_dir().join(format!("astroportd-charter-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let me = hex_of(&SECRET);
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'captain', 'cap@exemple.org', 1)", vec![me.clone().into()]).await.unwrap();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (db2, sent2) = (db.clone(), sent.clone());
        let publish: PublishFn = Arc::new(move |e: Event| {
            let (db, sent) = (db2.clone(), sent2.clone());
            Box::pin(async move {
                sent.lock().unwrap().push(e.clone());
                db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &e).map(|_| ())).await.map_err(|x| x.to_string())
            })
        });
        let svc = CharterAccept::new(db.clone(), Arc::new(StaticKeySource::new().with(SECRET)), publish, UP.into());
        (dir, db, svc, sent, me)
    }

    #[tokio::test]
    async fn a_captain_signs_the_current_charter_and_a_new_version_asks_again() {
        let (dir, db, svc, sent, me) = rig("accept").await;
        let (coop_hex, charter) = svc.current().await.unwrap();
        assert!(!charter.custom, "texte type tant que rien n'est publié");
        let before = svc.signatories(&me).await.unwrap();
        assert_eq!((before["list"][0]["current"].as_bool(), before["list"][0]["mine"].as_bool()), (Some(false), Some(true)));

        let out = svc.accept(&me, &charter.sha256).await.unwrap();
        assert_eq!(out["version"], "1.0");
        let ev = sent.lock().unwrap()[0].clone();
        assert_eq!((ev.kind, ev.pubkey.as_str(), ev.tag_value("d"), ev.tag_value("x"), ev.tag_value("u")), (31903, me.as_str(), Some("charter-accept"), Some(charter.sha256.as_str()), Some(coop_hex.as_str())));
        assert!(ev.content.is_empty() && ev.verify().is_ok(), "publique et bien signée");
        let after = svc.signatories(&me).await.unwrap();
        assert_eq!(after["list"][0]["current"], true);
        assert!(!after.to_string().contains("cap@exemple.org"), "aucune adresse email");

        // Le Capitaine de la coopérative publie une nouvelle version : l'ancienne acceptation ne compte plus.
        let mut patch = std::collections::BTreeMap::new();
        patch.insert("CHARTER_VERSION".to_owned(), Some("2.0".to_owned()));
        patch.insert("CHARTER_TEXT".to_owned(), Some("Nouvelle charte.".to_owned()));
        let publisher = Arc::new(Store(db.clone()));
        coop_config::save(&db, &publisher, UP, &patch).await.unwrap();
        let v2 = svc.signatories(&me).await.unwrap();
        assert_eq!((v2["version"].as_str(), v2["list"][0]["current"].as_bool(), v2["list"][0]["version"].as_str()), (Some("2.0"), Some(false), Some("1.0")));
        let err = svc.accept(&me, &charter.sha256).await.unwrap_err();
        assert!(err.contains("a changé"), "on ne signe pas ce qu'on n'a pas sous les yeux : {err}");
        let (_, c2) = svc.current().await.unwrap();
        svc.accept(&me, &c2.sha256).await.unwrap();
        assert_eq!(svc.signatories(&me).await.unwrap()["list"][0]["current"], true);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn only_a_captain_whose_key_is_here_can_sign_and_strangers_are_ignored() {
        let (dir, db, svc, _, me) = rig("keys").await;
        let (_, charter) = svc.current().await.unwrap();
        assert!(svc.accept(&"ab".repeat(32), &charter.sha256).await.unwrap_err().contains("absente"));
        // Un auteur qui n'est pas un Capitaine connu ne figure pas dans la liste, même s'il a publié une acceptation.
        let (_, coop_hex) = coop_config::coop_keys(UP).unwrap();
        let stranger = [9u8; 32];
        let ev = UnsignedEvent { created_at: 5, kind: 31903, tags: vec![vec!["d".into(), D_TAG.into()], vec!["x".into(), charter.sha256.clone()], vec!["u".into(), coop_hex]], content: String::new() }.sign(&stranger).unwrap();
        db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &ev).map(|_| ())).await.unwrap();
        let list = svc.signatories(&me).await.unwrap();
        assert_eq!(list["list"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Publieur qui range l'événement dans la base, comme le fait le relai local.
    struct Store(Arc<Db>);

    impl astro_bro::publisher::Publisher for Store {
        async fn publish(&self, event: Event) -> Result<(), String> {
            self.0.writer().transaction(move |tx| astro_db::events::insert_event(tx, &event).map(|_| ())).await.map_err(|e| e.to_string())
        }
    }
}
