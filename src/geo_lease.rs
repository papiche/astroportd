//! **Bail géographique** : quelle station d'une UPlanet tient (seule) les clés de quartier UMAP, SECTOR et REGION.
//!
//! Les clés d'une case se dérivent de `UPLANETNAME` et de ses coordonnées : toute station de l'UPlanet peut les calculer.
//! Le risque n'est donc pas la clé, mais deux stations qui publieraient chacune leur version du même quartier. Règle :
//! **une seule station écrit par case** — la plus proche du centre de la case parmi celles qui s'y intéressent (un de
//! leurs membres y habite, ou la station y est), le PeerID départageant deux distances égales.
//!
//! Le bail circule par le maillage P2P de l'UPlanet ([`PROTOCOL`]) : chaque station dit sa position et ses cases ; toutes
//! calculent le même propriétaire avec les mêmes informations. Une station qui ne répond plus depuis [`TTL_SECS`] (36 h,
//! soit un renouvellement quotidien manqué et demi) sort du calcul : la suivante reprend. Les autres stations n'écrivent
//! pas : elles laissent leurs contributions (publications, likes) sur les relais, où le propriétaire les lit.
//!
//! Noms de cases identiques à ceux d'Astroport.ONE (`NOSTR.UMAP.refresh.sh`) : UMAP `_43.61_1.44` (arrondi à 0,01°,
//! `printf %.2f`), SECTOR `_43.6_1.4` (dernière décimale ôtée), REGION `_43_1` (partie entière).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_p2p::P2pHandle;
use serde_json::{Value, json};
use tokio::sync::watch;

/// Protocole de question/réponse du bail.
pub const PROTOCOL: &str = "/astroport/geolease/1";
/// Une station muette depuis 36 h sort du calcul.
pub const TTL_SECS: i64 = 36 * 3600;
/// Période des questions aux stations voisines.
const EVERY: Duration = Duration::from_secs(600);
/// Cases annoncées au plus (une station de 500 membres en a ≤ 1 500).
const MAX_CELLS: usize = 4_000;
/// Stations interrogées par passage.
const MAX_PEERS: usize = 64;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Coordonnée au format d'Astroport.ONE (`makecoord` : `printf %.2f`, zéro de tête, jamais `-0.00`).
fn coord(v: f64) -> String {
    let s = format!("{:.2}", (v * 100.0).round() / 100.0);
    if s == "-0.00" { "0.00".to_owned() } else { s }
}

/// Les trois cases (UMAP, SECTOR, REGION) d'une position.
pub fn cells_of(lat: f64, lon: f64) -> [String; 3] {
    let (la, lo) = (coord(lat), coord(lon));
    let sector = |s: &str| s[..s.len() - 1].to_owned();
    let region = |s: &str| s.split('.').next().unwrap_or(s).to_owned();
    [format!("_{la}_{lo}"), format!("_{}_{}", sector(&la), sector(&lo)), format!("_{}_{}", region(&la), region(&lo))]
}

/// Centre d'une case (`_43.61_1.44`, `_43.6_1.4` ou `_43_1`).
pub fn center(cell: &str) -> Option<(f64, f64)> {
    let mut it = cell.strip_prefix('_')?.split('_');
    let (a, b) = (it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    let one = |s: &str| -> Option<f64> {
        let v: f64 = s.parse().ok()?;
        let decimals = s.split('.').nth(1).map_or(0, str::len);
        // UMAP (2 décimales) : arrondi, la valeur est le centre. SECTOR / REGION : troncature, le centre est à mi-pas.
        let half = match decimals {
            2 => 0.0,
            1 => 0.05,
            0 => 0.5,
            _ => return None,
        };
        Some(if s.starts_with('-') { v - half } else { v + half })
    };
    let (lat, lon) = (one(a)?, one(b)?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

fn km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let (dla, dlo) = ((b.0 - a.0).to_radians(), (b.1 - a.1).to_radians());
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    6_371.0 * 2.0 * h.sqrt().asin()
}

/// Une station candidate : PeerID, position, cases qui l'intéressent.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    /// PeerID.
    pub peer: String,
    /// Position de la station.
    pub pos: (f64, f64),
    /// Ses cases.
    pub cells: BTreeSet<String>,
    /// Dernière réponse.
    pub at: i64,
}

/// Propriétaire d'une case parmi `claims` : le plus proche de son centre, PeerID le plus petit à distance égale (au mètre).
pub fn owner<'a>(cell: &str, claims: impl IntoIterator<Item = &'a Claim>) -> Option<&'a Claim> {
    let c = center(cell)?;
    claims.into_iter().filter(|k| k.cells.contains(cell)).min_by(|x, y| {
        let (dx, dy) = ((km(x.pos, c) * 1000.0).round() as i64, (km(y.pos, c) * 1000.0).round() as i64);
        dx.cmp(&dy).then_with(|| x.peer.cmp(&y.peer))
    })
}

/// Bail géographique de cette station.
pub struct GeoLease {
    db: Arc<Db>,
    me: String,
    pos: Option<(f64, f64)>,
    peers: Mutex<HashMap<String, Claim>>,
    answers: Mutex<Option<astro_p2p::ask::Answers>>,
    activity: ActivityBus,
}

impl GeoLease {
    /// Bail de la station `me` (PeerID) placée en `pos` (`--lat`/`--lon` ; sans position, elle ne revendique rien).
    pub fn new(db: Arc<Db>, me: String, pos: Option<(f64, f64)>, activity: ActivityBus) -> Arc<Self> {
        Arc::new(Self { db, me, pos, peers: Mutex::new(HashMap::new()), answers: Mutex::new(None), activity })
    }

    /// Cases qui intéressent cette station : la sienne et celles de ses membres (position indiquée à l'inscription).
    pub async fn my_cells(&self) -> BTreeSet<String> {
        let rows: Vec<(f64, f64)> = self
            .db
            .readers()
            .read(|c| {
                let mut st = c.prepare("SELECT lat, lon FROM identities WHERE hosted = 1 AND lat IS NOT NULL AND lon IS NOT NULL LIMIT 2000")?;
                Ok(st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut out = BTreeSet::new();
        for (la, lo) in self.pos.into_iter().chain(rows) {
            out.extend(cells_of(la, lo));
        }
        out.into_iter().take(MAX_CELLS).collect()
    }

    async fn me_claim(&self) -> Option<Claim> {
        Some(Claim { peer: self.me.clone(), pos: self.pos?, cells: self.my_cells().await, at: now() })
    }

    fn fresh_peers(&self, at: i64) -> Vec<Claim> {
        let mut peers = self.peers.lock().unwrap_or_else(PoisonError::into_inner);
        peers.retain(|_, c| at.saturating_sub(c.at) < TTL_SECS);
        peers.values().cloned().collect()
    }

    /// Cette station tient-elle la case (seule à pouvoir publier avec ses clés) ?
    pub async fn holds(&self, cell: &str) -> bool {
        let Some(me) = self.me_claim().await else { return false };
        let others = self.fresh_peers(now());
        owner(cell, others.iter().chain(std::iter::once(&me))).is_some_and(|o| o.peer == self.me)
    }

    /// Vue du Capitaine : cases tenues, cases cédées (et à qui), stations voisines entendues.
    pub async fn view(&self) -> Value {
        let Some(me) = self.me_claim().await else {
            return json!({ "position": null, "held": [], "yielded": [], "peers": self.fresh_peers(now()).len(), "note": "station sans position (--lat/--lon) : elle ne tient aucun quartier" });
        };
        let others = self.fresh_peers(now());
        let (mut held, mut yielded) = (Vec::new(), Vec::new());
        for cell in &me.cells {
            match owner(cell, others.iter().chain(std::iter::once(&me))) {
                Some(o) if o.peer == self.me => held.push(cell.clone()),
                Some(o) => yielded.push(json!({ "cell": cell, "to": o.peer, "km": center(cell).map(|c| (km(o.pos, c) * 10.0).round() / 10.0) })),
                None => {}
            }
        }
        json!({ "position": [me.pos.0, me.pos.1], "held": held, "yielded": yielded, "peers": others.len() })
    }

    /// Ce que la station répond à une voisine.
    async fn answer_body(&self) -> Vec<u8> {
        let cells = self.my_cells().await;
        json!({ "v": 1, "peer": self.me, "pos": self.pos.map(|p| [p.0, p.1]), "cells": cells }).to_string().into_bytes()
    }

    /// Lit la réponse d'une voisine (bornée, vérifiée).
    pub fn parse_answer(peer: &str, body: &[u8], at: i64) -> Option<Claim> {
        let v: Value = serde_json::from_slice(body).ok()?;
        let pos = v["pos"].as_array()?;
        let (lat, lon) = (pos.first()?.as_f64()?, pos.get(1)?.as_f64()?);
        if !((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)) {
            return None;
        }
        let cells = v["cells"].as_array()?.iter().filter_map(Value::as_str).filter(|c| c.len() <= 24 && center(c).is_some()).take(MAX_CELLS).map(str::to_owned).collect();
        Some(Claim { peer: peer.to_owned(), pos: (lat, lon), cells, at })
    }

    /// Répond aux voisines et les interroge toutes les 10 minutes.
    pub async fn run(self: Arc<Self>, handle: P2pHandle, mut stop: watch::Receiver<bool>) {
        let me = self.clone();
        let f: astro_p2p::ask::AnswerFn = Arc::new(move |_peer| {
            let me = me.clone();
            Box::pin(async move { me.answer_body().await })
        });
        match astro_p2p::ask::answer(&handle, PROTOCOL, f) {
            Ok(a) => *self.answers.lock().unwrap_or_else(PoisonError::into_inner) = Some(a),
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "geo", format!("bail géographique : les stations voisines ne sauront pas quels quartiers nous tenons ({e})"), None);
            }
        }
        loop {
            let mut heard: BTreeMap<String, Claim> = BTreeMap::new();
            for peer in handle.connected_peers().into_iter().take(MAX_PEERS) {
                // Une station ancienne ne connaît pas le protocole : elle ne revendique rien, sans alerte.
                if let Ok(body) = astro_p2p::ask::ask(&handle, peer, PROTOCOL, Duration::from_secs(5)).await {
                    let id = peer.to_string();
                    if let Some(c) = Self::parse_answer(&id, &body, now()) {
                        heard.insert(id, c);
                    }
                }
            }
            self.peers.lock().unwrap_or_else(PoisonError::into_inner).extend(heard);
            tokio::select! {
                () = tokio::time::sleep(EVERY) => {}
                _ = stop.changed() => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(peer: &str, pos: (f64, f64), cells: &[String]) -> Claim {
        Claim { peer: peer.into(), pos, cells: cells.iter().cloned().collect(), at: 0 }
    }

    #[test]
    fn cells_are_named_like_astroport_one() {
        assert_eq!(cells_of(43.60789, 1.44321), ["_43.61_1.44".to_owned(), "_43.6_1.4".to_owned(), "_43_1".to_owned()]);
        assert_eq!(cells_of(-0.004, -1.256), ["_0.00_-1.26".to_owned(), "_0.0_-1.2".to_owned(), "_0_-1".to_owned()]);
        assert_eq!(center("_43.61_1.44"), Some((43.61, 1.44)));
        let (la, lo) = center("_43.6_-1.2").unwrap();
        assert!((la - 43.65).abs() < 1e-9 && (lo + 1.25).abs() < 1e-9);
        assert_eq!(center("_43_1"), Some((43.5, 1.5)));
        assert!(center("_91_0").is_none() && center("x").is_none() && center("_1.234_0").is_none());
    }

    #[test]
    fn the_nearest_interested_station_holds_the_cell_and_ties_go_to_the_smallest_peer() {
        let cells = cells_of(43.61, 1.44);
        let near = claim("12D3B", (43.62, 1.45), &cells);
        let far = claim("12D3A", (43.9, 2.1), &cells);
        let nearer_but_uninterested = claim("12D3C", (43.61, 1.44), &[]);
        assert_eq!(owner(&cells[0], [&near, &far, &nearer_but_uninterested]).unwrap().peer, "12D3B");
        // Même position : le plus petit PeerID, chez toutes les stations (calcul identique partout).
        let twin = claim("12D3A", (43.62, 1.45), &cells);
        assert_eq!(owner(&cells[0], [&near, &twin]).unwrap().peer, "12D3A");
        assert_eq!(owner(&cells[0], [&twin, &near]).unwrap().peer, "12D3A");
    }

    #[test]
    fn a_neighbour_answer_is_bounded_and_checked() {
        let ok = GeoLease::parse_answer("p", br#"{"v":1,"pos":[43.6,1.44],"cells":["_43.61_1.44","../../etc","_999_0"]}"#, 5).unwrap();
        assert_eq!(ok.cells.into_iter().collect::<Vec<_>>(), vec!["_43.61_1.44".to_owned()]);
        assert!(GeoLease::parse_answer("p", br#"{"pos":[120,0],"cells":[]}"#, 5).is_none());
        assert!(GeoLease::parse_answer("p", b"pas du json", 5).is_none());
    }

    #[tokio::test]
    async fn a_silent_station_drops_out_and_the_next_one_takes_over() {
        let dir = std::env::temp_dir().join(format!("astroportd-geolease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let lease = GeoLease::new(db, "12D3MOI".into(), Some((43.70, 1.50)), ActivityBus::new());
        let cell = cells_of(43.70, 1.50)[0].clone();
        assert!(lease.holds(&cell).await, "seule : elle tient sa case");
        let neighbour = claim("12D3AVOISINE", (43.70, 1.50), &cells_of(43.70, 1.50));
        lease.peers.lock().unwrap().insert(neighbour.peer.clone(), Claim { at: now(), ..neighbour.clone() });
        assert!(!lease.holds(&cell).await, "une voisine aussi proche et de plus petit PeerID la tient");
        assert_eq!(lease.view().await["yielded"][0]["to"], "12D3AVOISINE");
        lease.peers.lock().unwrap().insert(neighbour.peer.clone(), Claim { at: now() - TTL_SECS - 1, ..neighbour });
        assert!(lease.holds(&cell).await, "muette depuis 36 h : la case revient à la suivante");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
