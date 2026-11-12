//! **Nouvelle machine dans la constellation** : alerte du Capitaine.
//!
//! Toutes les [`EVERY`] secondes, la station relit l'annuaire des stations de son UPlanet (balises 30850) et la compare à
//! celles qu'elle a déjà vues (`kv`, portée [`SCOPE`]). Chaque station inconnue donne **une** alerte dans le flux du
//! Capitaine : son nom, son profil, son Power-Score, son GPU et la PAF qu'elle annonce, avec le lien du cockpit.
//!
//! Au tout premier passage d'une base, les stations déjà présentes sont notées **sans alerte** (sinon un Capitaine qui
//! installe cette version serait prévenu de toute la constellation d'un coup). Une station qui disparaît puis revient n'est
//! pas « nouvelle » : elle reste dans la mémoire. Une station ne peut annoncer que ce que sa balise dit d'elle : l'alerte
//! prévient, elle n'admet personne (l'admission au cercle social reste une décision du Capitaine).

use std::sync::Arc;
use std::time::Duration;

use astro_core::Audience;
use astro_db::Db;
use serde_json::Value;
use tokio::sync::watch;

use crate::beacon::Stations;

/// Mémoire des stations vues.
pub const SCOPE: &str = "stations_seen";
/// Marque du premier passage.
const BASELINE: &str = "_baseline";
/// Période de relecture.
const EVERY: Duration = Duration::from_secs(120);
/// Attente avant le premier passage (les balises arrivent après le démarrage).
const FIRST: Duration = Duration::from_secs(90);

/// Stations de `rows` (lignes de l'annuaire) que `seen` ne connaît pas, hors la station elle-même.
pub fn unseen<'a>(rows: &'a [Value], seen: &std::collections::HashSet<String>) -> Vec<&'a Value> {
    rows.iter().filter(|r| r["self"] != Value::Bool(true)).filter(|r| r["station"].as_str().is_some_and(|s| !seen.contains(s))).collect()
}

/// Texte de l'alerte.
pub fn message(row: &Value, constellation: &str) -> String {
    let name = row["name"].as_str().unwrap_or("station inconnue");
    let gpu = row["gpu_gb"].as_f64().filter(|g| *g > 0.0).map_or_else(String::new, |g| format!(", GPU {g:.0} Go"));
    let paf = row["paf_cents"].as_u64().filter(|p| *p > 0).map_or_else(|| "PAF non annoncée".to_owned(), |p| format!("PAF {} Ẑ/semaine", crate::paf::text_of(p)));
    let url = row["url"].as_str().map_or_else(String::new, |u| format!(" · {u}"));
    format!(
        "Nouvelle machine dans la constellation {constellation} : {name} (profil {}, Power-Score {:.0}{gpu}, {paf}){url} — à examiner dans #/constellation",
        row["tier"].as_str().unwrap_or("?"),
        row["power"].as_f64().unwrap_or(0.0)
    )
}

/// Surveillance de l'annuaire.
pub struct NewStations {
    db: Arc<Db>,
    stations: Arc<Stations>,
    activity: astro_core::ActivityBus,
}

impl NewStations {
    /// Surveillance de `stations`, alertes dans `activity`.
    pub fn new(db: Arc<Db>, stations: Arc<Stations>, activity: astro_core::ActivityBus) -> Self {
        Self { db, stations, activity }
    }

    async fn seen(&self) -> Result<std::collections::HashSet<String>, String> {
        self.db
            .readers()
            .read(|c| {
                let mut s = c.prepare("SELECT key FROM kv WHERE scope = ?1")?;
                Ok(s.query_map([SCOPE], |r| r.get::<_, String>(0))?.collect::<Result<std::collections::HashSet<_>, _>>()?)
            })
            .await
            .map_err(|e| e.to_string())
    }

    async fn remember(&self, key: &str, at: u64) {
        let params = vec![SCOPE.to_owned().into(), key.to_owned().into(), at.to_string().into(), i64::try_from(at).unwrap_or(0).into()];
        let _ = self.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO NOTHING", params).await;
    }

    /// Un passage : les stations nouvelles sont notées, et signalées sauf au premier passage de la base. Retourne le nombre d'alertes.
    pub async fn scan(&self, at: u64) -> usize {
        let Ok(list) = self.stations.list(at).await else { return 0 };
        let rows = list["stations"].as_array().cloned().unwrap_or_default();
        let Ok(seen) = self.seen().await else { return 0 };
        let first = !seen.contains(BASELINE);
        let label = if self.stations.uplanet_name().bytes().all(|b| b == b'0') { "ORIGIN".to_owned() } else { "ẐEN".to_owned() };
        let mut alerts = 0;
        for row in unseen(&rows, &seen) {
            let Some(id) = row["station"].as_str() else { continue };
            self.remember(id, at).await;
            if !first {
                self.activity.publish_to(Audience::Captain, "alert", "new-station", message(row, &label), None);
                alerts += 1;
            }
        }
        if first {
            self.remember(BASELINE, at).await;
        }
        alerts
    }

    /// Boucle de surveillance jusqu'à l'arrêt.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut wait = FIRST;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.changed() => return,
            }
            wait = EVERY;
            self.scan(u64::try_from(crate::private_backup::now()).unwrap_or(0)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: &str, me: bool) -> Value {
        json!({ "station": id, "name": format!("machine-{id}"), "self": me, "tier": "brain", "power": 61.4, "gpu_gb": 24.0, "paf_cents": 1450, "url": "https://m.example.org" })
    }

    #[test]
    fn seules_les_stations_inconnues_et_autres_que_soi_comptent() {
        let rows = vec![row("a", true), row("b", false), row("c", false)];
        let seen: std::collections::HashSet<String> = ["c".to_owned()].into();
        let new = unseen(&rows, &seen);
        assert_eq!(new.len(), 1);
        assert_eq!(new[0]["station"], "b");
    }

    #[test]
    fn le_message_dit_profil_puissance_gpu_et_paf() {
        let m = message(&row("b", false), "ORIGIN");
        for part in ["Nouvelle machine", "ORIGIN", "machine-b", "brain", "Power-Score 61", "GPU 24 Go", "PAF 14.50 Ẑ/semaine", "https://m.example.org", "#/constellation"] {
            assert!(m.contains(part), "manque « {part} » dans : {m}");
        }
        let mut r = row("b", false);
        r["paf_cents"] = Value::Null;
        r["gpu_gb"] = json!(0.0);
        let m = message(&r, "ẐEN");
        assert!(m.contains("PAF non annoncée") && !m.contains("GPU"), "{m}");
    }
}
