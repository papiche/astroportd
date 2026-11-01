//! Hébergement facturé par le Capitaine : **un prix hebdomadaire en ẑ par MULTIPASS hébergé**, viré sur la Ğ1 au
//! **compte technique d'hébergement** ([`crate::coop_wallets`]), puis redistribué chaque semaine par
//! [`crate::settlement`] (NODE, Capitaine, coopérative).
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
//! **C'est la machine d'états du cycle de vie qui décide** ([`astro_lifecycle::state::decide`]), et le prélèvement n'est
//! que son action `Charge` : tous les 7 jours depuis le dernier prélèvement réussi, le prix est viré ; solde
//! insuffisant, le compte entre en **période de grâce** (7 jours, il peut régulariser à tout moment), puis en
//! **suspension** ; il est réactivé dès que le virement passe. Grâce et suspension sont **un statut affiché et un
//! message au titulaire : elles n'affectent jamais le service** (accès, fichiers, restauration), et rien n'est jamais
//! supprimé. Les virements réussis comptent comme loyers (vue `rents`) pour la clôture hebdomadaire.
//!
//! Garde-fous :
//! - on ne facture que les MULTIPASS **hébergés ici**, dont la clé Ğ1 est sur cette station ; ni le Capitaine, ni les
//!   sociétaires (`cooperative_member`) ;
//! - la facturation d'un prix commence **une semaine après qu'il a été fixé** : fixer un prix ne débite jamais sur
//!   l'instant ;
//! - **un virement par cycle**, noté dans `kv('hosting_fee', '<hex>:<début du cycle>')` AVANT l'envoi ; une issue
//!   inconnue après envoi n'est JAMAIS rejouée (le Capitaine est prévenu de vérifier le solde) ;
//! - un prix remis à 0 rétablit un compte en grâce ou suspendu.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Amount as Cents, Audience};
use astro_db::{Db, Statement};
use astro_duniter::{Account, Amount, Chain, ChainError, Ed25519Signer, TransferMode};
use astro_lifecycle::dunikey::Dunikey;
use astro_lifecycle::state::{AccountState, Action, Contract, Cycle, Status, decide};
use astro_lifecycle::store;
use rusqlite::OptionalExtension;
use rusqlite::types::Value;
use serde_json::json;
use tokio::sync::watch;

use crate::coop_wallets::Wallets;
use crate::station::{RESERVE_G1_CENTS, ZEN_PER_G1};
use crate::upload_worker::Notify;

/// Espace des prix par compte.
pub const PRICE_SCOPE: &str = "hosting_price";
/// Espace des réglages par défaut (`default_price`, `default_quota`).
pub const DEFAULT_SCOPE: &str = "hosting";
/// Espace de l'état des virements (`<hex>:<début du cycle>`).
const FEE_SCOPE: &str = "hosting_fee";
/// Intervalle entre deux passages (le virement d'un cycle est idempotent).
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// Plafond du prix hebdomadaire : une faute de frappe ne vide pas un compte.
pub const MAX_WEEKLY_ZEN: u64 = 10_000;

/// Prix hebdomadaire (ẑ) de `owner` : le sien, sinon le défaut du Capitaine, sinon 0.
pub fn weekly_price(c: &rusqlite::Connection, owner: &str) -> rusqlite::Result<u64> {
    Ok(price_and_since(c, owner)?.0)
}

/// Prix applicable à `owner` et date (secondes) où il a été fixé.
fn price_and_since(c: &rusqlite::Connection, owner: &str) -> rusqlite::Result<(u64, u64)> {
    let own: Option<(String, i64)> = c.query_row("SELECT value, updated_at FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![PRICE_SCOPE, owner], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let row = match own {
        Some(v) => Some(v),
        None => c.query_row("SELECT value, updated_at FROM kv WHERE scope = ?1 AND key = 'default_price'", [DEFAULT_SCOPE], |r| Ok((r.get(0)?, r.get(1)?))).optional()?,
    };
    Ok(row.map_or((0, 0), |(v, at)| (v.trim().parse().unwrap_or(0), u64::try_from(at).unwrap_or(0))))
}

/// Cycle de vie du compte pour l'affichage : `{ status, last_ok_at, due_at }`, ou `None` s'il n'a jamais été évalué.
pub fn lifecycle_summary(c: &rusqlite::Connection, owner: &str) -> Option<serde_json::Value> {
    let raw: String = c.query_row(store::load_state_sql(), [owner], |r| r.get(0)).ok()?;
    let s = AccountState::from_json(&serde_json::from_str(&raw).ok()?)?;
    let since = s.last_ok_at.unwrap_or(s.created_at);
    Some(json!({ "status": s.status.as_str(), "last_ok_at": s.last_ok_at, "due_at": since + astro_lifecycle::state::CYCLE_SECS }))
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Bilan d'un passage.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Virements partis.
    pub paid: u32,
    /// Comptes entrés en période de grâce.
    pub grace: u32,
    /// Comptes suspendus.
    pub suspended: u32,
    /// Comptes rétablis (prix remis à 0).
    pub restored: u32,
    /// Issues inconnues après envoi (jamais rejouées).
    pub unknown: u32,
    /// Nœud Duniter injoignable avant l'envoi : au prochain passage.
    pub retry: u32,
}

impl Report {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Compte à facturer.
struct Member {
    hex: String,
    email: String,
    wallet: String,
    contract: Contract,
    price: u64,
    price_since: u64,
    state: Option<AccountState>,
}

/// Prélèvement hebdomadaire de l'hébergement.
pub struct HostingFees<C: Chain> {
    db: Arc<Db>,
    chain: Arc<C>,
    accounts_dir: PathBuf,
    wallets: Arc<Wallets>,
    activity: ActivityBus,
    notify: Option<Notify>,
}

impl<C: Chain> HostingFees<C> {
    /// `accounts_dir` : dossier des comptes (`~/.zen/game/nostr`), où se trouve le `.secret.dunikey` de chaque MULTIPASS.
    pub fn new(db: Arc<Db>, chain: Arc<C>, accounts_dir: PathBuf, wallets: Arc<Wallets>, activity: ActivityBus, notify: Option<Notify>) -> Self {
        Self { db, chain, accounts_dir, wallets, activity, notify }
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

    async fn marker(&self, key: &str) -> Option<String> {
        let k = key.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![FEE_SCOPE, k], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten()
    }

    async fn set_marker(&self, key: &str, value: &str) {
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![FEE_SCOPE.to_owned().into(), key.to_owned().into(), value.to_owned().into(), now().into()]).await;
    }

    async fn clear_marker(&self, key: &str) {
        let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![FEE_SCOPE.to_owned().into(), key.to_owned().into()]).await;
    }

    async fn save(&self, hex: &str, state: &AccountState, at: u64) {
        let status = Value::Text(state.status.as_str().to_owned());
        let stmts = vec![store::save_state(hex, state, at), Statement::new("UPDATE identities SET status = ?2 WHERE hex = ?1 AND status IS NOT ?2", vec![Value::Text(hex.to_owned()), status])];
        if let Err(e) = self.db.writer().batch(stmts).await {
            self.alert(format!("cycle de vie de {} : état non enregistré ({e})", &hex[..hex.len().min(8)]));
        }
    }

    async fn members(&self) -> Result<Vec<Member>, String> {
        self.db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT hex, email, g1pub_nostr, contract FROM identities WHERE hosted = 1 AND role = 'multipass' AND email IS NOT NULL AND g1pub_nostr IS NOT NULL ORDER BY hex")?;
                let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?))).collect_rows()?;
                let mut out = Vec::new();
                for (hex, email, wallet, contract) in rows {
                    let (price, price_since) = price_and_since(c, &hex)?;
                    let state = c
                        .query_row(store::load_state_sql(), [&hex], |r| r.get::<_, String>(0))
                        .optional()?
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .and_then(|v| AccountState::from_json(&v));
                    let contract = if contract.as_deref() == Some("cooperative_member") { Contract::CooperativeMember } else { Contract::ActiveRental };
                    out.push(Member { hex, email, wallet, contract, price, price_since, state });
                }
                Ok(out)
            })
            .await
            .map_err(|e| format!("base illisible : {e}"))
    }

    /// Signataire du MULTIPASS `m` : sa clé Ğ1 doit correspondre à son portefeuille connu.
    fn signer_of(&self, m: &Member) -> Option<Ed25519Signer> {
        let dk = std::fs::read_to_string(self.accounts_dir.join(&m.email).join(".secret.dunikey")).ok().and_then(|t| Dunikey::parse(&t).ok())?;
        let signer = Ed25519Signer::from_seed(*dk.ed25519_seed());
        (Account::from_str(&m.wallet).ok() == Some(signer.account())).then_some(signer)
    }

    /// Un passage à l'instant `at` (secondes Unix) : évalue chaque compte facturé et fait ce que la machine ordonne.
    pub async fn run_cycles(&self, at: u64) -> Result<Report, String> {
        let mut report = Report::default();
        let mut tech: Option<Account> = None;
        for m in self.members().await? {
            if m.contract != Contract::ActiveRental {
                continue;
            }
            let who = m.email.split('@').next().unwrap_or(&m.email).to_owned();
            let mut state = m.state.clone().unwrap_or_else(|| AccountState::new(m.contract, at));
            if m.price == 0 {
                if matches!(state.status, Status::GracePeriod | Status::Suspended) {
                    state.status = Status::good_standing(m.contract);
                    state.status_since = at;
                    state.last_ok_at = Some(at);
                    self.save(&m.hex, &state, at).await;
                    self.tell(&m.hex, "✅ Votre hébergement est gratuit : votre compte est rétabli.".into()).await;
                    report.restored += 1;
                }
                continue;
            }
            // Un prix fixé aujourd'hui ne débite que dans une semaine ; un compte jamais évalué part d'aujourd'hui.
            let cycle_start = state.last_ok_at.unwrap_or(state.created_at).max(m.price_since);
            if m.state.is_none() || cycle_start > state.last_ok_at.unwrap_or(state.created_at) {
                state.last_ok_at = Some(cycle_start);
            }
            let Some(signer) = self.signer_of(&m) else {
                self.alert(format!("Hébergement de {who} : clé Ğ1 du MULTIPASS illisible sur cette station"));
                continue;
            };
            let balance = match self.chain.balance(&signer.account()).await {
                Ok(b) => b.total.0.saturating_sub(RESERVE_G1_CENTS).saturating_mul(ZEN_PER_G1),
                Err(_) => {
                    report.retry += 1;
                    continue;
                }
            };
            let decision = decide(&state, &Cycle { rent_due: Cents(m.price.saturating_mul(100)), balance: Cents(balance), shares_valid: true }, at);
            match decision.action {
                Action::NotDue | Action::StillInGrace | Action::StillSuspended | Action::SharesValidated | Action::Reactivated(None) => {
                    if m.state.is_none() {
                        self.save(&m.hex, &state, at).await; // premier passage : le cycle commence
                    }
                }
                Action::Charge(_) | Action::Reactivated(Some(_)) => {
                    let to = match &tech {
                        Some(t) => *t,
                        None => *tech.insert(self.wallets.tech().await?.account()),
                    };
                    let reactivated = matches!(decision.action, Action::Reactivated(_));
                    match self.charge(&m, &signer, &to, cycle_start, &decision.state, at).await {
                        Step::Paid => {
                            report.paid += 1;
                            if reactivated {
                                self.tell(&m.hex, "✅ Hébergement réglé : votre compte est rétabli.".into()).await;
                            }
                        }
                        Step::Unknown => report.unknown += 1,
                        Step::Retry => report.retry += 1,
                        Step::Skipped => {}
                    }
                }
                Action::EnteredGrace => {
                    self.save(&m.hex, &decision.state, at).await;
                    self.tell(&m.hex, format!("⚠️ Hébergement de {} ẑ non réglé (solde insuffisant) : période de grâce de 7 jours. Votre service continue ; rechargez votre portefeuille, le prélèvement se fera tout seul.", m.price)).await;
                    self.alert(format!("Hébergement de {who} ({} ẑ) non réglé : période de grâce de 7 jours", m.price));
                    report.grace += 1;
                }
                Action::Suspended => {
                    self.save(&m.hex, &decision.state, at).await;
                    self.tell(&m.hex, format!("⛔ Hébergement de {} ẑ toujours non réglé : compte noté « suspendu ». Votre service et vos fichiers ne sont pas touchés ; le statut sera levé dès que le prélèvement passera.", m.price)).await;
                    self.alert(format!("Hébergement de {who} : compte noté suspendu (le service n'est pas coupé, rien n'est supprimé)"));
                    report.suspended += 1;
                }
            }
        }
        Ok(report)
    }

    /// Le virement du cycle : noté avant l'envoi, jamais rejoué si son issue est inconnue.
    async fn charge(&self, m: &Member, signer: &Ed25519Signer, to: &Account, cycle_start: u64, next: &AccountState, at: u64) -> Step {
        let key = format!("{}:{cycle_start}", m.hex);
        match self.marker(&key).await.as_deref() {
            Some("paid") => {
                // Le virement est parti mais l'état n'avait pas été enregistré (arrêt brutal) : on le rattrape sans rien virer.
                self.save(&m.hex, next, at).await;
                return Step::Skipped;
            }
            Some(_) => return Step::Skipped, // `sending` : issue inconnue, le Capitaine vérifie
            None => {}
        }
        let who = m.email.split('@').next().unwrap_or(&m.email);
        let g1_cents = m.price.saturating_mul(100) / ZEN_PER_G1;
        self.set_marker(&key, "sending").await;
        let memo = format!("UPLANET:HOSTING:{cycle_start}:{}", &m.hex[..m.hex.len().min(8)]);
        match self.chain.transfer(signer, to, Amount(g1_cents), TransferMode::KeepAlive, Some(&memo)).await {
            Ok(out) => {
                self.set_marker(&key, "paid").await;
                let record = json!({ "owner": m.hex, "cycle": cycle_start, "zen": m.price, "g1_cents": g1_cents, "block": hex::encode(out.block_hash) });
                let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('hosting_fee', ?1, ?2)", vec![now().into(), record.to_string().into()]).await;
                self.save(&m.hex, next, at).await;
                self.tell(&m.hex, format!("🏠 Hébergement : {} ẑ envoyés (prochain prélèvement dans 7 jours).", m.price)).await;
                Step::Paid
            }
            Err(ChainError::Connection(_)) => {
                self.clear_marker(&key).await; // perdu AVANT l'envoi : réessayer est sans danger
                Step::Retry
            }
            Err(ChainError::UnknownOutcome(why)) => {
                self.alert(format!("Hébergement de {who} : issue du virement inconnue ({why}) — vérifier son solde avant de rejouer"));
                Step::Unknown
            }
            Err(other) => {
                self.clear_marker(&key).await;
                self.alert(format!("Hébergement de {who} : virement refusé ({other})"));
                Step::Retry
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
            match self.run_cycles(at).await {
                Ok(r) if !r.is_empty() => println!("Hébergement : {} viré(s), {} en grâce, {} suspendu(s), {} rétabli(s), {} incertain(s), {} à retenter", r.paid, r.grace, r.suspended, r.restored, r.unknown, r.retry),
                Ok(_) => {}
                Err(why) => self.alert(why),
            }
        }
    }
}

enum Step {
    Paid,
    Skipped,
    Unknown,
    Retry,
}

/// Petit confort : `query_map(..)` puis collecte des lignes en une seule expression.
trait CollectRows<T> {
    fn collect_rows(self) -> rusqlite::Result<Vec<T>>;
}

impl<T, I: Iterator<Item = rusqlite::Result<T>>> CollectRows<T> for rusqlite::Result<I> {
    fn collect_rows(self) -> rusqlite::Result<Vec<T>> {
        self?.collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use astro_duniter::{Balance, TransferEvent, TxOutcome};

    use super::*;
    use crate::coop_wallets::Coop;

    /// Chaîne factice partagée avec les tests du règlement.
    pub(crate) struct Node {
        pub total: Mutex<u64>,
        pub sent: Mutex<Vec<(Account, u64, String)>>,
        pub fail: Mutex<Option<ChainError>>,
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
    const T0: u64 = 1_800_000_000;
    const DAY: u64 = 86_400;

    struct Rig {
        dir: PathBuf,
        db: Arc<Db>,
        node: Arc<Node>,
        told: Arc<Mutex<Vec<String>>>,
        fees: HostingFees<Node>,
        tech: Account,
    }

    async fn rig(tag: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("astroportd-hosting-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        db.writer().execute("INSERT INTO identities(hex, role, email, g1pub_nostr, hosted) VALUES ('cc', 'captain', 'cap@example.org', ?1, 1)", vec![Account([0x45; 32]).ss58().into()]).await.unwrap();
        for (hex, email, seed) in [(ALICE, "alice@example.org", 0x11u8), (BOB, "bob@example.org", 0x12)] {
            std::fs::create_dir_all(dir.join("accounts").join(email)).unwrap();
            std::fs::write(dir.join("accounts").join(email).join(".secret.dunikey"), astro_lifecycle::dunikey::render(&[seed; 32])).unwrap();
            let wallet = Ed25519Signer::from_seed([seed; 32]).account().ss58();
            db.writer().execute("INSERT INTO identities(hex, role, email, g1pub_nostr, hosted, contract) VALUES (?1, 'multipass', ?2, ?3, 1, 'active_rental')", vec![hex.to_owned().into(), email.to_owned().into(), wallet.into()]).await.unwrap();
        }
        let tech_signer = Ed25519Signer::from_seed([0x44; 32]);
        let tech = tech_signer.account();
        let coop = Coop { node: Account([1; 32]), captain: Account([2; 32]), impot: Account([3; 32]), treasury: Account([4; 32]), rnd: Account([5; 32]), assets: Account([6; 32]) };
        let wallets = Arc::new(Wallets::fixed(db.clone(), tech_signer, coop));
        let node = Arc::new(Node { total: Mutex::new(100 + 2_000), sent: Mutex::new(Vec::new()), fail: Mutex::new(None) }); // réserve + 20 Ğ1 = 200 ẑ
        let told = Arc::new(Mutex::new(Vec::new()));
        let log = told.clone();
        let notify: Notify = Arc::new(move |_, text| {
            let log = log.clone();
            Box::pin(async move { log.lock().unwrap().push(text) })
        });
        let fees = HostingFees::new(db.clone(), node.clone(), dir.join("accounts"), wallets, ActivityBus::new(), Some(notify));
        Rig { dir, db, node, told, fees, tech }
    }

    async fn set(r: &Rig, scope: &str, key: &str, value: &str, at: u64) {
        r.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![scope.to_owned().into(), key.to_owned().into(), value.to_owned().into(), i64::try_from(at).unwrap().into()]).await.unwrap();
    }

    async fn status(r: &Rig, hex: &str) -> Option<String> {
        let h = hex.to_owned();
        r.db.readers().read(move |c| Ok(lifecycle_summary(c, &h).and_then(|v| v["status"].as_str().map(str::to_owned)))).await.unwrap()
    }

    #[tokio::test]
    async fn nothing_is_charged_until_the_captain_sets_a_price() {
        let r = rig("free").await;
        assert!(r.fees.run_cycles(T0).await.unwrap().is_empty());
        assert!(r.fees.run_cycles(T0 + 30 * DAY).await.unwrap().is_empty());
        assert!(r.node.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn a_new_price_is_charged_a_week_later_then_every_week_to_the_technical_account() {
        let r = rig("paid").await;
        set(&r, PRICE_SCOPE, ALICE, "3", T0).await; // 128 Go à 3 ẑ
        set(&r, PRICE_SCOPE, BOB, "0", T0).await; // gratuit
        for d in [0, 3, 6] {
            assert_eq!(r.fees.run_cycles(T0 + d * DAY).await.unwrap().paid, 0, "jour {d} : fixer un prix ne débite pas");
        }
        assert_eq!(r.fees.run_cycles(T0 + 7 * DAY).await.unwrap().paid, 1);
        {
            let sent = r.node.sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!((sent[0].0, sent[0].1), (r.tech, 30), "3 ẑ = 0,3 Ğ1 = 30 centimes de Ğ1, vers le compte technique");
            assert!(sent[0].2.starts_with("UPLANET:HOSTING:") && sent[0].2.ends_with(":aa"), "{}", sent[0].2);
        }
        // Passages répétés dans le même cycle : rien de plus ; le suivant, une semaine plus tard.
        for h in [1, 7, 70] {
            assert_eq!(r.fees.run_cycles(T0 + 7 * DAY + h * 3_600).await.unwrap().paid, 0);
        }
        assert_eq!(r.fees.run_cycles(T0 + 14 * DAY).await.unwrap().paid, 1);
        assert_eq!(r.node.sent.lock().unwrap().len(), 2);
        assert_eq!(status(&r, ALICE).await.as_deref(), Some("active_rental"));
        // Les virements comptent comme loyers (clôture hebdomadaire et cycle de vie).
        let rents: i64 = r.db.readers().read(|c| Ok(c.query_row("SELECT COALESCE(SUM(amount_cents), 0) FROM rents WHERE owner_hex = 'aa'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(rents, 600);
        assert!(r.told.lock().unwrap().iter().any(|t| t.contains("3 ẑ envoyés")));
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn the_default_price_applies_to_accounts_without_their_own() {
        let r = rig("default").await;
        set(&r, DEFAULT_SCOPE, "default_price", "2", T0).await;
        set(&r, PRICE_SCOPE, BOB, "0", T0).await;
        r.fees.run_cycles(T0).await.unwrap(); // le cycle commence
        assert_eq!(r.fees.run_cycles(T0 + 7 * DAY).await.unwrap().paid, 1, "Alice au prix par défaut, Bob gratuit");
        assert_eq!(r.node.sent.lock().unwrap()[0].1, 20);
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn an_empty_wallet_goes_grace_then_suspended_nothing_is_cut_and_it_is_restored_when_paid() {
        let r = rig("poor").await;
        set(&r, PRICE_SCOPE, ALICE, "5", T0).await;
        *r.node.total.lock().unwrap() = 100 + 10; // 1 ẑ disponible
        r.fees.run_cycles(T0).await.unwrap(); // le cycle commence
        let week1 = r.fees.run_cycles(T0 + 7 * DAY).await.unwrap();
        assert_eq!((week1.paid, week1.grace), (0, 1));
        assert_eq!(status(&r, ALICE).await.as_deref(), Some("grace_period"));
        assert_eq!(r.fees.run_cycles(T0 + 10 * DAY).await.unwrap(), Report::default(), "toujours en grâce : rien de plus, pas de répétition du message");
        let week2 = r.fees.run_cycles(T0 + 14 * DAY).await.unwrap();
        assert_eq!(week2.suspended, 1);
        assert_eq!(status(&r, ALICE).await.as_deref(), Some("suspended"));
        assert_eq!(r.told.lock().unwrap().iter().filter(|t| t.contains("période de grâce")).count(), 1);
        assert!(r.node.sent.lock().unwrap().is_empty());
        // Le portefeuille est rechargé : le prochain passage prélève et rétablit.
        *r.node.total.lock().unwrap() = 100 + 2_000;
        assert_eq!(r.fees.run_cycles(T0 + 20 * DAY).await.unwrap().paid, 1);
        assert_eq!(status(&r, ALICE).await.as_deref(), Some("active_rental"));
        assert!(r.told.lock().unwrap().iter().any(|t| t.contains("rétabli")));
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn a_price_set_back_to_zero_restores_a_suspended_account() {
        let r = rig("zero").await;
        set(&r, PRICE_SCOPE, ALICE, "5", T0).await;
        *r.node.total.lock().unwrap() = 100;
        for d in [0, 7, 14] {
            r.fees.run_cycles(T0 + d * DAY).await.unwrap();
        }
        assert_eq!(status(&r, ALICE).await.as_deref(), Some("suspended"));
        set(&r, PRICE_SCOPE, ALICE, "0", T0 + 15 * DAY).await;
        assert_eq!(r.fees.run_cycles(T0 + 15 * DAY).await.unwrap().restored, 1);
        assert_eq!(status(&r, ALICE).await.as_deref(), Some("active_rental"));
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn an_unknown_outcome_is_never_replayed_and_an_unreachable_node_is_retried() {
        let r = rig("unknown").await;
        set(&r, PRICE_SCOPE, ALICE, "3", T0).await;
        r.fees.run_cycles(T0).await.unwrap();
        *r.node.fail.lock().unwrap() = Some(ChainError::Connection("coupé".into()));
        assert_eq!(r.fees.run_cycles(T0 + 7 * DAY).await.unwrap().retry, 1);
        *r.node.fail.lock().unwrap() = Some(ChainError::UnknownOutcome("délai".into()));
        assert_eq!(r.fees.run_cycles(T0 + 7 * DAY + 3_600).await.unwrap().unknown, 1);
        *r.node.fail.lock().unwrap() = None;
        let again = r.fees.run_cycles(T0 + 8 * DAY).await.unwrap();
        assert_eq!((again.paid, again.unknown), (0, 0), "jamais rejoué : le Capitaine vérifie le solde");
        assert!(r.node.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn the_captain_and_cooperative_members_are_never_billed() {
        let r = rig("cap").await;
        set(&r, PRICE_SCOPE, "cc", "9", T0).await;
        r.db.writer().execute("UPDATE identities SET contract = 'cooperative_member' WHERE hex = 'bb'", vec![]).await.unwrap();
        set(&r, PRICE_SCOPE, BOB, "4", T0).await;
        assert_eq!(r.fees.run_cycles(T0 + 8 * DAY).await.unwrap().paid, 0);
        assert!(r.node.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&r.dir);
    }
}
