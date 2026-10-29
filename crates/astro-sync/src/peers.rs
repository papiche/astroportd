//! Stations de la constellation, telles que leurs `12345.json` les décrivent (`~/.zen/tmp/swarm/<IPFSNODEID>/`).
//!
//! Le relai d'un pair se joint **par le maillage** : le tunnel `ipfs p2p` `/x/strfry-<IPFSNODEID>` (le nom que
//! `DRAGON_p2p_ssh.sh` et `backfill_constellation.sh` utilisent déjà). Aucun port n'est ouvert sur la box de l'hôte et
//! la constellation privée (swarm.key) n'a pas besoin d'adresse publique. L'adresse publique du relai (`myRELAY`),
//! quand elle existe, ne sert que de **repli**, et le résultat dit par quelle voie il est passé.

use std::path::Path;

use astro_nostr_proto::is_hex64;
use serde_json::Value;

/// Nom du service P2P qui expose le relai d'une station.
pub const RELAY_SERVICE: &str = "strfry";
/// Préfixe d'une cible joignable par le maillage.
pub const P2P_SCHEME: &str = "p2p://";

/// Une station de la constellation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// `IPFSNODEID` : PeerID libp2p de la station.
    pub node_id: String,
    /// Nom de machine.
    pub hostname: Option<String>,
    /// Adresse publique du relai (`myRELAY`), si la station en a une.
    pub relay: Option<String>,
    /// Clé Nostr du Capitaine de la station.
    pub captain_hex: Option<String>,
    /// Clé Nostr du NODE.
    pub node_hex: Option<String>,
    /// Adresse du Capitaine.
    pub captain_email: Option<String>,
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

impl Peer {
    /// Lit un `12345.json`. Un identifiant de clé qui n'est pas du hexadécimal sur 64 caractères est ignoré.
    pub fn from_station_json(node_id: &str, json: &Value) -> Self {
        let hex = |k: &str| text(json, k).filter(|h| is_hex64(h));
        Self {
            node_id: node_id.to_owned(),
            hostname: text(json, "hostname"),
            relay: text(json, "myRELAY").filter(|u| u.starts_with("ws://") || u.starts_with("wss://")),
            captain_hex: hex("captainHEX"),
            node_hex: hex("NODEHEX"),
            captain_email: text(json, "captain"),
        }
    }

    /// Cible de crawl : le tunnel P2P, puis l'adresse publique en repli (`p2p://<PeerID>|wss://…`).
    pub fn target(&self) -> String {
        match &self.relay {
            Some(url) => format!("{P2P_SCHEME}{}|{url}", self.node_id),
            None => format!("{P2P_SCHEME}{}", self.node_id),
        }
    }

    /// Nom lisible : machine, sinon début de l'identifiant.
    pub fn label(&self) -> String {
        self.hostname.clone().unwrap_or_else(|| self.node_id.chars().take(12).collect())
    }
}

/// Lit les `<dossier>/<node_id>/12345.json`. Un dossier absent ou un fichier illisible est ignoré. **Bloquant** :
/// à appeler depuis `spawn_blocking`.
pub fn read_peers(dir: &Path) -> Vec<Peer> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut peers: Vec<Peer> = entries
        .flatten()
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path().join("12345.json")).ok()?;
            let json = serde_json::from_str::<Value>(&text).ok()?;
            Some(Peer::from_station_json(&e.file_name().to_string_lossy(), &json))
        })
        .collect();
    peers.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    peers
}

/// Cible décomposée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target<'a> {
    /// PeerID du pair, pour une cible `p2p://`.
    pub peer: Option<&'a str>,
    /// Adresse WebSocket directe (cible ordinaire) ou de repli.
    pub url: Option<&'a str>,
}

/// Décompose `p2p://<PeerID>[|<url>]` ou une adresse `ws(s)://…`.
pub fn parse_target(s: &str) -> Target<'_> {
    match s.strip_prefix(P2P_SCHEME) {
        Some(rest) => match rest.split_once('|') {
            Some((peer, url)) => Target { peer: Some(peer), url: Some(url).filter(|u| !u.is_empty()) },
            None => Target { peer: Some(rest), url: None },
        },
        None => Target { peer: None, url: Some(s) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(extra: &str) -> Value {
        serde_json::from_str(&format!(r#"{{"hostname":"sagittarius.localhost","myRELAY":"wss://relay.sagittarius.copylaradio.com","captain":"support@qo-op.com","captainHEX":"{}","NODEHEX":"{}"{extra}}}"#, "d8".repeat(32), "e9".repeat(32))).unwrap()
    }

    #[test]
    fn a_station_file_is_read_and_its_target_goes_through_the_mesh_first() {
        let p = Peer::from_station_json("12D3KooWAAA", &json(""));
        assert_eq!((p.hostname.as_deref(), p.captain_email.as_deref()), (Some("sagittarius.localhost"), Some("support@qo-op.com")));
        assert_eq!(p.captain_hex, Some("d8".repeat(32)));
        assert_eq!(p.target(), "p2p://12D3KooWAAA|wss://relay.sagittarius.copylaradio.com");
        let no_public = Peer::from_station_json("12D3KooWBBB", &serde_json::json!({ "hostname": "rpi" }));
        assert_eq!(no_public.target(), "p2p://12D3KooWBBB", "sans adresse publique : le tunnel seul");
        assert_eq!(no_public.label(), "rpi");
    }

    #[test]
    fn bad_keys_and_non_websocket_relays_are_ignored() {
        let p = Peer::from_station_json("x", &serde_json::json!({ "captainHEX": "pas-un-hex", "NODEHEX": "ab", "myRELAY": "http://x" }));
        assert_eq!((p.captain_hex, p.node_hex, p.relay), (None, None, None));
    }

    #[test]
    fn targets_are_parsed_both_ways() {
        assert_eq!(parse_target("p2p://12D3A|wss://r.example"), Target { peer: Some("12D3A"), url: Some("wss://r.example") });
        assert_eq!(parse_target("p2p://12D3A"), Target { peer: Some("12D3A"), url: None });
        assert_eq!(parse_target("ws://127.0.0.1:7777"), Target { peer: None, url: Some("ws://127.0.0.1:7777") });
    }

    #[test]
    fn peers_are_read_from_a_swarm_directory_sorted_by_id() {
        let dir = std::env::temp_dir().join(format!("astro-peers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for id in ["12D3B", "12D3A"] {
            std::fs::create_dir_all(dir.join(id)).unwrap();
            std::fs::write(dir.join(id).join("12345.json"), json("").to_string()).unwrap();
        }
        std::fs::create_dir_all(dir.join("12D3C")).unwrap(); // sans 12345.json : ignoré
        std::fs::create_dir_all(dir.join("12D3D")).unwrap();
        std::fs::write(dir.join("12D3D").join("12345.json"), "pas du json").unwrap();
        assert_eq!(read_peers(&dir).iter().map(|p| p.node_id.as_str()).collect::<Vec<_>>(), ["12D3A", "12D3B"]);
        assert!(read_peers(&dir.join("absent")).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
