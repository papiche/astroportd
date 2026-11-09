//! Routage vers les services de la constellation : **quelle station** peut me servir `ollama`, `comfyui`, `ssh`… maintenant ?
//!
//! Remplace la lecture des `12345.json` d'Astroport.ONE (`~/.zen/tmp/swarm`) par ce que le démon connaît déjà :
//!
//! - l'**annuaire des stations** ([`Stations`]) : balises signées 30850 de l'UPlanet (PeerID, Power-Score, case UMAP) ;
//! - la **réponse de chaque station** à « quels services m'ouvres-tu ? » ([`ask_services`]) : elle ne cite que ce que sa
//!   politique de tunnels m'autorise. Un service qui existe mais m'est fermé n'est donc jamais proposé, ce que le champ
//!   `dragon_services` d'un `12345.json` (public, valable pour tous) ne pouvait pas dire.
//!
//! Le choix lui-même (Power-Score, distance, tirage) reste [`astro_p2p::routing::select`]. L'accès ouvert est un accès
//! local à chaud ([`Forwards`]) : le même que l'écran des Capitaines, sous la même clé `cap:<station>/<service>`.

use std::sync::Arc;

use astro_p2p::P2pHandle;
use astro_p2p::routing::{Candidate, Mode, haversine_km, random_u64, select};
use serde_json::{Value, json};

use crate::beacon::Stations;
use crate::forwards::Forwards;
use crate::tunnel_policy::ask_services;

/// Stations interrogées au plus pour un choix (les plus puissantes d'abord).
const MAX_ASKED: usize = 12;
/// Durée d'un accès ouvert sans précision.
const DEFAULT_HOURS: i64 = 8;
/// Durée maximale d'un accès (une semaine).
const MAX_HOURS: i64 = 168;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Clé d'un accès local : la même que celle de l'écran des Capitaines.
fn local_id(peer: &str, service: &str) -> String {
    format!("cap:{peer}/{service}")
}

/// Le routage et les accès vers les services des autres stations.
pub struct SwarmRoute {
    stations: Arc<Stations>,
    handle: Option<P2pHandle>,
    forwards: Arc<Forwards>,
    /// Position de cette station donnée à `serve` (`--lat`, `--lon`), si l'annuaire ne la connaît pas encore.
    origin: Option<(f64, f64)>,
}

/// Une station de l'annuaire, avant qu'on lui demande ses services.
struct Known {
    candidate: Candidate,
    name: String,
}

impl SwarmRoute {
    /// `handle` : le maillage (`None` : rien ne peut être demandé ni ouvert).
    pub fn new(stations: Arc<Stations>, handle: Option<P2pHandle>, forwards: Arc<Forwards>, origin: Option<(f64, f64)>) -> Self {
        Self { stations, handle, forwards, origin }
    }

    /// Les autres stations de l'UPlanet (sans leurs services) et la position de celle-ci.
    async fn known(&self) -> Result<(Vec<Known>, Option<(f64, f64)>), String> {
        let list = self.stations.list(u64::try_from(now()).unwrap_or(0)).await?;
        let mut mine = self.origin;
        let mut out = Vec::new();
        for s in list["stations"].as_array().into_iter().flatten() {
            let Some(id) = s["station"].as_str() else { continue };
            let pos = s["lat"].as_f64().zip(s["lon"].as_f64());
            if s["self"].as_bool() == Some(true) {
                mine = pos.or(mine);
                continue;
            }
            let name = s["name"].as_str().unwrap_or(id).to_owned();
            out.push(Known { candidate: Candidate { node_id: id.to_owned(), power_score: s["power"].as_f64().unwrap_or(0.0), lat: pos.map(|p| p.0), lon: pos.map(|p| p.1), services: Vec::new() }, name });
        }
        // Les plus puissantes d'abord : ce sont elles qu'on interroge si l'UPlanet est grande.
        out.sort_by(|a, b| b.candidate.power_score.total_cmp(&a.candidate.power_score).then_with(|| a.candidate.node_id.cmp(&b.candidate.node_id)));
        Ok((out, mine))
    }

    /// Demande leurs services (ouverts à nous) aux `MAX_ASKED` premières stations, en parallèle. Une station qui ne répond
    /// pas (hors ligne, version ancienne) est rendue avec l'erreur, pas silencieusement ignorée.
    async fn ask_all(&self, known: Vec<Known>) -> Vec<(Known, Result<Vec<String>, String>)> {
        let Some(h) = self.handle.as_ref() else { return known.into_iter().map(|k| (k, Err("maillage P2P coupé".to_owned()))).collect() };
        let answers = futures_util::future::join_all(known.iter().take(MAX_ASKED).map(|k| ask_services(h, &k.candidate.node_id))).await;
        let mut answers = answers.into_iter();
        known.into_iter().map(|k| (k, answers.next().unwrap_or_else(|| Err("non interrogée (trop de stations)".to_owned())))).collect()
    }

    /// Les stations qui m'ouvrent `service`, avec la position de celle-ci.
    async fn providers(&self, service: &str) -> Result<(Vec<Candidate>, Option<(f64, f64)>), String> {
        let (known, mine) = self.known().await?;
        let mut out = Vec::new();
        for (mut k, answer) in self.ask_all(known).await {
            if let Ok(services) = answer {
                if services.iter().any(|s| s == service) {
                    k.candidate.services = services;
                    out.push(k.candidate);
                }
            }
        }
        Ok((out, mine))
    }

    /// Station retenue pour `service` : `(PeerID, compte rendu)`.
    pub async fn route(&self, service: &str, mode: Mode) -> Result<(String, Value), String> {
        let (providers, mine) = self.providers(service).await?;
        let sel = select(&providers, service, mode, mine, random_u64()).ok_or_else(|| format!("aucune station ne vous ouvre « {service} » (`astroportd tunnel list` montre ce que chacune vous ouvre)"))?;
        let distance = sel.distance_km.or_else(|| mine.zip(sel.node.lat.zip(sel.node.lon)).map(|(a, b)| haversine_km(a, b)));
        let report = json!({
            "peer": sel.node.node_id, "power_score": (sel.node.power_score * 10.0).round() / 10.0, "mode": format!("{:?}", sel.mode).to_lowercase(),
            "fell_back": sel.fell_back, "distance_km": distance.map(|d| d.round()), "candidates": providers.len(),
        });
        Ok((sel.node.node_id.clone(), report))
    }

    /// Vue : les stations connues, ce qu'elles nous ouvrent, et les accès locaux ouverts.
    pub async fn view(&self) -> Result<Value, String> {
        let (known, mine) = self.known().await?;
        let at = now();
        let rows: Vec<Value> = self
            .ask_all(known)
            .await
            .into_iter()
            .map(|(k, answer)| {
                let c = &k.candidate;
                let distance = mine.zip(c.lat.zip(c.lon)).map(|(a, b)| haversine_km(a, b).round());
                let services = match answer {
                    Ok(list) => json!(list
                        .iter()
                        .map(|s| {
                            let open = self.forwards.list(at).into_iter().find(|o| o.0 == local_id(&c.node_id, s));
                            json!({ "name": s, "local": open.map(|o| json!({ "addr": o.1.to_string(), "until": o.2 })) })
                        })
                        .collect::<Vec<_>>()),
                    Err(e) => json!({ "error": e }),
                };
                json!({ "peer": c.node_id, "name": k.name, "power_score": c.power_score, "distance_km": distance, "services": services })
            })
            .collect();
        Ok(json!({ "mesh": self.handle.is_some(), "stations": rows }))
    }

    /// Actions : `{ route: { service, mode? } }`, `{ connect: { service, peer?, mode?, port?, hours? } }` (sans `peer` : la
    /// station choisie par le routage), `{ disconnect: { service, peer? } }`. Sans action : la vue.
    pub async fn act(&self, b: &Value) -> Result<Value, String> {
        if let Some(r) = b.get("route").filter(|r| r.is_object()) {
            let service = r["service"].as_str().unwrap_or_default();
            let mode = r["mode"].as_str().map_or(Ok(Mode::Power), str::parse::<Mode>)?;
            return self.route(service, mode).await.map(|(_, report)| report);
        }
        if let Some(c) = b.get("connect").filter(|c| c.is_object()) {
            let service = c["service"].as_str().filter(|s| !s.is_empty()).ok_or("service manquant")?;
            let hours = c["hours"].as_i64().unwrap_or(DEFAULT_HOURS);
            if !(1..=MAX_HOURS).contains(&hours) {
                return Err(format!("durée : 1 à {MAX_HOURS} heures"));
            }
            let port = match c["port"].as_u64() {
                None => 0,
                Some(p) => u16::try_from(p).map_err(|_| "port invalide".to_owned())?,
            };
            let (peer, report) = match c["peer"].as_str().filter(|p| !p.is_empty() && *p != "auto") {
                Some(p) => (p.to_owned(), Value::Null),
                None => {
                    let mode = c["mode"].as_str().map_or(Ok(Mode::Power), str::parse::<Mode>)?;
                    self.route(service, mode).await?
                }
            };
            let at = now();
            let addr = self.forwards.open_on(&local_id(&peer, service), &peer, service, at + hours * 3600, at, port).await?;
            return Ok(json!({ "peer": peer, "service": service, "addr": addr.to_string(), "until": at + hours * 3600, "route": report }));
        }
        if let Some(c) = b.get("disconnect").filter(|c| c.is_object()) {
            let service = c["service"].as_str().filter(|s| !s.is_empty()).ok_or("service manquant")?;
            let at = now();
            let suffix = format!("/{service}");
            let ids: Vec<String> = self.forwards.list(at).into_iter().map(|o| o.0).filter(|id| match c["peer"].as_str().filter(|p| !p.is_empty()) {
                Some(p) => *id == local_id(p, service),
                None => id.starts_with("cap:") && id.ends_with(&suffix),
            }).collect();
            if ids.is_empty() {
                return Err(format!("aucun accès ouvert à « {service} »"));
            }
            for id in &ids {
                self.forwards.close(id);
            }
            return Ok(json!({ "closed": ids.len() }));
        }
        if b.as_object().is_some_and(|o| !o.is_empty()) {
            return Err("action attendue : route, connect ou disconnect".into());
        }
        self.view().await
    }
}
