//! **Transaction primale** des portefeuilles d'une UPlanet (équivalent de `UPLANET.init.sh` d'Astroport.ONE).
//!
//! Un portefeuille coopératif existe, et appartient à cette UPlanet, parce que son **premier virement reçu** vient du
//! portefeuille maître `UPLANETNAME_G1` : c'est son **primal**, que la station vérifie pour accepter un destinataire
//! (voir [`crate::funds`]). Chaque portefeuille vide (moins de 1 Ğ1) reçoit donc **1 Ğ1** du maître, ce qui l'initialise
//! à 0 ẑ et lui permet de recevoir ensuite de petits montants (le dépôt d'existence d'un compte est de 1 Ğ1).
//!
//! Portefeuilles concernés : ceux de `tools/my.sh` (`UPLANETNAME`, `SOCIETY`, `TREASURY`, `RND`, `ASSETS`, `IMPOT`,
//! `INTRUSION`, `CAPITAL`, `AMORTISSEMENT`), le **compte technique d'hébergement** et le **NODE**.
//!
//! Garde-fous :
//! - **un seul envoi par portefeuille**, noté AVANT la signature (`kv('primal_init', <nom>)`) ; une issue inconnue n'est
//!   jamais rejouée (alerte au Capitaine) ;
//! - le maître garde son propre dépôt d'existence : s'il n'a pas de quoi initialiser, la station le dit (une fois par jour)
//!   et reprend au passage suivant, sans rien casser ;
//! - un portefeuille qui détient déjà 1 Ğ1 ou plus est considéré comme initialisé (comme le legacy).

use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_duniter::{Amount, Chain, ChainError, TransferMode};
use serde_json::json;
use tokio::sync::watch;

use crate::coop_wallets::Wallets;
use crate::station::RESERVE_G1_CENTS;

/// Montant de la transaction primale : 1 Ğ1, en centimes de Ğ1.
pub const INIT_G1_CENTS: u64 = 100;
pub(crate) const SCOPE: &str = "primal_init";
/// Clé du dernier bilan (JSON) dans `kv(primal_init, …)`.
pub const STATUS_KEY: &str = "_status";
const EVERY: Duration = Duration::from_secs(6 * 3600);

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Bilan d'un passage.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Portefeuilles initialisés ce passage.
    pub initialised: u32,
    /// Déjà initialisés (1 Ğ1 ou plus, ou envoi déjà fait).
    pub already: u32,
    /// Maître trop pauvre pour initialiser ceux-ci : au prochain passage.
    pub waiting: u32,
    /// Issues inconnues après envoi (jamais rejouées).
    pub unknown: u32,
    /// Nœud Duniter injoignable : au prochain passage.
    pub retry: u32,
}

/// Initialisation primale des portefeuilles de l'UPlanet.
pub struct PrimalInit<C: Chain> {
    db: Arc<Db>,
    chain: Arc<C>,
    wallets: Arc<Wallets>,
    activity: ActivityBus,
}

impl<C: Chain> PrimalInit<C> {
    /// Initialisation primale depuis le maître de `wallets`.
    pub fn new(db: Arc<Db>, chain: Arc<C>, wallets: Arc<Wallets>, activity: ActivityBus) -> Self {
        Self { db, chain, wallets, activity }
    }

    fn alert(&self, text: String) {
        self.activity.publish_to(Audience::Captain, "alert", "primal", text, None);
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

    /// Un passage : marque par transaction primale chaque portefeuille encore vide.
    pub async fn run_once(&self, at: u64) -> Result<Report, String> {
        let mut report = Report::default();
        let targets = self.wallets.targets().await?;
        let master = self.wallets.master().await?;
        let mut master_spendable = match self.chain.balance(&master.account()).await {
            Ok(b) => b.spendable(Amount(RESERVE_G1_CENTS)).0,
            Err(_) => {
                report.retry += 1;
                return Ok(report);
            }
        };
        let mut low = false;
        for (name, account) in targets.iter() {
            if matches!(self.marker(name).await.as_deref(), Some("paid" | "sending")) {
                report.already += 1;
                continue;
            }
            let balance = match self.chain.balance(account).await {
                Ok(b) => b.total.0,
                Err(_) => {
                    report.retry += 1;
                    continue;
                }
            };
            if balance >= INIT_G1_CENTS {
                report.already += 1;
                continue;
            }
            if master_spendable < INIT_G1_CENTS {
                report.waiting += 1;
                low = true;
                continue;
            }
            self.set_marker(name, "sending").await;
            let memo = format!("UPLANET:PRIMAL:{name}");
            match self.chain.transfer(&master, account, Amount(INIT_G1_CENTS), TransferMode::KeepAlive, Some(&memo)).await {
                Ok(out) => {
                    self.set_marker(name, "paid").await;
                    master_spendable -= INIT_G1_CENTS;
                    report.initialised += 1;
                    let record = json!({ "wallet": name, "to": account.ss58(), "g1_cents": INIT_G1_CENTS, "block": hex::encode(out.block_hash) });
                    let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('primal_init', ?1, ?2)", vec![i64::try_from(at).unwrap_or(0).into(), record.to_string().into()]).await;
                }
                Err(ChainError::Connection(_)) => {
                    self.clear_marker(name).await; // perdu AVANT l'envoi : réessayer est sans danger
                    report.retry += 1;
                }
                Err(ChainError::UnknownOutcome(why)) => {
                    report.unknown += 1;
                    self.alert(format!("Transaction primale de « {name} » : issue inconnue ({why}) — vérifier son solde avant de rejouer"));
                }
                Err(other) => {
                    self.clear_marker(name).await;
                    report.retry += 1;
                    self.alert(format!("Transaction primale de « {name} » refusée : {other}"));
                }
            }
        }
        // Bilan lisible par le cockpit (carte « Réserve de démarrage ») sans appel au réseau Ğ1.
        let needed = u64::from(report.waiting).saturating_mul(INIT_G1_CENTS).saturating_sub(master_spendable);
        let status = json!({ "master": master.account().ss58(), "spendable_g1_cents": master_spendable, "needed_g1_cents": needed, "waiting": report.waiting, "ready": report.already + report.initialised, "total": targets.len(), "unknown": report.unknown, "at": at });
        self.set_marker(STATUS_KEY, &status.to_string()).await;
        if low {
            // Un rappel par jour, pas toutes les 6 h.
            let day = (at / 86_400).to_string();
            if self.marker("_master_low").await.as_deref() != Some(day.as_str()) {
                self.set_marker("_master_low", &day).await;
                self.alert(format!("{} portefeuille(s) coopératif(s) à ouvrir : déposez {} Ğ1 sur le portefeuille maître {} (carte « Carburant » du cockpit)", report.waiting, needed.div_ceil(100), master.account().ss58()));
            }
        }
        Ok(report)
    }

    /// Boucle de fond : un passage peu après le démarrage, puis toutes les 6 h.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut wait = Duration::from_secs(60);
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.changed() => return,
            }
            wait = EVERY;
            let Ok(at) = astro_core::solar::system_now_checked() else { continue };
            match self.run_once(at).await {
                Ok(r) if r.initialised + r.waiting + r.unknown + r.retry > 0 => println!("Transactions primales : {} initialisé(s), {} en attente de fonds du maître, {} incertain(s), {} à retenter", r.initialised, r.waiting, r.unknown, r.retry),
                Ok(_) => {}
                Err(why) => self.alert(why),
            }
        }
    }
}

/// Dernier bilan de la transaction primale (`None` : jamais passée, ou option `--primal-init` absente).
pub async fn last_status(db: &Db) -> Option<serde_json::Value> {
    let text: Option<String> = db.readers().read(|c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, STATUS_KEY], |r| r.get::<_, String>(0)))?)).await.ok().flatten();
    text.and_then(|t| serde_json::from_str(&t).ok())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_duniter::{Account, Balance, Ed25519Signer, TransferEvent, TxOutcome};

    use super::*;
    use crate::coop_wallets::Coop;

    /// Chaîne factice : soldes par compte, virements enregistrés.
    struct Ledger {
        balances: Mutex<std::collections::HashMap<Account, u64>>,
        sent: Mutex<Vec<(Account, Account, u64, String)>>,
        fail: Mutex<Option<ChainError>>,
    }

    impl Chain for Ledger {
        async fn balance(&self, a: &Account) -> Result<Balance, ChainError> {
            { let t = self.balances.lock().unwrap().get(a).copied().unwrap_or(0); Ok(Balance { total: Amount(t), transferable: Amount(t), unclaimed_uds: Amount(0) }) }
        }
        async fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, _: TransferMode, memo: Option<&str>) -> Result<TxOutcome, ChainError> {
            if let Some(e) = self.fail.lock().unwrap().clone() {
                return Err(e);
            }
            let mut b = self.balances.lock().unwrap();
            *b.entry(from.account()).or_default() -= amount.0;
            *b.entry(*to).or_default() += amount.0;
            self.sent.lock().unwrap().push((from.account(), *to, amount.0, memo.unwrap_or_default().to_owned()));
            Ok(TxOutcome { block_hash: [9; 32], transferred: amount })
        }
        async fn watch_transfers(&self, _: tokio::sync::mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
            std::future::pending().await
        }
        fn existential_deposit(&self) -> Amount {
            Amount(100)
        }
    }

    struct Rig {
        dir: std::path::PathBuf,
        chain: Arc<Ledger>,
        init: PrimalInit<Ledger>,
        master: Account,
        targets: Vec<Account>,
        db: Arc<Db>,
    }

    async fn rig(tag: &str, master_g1_cents: u64) -> Rig {
        let dir = std::env::temp_dir().join(format!("astroportd-primal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let master_signer = Ed25519Signer::from_seed([0x01; 32]);
        let master = master_signer.account();
        let coop = Coop { node: Account([1; 32]), captain: Account([2; 32]), impot: Account([3; 32]), treasury: Account([4; 32]), rnd: Account([5; 32]), assets: Account([6; 32]) };
        let wallets = Arc::new(Wallets::fixed(db.clone(), Ed25519Signer::from_seed([0x44; 32]), coop).with_master(master_signer));
        let targets: Vec<Account> = wallets.targets().await.unwrap().iter().map(|(_, a)| *a).collect();
        let chain = Arc::new(Ledger { balances: Mutex::new(std::collections::HashMap::from([(master, master_g1_cents)])), sent: Mutex::new(Vec::new()), fail: Mutex::new(None) });
        let init = PrimalInit::new(db.clone(), chain.clone(), wallets, ActivityBus::new());
        Rig { dir, chain, init, master, targets, db }
    }

    #[tokio::test]
    async fn every_empty_wallet_gets_one_g1_from_the_master_once() {
        let r = rig("all", 100 + 10 * 100).await; // 1 Ğ1 de réserve + 10 Ğ1
        let first = r.init.run_once(1_800_000_000).await.unwrap();
        assert_eq!((first.initialised, first.waiting), (6, 0));
        let sent = r.chain.sent.lock().unwrap().clone();
        assert!(sent.iter().all(|(from, _, g1, memo)| *from == r.master && *g1 == 100 && memo.starts_with("UPLANET:PRIMAL:")));
        let got: std::collections::HashSet<_> = sent.iter().map(|t| t.1).collect();
        assert_eq!(got, r.targets.iter().copied().collect(), "chaque portefeuille a reçu sa primale, une fois");
        // Rejeu : rien de plus.
        assert_eq!(r.init.run_once(1_800_000_100).await.unwrap().initialised, 0);
        assert_eq!(r.chain.sent.lock().unwrap().len(), 6);
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn a_wallet_that_already_holds_one_g1_is_left_alone_and_a_poor_master_waits_without_breaking() {
        let r = rig("poor", 100 + 2 * 100).await; // de quoi en initialiser 2 seulement
        r.chain.balances.lock().unwrap().insert(r.targets[0], 250);
        let first = r.init.run_once(1_800_000_000).await.unwrap();
        assert_eq!((first.initialised, first.waiting, first.already), (2, 3, 1), "{first:?}");
        // Le cockpit lit le bilan : l'adresse à créditer et ce qui manque (3 Ğ1, le maître gardant sa réserve).
        let st = last_status(&r.db).await.unwrap();
        assert_eq!((st["master"].as_str(), st["needed_g1_cents"].as_u64(), st["waiting"].as_u64(), st["ready"].as_u64(), st["total"].as_u64()), (Some(r.master.ss58().as_str()), Some(300), Some(3), Some(3), Some(6)), "{st}");
        // Le maître est rechargé : les trois restants sont initialisés au passage suivant.
        r.chain.balances.lock().unwrap().insert(r.master, 100 + 5 * 100);
        let second = r.init.run_once(1_800_021_600).await.unwrap();
        assert_eq!((second.initialised, second.waiting), (3, 0));
        assert_eq!(last_status(&r.db).await.unwrap()["needed_g1_cents"], 0, "plein fait : la carte disparaît");
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn an_unknown_outcome_is_never_replayed() {
        let r = rig("unknown", 100 + 10 * 100).await;
        *r.chain.fail.lock().unwrap() = Some(ChainError::UnknownOutcome("délai".into()));
        assert_eq!(r.init.run_once(1_800_000_000).await.unwrap().unknown, 6);
        *r.chain.fail.lock().unwrap() = None;
        assert_eq!(r.init.run_once(1_800_000_100).await.unwrap().initialised, 0, "jamais rejoué : le Capitaine vérifie");
        assert!(r.chain.sent.lock().unwrap().is_empty());
        let _ = (&r.db, std::fs::remove_dir_all(&r.dir));
    }
}

