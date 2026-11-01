//! Hébergement facturé par le Capitaine : **un prix hebdomadaire en ẑ par MULTIPASS hébergé**, viré au Capitaine.
//!
//! (ẑ minuscule : les Ẑen du MULTIPASS, côté Opex. Le Ẑ majuscule est réservé à la ZenCard, côté Capex, qui n'est pas
//! concernée ici.)
//!
//! Réglages du Capitaine, tous dans `kv` (état local, rien à rejouer) :
//! - `hosting_price / <hex>` : prix hebdomadaire en ẑ de ce compte ; `0` = gratuit ;
//! - `hosting / default_price` : prix des comptes qui n'ont pas le leur (absent = `0`) ;
//! - `hosting / default_quota` : quota en octets des MULTIPASS sans quota propre (le « palier gratuit », lu par
//!   `astro_dav::auth`).
//!
//! Règles :
//! - le Capitaine ne se facture pas lui-même ; seul un MULTIPASS **hébergé ici** dont la clé Ğ1 est sur cette station
//!   peut être débité, vers le portefeuille du Capitaine (même constellation : pas de contrôle primal à faire) ;
//! - **un virement par compte et par semaine ISO**, état noté dans `kv('hosting_fee', '<hex>:<semaine>')` AVANT
//!   l'envoi ; une issue inconnue après envoi n'est JAMAIS rejouée (le Capitaine est prévenu de vérifier le solde) ;
//! - solde insuffisant : le membre et le Capitaine sont prévenus une fois, le virement est retenté à chaque passage
//!   de la semaine ; **rien n'est jamais supprimé ni coupé** (la mise en grâce reste l'affaire du cycle de vie) ;
//! - le prix est celui de la semaine en cours au moment du passage : un changement de prix en cours de semaine joue
//!   tant que le virement de cette semaine n'est pas parti.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use astro_core::week::{iso_week, week_start};
use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_duniter::{Account, Amount, Chain, ChainError, Ed25519Signer, TransferMode};
use astro_lifecycle::dunikey::Dunikey;
use rusqlite::OptionalExtension;
use serde_json::json;
use tokio::sync::watch;

use crate::station::{RESERVE_G1_CENTS, ZEN_PER_G1};
use crate::upload_worker::Notify;

/// Espace des prix par compte.
pub const PRICE_SCOPE: &str = "hosting_price";
/// Espace des réglages par défaut (`default_price`, `default_quota`).
pub const DEFAULT_SCOPE: &str = "hosting";
/// Espace de l'état des virements (`<hex>:<semaine>`).
const FEE_SCOPE: &str = "hosting_fee";
/// Intervalle entre deux passages (le virement d'une semaine est idempotent).
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// Plafond du prix hebdomadaire : une faute de frappe ne vide pas un compte.
pub const MAX_WEEKLY_ZEN: u64 = 10_000;

/// Prix hebdomadaire (ẑ) de `owner` : le sien, sinon le défaut du Capitaine, sinon 0.
pub fn weekly_price(c: &rusqlite::Connection, owner: &str) -> rusqlite::Result<u64> {
    let own: Option<String> = c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![PRICE_SCOPE, owner], |r| r.get(0)).optional()?;
    let value = match own {
        Some(v) => Some(v),
        None => c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = 'default_price'", [DEFAULT_SCOPE], |r| r.get(0)).optional()?,
    };
    Ok(value.and_then(|v| v.trim().parse().ok()).unwrap_or(0))
}

/// État du virement de la semaine en cours pour `owner` : `paid`, `unpaid`, `sending`, ou `None` (rien de dû ou pas encore passé).
pub fn current_state(c: &rusqlite::Connection, owner: &str, at: u64) -> Option<String> {
    let key = format!("{owner}:{}", iso_week(week_start(at)));
    c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![FEE_SCOPE, key], |r| r.get(0)).ok()
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Bilan d'un passage.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Semaine ISO traitée.
    pub week: String,
    /// Virements partis.
    pub paid: u32,
    /// Comptes à solde insuffisant (ou refusés), à retenter.
    pub unpaid: u32,
    /// Issues inconnues après envoi (jamais rejouées).
    pub unknown: u32,
    /// Nœud Duniter injoignable avant l'envoi : au prochain passage.
    pub retry: u32,
}

/// Prélèvement hebdomadaire de l'hébergement.
pub struct HostingFees<C: Chain> {
    db: Arc<Db>,
    chain: Arc<C>,
    accounts_dir: PathBuf,
    activity: ActivityBus,
    notify: Option<Notify>,
}

impl<C: Chain> HostingFees<C> {
    /// `accounts_dir` : dossier des comptes (`~/.zen/game/nostr`), où se trouve le `.secret.dunikey` de chaque MULTIPASS.
    pub fn new(db: Arc<Db>, chain: Arc<C>, accounts_dir: PathBuf, activity: ActivityBus, notify: Option<Notify>) -> Self {
        Self { db, chain, accounts_dir, activity, notify }
    }

    async fn tell(&self, owner: &str, text: String) {
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "economy", "hosting", text.clone(), None);
        if let Some(n) = &self.notify {
            n(owner.to_owned(), text).await;
        }
    }

    fn alert(&self, text: String) {
        self.activity.publish_to(Audience::Captain, "alert", "hosting", text, None);
    }

    async fn state(&self, key: &str) -> Option<String> {
        let k = key.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![FEE_SCOPE, k], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten()
    }

    async fn set_state(&self, key: &str, value: &str) {
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![FEE_SCOPE.to_owned().into(), key.to_owned().into(), value.to_owned().into(), now().into()]).await;
    }

    async fn clear_state(&self, key: &str) {
        let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![FEE_SCOPE.to_owned().into(), key.to_owned().into()]).await;
    }

    /// Passage de la semaine ISO qui contient `at` (secondes Unix).
    pub async fn charge_week(&self, at: u64) -> Result<Report, String> {
        let week = iso_week(week_start(at));
        let mut report = Report { week: week.clone(), ..Report::default() };
        let (captain, members): (Option<String>, Vec<Member>) = self
            .db
            .readers()
            .read(|c| {
                let captain = c.query_row("SELECT g1pub_nostr FROM identities WHERE role = 'captain' AND hosted = 1 AND g1pub_nostr IS NOT NULL LIMIT 1", [], |r| r.get::<_, String>(0)).optional()?;
                let mut stmt = c.prepare("SELECT hex, email, g1pub_nostr FROM identities WHERE hosted = 1 AND role = 'multipass' AND email IS NOT NULL AND g1pub_nostr IS NOT NULL ORDER BY hex")?;
                let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut members = Vec::new();
                for (hex, email, wallet) in rows {
                    let price = weekly_price(c, &hex)?;
                    if price > 0 {
                        members.push((hex, email, wallet, price));
                    }
                }
                Ok((captain, members))
            })
            .await
            .map_err(|e| format!("base illisible : {e}"))?;
        if members.is_empty() {
            return Ok(report);
        }
        let Some(to) = captain.as_deref().and_then(|w| Account::from_str(w).ok()) else { return Err("portefeuille Ğ1 du Capitaine introuvable : aucun hébergement facturé".into()) };
        for (hex, email, wallet, zen) in members {
            let key = format!("{hex}:{week}");
            if matches!(self.state(&key).await.as_deref(), Some("paid" | "sending")) {
                continue;
            }
            match self.pay(&hex, &email, &wallet, zen, &to, &key, &week).await {
                Step::Paid => report.paid += 1,
                Step::Unpaid => report.unpaid += 1,
                Step::Unknown => report.unknown += 1,
                Step::Retry => report.retry += 1,
            }
        }
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    async fn pay(&self, hex: &str, email: &str, wallet: &str, zen: u64, to: &Account, key: &str, week: &str) -> Step {
        let who = email.split('@').next().unwrap_or(email);
        // Une seule alerte par compte et par semaine, même si le passage se répète toutes les 6 h.
        let first = self.state(key).await.is_none();
        let refuse = |why: String| async move {
            if first {
                self.set_state(key, "unpaid").await;
                self.tell(hex, format!("⚠️ Hébergement de la semaine {week} ({zen} ẑ) non réglé : {why}.")).await;
                self.alert(format!("Hébergement de {who} ({zen} ẑ, semaine {week}) non prélevé : {why}"));
            }
            Step::Unpaid
        };
        let key_file = std::fs::read_to_string(self.accounts_dir.join(email).join(".secret.dunikey")).ok().and_then(|t| Dunikey::parse(&t).ok());
        let Some(dk) = key_file else { return refuse("clé Ğ1 du MULTIPASS introuvable sur cette station".into()).await };
        let signer = Ed25519Signer::from_seed(*dk.ed25519_seed());
        if Account::from_str(wallet).ok() != Some(signer.account()) {
            return refuse("la clé Ğ1 du compte ne correspond pas à son portefeuille".into()).await;
        }
        let g1_cents = zen.saturating_mul(100) / ZEN_PER_G1;
        let balance = match self.chain.balance(&signer.account()).await {
            Ok(b) => b,
            Err(ChainError::Connection(_)) => return Step::Retry,
            Err(other) => return refuse(format!("solde illisible ({other})")).await,
        };
        let spendable = balance.total.0.saturating_sub(RESERVE_G1_CENTS);
        if spendable < g1_cents {
            return refuse(format!("solde insuffisant ({} ẑ disponibles)", spendable * ZEN_PER_G1 / 100)).await;
        }
        // Point de non-retour : on note l'envoi AVANT de signer. Une reprise après un arrêt brutal ne rejoue jamais.
        self.set_state(key, "sending").await;
        let memo = format!("UPLANET:HOSTING:{week}:{}", &hex[..hex.len().min(8)]);
        match self.chain.transfer(&signer, to, Amount(g1_cents), TransferMode::KeepAlive, Some(&memo)).await {
            Ok(out) => {
                self.set_state(key, "paid").await;
                let record = json!({ "owner": hex, "week": week, "zen": zen, "g1_cents": g1_cents, "block": hex::encode(out.block_hash) });
                let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('hosting_fee', ?1, ?2)", vec![now().into(), record.to_string().into()]).await;
                self.tell(hex, format!("🏠 Hébergement de la semaine {week} : {zen} ẑ envoyés au Capitaine.")).await;
                Step::Paid
            }
            Err(ChainError::Connection(_)) => {
                self.clear_state(key).await; // perdu AVANT l'envoi : réessayer est sans danger
                Step::Retry
            }
            Err(ChainError::UnknownOutcome(why)) => {
                self.alert(format!("Hébergement de {who} (semaine {week}) : issue du virement inconnue ({why}) — vérifier son solde avant de rejouer"));
                Step::Unknown
            }
            Err(other) => {
                self.clear_state(key).await;
                refuse(other.to_string()).await
            }
        }
    }

    /// Boucle de fond : un passage peu après le démarrage, puis toutes les 6 h.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut wait = Duration::from_secs(120);
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.changed() => return,
            }
            wait = EVERY;
            let Ok(at) = astro_core::solar::system_now_checked() else { continue };
            match self.charge_week(at).await {
                Ok(r) if r.paid + r.unpaid + r.unknown + r.retry > 0 => {
                    println!("Hébergement {} : {} viré(s), {} non réglé(s), {} incertain(s), {} à retenter", r.week, r.paid, r.unpaid, r.unknown, r.retry);
                }
                Ok(_) => {}
                Err(why) => self.alert(why),
            }
        }
    }
}

/// Compte à facturer : `(hex, email, portefeuille Ğ1, prix hebdomadaire en ẑ)`.
type Member = (String, String, String, u64);

enum Step {
    Paid,
    Unpaid,
    Unknown,
    Retry,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_duniter::{Balance, TransferEvent, TxOutcome};

    use super::*;

    struct Node {
        total: Mutex<u64>,
        sent: Mutex<Vec<(Account, u64, String)>>,
        fail: Mutex<Option<ChainError>>,
    }

    impl Chain for Node {
        async fn balance(&self, _: &Account) -> Result<Balance, ChainError> {
            Ok(Balance { total: Amount(*self.total.lock().unwrap()), transferable: Amount(0), unclaimed_uds: Amount(0) })
        }
        async fn transfer(&self, _: &Ed25519Signer, to: &Account, amount: Amount, _: TransferMode, memo: Option<&str>) -> Result<TxOutcome, ChainError> {
            if let Some(e) = self.fail.lock().unwrap().clone() {
                return Err(e);
            }
            self.sent.lock().unwrap().push((*to, amount.0, memo.unwrap_or_default().to_owned()));
            Ok(TxOutcome { block_hash: [7; 32], transferred: amount })
        }
        async fn watch_transfers(&self, _: tokio::sync::mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
            std::future::pending().await
        }
        fn existential_deposit(&self) -> Amount {
            Amount(100)
        }
    }

    const ALICE: &str = "aa";
    const BOB: &str = "bb";
    const WEEK1: u64 = 1_700_000_000;
    const WEEK2: u64 = WEEK1 + 8 * 86_400;

    struct Rig {
        dir: PathBuf,
        db: Arc<Db>,
        node: Arc<Node>,
        told: Arc<Mutex<Vec<String>>>,
        fees: HostingFees<Node>,
        captain: Account,
    }

    async fn rig(tag: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("astroportd-hosting-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let captain = Account([0x44; 32]);
        db.writer().execute("INSERT INTO identities(hex, role, email, g1pub_nostr, hosted) VALUES ('cc', 'captain', 'cap@example.org', ?1, 1)", vec![captain.ss58().into()]).await.unwrap();
        for (hex, email, seed) in [(ALICE, "alice@example.org", 0x11u8), (BOB, "bob@example.org", 0x12)] {
            std::fs::create_dir_all(dir.join("accounts").join(email)).unwrap();
            std::fs::write(dir.join("accounts").join(email).join(".secret.dunikey"), astro_lifecycle::dunikey::render(&[seed; 32])).unwrap();
            let wallet = Ed25519Signer::from_seed([seed; 32]).account().ss58();
            db.writer().execute("INSERT INTO identities(hex, role, email, g1pub_nostr, hosted) VALUES (?1, 'multipass', ?2, ?3, 1)", vec![hex.to_owned().into(), email.to_owned().into(), wallet.into()]).await.unwrap();
        }
        let node = Arc::new(Node { total: Mutex::new(100 + 2_000), sent: Mutex::new(Vec::new()), fail: Mutex::new(None) }); // réserve + 20 Ğ1 = 200 ẑ
        let told = Arc::new(Mutex::new(Vec::new()));
        let log = told.clone();
        let notify: Notify = Arc::new(move |_, text| {
            let log = log.clone();
            Box::pin(async move { log.lock().unwrap().push(text) })
        });
        let fees = HostingFees::new(db.clone(), node.clone(), dir.join("accounts"), ActivityBus::new(), Some(notify));
        Rig { dir, db, node, told, fees, captain }
    }

    async fn set(r: &Rig, scope: &str, key: &str, value: &str) {
        r.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, 0)", vec![scope.to_owned().into(), key.to_owned().into(), value.to_owned().into()]).await.unwrap();
    }

    #[tokio::test]
    async fn nothing_is_charged_until_the_captain_sets_a_price() {
        let r = rig("free").await;
        let report = r.fees.charge_week(WEEK1).await.unwrap();
        assert_eq!((report.paid, report.unpaid), (0, 0));
        assert!(r.node.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn each_account_pays_its_own_price_once_a_week_to_the_captain() {
        let r = rig("paid").await;
        set(&r, PRICE_SCOPE, ALICE, "3").await; // 128 Go à 3 ẑ
        set(&r, PRICE_SCOPE, BOB, "0").await; // 128 Go gratuit
        let report = r.fees.charge_week(WEEK1).await.unwrap();
        assert_eq!((report.paid, report.unpaid), (1, 0));
        {
            let sent = r.node.sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!((sent[0].0, sent[0].1), (r.captain, 30), "3 ẑ = 0,3 Ğ1 = 30 centimes de Ğ1");
            assert!(sent[0].2.starts_with("UPLANET:HOSTING:") && sent[0].2.ends_with(":aa"), "{}", sent[0].2);
        }
        // Même semaine, passages répétés : rien de plus.
        for _ in 0..3 {
            assert_eq!(r.fees.charge_week(WEEK1 + 3_600).await.unwrap().paid, 0);
        }
        assert_eq!(r.node.sent.lock().unwrap().len(), 1);
        // Semaine suivante : un nouveau virement.
        assert_eq!(r.fees.charge_week(WEEK2).await.unwrap().paid, 1);
        assert_eq!(r.node.sent.lock().unwrap().len(), 2);
        let journal: i64 = r.db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM journal WHERE stream = 'hosting_fee'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(journal, 2);
        assert!(r.told.lock().unwrap().iter().any(|t| t.contains("3 ẑ envoyés au Capitaine")));
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn the_default_price_applies_to_accounts_without_their_own() {
        let r = rig("default").await;
        set(&r, DEFAULT_SCOPE, "default_price", "2").await;
        set(&r, PRICE_SCOPE, BOB, "0").await;
        assert_eq!(r.fees.charge_week(WEEK1).await.unwrap().paid, 1, "Alice au prix par défaut, Bob gratuit");
        assert_eq!(r.node.sent.lock().unwrap()[0].1, 20);
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn an_empty_wallet_is_told_once_nothing_is_cut_and_it_pays_when_refilled() {
        let r = rig("poor").await;
        set(&r, PRICE_SCOPE, ALICE, "5").await;
        *r.node.total.lock().unwrap() = 100 + 10; // 1 ẑ disponible
        for i in 0..3 {
            let report = r.fees.charge_week(WEEK1 + i * 3_600).await.unwrap();
            assert_eq!((report.paid, report.unpaid), (0, 1));
        }
        assert!(r.node.sent.lock().unwrap().is_empty());
        assert_eq!(r.told.lock().unwrap().iter().filter(|t| t.contains("solde insuffisant")).count(), 1, "prévenu une seule fois");
        *r.node.total.lock().unwrap() = 100 + 2_000;
        assert_eq!(r.fees.charge_week(WEEK1 + 4 * 3_600).await.unwrap().paid, 1);
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn an_unknown_outcome_is_never_replayed_and_an_unreachable_node_is_retried() {
        let r = rig("unknown").await;
        set(&r, PRICE_SCOPE, ALICE, "3").await;
        *r.node.fail.lock().unwrap() = Some(ChainError::Connection("coupé".into()));
        assert_eq!(r.fees.charge_week(WEEK1).await.unwrap().retry, 1);
        *r.node.fail.lock().unwrap() = Some(ChainError::UnknownOutcome("délai".into()));
        assert_eq!(r.fees.charge_week(WEEK1).await.unwrap().unknown, 1);
        *r.node.fail.lock().unwrap() = None;
        let again = r.fees.charge_week(WEEK1 + 3_600).await.unwrap();
        assert_eq!((again.paid, again.unknown), (0, 0), "jamais rejoué : le Capitaine vérifie le solde");
        assert!(r.node.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn the_captain_is_never_billed_and_a_missing_captain_wallet_is_said() {
        let r = rig("cap").await;
        set(&r, PRICE_SCOPE, "cc", "9").await;
        assert_eq!(r.fees.charge_week(WEEK1).await.unwrap().paid, 0);
        set(&r, PRICE_SCOPE, ALICE, "3").await;
        r.db.writer().execute("UPDATE identities SET g1pub_nostr = NULL WHERE role = 'captain'", vec![]).await.unwrap();
        assert!(r.fees.charge_week(WEEK1).await.is_err());
        let _ = std::fs::remove_dir_all(&r.dir);
    }
}
