//! Orchestrateur de nuit : le **20h12 solaire local vrai** de la station.
//!
//! L'instant se calcule chaque jour à partir de la longitude de la station et de l'équation du temps de Spencer
//! (`astro_core::solar`). À cet instant, dans cet ordre, et chaque étape indépendante des suivantes (un échec est
//! dit, il n'arrête pas la nuit) :
//!
//! 1. garde-fou RTC : horloge antérieure à 2026 ⇒ **rien** n'est fait ;
//! 2. verrou du jour (`done`), posé **avant** tout travail : deux déclenchements le même jour solaire ne font pas
//!    deux nuits (leçon du doublon de mail du legacy) ;
//! 3. snapshot atomique de `uplanet.db` (`VACUUM INTO`), les `N` plus récents sont gardés ;
//! 4. purge des réclamations anciennes (`claimed`, 7 jours), des événements Nostr expirés (NIP-40) et des liens d'invités expirés ;
//! 5. balayage sémantique RÊVE ;
//! 6. sauvegarde privée : envoi chez les amis des fichiers non partagés pas encore sauvegardés ;
//! 7. clôture hebdomadaire de la régie (3×1/3) : toute semaine ISO terminée et pas encore close, soit le lundi
//!    pour la semaine finie le dimanche, avec rattrapage jusqu'à 5 semaines si la station était éteinte ;
//! 8. émission de l'événement de santé économique (kind 30850, `d = economic-health`).
//!
//! Une station arrêtée à 20h12 ne rejoue pas la nuit manquée (sauf la clôture hebdomadaire, qui se rattrape).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use astro_bro::publisher::Publisher;
use astro_core::solar::{Coordinates, ensure_clock_sane, next_20h12, solar_20h12_of, system_now_checked};
use astro_core::week::{date_string, iso_week, week_start};
use astro_core::{ActivityBus, Amount};
use astro_db::Db;
use astro_economy::love::{WeekInput, WeekOutcome, close_week};
use astro_economy::split::SplitConfig;
use astro_economy::store as economy_store;
use astro_nostr_proto::{UnsignedEvent, kind};
use rusqlite::types::Value;
use tokio::sync::watch;

const DAY: u64 = 86_400;
/// Âge au-delà duquel une réclamation `claimed` est purgée.
pub const CLAIM_RETENTION_SECS: u64 = 7 * DAY;
/// Semaines passées que la clôture rattrape si la station était éteinte.
pub const CATCH_UP_WEEKS: u64 = 5;
/// Snapshots conservés.
pub const KEEP_SNAPSHOTS: usize = 7;

/// Futur boxé rendu par un crochet.
pub type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;
/// Crochet d'une étape : reçoit l'heure courante, rend un compte rendu ou une erreur dite en clair.
pub type Hook = Arc<dyn Fn(u64) -> BoxFut<Result<String, String>> + Send + Sync>;

/// Compte rendu d'une étape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    /// Nom court de l'étape.
    pub name: &'static str,
    /// Réussie ?
    pub ok: bool,
    /// Ce qui s'est passé, ou pourquoi ça a échoué.
    pub detail: String,
}

/// Compte rendu d'une nuit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NightReport {
    /// Jour solaire (`AAAA-MM-JJ`).
    pub date: String,
    /// La nuit de ce jour solaire avait déjà eu lieu : rien n'a été refait.
    pub already_done: bool,
    /// Étapes jouées, dans l'ordre.
    pub steps: Vec<StepReport>,
}

impl NightReport {
    /// Toutes les étapes ont réussi ?
    pub fn all_ok(&self) -> bool {
        self.steps.iter().all(|s| s.ok)
    }

    fn step(&mut self, name: &'static str, r: Result<String, String>) {
        let (ok, detail) = match r {
            Ok(d) => (true, d),
            Err(e) => (false, e),
        };
        self.steps.push(StepReport { name, ok, detail });
    }
}

/// Les moyens de la nuit.
pub struct Nightly<P: Publisher> {
    /// Base `uplanet.db`.
    pub db: Arc<Db>,
    /// Dossier des snapshots.
    pub snapshot_dir: PathBuf,
    /// Clé Nostr (hex) du NODE qui encaisse les loyers.
    pub node_hex: String,
    /// Clé du NODE : signe l'événement de santé. Sans elle, l'étape est sautée en le disant.
    pub node_secret: Option<[u8; 32]>,
    /// PeerID de la station (tag `station`).
    pub peer_id: Option<String>,
    /// Charges hebdomadaires (PAF) à déduire des recettes pour la clôture.
    pub weekly_costs: Amount,
    /// Réglages de la ventilation 3×1/3.
    pub split: SplitConfig,
    /// Purge des liens d'invités expirés.
    pub purge_shares: Option<Hook>,
    /// Balayage RÊVE.
    pub dream: Option<Hook>,
    /// Sauvegarde privée chez les amis (envoi du jour).
    pub backup: Option<Hook>,
    /// Contrôle d'intégrité du stockage (blobs absents ou corrompus).
    pub integrity: Option<Hook>,
    /// Publication de l'événement de santé (relai local + relais).
    pub publisher: Option<Arc<P>>,
    /// Journal d'activité.
    pub activity: Option<ActivityBus>,
    /// Respiration entre deux étapes lourdes : laisse passer le trafic HTTP/WebDAV sur un petit CPU (zéro en test).
    pub pause: Duration,
}

/// Durée de garde du journal des changements du carnet d'adresses.
const DAV_CHANGES_RETENTION_SECS: u64 = 30 * 24 * 3600;

fn int(n: u64) -> Value {
    Value::Integer(i64::try_from(n).unwrap_or(i64::MAX))
}

impl<P: Publisher> Nightly<P> {
    /// Joue la nuit de l'instant `now` pour une station à la longitude `lon` (le jour solaire en dépend).
    pub async fn run_night(&self, now: u64, lon: f64) -> NightReport {
        let solar_day = ((now as f64 + lon * 240.0).max(0.0)) as u64;
        self.run_night_of(now, date_string(solar_day)).await
    }

    /// Joue la nuit du jour solaire `date` à l'instant `now` (la nuit de la veille, pour un rattrapage).
    async fn run_night_of(&self, now: u64, date: String) -> NightReport {
        let mut report = NightReport { date: date.clone(), already_done: false, steps: Vec::new() };

        // 1. Garde-fou RTC : sans heure fiable, aucune échéance, aucun marqueur, aucune clôture.
        if let Err(e) = ensure_clock_sane(now) {
            report.step("rtc", Err(e.to_string()));
            return self.finish(report, now).await;
        }
        report.step("rtc", Ok(format!("horloge saine (≥ 2026) : {}", date_string(now))));

        // 2. Verrou d'abord, travail ensuite.
        let claim = self.db.writer().execute("INSERT OR IGNORE INTO done(key, value, at) VALUES (?1, 'nightly', ?2)", vec![Value::Text(format!("nightly:{date}")), int(now)]).await;
        match claim {
            Ok(o) if o.changes == 0 => {
                report.already_done = true;
                report.step("verrou", Ok(format!("la nuit du {date} a déjà eu lieu")));
                return report;
            }
            Ok(_) => {}
            Err(e) => {
                report.step("verrou", Err(format!("base inscriptible impossible : {e}")));
                return self.finish(report, now).await;
            }
        }

        let r = self.snapshot(&date).await;
        report.step("snapshot", r);
        let r = self.db.writer().checkpoint().await.map(|()| "journal WAL tronqué".to_owned()).map_err(|e| format!("wal_checkpoint : {e}"));
        report.step("checkpoint", r);
        let r = self.purge(now).await;
        report.step("purge", r);
        tokio::time::sleep(self.pause).await;
        let r = match &self.dream {
            Some(h) => h(now).await,
            None => Ok("mémoire sémantique inactive (pas d'Ollama)".into()),
        };
        report.step("rêve", r);
        tokio::time::sleep(self.pause).await;
        let r = match &self.backup {
            Some(h) => h(now).await,
            None => Ok("sauvegarde privée inactive".into()),
        };
        report.step("sauvegarde", r);
        tokio::time::sleep(self.pause).await;
        let r = match &self.integrity {
            Some(h) => h(now).await,
            None => Ok("contrôle d'intégrité inactif".into()),
        };
        report.step("intégrité", r);
        let r = self.close_weeks(now).await;
        report.step("clôture", r);
        let r = self.health(now).await;
        report.step("santé", r);
        self.finish(report, now).await
    }

    /// Journal + bus d'activité : une nuit ratée se voit.
    async fn finish(&self, report: NightReport, now: u64) -> NightReport {
        let failed: Vec<String> = report.steps.iter().filter(|s| !s.ok).map(|s| format!("{} : {}", s.name, s.detail)).collect();
        let data = serde_json::json!({ "date": report.date, "ok": failed.is_empty(), "steps": report.steps.iter().map(|s| serde_json::json!({ "step": s.name, "ok": s.ok, "detail": s.detail })).collect::<Vec<_>>() });
        let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('nightly', ?1, ?2)", vec![int(now), Value::Text(data.to_string())]).await;
        if let Some(bus) = &self.activity {
            if failed.is_empty() {
                bus.publish("system", "solar_cron", format!("20h12 solaire : nuit du {} terminée ({} étapes)", report.date, report.steps.len()), None);
            } else {
                bus.publish("alert", "solar_cron", format!("20h12 solaire : {}", failed.join(" ; ")), None);
            }
        }
        report
    }

    /// Snapshot atomique `uplanet-AAAA-MM-JJ.db`, les [`KEEP_SNAPSHOTS`] plus récents sont conservés.
    async fn snapshot(&self, date: &str) -> Result<String, String> {
        tokio::fs::create_dir_all(&self.snapshot_dir).await.map_err(|e| format!("dossier {} : {e}", self.snapshot_dir.display()))?;
        let dest = self.snapshot_dir.join(format!("uplanet-{date}.db"));
        self.db.writer().vacuum_into(&dest).await.map_err(|e| format!("VACUUM INTO : {e}"))?;
        let mut names = Vec::new();
        let mut dir = tokio::fs::read_dir(&self.snapshot_dir).await.map_err(|e| e.to_string())?;
        while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("uplanet-") && name.ends_with(".db") {
                names.push(name);
            }
        }
        names.sort();
        let mut removed = 0;
        while names.len() > KEEP_SNAPSHOTS {
            let old = names.remove(0);
            tokio::fs::remove_file(self.snapshot_dir.join(&old)).await.map_err(|e| format!("suppression de {old} : {e}"))?;
            removed += 1;
        }
        let size = tokio::fs::metadata(&dest).await.map_or(0, |m| m.len());
        Ok(format!("{} ({} o), {removed} ancien(s) retiré(s)", dest.display(), size))
    }

    /// Réclamations de plus de 7 jours et liens d'invités expirés.
    async fn purge(&self, now: u64) -> Result<String, String> {
        let cutoff = int(now.saturating_sub(CLAIM_RETENTION_SECS));
        let claimed = self.db.writer().execute("DELETE FROM claimed WHERE at < ?1", vec![cutoff]).await.map_err(|e| format!("claimed : {e}"))?.changes;
        let shares = match &self.purge_shares {
            Some(h) => h(now).await.map_err(|e| format!("liens d'invités : {e}"))?,
            None => "liens d'invités : sans objet".into(),
        };
        // NIP-40 : les événements expirés ne sont plus servis ; la nuit les retire de la base (leurs tags suivent).
        let expired = self.db.writer().execute("DELETE FROM nostr_events WHERE expires_at IS NOT NULL AND expires_at <= ?1", vec![int(now)]).await.map_err(|e| format!("événements expirés : {e}"))?.changes;
        // Journal des changements CardDAV : 30 jours de mémoire ; le dernier numéro purgé devient le plancher des jetons valides.
        let keep = int(now.saturating_sub(DAV_CHANGES_RETENTION_SECS));
        self
            .db
            .writer()
            .batch(vec![
                astro_db::Statement::new("INSERT OR REPLACE INTO schema_info(key, value) SELECT 'dav_changes_floor', CAST(MAX(seq) AS TEXT) FROM dav_changes WHERE at < ?1 HAVING MAX(seq) IS NOT NULL", vec![keep.clone()]),
                astro_db::Statement::new("DELETE FROM dav_changes WHERE at < ?1", vec![keep]),
            ])
            .await
            .map_err(|e| format!("journal CardDAV : {e}"))?;
        Ok(format!("{claimed} réclamation(s) purgée(s) ; {expired} événement(s) expiré(s) ; journal CardDAV purgé ; {shares}"))
    }

    /// Recettes d'une semaine `[start, end[` : loyers constatés.
    async fn revenue(&self, start: u64, end: u64) -> Result<Amount, String> {
        let cents: i64 = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE(SUM(amount_cents), 0) FROM rent_payments WHERE at >= ?1 AND at < ?2",
                    rusqlite::params![i64::try_from(start).unwrap_or(0), i64::try_from(end).unwrap_or(i64::MAX)],
                    |r| r.get(0),
                )?)
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(Amount(u64::try_from(cents).unwrap_or(0)))
    }

    /// Trésorerie (`CASH` des ventilations moins les prélèvements) et résultat de l'exercice avant `week`.
    async fn books(&self, week: &str) -> Result<(Amount, Amount), String> {
        let year = week.split('-').next().unwrap_or_default().to_owned();
        let rows: Vec<(String, String)> = self
            .db
            .readers()
            .read(|c| {
                let mut s = c.prepare("SELECT stream, data FROM journal WHERE stream IN ('economy.3x13', 'economy.drawdown') ORDER BY id")?;
                Ok(s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(|e| e.to_string())?;
        let (mut cash, mut taken, mut ytd) = (0u64, 0u64, 0u64);
        let dec = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).and_then(|x| x.parse::<Amount>().ok()).map_or(0, |a| a.0);
        for (stream, data) in rows {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
            if stream == "economy.3x13" {
                cash = cash.saturating_add(dec(&v, "cash"));
                if v.get("week").and_then(|w| w.as_str()).is_some_and(|w| w.starts_with(&year)) {
                    ytd = ytd.saturating_add(dec(&v, "surplus"));
                }
            } else {
                taken = taken.saturating_add(dec(&v, "gap"));
            }
        }
        Ok((Amount(cash.saturating_sub(taken)), Amount(ytd)))
    }

    /// Clôture les semaines ISO terminées et pas encore closes (rattrapage borné, jamais avant la première nuit).
    async fn close_weeks(&self, now: u64) -> Result<String, String> {
        let w = self.db.writer();
        w.execute("INSERT OR IGNORE INTO kv(scope, key, value, updated_at) VALUES ('nightly', 'first_run', CAST(?1 AS TEXT), ?1)", vec![int(now)]).await.map_err(|e| e.to_string())?;
        let first: i64 = self.db.readers().read(|c| Ok(c.query_row("SELECT CAST(value AS INTEGER) FROM kv WHERE scope = 'nightly' AND key = 'first_run'", [], |r| r.get(0))?)).await.map_err(|e| e.to_string())?;
        let floor = week_start(u64::try_from(first).unwrap_or(now));
        let this_week = week_start(now);
        let mut done = Vec::new();
        for back in (1..=CATCH_UP_WEEKS).rev() {
            let start = this_week.saturating_sub(back * 7 * DAY);
            if start < floor {
                continue;
            }
            let week = iso_week(start);
            let key = economy_store::week_key(&week);
            let closed: bool = self.db.readers().read({ let key = key.clone(); move |c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM done WHERE key = ?1)", [key], |r| r.get(0))?) }).await.map_err(|e| e.to_string())?;
            if closed {
                continue;
            }
            let revenue = self.revenue(start, start + 7 * DAY).await?;
            let (cash_balance, ytd_profit) = self.books(&week).await?;
            let input = WeekInput { week: week.clone(), revenue, costs: self.weekly_costs, cash_balance, ytd_profit };
            let outcome = close_week(&self.split, &input).map_err(|e| format!("{week} : {e}"))?;
            let label = match &outcome {
                WeekOutcome::Split(a) => format!("{week} ventilée (surplus {} Ẑ)", a.surplus),
                WeekOutcome::Drawdown { gap, .. } => format!("{week} : trésorerie entamée de {gap} Ẑ"),
                WeekOutcome::Volunteer(v) => format!("{week} : bénévolat actif, {} Ẑ couverts par le Capitaine", v.deficit),
            };
            w.batch(economy_store::close_statements(&week, &outcome, &self.node_hex, now)).await.map_err(|e| format!("{week} : {e}"))?;
            done.push(label);
        }
        Ok(if done.is_empty() { "aucune semaine à clôturer".into() } else { done.join(" ; ") })
    }

    /// Événement 30850 `economic-health` de la dernière semaine terminée, signé par le NODE.
    async fn health(&self, now: u64) -> Result<String, String> {
        let Some(secret) = self.node_secret else { return Ok("sautée : clé du NODE absente".into()) };
        let Some(publisher) = &self.publisher else { return Ok("sautée : aucun relai pour publier".into()) };
        let start = week_start(now).saturating_sub(7 * DAY);
        let week = iso_week(start);
        let key = economy_store::week_key(&week);
        let status: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM done WHERE key = ?1", [key], |r| r.get(0)).ok())).await.map_err(|e| e.to_string())?;
        let health = match status.as_deref() {
            Some("split") => "healthy",
            Some("drawdown") => "drawdown",
            Some("volunteer") => "volunteer",
            _ => "unknown",
        };
        let revenue = self.revenue(start, start + 7 * DAY).await?;
        let (cash, _) = self.books(&week).await?;
        let mut tags = vec![
            vec!["d".to_owned(), "economic-health".to_owned()],
            vec!["week".to_owned(), week.clone()],
            vec!["health:status".to_owned(), health.to_owned()],
            vec!["health:revenue".to_owned(), revenue.to_string()],
            vec!["health:costs".to_owned(), self.weekly_costs.to_string()],
            vec!["health:cash".to_owned(), cash.to_string()],
        ];
        if let Some(p) = &self.peer_id {
            tags.push(vec!["station".to_owned(), p.clone()]);
        }
        let event = UnsignedEvent { created_at: now, kind: kind::ECONOMIC_HEALTH, tags, content: String::new() }.sign(&secret).map_err(|e| e.to_string())?;
        let id = event.id.clone();
        publisher.publish(event).await.map_err(|e| format!("publication impossible : {e}"))?;
        Ok(format!("30850 {week} « {health} » publié ({})", &id[..12]))
    }

    /// Rattrapage au démarrage : joue les nuits échues que la station a manquées, sans bloquer l'appelant (à lancer
    /// dans une tâche de fond). Le verrou `done nightly:<jour>` rend l'appel idempotent.
    ///
    /// - la nuit du jour solaire **courant** est due si son 20h12 est passé et qu'elle n'a pas eu lieu ;
    /// - la nuit de la **veille** est due si elle n'a pas eu lieu alors que la station avait déjà fait au moins une
    ///   nuit (station éteinte à 20h12). Les nuits plus anciennes ne sont pas rejouées ; la clôture hebdomadaire,
    ///   elle, rattrape jusqu'à [`CATCH_UP_WEEKS`] semaines à chaque nuit jouée.
    ///
    /// Horloge antérieure à 2026 : rien n'est fait, et le dit.
    pub async fn catch_up(&self, now: u64, coords: &Coordinates) -> Vec<NightReport> {
        let mut out = Vec::new();
        if let Err(e) = ensure_clock_sane(now) {
            eprintln!("20h12 solaire : rattrapage au démarrage impossible : {e}");
            return out;
        }
        let today = ((now as f64 + coords.lon() * 240.0).max(0.0) as u64) / DAY;
        let done = |day: u64| {
            let key = format!("nightly:{}", date_string(day * DAY));
            let db = self.db.clone();
            async move { db.readers().read(move |c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM done WHERE key = ?1)", [key], |r| r.get::<_, bool>(0))?)).await }
        };
        let ever: bool = self.db.readers().read(|c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM done WHERE key LIKE 'nightly:%')", [], |r| r.get(0))?)).await.unwrap_or(false);
        let mut due = Vec::new();
        if ever && today > 0 && matches!(done(today - 1).await, Ok(false)) {
            due.push(today - 1);
        }
        if solar_20h12_of(today, coords, now).is_ok_and(|t| now >= t) && matches!(done(today).await, Ok(false)) {
            due.push(today);
        }
        for day in due {
            let report = self.run_night_of(now, date_string(day * DAY)).await;
            for s in report.steps.iter().filter(|s| !s.ok) {
                eprintln!("20h12 solaire (rattrapage) : {} : {}", s.name, s.detail);
            }
            out.push(report);
        }
        out
    }

    /// Boucle du démon : dort jusqu'au prochain 20h12 solaire, joue la nuit, recommence.
    pub async fn run(self: Arc<Self>, coords: Coordinates, mut stop: watch::Receiver<bool>) {
        loop {
            let wait = |secs: u64| tokio::time::sleep(Duration::from_secs(secs));
            let now = match system_now_checked() {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("20h12 solaire : {e} (nouvel essai dans 1 h)");
                    tokio::select! { () = wait(3_600) => continue, _ = stop.changed() => return }
                }
            };
            let next = match next_20h12(now, &coords) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("20h12 solaire : {e} (nouvel essai dans 1 h)");
                    tokio::select! { () = wait(3_600) => continue, _ = stop.changed() => return }
                }
            };
            // Tranches de 5 minutes : une mise en veille ou un saut d'horloge ne décale pas la nuit d'un jour.
            while let Ok(n) = system_now_checked() {
                if n >= next {
                    break;
                }
                tokio::select! { () = wait((next - n).min(300)) => {}, _ = stop.changed() => return }
            }
            let report = self.run_night(next.max(system_now_checked().unwrap_or(next)), coords.lon()).await;
            for s in report.steps.iter().filter(|s| !s.ok) {
                eprintln!("20h12 solaire : {} : {}", s.name, s.detail);
            }
            tokio::select! { () = wait(2) => {}, _ = stop.changed() => return }
        }
    }
}
