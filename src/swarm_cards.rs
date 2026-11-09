//! **Cartes de station** échangées par le maillage, et dossier `~/.zen/tmp/swarm/<PeerID>/12345.json` tenu par astroportd.
//!
//! Astroport.ONE remplit ce dossier en lisant les `12345.json` publiés sur IPNS ; une station astroportd seule n'avait
//! donc aucune station de l'essaim à interroger (synchronisation N¹/N², envoi des partages, réplication, routage). Ici :
//!
//! - chaque station répond sur le protocole [`PROTOCOL`] par sa carte : un `12345.json` réduit, avec les **mêmes noms de
//!   champs** que `_12345.sh` (`ipfsnodeid`, `hostname`, `captainHEX`, `NODEHEX`, `myRELAY`, `STATION_LAT`/`STATION_LON`
//!   arrondis à la case UMAP), plus `p2p_addrs` (ses adresses de maillage) et `_source: "astroportd"` ;
//! - toutes les [`EVERY_SECS`], la station demande sa carte à chaque station **connectée** et l'écrit dans le dossier.
//!   La réponse vient de la connexion chiffrée : une station ne peut décrire qu'elle-même.
//!
//! Un `12345.json` écrit par Astroport.ONE (sans `_source: "astroportd"`) n'est **jamais** remplacé : il est plus complet.
//! Une carte écrite ici et non renouvelée depuis [`STALE_SECS`] est retirée. Avec une `swarm.key`, seules les stations de
//! la même constellation sont connectées, donc décrites.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use astro_db::Db;
use astro_p2p::{P2pHandle, PeerId};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

/// Protocole de la carte de station.
pub const PROTOCOL: &str = "/astroport/station/1";
/// Marque des cartes écrites par astroportd.
pub const SOURCE: &str = "astroportd";
/// Relevé des stations connectées.
pub const EVERY_SECS: u64 = 300;
/// Une carte non renouvelée depuis 7 jours est retirée.
pub const STALE_SECS: u64 = 7 * 86_400;
/// Délai d'une réponse.
const ASK_TIMEOUT: Duration = Duration::from_secs(4);

/// Ce que la station dit d'elle-même.
pub struct Card {
    /// Base `uplanet.db` (clé du Capitaine, lue à chaque demande : il peut arriver après le démarrage).
    pub db: Arc<Db>,
    /// Maillage.
    pub handle: P2pHandle,
    /// Nom de machine.
    pub hostname: String,
    /// Clé Nostr du NODE.
    pub node_hex: String,
    /// Relai public (`wss://<domaine>`), si la station a un domaine.
    pub my_relay: Option<String>,
    /// Position, publiée arrondie à la case UMAP.
    pub origin: Option<(f64, f64)>,
}

impl Card {
    /// Carte au format `12345.json`.
    pub async fn json(&self) -> Value {
        let captain: Option<String> = self
            .db
            .readers()
            .read(|c| Ok(c.query_row("SELECT hex FROM identities WHERE hosted = 1 AND role = 'captain' ORDER BY hex LIMIT 1", [], |r| r.get::<_, String>(0)).optional()?))
            .await
            .ok()
            .flatten();
        let me = self.handle.peer_id();
        // Adresses de réseau local comprises : une station du même réseau les utilise ; les autres échouent sans dommage.
        let addrs: Vec<String> = self
            .handle
            .listen_addrs()
            .iter()
            .map(ToString::to_string)
            .filter(|a| !a.contains("/127.0.0.1/") && !a.contains("/::1/"))
            .map(|a| if a.ends_with(&format!("/p2p/{me}")) { a } else { format!("{a}/p2p/{me}") })
            .collect();
        let mut card = json!({
            "ipfsnodeid": me.to_string(),
            "hostname": self.hostname,
            "NODEHEX": self.node_hex,
            "p2p_addrs": addrs,
            "_source": SOURCE,
            "version": env!("CARGO_PKG_VERSION"),
            "created": now(),
        });
        if let Some(c) = captain {
            card["captainHEX"] = json!(c);
        }
        if let Some(r) = &self.my_relay {
            card["myRELAY"] = json!(r);
        }
        if let Some((lat, lon)) = self.origin {
            card["STATION_LAT"] = json!(crate::beacon::umap_coord(lat));
            card["STATION_LON"] = json!(crate::beacon::umap_coord(lon));
        }
        card
    }

    /// Répond aux stations qui demandent la carte.
    pub fn serve(self: &Arc<Self>) -> Result<astro_p2p::ask::Answers, String> {
        let me = self.clone();
        let f: astro_p2p::ask::AnswerFn = Arc::new(move |_peer| {
            let me = me.clone();
            Box::pin(async move { serde_json::to_vec(&me.json().await).unwrap_or_default() })
        });
        astro_p2p::ask::answer(&self.handle, PROTOCOL, f).map_err(|e| e.to_string())
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Carte reçue de `peer` : un objet JSON qui se dit être `peer`. Rend le texte à écrire.
pub fn accept(peer: &PeerId, body: &[u8]) -> Result<String, String> {
    let mut v: Value = serde_json::from_slice(body).map_err(|e| format!("carte illisible : {e}"))?;
    let obj = v.as_object_mut().ok_or("carte : objet JSON attendu")?;
    if obj.get("ipfsnodeid").and_then(Value::as_str) != Some(peer.to_string().as_str()) {
        return Err("carte : ipfsnodeid ne correspond pas à la station qui répond".into());
    }
    obj.insert("_source".into(), json!(SOURCE));
    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
}

/// Le `12345.json` de `dir` a-t-il été écrit par astroportd (ou est-il absent) ?
fn ours_or_absent(file: &Path) -> bool {
    match std::fs::read_to_string(file) {
        Ok(t) => serde_json::from_str::<Value>(&t).ok().and_then(|v| v.get("_source").and_then(Value::as_str).map(|s| s == SOURCE)).unwrap_or(false),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Écrit la carte de `peer` dans `swarm/<peer>/12345.json`, sauf si Astroport.ONE y a mis la sienne. **Bloquant.**
pub fn write(swarm: &Path, peer: &PeerId, text: &str) -> Result<bool, String> {
    let dir = swarm.join(peer.to_string());
    let file = dir.join("12345.json");
    if !ours_or_absent(&file) {
        return Ok(false);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    let tmp = dir.join(".12345.json.astroportd");
    std::fs::write(&tmp, text).map_err(|e| format!("{} : {e}", tmp.display()))?;
    std::fs::rename(&tmp, &file).map_err(|e| format!("{} : {e}", file.display()))?;
    Ok(true)
}

/// Retire les cartes écrites par astroportd et non renouvelées depuis `max_age`. **Bloquant.** Rend leur nombre.
pub fn prune(swarm: &Path, max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(swarm) else { return 0 };
    let mut n = 0;
    for e in entries.flatten() {
        let file = e.path().join("12345.json");
        let old = std::fs::metadata(&file).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > max_age);
        if old && ours_or_absent(&file) && std::fs::remove_file(&file).is_ok() {
            let _ = std::fs::remove_dir(e.path());
            n += 1;
        }
    }
    n
}

/// Tient le dossier : demande sa carte à chaque station connectée, toutes les [`EVERY_SECS`] (la première fois après 30 s).
pub async fn keep(handle: P2pHandle, swarm: PathBuf, stop: tokio::sync::watch::Receiver<bool>) {
    keep_every(handle, swarm, Duration::from_secs(30), Duration::from_secs(EVERY_SECS), stop).await;
}

/// [`keep`], avec le premier délai et le pas donnés.
pub async fn keep_every(handle: P2pHandle, swarm: PathBuf, first: Duration, every: Duration, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut wait = first;
    loop {
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            _ = stop.changed() => return,
        }
        wait = every;
        for peer in handle.connected_peers() {
            let body = match astro_p2p::ask::ask(&handle, peer, PROTOCOL, ASK_TIMEOUT).await {
                Ok(b) => b,
                // Une station qui ne connaît pas le protocole (version ancienne) ou un relais qui ne répond pas : rien à écrire.
                Err(_) => continue,
            };
            let text = match accept(&peer, &body) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("essaim : {peer} : {e}");
                    continue;
                }
            };
            let dir = swarm.clone();
            match tokio::task::spawn_blocking(move || write(&dir, &peer, &text)).await {
                Ok(Err(e)) => eprintln!("essaim : carte de {peer} non écrite : {e}"),
                Err(e) => eprintln!("essaim : {e}"),
                Ok(Ok(_)) => {}
            }
        }
        let dir = swarm.clone();
        let _ = tokio::task::spawn_blocking(move || prune(&dir, Duration::from_secs(STALE_SECS))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("astroportd-swarm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_card_describes_only_the_station_that_answers_and_never_replaces_a_legacy_file() {
        let peer: PeerId = "12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG".parse().unwrap();
        let other = "12D3KooWKnDdG3iXw9eTFijk3EWSunZcFi54Zka4wmtqtt6rPxc8";
        assert!(accept(&peer, format!(r#"{{"ipfsnodeid":"{other}"}}"#).as_bytes()).is_err(), "une station ne décrit qu'elle-même");
        assert!(accept(&peer, b"[]").is_err() && accept(&peer, b"pas du json").is_err());
        let text = accept(&peer, format!(r#"{{"ipfsnodeid":"{peer}","hostname":"libra","NODEHEX":"{}"}}"#, "ab".repeat(32)).as_bytes()).unwrap();

        let swarm = tmp("w");
        assert!(write(&swarm, &peer, &text).unwrap());
        let read = astro_sync::read_peers(&swarm);
        assert_eq!((read.len(), read[0].hostname.as_deref(), read[0].node_id.as_str()), (1, Some("libra"), peer.to_string().as_str()), "lisible comme un 12345.json d'Astroport.ONE");
        assert!(write(&swarm, &peer, &text).unwrap(), "une carte d'astroportd se renouvelle");

        let legacy = swarm.join(other);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("12345.json"), r#"{"hostname":"legacy"}"#).unwrap();
        let other: PeerId = other.parse().unwrap();
        assert!(!write(&swarm, &other, &text).unwrap(), "le fichier d'Astroport.ONE reste intact");
        assert_eq!(std::fs::read_to_string(legacy.join("12345.json")).unwrap(), r#"{"hostname":"legacy"}"#);

        assert_eq!(prune(&swarm, Duration::from_secs(3600)), 0, "cartes récentes gardées");
        assert_eq!(prune(&swarm, Duration::ZERO), 1, "seule la carte d'astroportd vieillie part");
        assert!(legacy.join("12345.json").exists());
        let _ = std::fs::remove_dir_all(&swarm);
    }
}
