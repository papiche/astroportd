//! **Balise de station** et **annuaire des stations** d'une UPlanet (kind 30850, `d = economic-health`).
//!
//! La balise publie, signé par le NODE, l'état public de la station : santé économique de la dernière semaine close,
//! places libres (`capacity:multipass_used` / `capacity:multipass_total`, `register:open`), position arrondie à la
//! case UMAP (0,01°, seulement si `--lat` / `--lon` sont réglés), matériel, prix du MULTIPASS, adresse publique et
//! relai de la coopérative. Les noms de tags sont ceux d'`ECONOMY.broadcast.sh` (legacy) : les tableaux de bord
//! existants (`heartbox.js`, vue LIVE d'astroport.one) les lisent sans changement.
//!
//! Fraîcheur : publication au démarrage, dès que le nombre de comptes ou l'ouverture des inscriptions change (au plus
//! une fois toutes les [`MIN_GAP_SECS`]), toutes les [`EVERY_SECS`] et après la clôture de la nuit (20h12).
//!
//! Où : relai local, relais `--relay`, et **`wss://relay.<zone>`**, la zone DNS de la configuration OVH de la
//! coopérative (`OVH_ZONE`) : c'est le relai commun de l'UPlanet, celui que lisent la vue LIVE et l'annuaire.
//!
//! Avec le plugin système `energy` (dépôt `astroportd-plugins`), la balise porte aussi la consommation et la production
//! d'énergie de la station (tags `hw:energy_*`, `hw:solar_*` et contenu `hardware.energy` / `hardware.solar`, voir
//! [`crate::energy`]).
//!
//! L'annuaire relit ces mêmes relais toutes les [`REFRESH_SECS`] et rend les stations de **la même UPlanet**
//! (`swarm_id`), vues depuis moins de [`MAX_AGE_SECS`] : c'est ce que sert `GET /api/v1/stations` (page `/stations`),
//! pour qu'un futur membre trouve la station la plus proche qui a encore de la place.

use std::sync::Arc;
use std::time::Duration;

use astro_bro::publisher::Publisher;
use astro_core::{ActivityBus, Amount};
use astro_db::Db;
use astro_nostr_proto::{Filter, UnsignedEvent, kind};
use astro_p2p::routing::Hardware;
use astro_sync::{Ingested, RelayClient, WsClient, ingest_event};
use serde_json::{Value, json};
use tokio::sync::{Notify, OnceCell, watch};

use crate::register::Registrar;

/// Publication complète au moins toutes les 6 heures.
pub const EVERY_SECS: u64 = 6 * 3_600;
/// Écart minimal entre deux publications dues à un changement de places.
pub const MIN_GAP_SECS: u64 = 120;
/// Relecture de l'annuaire sur les relais.
pub const REFRESH_SECS: u64 = 15 * 60;
/// Une station muette depuis plus de 8 jours sort de l'annuaire (le legacy publie une fois par semaine).
pub const MAX_AGE_SECS: u64 = 8 * 86_400;
/// Délai avant la première publication : le relai local et le maillage démarrent.
const FIRST_DELAY_SECS: u64 = 20;
/// Pas de la boucle : relevé du nombre de comptes.
const TICK_SECS: u64 = 60;
/// Propriétaire des réclamations d'ingestion de l'annuaire.
const CLAIM_OWNER: &str = "stations";

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Coordonnée arrondie à la case UMAP (0,01°, comme `NOSTR.UMAP.refresh.sh`).
pub fn umap_coord(v: f64) -> String {
    let s = format!("{v:.2}");
    if s == "-0.00" { "0.00".to_owned() } else { s }
}

/// Relai commun de l'UPlanet : `wss://relay.<zone OVH de la coopérative>`.
pub async fn coop_relay(db: &Db, uplanet_name: &str) -> Option<String> {
    crate::coop_config::domain(db, uplanet_name).await.map(|zone| format!("wss://relay.{zone}"))
}

/// Identité publique d'une UPlanet et de la station qui la regarde (commune à la balise et à l'annuaire).
pub struct Stations {
    db: Arc<Db>,
    uplanet_name: String,
    peer_id: Option<String>,
    swarm: OnceCell<String>,
}

impl Stations {
    /// Annuaire de l'UPlanet `uplanet_name`, vu par la station `peer_id`.
    pub fn new(db: Arc<Db>, uplanet_name: String, peer_id: Option<String>) -> Self {
        Self { db, uplanet_name, peer_id, swarm: OnceCell::new() }
    }

    /// `swarm_id` de l'UPlanet : adresse ss58 du portefeuille `<UPLANETNAME>` (calculée une fois).
    pub async fn swarm_id(&self) -> Result<String, String> {
        self.swarm
            .get_or_try_init(|| async {
                let name = self.uplanet_name.clone();
                tokio::task::spawn_blocking(move || crate::coop_wallets::uplanet_g1pub(&name)).await.map_err(|e| e.to_string())?
            })
            .await
            .cloned()
    }

    /// Stations de la même UPlanet vues depuis moins de [`MAX_AGE_SECS`], la plus récente d'abord (une ligne par
    /// station : un même PeerID publié par deux clés, NODE et Capitaine dans le legacy, ne compte qu'une fois).
    pub async fn list(&self, at: u64) -> Result<Value, String> {
        let swarm = self.swarm_id().await?;
        let since = i64::try_from(at.saturating_sub(MAX_AGE_SECS)).unwrap_or(0);
        let rows: Vec<(String, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT value, updated_at FROM kv WHERE scope = 'economic_health' AND updated_at >= ?1 ORDER BY updated_at DESC")?;
                Ok(s.query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(|e| e.to_string())?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (value, updated) in rows {
            let Ok(v) = serde_json::from_str::<Value>(&value) else { continue };
            let Some(row) = station_row(&v, &swarm, self.peer_id.as_deref(), updated) else { continue };
            if seen.insert(row["station"].as_str().unwrap_or_default().to_owned()) {
                out.push(row);
            }
        }
        Ok(json!({ "swarm_id": swarm, "self": self.peer_id, "max_age_secs": MAX_AGE_SECS, "stations": out }))
    }
}

/// Une station de l'annuaire, depuis le résumé projeté d'un 30850 (`{ tags: { nom: valeur } }`) ; `None` si elle
/// n'est pas de l'UPlanet `swarm` ou n'a pas de PeerID.
pub fn station_row(summary: &Value, swarm: &str, me: Option<&str>, updated: i64) -> Option<Value> {
    let tags = summary.get("tags")?;
    let tag = |k: &str| tags.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    if tag("swarm_id")? != swarm {
        return None;
    }
    let station = tag("station")?;
    let num = |k: &str| tag(k).and_then(|v| v.parse::<f64>().ok()).filter(|n| n.is_finite());
    let count = |k: &str| tag(k).and_then(|v| v.parse::<u64>().ok());
    let (lat, lon) = match (num("geo:lat"), num("geo:lon")) {
        // 0,0 : position inconnue (le legacy publie `${STATION_LAT:-0}`).
        (Some(la), Some(lo)) if (la != 0.0 || lo != 0.0) && la.abs() <= 90.0 && lo.abs() <= 180.0 => (Some(la), Some(lo)),
        _ => (None, None),
    };
    let used = count("capacity:multipass_used").unwrap_or(0);
    let total = count("capacity:multipass_total").unwrap_or(0);
    // `register:open` n'existe que chez astroportd ; le legacy n'annonce que ses places (0 = sans limite).
    let astroportd = tag("register:open").is_some();
    let open = match tag("register:open") {
        Some(v) => v == "true",
        None => total == 0 || used < total,
    };
    let url = tag("url").or_else(|| tag("station:url")).filter(|u| u.starts_with("https://") || u.starts_with("http://")).map(|u| u.trim_end_matches('/').to_owned());
    let join = url.as_ref().map(|u| format!("{u}{}", if astroportd { "/register" } else { "/g1" }));
    Some(json!({
        "station": station,
        "name": tag("station:name").map_or_else(|| format!("…{}", &station[station.len().saturating_sub(8)..]), str::to_owned),
        "self": me == Some(station),
        "url": url,
        "join": join,
        "relay": tag("relay"),
        "lat": lat,
        "lon": lon,
        "used": used,
        "total": total,
        "open": open,
        "astroportd": astroportd,
        "tier": tag("hw:tier").unwrap_or("light").to_ascii_lowercase(),
        "power": num("hw:power_score").unwrap_or(0.0),
        "gpu_gb": num("hw:gpu_vram_gb").unwrap_or(0.0),
        "price": tag("price:multipass"),
        "health": tag("health:status").unwrap_or("unknown"),
        "week": tag("week"),
        "at": summary.get("created_at").and_then(Value::as_i64).unwrap_or(updated),
    }))
}

/// État des places, pour décider d'une publication anticipée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seats {
    used: u64,
    open: bool,
}

/// Les moyens de la balise.
pub struct Beacon<P: Publisher> {
    /// Base `uplanet.db`.
    pub db: Arc<Db>,
    /// Clé du NODE : signe la balise.
    pub node_secret: [u8; 32],
    /// Relai local + relais `--relay`.
    pub publisher: Arc<P>,
    /// Client WebSocket (publication et lecture sur le relai de la coopérative).
    pub client: WsClient,
    /// Relais `--relay` (relus par l'annuaire).
    pub relays: Vec<String>,
    /// Inscriptions : places et ouverture.
    pub registrar: Arc<Registrar>,
    /// Annuaire (UPlanet, PeerID).
    pub stations: Arc<Stations>,
    /// Nom affiché (`station:name`).
    pub name: String,
    /// Adresse publique (`https://…`).
    pub public_url: Option<String>,
    /// Position (`--lat`, `--lon`), publiée arrondie à la case UMAP.
    pub origin: Option<(f64, f64)>,
    /// Matériel mesuré au démarrage.
    pub hardware: Hardware,
    /// Loyer hebdomadaire du MULTIPASS.
    pub weekly_rent: Amount,
    /// Charges hebdomadaires (PAF).
    pub weekly_costs: Amount,
    /// Dossier de la station (celui de `uplanet.db`) : les plugins système y déposent leurs données (`plugins/<nom>/`).
    pub station_home: Option<std::path::PathBuf>,
    /// Réveil (fin de la nuit, changement de réglage).
    pub nudge: Arc<Notify>,
    /// Journal d'activité.
    pub activity: Option<ActivityBus>,
}

impl<P: Publisher> Beacon<P> {
    async fn seats(&self) -> Result<Seats, String> {
        let limits = self.registrar.limits();
        let used = self.registrar.hosted_accounts().await.map_err(|e| e.to_string())?;
        let open = limits.open && !self.registrar.awaiting_captain().await && (limits.max_accounts == 0 || used < limits.max_accounts);
        Ok(Seats { used, open })
    }

    /// Tags complets de la balise à l'instant `at`.
    pub async fn tags(&self, at: u64) -> Result<Vec<Vec<String>>, String> {
        let (_, _, mut tags) = crate::solar_cron::health_tags(&self.db, at, self.weekly_costs).await?;
        let seats = self.seats().await?;
        let swarm = self.stations.swarm_id().await?;
        let mut add = |k: &str, v: String| tags.push(vec![k.to_owned(), v]);
        add("t", "uplanet".into());
        add("t", "economic-health".into());
        if let Some(p) = &self.stations.peer_id {
            add("station", p.clone());
        }
        add("station:name", self.name.clone());
        add("swarm_id", swarm.clone());
        add("constellation", swarm);
        if let Some(u) = self.public_url.as_ref().filter(|u| u.starts_with("https://") || u.starts_with("http://")) {
            let u = u.trim_end_matches('/').to_owned();
            add("url", u.clone());
            add("station:url", u);
        }
        if let Some(r) = coop_relay(&self.db, &self.stations.uplanet_name).await {
            add("relay", r);
        }
        if let Some((lat, lon)) = self.origin {
            add("geo:lat", umap_coord(lat));
            add("geo:lon", umap_coord(lon));
        }
        add("capacity:multipass_used", seats.used.to_string());
        add("capacity:multipass_total", self.registrar.limits().max_accounts.to_string());
        add("register:open", seats.open.to_string());
        add("price:multipass", self.weekly_rent.to_string());
        add("cost:paf", self.weekly_costs.to_string());
        let hw = self.hardware;
        add("hw:tier", hw.tier().label().to_ascii_lowercase());
        add("hw:power_score", format!("{:.0}", hw.score()));
        add("hw:cpu_cores", hw.cpu_cores.to_string());
        add("hw:ram_gb", format!("{:.1}", hw.ram_gb));
        add("hw:gpu_vram_gb", format!("{:.0}", hw.vram_gb));
        add("software", format!("astroportd/{}", env!("CARGO_PKG_VERSION")));
        Ok(tags)
    }

    /// Signe et publie la balise : relai local, relais `--relay`, puis relai de la coopérative. Réussie dès qu'un
    /// relai l'accepte ; rend le compte rendu.
    pub async fn publish(&self, at: u64) -> Result<String, String> {
        let mut tags = self.tags(at).await?;
        // Énergie (plugin `energy`) : seulement des mesures à jour, sous les noms du rapport d'Astroport.ONE.
        let mut content = String::new();
        if let Some(crate::energy::Energy::Fresh(r)) = self.station_home.as_deref().map(|h| crate::energy::read(h, at)) {
            tags.extend(crate::energy::beacon_tags(&r));
            content = crate::energy::beacon_content(&r).map(|v| v.to_string()).unwrap_or_default();
        }
        let event = UnsignedEvent { created_at: at, kind: kind::ECONOMIC_HEALTH, tags, content }.sign(&self.node_secret).map_err(|e| e.to_string())?;
        let id = event.id[..12].to_owned();
        let local = self.publisher.publish(event.clone()).await;
        let mut notes = Vec::new();
        let mut accepted = local.is_ok();
        if let Err(e) = &local {
            notes.push(e.clone());
        }
        if let Some(coop) = coop_relay(&self.db, &self.stations.uplanet_name).await.filter(|r| !self.relays.contains(r)) {
            match self.client.publish(&coop, &event).await {
                Ok((true, _)) => accepted = true,
                Ok((false, why)) => notes.push(format!("{coop} : {why}")),
                Err(e) => notes.push(format!("{coop} : {e}")),
            }
        }
        match (accepted, notes.is_empty()) {
            (true, true) => Ok(format!("balise 30850 publiée ({id})")),
            (true, false) => Ok(format!("balise 30850 publiée ({id}) ; refusée par : {}", notes.join(" ; "))),
            (false, _) => Err(format!("balise 30850 non publiée : {}", notes.join(" ; "))),
        }
    }

    /// Relit les 30850 récents sur les relais `--relay` et celui de la coopérative ; rend le nombre de nouveaux.
    pub async fn refresh(&self, at: u64) -> Result<usize, String> {
        let mut relays = self.relays.clone();
        if let Some(coop) = coop_relay(&self.db, &self.stations.uplanet_name).await.filter(|r| !relays.contains(r)) {
            relays.push(coop);
        }
        let filter = Filter {
            kinds: Some(vec![kind::ECONOMIC_HEALTH]),
            since: Some(at.saturating_sub(MAX_AGE_SECS)),
            limit: Some(500),
            tags: [('d', vec!["economic-health".to_owned()])].into_iter().collect(),
            ..Filter::default()
        };
        let (mut fresh, mut errors) = (0, Vec::new());
        for relay in &relays {
            match self.client.fetch(relay, &filter).await {
                Ok(events) => {
                    for e in events.into_iter().filter(|e| e.kind == kind::ECONOMIC_HEALTH) {
                        if matches!(ingest_event(self.db.writer(), e, CLAIM_OWNER).await, Ok(Ingested::Inserted)) {
                            fresh += 1;
                        }
                    }
                }
                Err(e) => errors.push(format!("{relay} : {e}")),
            }
        }
        if !relays.is_empty() && errors.len() == relays.len() {
            return Err(errors.join(" ; "));
        }
        Ok(fresh)
    }

    fn tell(&self, kind: &str, msg: String) {
        match &self.activity {
            Some(bus) => {
                bus.publish(kind, "beacon", msg, None);
            }
            None => println!("{msg}"),
        }
    }

    /// Boucle : publication au démarrage, sur changement de places, toutes les [`EVERY_SECS`] et sur réveil ;
    /// relecture de l'annuaire toutes les [`REFRESH_SECS`].
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        tokio::select! { () = tokio::time::sleep(Duration::from_secs(FIRST_DELAY_SECS)) => {}, _ = stop.changed() => return }
        let (mut last_pub, mut last_refresh, mut last_seats, mut last_error) = (0u64, 0u64, None::<Seats>, None::<String>);
        let mut nudged = true;
        loop {
            let at = now();
            let seats = self.seats().await.ok();
            let changed = seats.is_some() && seats != last_seats && at.saturating_sub(last_pub) >= MIN_GAP_SECS;
            if nudged || changed || at.saturating_sub(last_pub) >= EVERY_SECS {
                match self.publish(at).await {
                    Ok(msg) => {
                        last_seats = seats;
                        if last_error.take().is_some() || last_pub == 0 {
                            self.tell("system", msg);
                        }
                    }
                    // Une même erreur n'est dite qu'une fois ; la suivante réessaie au prochain pas.
                    Err(e) => {
                        if last_error.as_ref() != Some(&e) {
                            self.tell("alert", e.clone());
                            last_error = Some(e);
                        }
                    }
                }
                last_pub = at;
            }
            if at.saturating_sub(last_refresh) >= REFRESH_SECS {
                if let Err(e) = self.refresh(at).await {
                    eprintln!("annuaire des stations : {e}");
                }
                last_refresh = at;
            }
            nudged = false;
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(TICK_SECS)) => {},
                () = self.nudge.notified() => nudged = true,
                _ = stop.changed() => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(tags: &[(&str, &str)], at: i64) -> Value {
        json!({ "created_at": at, "tags": tags.iter().map(|(k, v)| ((*k).to_owned(), json!(v))).collect::<serde_json::Map<_, _>>() })
    }

    #[test]
    fn coordinates_are_rounded_to_the_umap_cell() {
        assert_eq!((umap_coord(43.60456), umap_coord(1.44419), umap_coord(-0.001)), ("43.60".to_owned(), "1.44".to_owned(), "0.00".to_owned()));
    }

    #[test]
    fn a_legacy_station_of_the_same_uplanet_is_listed_with_its_seats_and_its_g1_page() {
        let s = summary(&[("station", "12D3KooWlibra"), ("station:name", "libra"), ("swarm_id", "g1X"), ("geo:lat", "46.16"), ("geo:lon", "-1.14"), ("capacity:multipass_used", "5"), ("capacity:multipass_total", "0"), ("station:url", "https://u.exemple.org/"), ("hw:tier", "standard")], 100);
        let row = station_row(&s, "g1X", Some("12D3KooWme"), 90).unwrap();
        assert_eq!((row["name"].as_str(), row["open"].as_bool(), row["self"].as_bool()), (Some("libra"), Some(true), Some(false)), "0 place annoncée = sans limite");
        assert_eq!((row["join"].as_str(), row["lat"].as_f64(), row["astroportd"].as_bool()), (Some("https://u.exemple.org/g1"), Some(46.16), Some(false)));
        assert!(station_row(&s, "g1Autre", None, 90).is_none(), "une autre UPlanet n'apparaît pas");
    }

    #[test]
    fn an_astroportd_station_says_itself_whether_it_accepts_members() {
        let full = summary(&[("station", "12D3KooWme"), ("swarm_id", "g1X"), ("register:open", "false"), ("capacity:multipass_used", "3"), ("capacity:multipass_total", "500"), ("url", "https://st.exemple.org"), ("geo:lat", "0"), ("geo:lon", "0")], 100);
        let row = station_row(&full, "g1X", Some("12D3KooWme"), 90).unwrap();
        assert_eq!((row["open"].as_bool(), row["self"].as_bool(), row["join"].as_str()), (Some(false), Some(true), Some("https://st.exemple.org/register")), "fermée par son Capitaine malgré les places");
        assert!(row["lat"].is_null(), "0,0 = position inconnue");
        assert!(station_row(&summary(&[("swarm_id", "g1X")], 1), "g1X", None, 1).is_none(), "sans PeerID, pas de station");
    }
}
