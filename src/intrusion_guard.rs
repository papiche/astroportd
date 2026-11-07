//! **Correction des intrusions** : un virement reçu par un compte de la station (MULTIPASS, Capitaine, portefeuille
//! coopératif) dont l'émetteur a une primale **prouvée** étrangère (≠ `UPLANETNAME_G1`, accord de deux indexeurs, voir
//! [`crate::funds::primal_consensus`]) est renvoyé vers le portefeuille `INTRUSION`, pour que les comptes en Ẑen restent
//! justes. Équivalent corrigé de `tools/primal_wallet_control.sh`.
//!
//! Garde-fous (leçons des renvois à tort du script bash, constatés sur la chaîne en novembre 2026) :
//! - **jamais sur une primale non prouvée** : l'appelant ne transmet que des primales vérifiées ;
//! - **jamais la réserve** : le renvoi est plafonné à ce qui dépasse 1 Ğ1 (`KeepAlive`), jamais de vidage ni de destruction ;
//!   ce qui manque est dit au Capitaine ;
//! - **une seule fois** par virement : marque écrite AVANT la signature ; une issue inconnue n'est jamais rejouée ;
//! - mémo au format du script (`…:INTRUSION:<émetteur>:ID:<bloc>_<émetteur>`) : le legacy reconnaît le renvoi comme fait.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_duniter::{Amount, Chain, ChainError, Ed25519Signer, TransferMode};
use serde_json::json;

use crate::coop_wallets::Wallets;
use crate::station::RESERVE_G1_CENTS;

/// Marques : `kv(intrusion_fix, <bloc>:<émetteur>:<portefeuille>)` = `sending` | `done:<centimes>` | `short` | `unknown`.
pub const SCOPE: &str = "intrusion_fix";

/// Un virement intrusif à renvoyer.
pub struct Fix {
    /// Nom lisible du compte (email ou nom du portefeuille coopératif).
    pub label: String,
    /// Clé de signature du portefeuille qui a reçu.
    pub signer: Arc<Ed25519Signer>,
    /// Émetteur (SS58).
    pub from: String,
    /// Sa primale (prouvée).
    pub primal: String,
    /// Montant reçu, en centimes de Ğ1.
    pub cents: u64,
    /// Bloc du virement.
    pub block: u64,
}

/// Rappel de correction, branché sur [`crate::funds::FundsWatch`].
pub type FixFn = Arc<dyn Fn(Fix) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Identifiant du renvoi, au format de `primal_wallet_control.sh` (`<bloc>_<12 premiers caractères de l'émetteur>`).
pub fn tx_id(block: u64, from: &str) -> String {
    format!("{block}_{}", from.chars().take(12).collect::<String>()).chars().filter(|c| !matches!(c, ':' | '.' | ' ' | '-')).collect()
}

/// Renvoi des intrusions vers `INTRUSION`.
pub struct IntrusionGuard<C: Chain> {
    db: Arc<Db>,
    chain: Arc<C>,
    wallets: Arc<Wallets>,
    activity: ActivityBus,
}

impl<C: Chain> IntrusionGuard<C> {
    /// Garde de la constellation de `wallets`.
    pub fn new(db: Arc<Db>, chain: Arc<C>, wallets: Arc<Wallets>, activity: ActivityBus) -> Self {
        Self { db, chain, wallets, activity }
    }

    async fn mark(&self, key: &str) -> Option<String> {
        let k = key.to_owned();
        self.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, k], |r| r.get::<_, String>(0)))?)).await.ok().flatten()
    }

    async fn set_mark(&self, key: &str, value: Option<&str>) {
        let w = self.db.writer();
        let r = match value {
            Some(v) => w.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![SCOPE.to_owned().into(), key.to_owned().into(), v.to_owned().into(), now().into()]).await,
            None => w.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SCOPE.to_owned().into(), key.to_owned().into()]).await,
        };
        if let Err(e) = r {
            self.alert(format!("intrusion {key} : marque non écrite ({e})"));
        }
    }

    fn alert(&self, text: String) {
        self.activity.publish_to(Audience::Captain, "alert", "intrusion", text, None);
    }

    /// Renvoie un virement intrusif. Rend les centimes renvoyés (0 : rien, déjà fait ou impossible — le Capitaine le sait).
    pub async fn correct(&self, fix: Fix) -> u64 {
        let wallet = fix.signer.account();
        let key = format!("{}:{}:{}", fix.block, fix.from, wallet.ss58());
        if self.mark(&key).await.is_some() {
            return 0;
        }
        let intrusion = match self.wallets.targets().await.map(|t| t.iter().find(|(n, _)| n == "INTRUSION").map(|(_, a)| *a)) {
            Ok(Some(a)) => a,
            Ok(None) => {
                self.alert(format!("intrusion reçue par {} : portefeuille INTRUSION inconnu, rien renvoyé", fix.label));
                return 0;
            }
            Err(e) => {
                self.alert(format!("intrusion reçue par {} : portefeuilles coopératifs illisibles ({e})", fix.label));
                return 0;
            }
        };
        if wallet == intrusion {
            return 0; // INTRUSION reçoit par construction des fonds extérieurs
        }
        let spendable = match self.chain.balance(&wallet).await {
            Ok(b) => b.spendable(Amount(RESERVE_G1_CENTS)).0,
            Err(e) => {
                self.alert(format!("intrusion reçue par {} ({} Ğ1 de {}) : solde illisible ({e}), à revoir", fix.label, Amount(fix.cents), fix.from));
                return 0;
            }
        };
        let amount = fix.cents.min(spendable);
        if amount == 0 {
            self.set_mark(&key, Some("short")).await;
            self.alert(format!("intrusion reçue par {} ({} Ğ1 de {}, primale {}) : déjà dépensée, rien à renvoyer sans toucher la réserve — à examiner", fix.label, Amount(fix.cents), fix.from, fix.primal));
            return 0;
        }
        let master8: String = self.wallets.master().await.map(|m| m.account().ss58().chars().take(8).collect()).unwrap_or_default();
        let id = tx_id(fix.block, &fix.from);
        let memo = format!("UPLANET:{master8}:INTRUSION:{}:ID:{id}", fix.from.chars().take(8).collect::<String>());
        self.set_mark(&key, Some("sending")).await;
        match self.chain.transfer(&fix.signer, &intrusion, Amount(amount), TransferMode::KeepAlive, Some(&memo)).await {
            Ok(out) => {
                self.set_mark(&key, Some(&format!("done:{amount}"))).await;
                let record = json!({ "wallet": wallet.ss58(), "label": fix.label, "from": fix.from, "primal": fix.primal, "received_cents": fix.cents, "sent_cents": amount, "id": id, "block": hex::encode(out.block_hash) });
                let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('intrusion_fix', ?1, ?2)", vec![now().into(), record.to_string().into()]).await;
                let short = if amount < fix.cents { format!(" (sur {} reçus : le reste a déjà été dépensé)", Amount(fix.cents)) } else { String::new() };
                self.alert(format!("🚨 intrusion corrigée : {} Ğ1{short} de {} (primale {}) reçus par {} renvoyés vers INTRUSION", Amount(amount), fix.from, fix.primal, fix.label));
                amount
            }
            Err(ChainError::Connection(why)) => {
                self.set_mark(&key, None).await; // perdu avant l'envoi : réessayable
                self.alert(format!("intrusion reçue par {} : nœud injoignable ({why}), renvoi à refaire", fix.label));
                0
            }
            Err(ChainError::UnknownOutcome(why)) => {
                self.set_mark(&key, Some("unknown")).await;
                self.alert(format!("intrusion reçue par {} : issue du renvoi inconnue ({why}) — vérifier avant de rejouer", fix.label));
                0
            }
            Err(other) => {
                self.set_mark(&key, None).await;
                self.alert(format!("intrusion reçue par {} : renvoi refusé ({other})", fix.label));
                0
            }
        }
    }

    /// Rappel à donner à [`crate::funds::FundsWatch::with_guard`].
    pub fn hook(self: Arc<Self>) -> FixFn
    where
        C: 'static,
    {
        Arc::new(move |fix| {
            let me = self.clone();
            Box::pin(async move {
                me.correct(fix).await;
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_duniter::{Account, Balance, TransferEvent, TxOutcome};

    use super::*;
    use crate::coop_wallets::Coop;

    struct Ledger {
        balances: Mutex<std::collections::HashMap<Account, u64>>,
        sent: Mutex<Vec<(Account, Account, u64, String)>>,
    }

    impl Chain for Ledger {
        async fn balance(&self, a: &Account) -> Result<Balance, ChainError> {
            let t = self.balances.lock().unwrap().get(a).copied().unwrap_or(0);
            Ok(Balance { total: Amount(t), transferable: Amount(t), unclaimed_uds: Amount(0) })
        }
        async fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, _: TransferMode, memo: Option<&str>) -> Result<TxOutcome, ChainError> {
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

    #[tokio::test]
    async fn an_intrusion_is_sent_back_once_and_never_eats_the_reserve() {
        let dir = std::env::temp_dir().join(format!("astroportd-intrusion-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let intrusion = Account([9; 32]);
        let coop = Coop { node: Account([1; 32]), captain: Account([2; 32]), impot: Account([3; 32]), treasury: Account([4; 32]), rnd: Account([5; 32]), assets: Account([6; 32]) };
        let wallets = Arc::new(Wallets::fixed(db.clone(), Ed25519Signer::from_seed([0x44; 32]), coop).with_master(Ed25519Signer::from_seed([0x01; 32])).with_target("INTRUSION", intrusion));
        let member = Arc::new(Ed25519Signer::from_seed([0x22; 32]));
        let chain = Arc::new(Ledger { balances: Mutex::new(std::collections::HashMap::from([(member.account(), 100 + 3_700)])), sent: Mutex::new(Vec::new()) });
        let guard = IntrusionGuard::new(db.clone(), chain.clone(), wallets, ActivityBus::new());
        let fix = |cents| Fix { label: "ana@ex.org".into(), signer: member.clone(), from: "g1NYitPsi37wXYZ".into(), primal: "g1KA5dcr4S".into(), cents, block: 84_194 };

        assert_eq!(guard.correct(fix(3_700)).await, 3_700);
        assert_eq!(guard.correct(fix(3_700)).await, 0, "une seule fois");
        let sent = chain.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].1, sent[0].2), (intrusion, 3_700));
        assert!(sent[0].3.ends_with(":INTRUSION:g1NYitPs:ID:84194_g1NYitPsi37w"), "mémo reconnu par le script bash : {}", sent[0].3);

        // Un autre virement intrusif alors qu'il ne reste que la réserve : rien n'est pris.
        let mut later = fix(500);
        later.block = 84_300;
        assert_eq!(guard.correct(later).await, 0);
        assert_eq!(chain.balances.lock().unwrap()[&member.account()], 100, "la réserve de 1 Ğ1 reste");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
