//! Règlement hebdomadaire du **compte technique d'hébergement** ([`crate::coop_wallets`]), une fois la semaine clôturée
//! par la nuit de 20h12 ([`crate::solar_cron`]) : ce que les MULTIPASS ont payé pour leur hébergement est redistribué
//! **sur la Ğ1**, dans cet ordre et seulement **si le montant reçu le permet** :
//!
//! 1. **1×PAF → NODE** (loyer matériel de l'Armateur) ;
//! 2. **2×PAF → MULTIPASS du Capitaine** (rétribution du travail) ;
//! 3. le surplus, tel que la clôture l'a ventilé (règle 3×1/3, [`astro_economy::split`]) : provision d'impôt
//!    (`IMPOT`), prime de gestion (1 % → MULTIPASS du Capitaine), puis `TREASURY`, `RND` et `ASSETS`.
//!
//! `weekly_costs` est la charge hebdomadaire totale **3×PAF** (PAF = un tiers). Comme dans les scripts d'Astroport.ONE,
//! `weekly_costs = 0` désactive l'économie : rien n'est viré.
//!
//! Garde-fous :
//! - on ne vire jamais plus que les recettes de la semaine (loyers constatés, vue `rents`) **ni** plus que ce que le
//!   compte technique détient au-dessus de sa réserve ;
//! - une semaine n'est réglée qu'une fois : **chaque virement est noté AVANT l'envoi** (`kv('settle', '<semaine>:<étape>')`),
//!   une issue inconnue n'est JAMAIS rejouée (le Capitaine est prévenu) ;
//! - le déficit éventuel (recettes < PAF) est celui de la clôture : trésorerie ou bénévolat actif (Love Ledger), jamais un
//!   prélèvement sur `TREASURY`, `RND` ou `ASSETS`.

use std::sync::Arc;
use std::time::Duration;

use astro_core::week::{iso_week, week_start};
use astro_core::{ActivityBus, Amount as Cents, Audience};
use astro_db::Db;
use astro_duniter::{Account, Amount, Chain, ChainError, TransferMode};
use serde_json::json;
use tokio::sync::watch;

use crate::coop_wallets::Wallets;
use crate::station::{RESERVE_G1_CENTS, ZEN_PER_G1};

/// Espace des marqueurs de règlement.
const SCOPE: &str = "settle";
/// Intervalle entre deux passages.
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// Semaines rattrapées au plus (comme la clôture).
const CATCH_UP_WEEKS: u64 = 4;
const DAY: u64 = 86_400;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Bilan d'un passage.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Virements partis.
    pub paid: u32,
    /// Étapes sans rien à virer (recettes ou solde épuisés).
    pub empty: u32,
    /// Issues inconnues après envoi (jamais rejouées).
    pub unknown: u32,
    /// Nœud Duniter injoignable : au prochain passage.
    pub retry: u32,
    /// Semaines réglées jusqu'au bout.
    pub weeks: u32,
}

/// Une étape du règlement : nom, destinataire, montant voulu (centimes de ẑ).
struct Step {
    name: &'static str,
    to: Account,
    wanted: u64,
}

/// Règlement hebdomadaire.
pub struct Settlement<C: Chain> {
    db: Arc<Db>,
    chain: Arc<C>,
    wallets: Arc<Wallets>,
    activity: ActivityBus,
    weekly_costs: Cents,
}

impl<C: Chain> Settlement<C> {
    /// `weekly_costs` : charge hebdomadaire totale (3×PAF), en centimes de ẑ.
    pub fn new(db: Arc<Db>, chain: Arc<C>, wallets: Arc<Wallets>, activity: ActivityBus, weekly_costs: Cents) -> Self {
        Self { db, chain, wallets, activity, weekly_costs }
    }

    fn alert(&self, text: String) {
        self.activity.publish_to(Audience::Captain, "alert", "settlement", text, None);
    }

    async fn marker(&self, key: &str) -> Option<String> {
        let k = key.to_owned();
        self.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, k], |r| r.get::<_, String>(0)))?)).await.ok().flatten()
    }

    async fn set_marker(&self, key: &str, value: &str) {
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![SCOPE.to_owned().into(), key.to_owned().into(), value.to_owned().into(), now().into()]).await;
    }

    async fn clear_marker(&self, key: &str) {
        let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SCOPE.to_owned().into(), key.to_owned().into()]).await;
    }

    /// Montants déjà engagés pour `week` (virements partis ou d'issue inconnue), en centimes de ẑ.
    async fn spent(&self, week: &str) -> u64 {
        let like = format!("{week}:%");
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT value FROM kv WHERE scope = ?1 AND key LIKE ?2")?;
                let values = stmt.query_map(rusqlite::params![SCOPE, like], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok(values.iter().filter_map(|v| v.split_once(':').and_then(|(s, n)| (s != "skipped").then(|| n.parse::<u64>().ok()).flatten())).sum())
            })
            .await
            .unwrap_or(0)
    }

    /// Règle les semaines clôturées pas encore réglées (les plus anciennes d'abord).
    pub async fn run_weeks(&self, at: u64) -> Result<Report, String> {
        let mut report = Report::default();
        if self.weekly_costs.0 == 0 {
            return Ok(report);
        }
        for back in (1..=CATCH_UP_WEEKS).rev() {
            let start = week_start(at).saturating_sub(back * 7 * DAY);
            let week = iso_week(start);
            let close = astro_economy::store::week_key(&week);
            let status: Option<String> = self.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM done WHERE key = ?1", [close], |r| r.get::<_, String>(0)))?)).await.map_err(|e| e.to_string())?;
            let Some(status) = status else { continue }; // pas encore clôturée
            if self.marker(&week).await.as_deref() == Some("done") {
                continue;
            }
            self.settle_week(&week, start, &status, &mut report).await?;
        }
        Ok(report)
    }

    async fn settle_week(&self, week: &str, start: u64, status: &str, report: &mut Report) -> Result<(), String> {
        let coop = self.wallets.coop().await?;
        let signer = self.wallets.tech().await?;
        let (w, from, to) = (week.to_owned(), i64::try_from(start).unwrap_or(0), i64::try_from(start + 7 * DAY).unwrap_or(i64::MAX));
        let (revenue, allocation): (i64, Option<serde_json::Value>) = self
            .db
            .readers()
            .read(move |c| {
                let revenue = c.query_row("SELECT COALESCE(SUM(amount_cents), 0) FROM rents WHERE at >= ?1 AND at < ?2", [from, to], |r| r.get::<_, i64>(0))?;
                let mut stmt = c.prepare("SELECT data FROM journal WHERE stream = 'economy.3x13' ORDER BY id DESC")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                let allocation = rows.iter().filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok()).find(|v| v.get("week").and_then(|x| x.as_str()) == Some(&w));
                Ok((revenue, allocation))
            })
            .await
            .map_err(|e| e.to_string())?;
        let dec = |k: &str| allocation.as_ref().and_then(|v| v.get(k)).and_then(|x| x.as_str()).and_then(|x| x.parse::<Cents>().ok()).map_or(0, |a| a.0);
        let paf = self.weekly_costs.0 / 3;
        let mut steps = vec![Step { name: "node", to: coop.node, wanted: paf }, Step { name: "captain", to: coop.captain, wanted: self.weekly_costs.0 - paf }];
        if status == "split" {
            steps.extend([
                Step { name: "impot", to: coop.impot, wanted: dec("is_provision") },
                Step { name: "bonus", to: coop.captain, wanted: dec("captain_bonus") },
                Step { name: "treasury", to: coop.treasury, wanted: dec("cash") },
                Step { name: "rnd", to: coop.rnd, wanted: dec("rnd") },
                Step { name: "assets", to: coop.assets, wanted: dec("assets") },
            ]);
        }
        let balance = match self.chain.balance(&signer.account()).await {
            Ok(b) => b.total.0.saturating_sub(RESERVE_G1_CENTS).saturating_mul(ZEN_PER_G1),
            Err(_) => {
                report.retry += 1;
                return Ok(());
            }
        };
        let spent = self.spent(week).await;
        let mut avail = u64::try_from(revenue).unwrap_or(0).saturating_sub(spent).min(balance);
        let mut complete = true;
        for step in &steps {
            let key = format!("{week}:{}", step.name);
            if self.marker(&key).await.is_some() {
                continue;
            }
            let amount = step.wanted.min(avail);
            let g1_cents = amount / ZEN_PER_G1;
            if g1_cents == 0 {
                self.set_marker(&key, "skipped:0").await;
                if step.wanted > 0 {
                    report.empty += 1;
                    self.alert(format!("Règlement {week} : rien à virer à « {} » ({} ẑ voulus) : recettes ou solde du compte technique épuisés", step.name, step.wanted as f64 / 100.0));
                }
                continue;
            }
            self.set_marker(&key, &format!("sending:{amount}")).await;
            let memo = format!("UPLANET:SETTLE:{week}:{}", step.name);
            match self.chain.transfer(&signer, &step.to, Amount(g1_cents), TransferMode::KeepAlive, Some(&memo)).await {
                Ok(out) => {
                    self.set_marker(&key, &format!("paid:{amount}")).await;
                    avail -= amount;
                    report.paid += 1;
                    let record = json!({ "week": week, "step": step.name, "to": step.to.ss58(), "zen_cents": amount, "g1_cents": g1_cents, "block": hex::encode(out.block_hash) });
                    let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('settlement', ?1, ?2)", vec![now().into(), record.to_string().into()]).await;
                }
                Err(ChainError::Connection(_)) => {
                    self.clear_marker(&key).await; // perdu AVANT l'envoi : réessayer est sans danger
                    report.retry += 1;
                    return Ok(());
                }
                Err(ChainError::UnknownOutcome(why)) => {
                    avail = avail.saturating_sub(amount);
                    report.unknown += 1;
                    complete = false;
                    self.alert(format!("Règlement {week}, étape « {} » : issue du virement inconnue ({why}) — vérifier le compte technique avant de rejouer", step.name));
                }
                Err(other) => {
                    self.clear_marker(&key).await;
                    report.retry += 1;
                    self.alert(format!("Règlement {week}, étape « {} » : virement refusé ({other})", step.name));
                    return Ok(());
                }
            }
        }
        if complete {
            self.set_marker(week, "done").await;
            report.weeks += 1;
            self.activity.publish_to(Audience::Captain, "economy", "settlement", format!("Semaine {week} réglée sur la Ğ1 : PAF au NODE, 2×PAF au Capitaine{}", if status == "split" { ", surplus à la coopérative" } else { "" }), None);
        }
        Ok(())
    }

    /// Boucle de fond : un passage peu après le démarrage, puis toutes les 6 h.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut wait = Duration::from_secs(300);
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.changed() => return,
            }
            wait = EVERY;
            let Ok(at) = astro_core::solar::system_now_checked() else { continue };
            match self.run_weeks(at).await {
                Ok(r) if r != Report::default() => println!("Règlement hebdomadaire : {} virement(s), {} étape(s) à vide, {} incertain(s), {} à retenter, {} semaine(s) réglée(s)", r.paid, r.empty, r.unknown, r.retry, r.weeks),
                Ok(_) => {}
                Err(why) => self.alert(why),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_duniter::Ed25519Signer;
    use astro_economy::split::SplitConfig;

    use super::*;
    use crate::coop_wallets::Coop;
    use crate::hosting_fee::tests::Node;

    const NOW: u64 = 1_800_000_000;

    struct Rig {
        dir: std::path::PathBuf,
        db: Arc<Db>,
        node: Arc<Node>,
        coop: Coop,
        week: String,
        week_at: i64,
    }

    async fn rig(tag: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("astroportd-settle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let node = Arc::new(Node { total: Mutex::new(100 + 50_000), sent: Mutex::new(Vec::new()), fail: Mutex::new(None) });
        let coop = Coop { node: Account([1; 32]), captain: Account([2; 32]), impot: Account([3; 32]), treasury: Account([4; 32]), rnd: Account([5; 32]), assets: Account([6; 32]) };
        let start = week_start(NOW) - 7 * DAY;
        Rig { dir, db, node, coop, week: iso_week(start), week_at: i64::try_from(start + 3_600).unwrap() }
    }

    impl Rig {
        fn settlement(&self, costs: u64) -> Settlement<Node> {
            let wallets = Arc::new(Wallets::fixed(self.db.clone(), Ed25519Signer::from_seed([0x44; 32]), self.coop.clone()));
            Settlement::new(self.db.clone(), self.node.clone(), wallets, ActivityBus::new(), Cents(costs))
        }

        /// Loyers de la semaine : des virements d'hébergement (en ẑ entiers) reçus dans la semaine.
        async fn rent(&self, zen: u64) {
            self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('hosting_fee', ?1, ?2)", vec![self.week_at.into(), json!({ "owner": "aa", "zen": zen }).to_string().into()]).await.unwrap();
        }

        /// Clôture de la semaine telle que la nuit de 20h12 l'acte.
        async fn close(&self, costs: u64, revenue: u64) -> &'static str {
            let input = astro_economy::love::WeekInput { week: self.week.clone(), revenue: Cents(revenue), costs: Cents(costs), cash_balance: Cents(0), ytd_profit: Cents(0) };
            let outcome = astro_economy::love::close_week(&SplitConfig::default(), &input).unwrap();
            let stmts = astro_economy::store::close_statements(&self.week, &outcome, "cc", NOW);
            self.db.writer().batch(stmts).await.unwrap();
            match outcome {
                astro_economy::love::WeekOutcome::Split(_) => "split",
                astro_economy::love::WeekOutcome::Drawdown { .. } => "drawdown",
                astro_economy::love::WeekOutcome::Volunteer(_) => "volunteer",
            }
        }

        fn sent(&self) -> Vec<(Account, u64, String)> {
            self.node.sent.lock().unwrap().clone()
        }
    }

    #[tokio::test]
    async fn a_surplus_week_pays_node_captain_then_the_cooperative_wallets_once() {
        let r = rig("split").await;
        r.rent(100).await; // 100 ẑ reçus ; PAF = 14 ẑ (3×PAF = 42 ẑ)
        assert_eq!(r.close(4_200, 10_000).await, "split");
        let s = r.settlement(4_200);
        let first = s.run_weeks(NOW).await.unwrap();
        assert_eq!((first.paid, first.weeks), (7, 1));
        let sent = r.sent();
        let (node, captain) = (&sent[0], &sent[1]);
        assert_eq!((node.0, node.1), (r.coop.node, 140), "1×PAF = 14 ẑ = 1,4 Ğ1 = 140 centimes de Ğ1");
        assert_eq!((captain.0, captain.1), (r.coop.captain, 280), "2×PAF = 28 ẑ");
        let order: Vec<Account> = sent.iter().map(|t| t.0).collect();
        assert_eq!(order, [r.coop.node, r.coop.captain, r.coop.impot, r.coop.captain, r.coop.treasury, r.coop.rnd, r.coop.assets], "NODE, Capitaine, IMPOT, prime, TREASURY, RND, ASSETS");
        assert!(sent.iter().all(|t| t.2.starts_with("UPLANET:SETTLE:")));
        let total: u64 = sent.iter().map(|t| t.1).sum();
        assert!(total <= 1_000, "jamais plus que les 100 ẑ reçus ({total} centimes de Ğ1)");
        // Une semaine ne se règle qu'une fois.
        assert_eq!(s.run_weeks(NOW + 3_600).await.unwrap(), Report::default());
        assert_eq!(r.sent().len(), 7);
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn when_the_money_received_is_short_the_node_comes_first_and_nothing_else_is_taken() {
        let r = rig("short").await;
        r.rent(20).await; // 20 ẑ pour 42 ẑ de charges
        let status = r.close(4_200, 2_000).await;
        assert_ne!(status, "split");
        let s = r.settlement(4_200).run_weeks(NOW).await.unwrap();
        assert_eq!(s.paid, 2);
        let sent = r.sent();
        assert_eq!((sent[0].0, sent[0].1), (r.coop.node, 140), "le NODE reçoit sa PAF entière");
        assert_eq!((sent[1].0, sent[1].1), (r.coop.captain, 60), "le Capitaine reçoit le reste (6 ẑ sur 28)");
        assert_eq!(sent.len(), 2, "aucun surplus, aucun prélèvement sur la coopérative");
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn the_account_never_pays_out_more_than_it_holds_and_never_without_a_closed_week() {
        let r = rig("empty").await;
        r.rent(100).await;
        // Semaine pas clôturée : rien.
        assert_eq!(r.settlement(4_200).run_weeks(NOW).await.unwrap(), Report::default());
        r.close(4_200, 10_000).await;
        // Compte technique vide (réserve seule) : rien n'est viré.
        *r.node.total.lock().unwrap() = 100;
        let s = r.settlement(4_200).run_weeks(NOW).await.unwrap();
        assert_eq!(s.paid, 0);
        assert!(r.sent().is_empty());
        // Économie désactivée (PAF = 0) : rien non plus, même avec des fonds.
        *r.node.total.lock().unwrap() = 100_000;
        r.db.writer().execute("DELETE FROM kv WHERE scope = 'settle'", vec![]).await.unwrap();
        assert_eq!(r.settlement(0).run_weeks(NOW).await.unwrap(), Report::default());
        assert!(r.sent().is_empty());
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn an_unknown_outcome_is_never_replayed() {
        let r = rig("unknown").await;
        r.rent(100).await;
        r.close(4_200, 10_000).await;
        *r.node.fail.lock().unwrap() = Some(ChainError::UnknownOutcome("délai".into()));
        let s = r.settlement(4_200);
        let first = s.run_weeks(NOW).await.unwrap();
        assert_eq!((first.paid, first.unknown), (0, 7));
        *r.node.fail.lock().unwrap() = None;
        assert_eq!(s.run_weeks(NOW + 3_600).await.unwrap().paid, 0, "jamais rejoué : le Capitaine vérifie le compte technique");
        assert!(r.sent().is_empty());
        let _ = std::fs::remove_dir_all(&r.dir);
    }
}
