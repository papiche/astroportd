//! Maillage P2P vu par le Capitaine : sa station (adresses à donner), les stations jointes, et l'ajout **à chaud** d'une
//! station (adresse `/ip4/…/tcp/…/p2p/<PeerID>`), gardée ou non comme « bootstrap » (rejointe à chaque démarrage et
//! rappelée si la liaison tombe).
//!
//! **Relais et découverte automatiques.** Toutes les minutes, la station joint les **stations relais** de sa constellation
//! (`--p2p-use-relay` et `P2P_RELAYS` de la configuration coopérative) et y tient une place réservée, refaite si elle se
//! perd (relais redémarré, coupure) : derrière une box, elle reste joignable par le relais. Toutes les 5 minutes, elle
//! rappelle les stations gardées et joint les stations sœurs que l'annuaire des balises (`p2p:addrs`) lui fait connaître.
//! Sur le réseau local, mDNS relie les stations sans adresse (voir `astro_p2p`).
//!
//! Multi-UPlanet : chaque plan a son propre maillage (sa `swarm.key`, son port) et sa propre base, donc sa propre liste de
//! bootstrap. Une station d'une autre UPlanet ne peut pas être jointe : la clé de réseau privé diffère.

use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_p2p::multiaddr::Protocol;
use astro_p2p::{Multiaddr, P2pHandle, PeerId};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

const SCOPE: &str = "options";
const KEY: &str = "p2p_bootstrap";
/// Stations gardées au plus.
pub const MAX_KEPT: usize = 32;
/// Port libp2p par défaut (celui de DRAGON).
pub const DEFAULT_PORT: u16 = 4101;
/// Stations de l'annuaire jointes à chaque passage, au plus.
pub const MAX_DIRECTORY_DIALS: usize = 32;
/// Pas de la surveillance des relais.
const RELAY_TICK: Duration = Duration::from_secs(60);

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Adresse d'une station : multiadresse qui finit par `/p2p/<PeerID>`. Rend l'adresse et le PeerID.
pub fn parse_station(addr: &str) -> Result<(Multiaddr, PeerId), String> {
    let addr: Multiaddr = addr.trim().parse().map_err(|_| "adresse illisible : attendu /ip4/…/tcp/4101/p2p/12D3Koo…".to_owned())?;
    let peer = match addr.iter().last() {
        Some(Protocol::P2p(p)) => p,
        _ => return Err("l'adresse doit finir par /p2p/<PeerID> de la station".into()),
    };
    Ok((addr, peer))
}

/// Premier port TCP libre à partir de [`DEFAULT_PORT`] (sur toutes les interfaces), pour l'écoute par défaut.
pub fn free_port() -> Option<u16> {
    (DEFAULT_PORT..DEFAULT_PORT + 100).find(|p| std::net::TcpListener::bind(("0.0.0.0", *p)).is_ok())
}

/// Stations gardées comme bootstrap (base de ce plan).
pub async fn kept(db: &Db) -> Vec<Multiaddr> {
    let raw: Option<String> = db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, KEY], |r| r.get(0)).optional()?)).await.ok().flatten();
    raw.and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok()).unwrap_or_default().iter().filter_map(|a| parse_station(a).ok().map(|(m, _)| m)).take(MAX_KEPT).collect()
}

async fn store(db: &Db, list: &[Multiaddr]) -> Result<(), String> {
    let value = serde_json::to_string(&list.iter().map(ToString::to_string).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    db.writer()
        .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![SCOPE.to_owned().into(), KEY.to_owned().into(), value.into(), now().into()])
        .await
        .map(|_| ())
        .map_err(|e| format!("liste non enregistrée : {e}"))
}

/// Le maillage de la station, pour l'écran du Capitaine.
pub struct Mesh {
    db: Arc<Db>,
    handle: Option<P2pHandle>,
    /// Stations données au démarrage (`--p2p-bootstrap`) : affichées, jamais retirées d'ici.
    cli: Vec<Multiaddr>,
    /// Nom de l'UPlanet (`ORIGIN` ou le nom du plan).
    plane: String,
    activity: ActivityBus,
    geo: Option<Arc<crate::geo_lease::GeoLease>>,
    /// Relais donnés au démarrage (`--p2p-use-relay`).
    relays_cli: Vec<Multiaddr>,
    /// UPlanet dont la configuration coopérative donne les relais (`P2P_RELAYS`).
    coop_uplanet: Option<String>,
    /// Cette station sert elle-même de relais : elle ne réserve pas de place ailleurs.
    is_relay: bool,
    /// Annuaire des balises (adresses des stations sœurs) et réveil de la balise (nouvelle place réservée à annoncer).
    directory: Option<(Arc<crate::beacon::Stations>, Arc<tokio::sync::Notify>)>,
}

impl Mesh {
    /// Bail géographique de la station (quartiers tenus et cédés), montré dans la vue.
    pub fn with_geo(mut self, geo: Arc<crate::geo_lease::GeoLease>) -> Self {
        self.geo = Some(geo);
        self
    }

    /// `handle` : `None` si le maillage est coupé (`--no-p2p`, ou NODE absent).
    pub fn new(db: Arc<Db>, handle: Option<P2pHandle>, cli: Vec<Multiaddr>, plane: String, activity: ActivityBus) -> Self {
        Self { db, handle, cli, plane, activity, geo: None, relays_cli: Vec::new(), coop_uplanet: None, is_relay: false, directory: None }
    }

    /// Relais de la constellation : ceux de `--p2p-use-relay`, et ceux que la coopérative de `uplanet_name` publie.
    pub fn with_relays(mut self, cli: Vec<Multiaddr>, uplanet_name: Option<String>, is_relay: bool) -> Self {
        self.relays_cli = cli;
        self.coop_uplanet = uplanet_name;
        self.is_relay = is_relay;
        self
    }

    /// Annuaire des balises à suivre, et réveil de la balise.
    pub fn with_directory(mut self, stations: Arc<crate::beacon::Stations>, nudge: Arc<tokio::sync::Notify>) -> Self {
        self.directory = Some((stations, nudge));
        self
    }

    /// Relais de la constellation, avec leur source (`démarrage` ou `coopérative`), sans doublon de station.
    pub async fn relays(&self) -> Vec<(Multiaddr, PeerId, &'static str)> {
        let mut out: Vec<(Multiaddr, PeerId, &'static str)> = self.relays_cli.iter().filter_map(|m| parse_station(&m.to_string()).ok().map(|(m, p)| (m, p, "démarrage"))).collect();
        if let Some(name) = &self.coop_uplanet {
            for (m, p) in crate::coop_config::p2p_relays(&self.db, name).await {
                if !out.iter().any(|(_, q, _)| *q == p) {
                    out.push((m, p, "coopérative"));
                }
            }
        }
        out
    }

    /// Vue : ma station, les stations connectées, la liste de bootstrap (démarrage + gardées).
    pub async fn view(&self) -> Value {
        let Some(h) = &self.handle else {
            return json!({ "active": false, "plane": self.plane });
        };
        let me = h.peer_id();
        let connected = h.connected_peers();
        let addrs: Vec<String> = h.listen_addrs().iter().filter(|a| !a.to_string().contains("/127.0.0.1/") && !a.to_string().contains("/::1/")).map(|a| {
                let t = a.to_string();
                if t.ends_with(&format!("/p2p/{me}")) { t } else { format!("{t}/p2p/{me}") }
            })
            .collect();
        let peer_of = |m: &Multiaddr| parse_station(&m.to_string()).ok().map(|(_, p)| p);
        let row = |m: &Multiaddr, source: &str| json!({ "addr": m.to_string(), "source": source, "connected": peer_of(m).is_some_and(|p| connected.contains(&p)) });
        let mut boot: Vec<Value> = self.cli.iter().map(|m| row(m, "démarrage")).collect();
        boot.extend(kept(&self.db).await.iter().filter(|m| !self.cli.contains(m)).map(|m| row(m, "gardée")));
        let reserved = h.reserved_relays();
        let relays: Vec<Value> = self
            .relays()
            .await
            .iter()
            .map(|(m, p, source)| json!({ "addr": m.to_string(), "source": source, "self": *p == me, "connected": connected.contains(p), "reserved": reserved.contains(p) }))
            .collect();
        let geo = match &self.geo {
            Some(g) => g.view().await,
            None => Value::Null,
        };
        json!({
            "active": true, "plane": self.plane, "peer_id": me.to_string(), "addrs": addrs,
            "connected": connected.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "bootstrap": boot, "max_kept": MAX_KEPT, "geo": geo,
            "relay_server": self.is_relay,
            "relays": relays,
        })
    }

    /// `{ dial: <adresse>, keep: bool }` (joindre, et garder si demandé), `{ keep: <adresse> }`, `{ forget: <adresse> }`.
    pub async fn act(&self, b: &Value) -> Result<Value, String> {
        let h = self.handle.as_ref().ok_or("maillage P2P coupé sur cette station")?;
        if let Some(addr) = b["dial"].as_str() {
            let (m, peer) = parse_station(addr)?;
            if peer == h.peer_id() {
                return Err("c'est l'adresse de cette station".into());
            }
            h.dial(m.clone()).await.map_err(|e| format!("station injoignable : {e} (même UPlanet ? port ouvert ?)"))?;
            self.activity.publish_to(Audience::Captain, "system", "maillage", format!("station jointe : {}", peer.to_string().chars().take(16).collect::<String>()), None);
            if b["keep"].as_bool() == Some(true) {
                self.keep(m).await?;
            }
        } else if let Some(addr) = b["keep"].as_str() {
            self.keep(parse_station(addr)?.0).await?;
        } else if let Some(addr) = b["forget"].as_str() {
            let m = parse_station(addr)?.0;
            let mut list = kept(&self.db).await;
            let before = list.len();
            list.retain(|x| *x != m);
            if list.len() == before {
                return Err(if self.cli.contains(&m) { "station donnée au démarrage (--p2p-bootstrap) : retirez l'option".into() } else { "station absente de la liste".into() });
            }
            store(&self.db, &list).await?;
        } else {
            return Err("action attendue : dial, keep ou forget".into());
        }
        Ok(self.view().await)
    }

    async fn keep(&self, m: Multiaddr) -> Result<(), String> {
        let mut list = kept(&self.db).await;
        if list.contains(&m) || self.cli.contains(&m) {
            return Ok(());
        }
        if list.len() >= MAX_KEPT {
            return Err(format!("{MAX_KEPT} stations gardées au plus"));
        }
        list.push(m);
        store(&self.db, &list).await
    }

    /// Toutes les minutes : joint les relais et y tient une place (refaite si elle se perd). Toutes les `every` (la première
    /// fois tout de suite) : rappelle les stations gardées et joint les stations sœurs de l'annuaire.
    pub async fn keep_alive(self: Arc<Self>, every: Duration, mut stop: tokio::sync::watch::Receiver<bool>) {
        let Some(h) = self.handle.clone() else { return };
        let me = h.peer_id();
        let rounds = (every.as_secs() / RELAY_TICK.as_secs()).max(1);
        let mut round: u64 = 0;
        // Relais en échec : dit une fois, puis seulement quand il revient.
        let mut failing: std::collections::HashSet<PeerId> = std::collections::HashSet::new();
        loop {
            let connected = h.connected_peers();
            let reserved = h.reserved_relays();
            let mut gained = false;
            for (m, peer, _) in self.relays().await {
                if peer == me {
                    continue;
                }
                if self.is_relay || reserved.contains(&peer) {
                    if !connected.contains(&peer) {
                        let _ = h.dial(m).await;
                    }
                    continue;
                }
                match h.listen_via_relay(m.clone()).await {
                    Ok(addr) => {
                        failing.remove(&peer);
                        gained = true;
                        println!("P2P : joignable derrière la box via {addr}");
                        self.activity.publish_to(Audience::Captain, "system", "maillage", format!("place réservée chez la station relais {} : la station est joignable derrière la box", short(&peer)), None);
                    }
                    Err(e) => {
                        if failing.insert(peer) {
                            eprintln!("P2P : relais {m} indisponible : {e} (nouvel essai chaque minute)");
                        }
                    }
                }
            }
            if gained && let Some((_, nudge)) = &self.directory {
                nudge.notify_one();
            }
            if round % rounds == 0 {
                let mut all = self.cli.clone();
                all.extend(kept(&self.db).await);
                for m in all {
                    if let Ok((_, peer)) = parse_station(&m.to_string()) {
                        if !connected.contains(&peer) && h.dial(m.clone()).await.is_err() {
                            // Une station éteinte n'est pas une alerte : elle reste dans la liste et sera rappelée.
                            eprintln!("maillage : {m} injoignable pour l'instant");
                        }
                    }
                }
                if let Some((stations, _)) = &self.directory {
                    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                    let mut seen = std::collections::HashSet::new();
                    let dials: Vec<_> = stations
                        .p2p_addrs(at)
                        .await
                        .into_iter()
                        .filter(|(_, p)| *p != me && !connected.contains(p))
                        .filter(|(_, p)| seen.insert(*p))
                        .take(MAX_DIRECTORY_DIALS)
                        .map(|(m, _)| {
                            let h = h.clone();
                            async move { h.dial(m).await.is_ok() }
                        })
                        .collect();
                    // Une station sœur injoignable (éteinte, relais perdu) est retentée au passage suivant, sans bruit.
                    let _ = futures_util::future::join_all(dials).await;
                }
            }
            round += 1;
            tokio::select! {
                () = tokio::time::sleep(RELAY_TICK) => {}
                _ = stop.changed() => return,
            }
        }
    }
}

fn short(p: &PeerId) -> String {
    p.to_string().chars().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_station_address_must_name_its_peer() {
        let peer = "12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG";
        assert_eq!(parse_station(&format!("/ip4/1.2.3.4/tcp/4101/p2p/{peer}")).unwrap().1.to_string(), peer);
        for bad in ["", "/ip4/1.2.3.4/tcp/4101", "http://exemple.org", "/ip4/1.2.3.4/tcp/4101/p2p/pas-un-peer", "/dns4/x/tcp/1/p2p/12D3KooWx"] {
            assert!(parse_station(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn kept_stations_survive_a_restart_and_the_list_is_bounded() {
        let dir = std::env::temp_dir().join(format!("astroportd-mesh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let mesh = Mesh::new(db.clone(), None, Vec::new(), "ORIGIN".into(), ActivityBus::new());
        assert!(mesh.act(&json!({ "keep": "/ip4/1.2.3.4/tcp/4101" })).await.is_err(), "maillage coupé : rien à faire");
        assert_eq!(mesh.view().await["active"], false);
        let addrs: Vec<String> = (0..MAX_KEPT + 3).map(|i| format!("/ip4/10.0.0.{}/tcp/4101/p2p/{}", i % 250, astro_p2p::keypair_from_seed(&[u8::try_from(i + 1).unwrap(); 32]).unwrap().public().to_peer_id())).collect();
        let mut list = Vec::new();
        for a in &addrs[..MAX_KEPT] {
            list.push(parse_station(a).unwrap().0);
        }
        store(&db, &list).await.unwrap();
        assert_eq!(kept(&db).await.len(), MAX_KEPT);
        // Une liste altérée sur disque (entrées illisibles) est filtrée à la lecture.
        db.writer().execute("UPDATE kv SET value = ?1 WHERE scope = 'options' AND key = 'p2p_bootstrap'", vec![json!(["n'importe quoi", addrs[0]]).to_string().into()]).await.unwrap();
        assert_eq!(kept(&db).await.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
