//! **Transaction primale des MULTIPASS** (1 Ğ1 du portefeuille maître `UPLANETNAME_G1`), envoyée seulement quand le membre
//! a **prouvé** qu'il a bien reçu et qu'il utilise ses clés : sa première connexion authentifiée (navigateur avec son jeton,
//! WebDAV, NIP-98). Équivalent du « PRIMO TX » de `make_NOSTRCARD.sh`, qui, lui, payait dès la création.
//!
//! Pourquoi attendre : un compte dont personne n'a noté les clés est perdu ; lui envoyer 1 Ğ1 serait les perdre aussi, et
//! des inscriptions en rafale videraient le maître. Un MULTIPASS créé ici qui n'a donné aucune preuve en 24 h peut être
//! **éjecté** par le Capitaine (écran Station) : il n'a jamais reçu de Ğ1, rien n'est perdu.
//!
//! Garde-fous (mêmes règles que [`crate::primal_init`]) :
//! - seuls les comptes **créés par cette station** (marque `member_born`) sont concernés : les comptes plus anciens ont été
//!   initialisés par Astroport.ONE ;
//! - un seul envoi par compte, noté AVANT la signature ; une issue inconnue n'est jamais rejouée (alerte au Capitaine) ;
//! - un compte qui détient déjà 1 Ğ1 ou plus est considéré comme initialisé ;
//! - au plus [`MAX_PER_DAY`] primales par jour ; le maître garde sa réserve.

use std::collections::HashSet;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_duniter::{Account, Amount, Chain, ChainError, TransferMode};
use serde_json::json;
use tokio::sync::watch;

use crate::coop_wallets::Wallets;
use crate::primal_init::INIT_G1_CENTS;
use crate::station::RESERVE_G1_CENTS;

/// `kv(member_born, <hex>)` = date de création du compte par cette station.
pub const BORN_SCOPE: &str = "member_born";
/// `kv(member_proof, <hex>)` = date de la première connexion authentifiée (preuve de réception des clés).
pub const PROOF_SCOPE: &str = "member_proof";
/// `kv(member_primo, <hex>)` = `sending` | `paid` | `unknown`.
pub const PRIMO_SCOPE: &str = "member_primo";
/// Délai après lequel un compte sans preuve peut être éjecté.
pub const EJECT_AFTER_SECS: i64 = 86_400;
/// Plafond de primales par jour glissant.
pub const MAX_PER_DAY: i64 = 20;
const EVERY: Duration = Duration::from_secs(300);

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Note la preuve de réception des clés. Appelé à chaque requête authentifiée : une seule écriture par compte et par
/// processus, et seulement pour un compte créé ici.
pub struct Proofs {
    db: Arc<Db>,
    seen: Mutex<HashSet<String>>,
}

impl Proofs {
    /// Registre des preuves.
    pub fn new(db: Arc<Db>) -> Arc<Self> {
        Arc::new(Self { db, seen: Mutex::new(HashSet::new()) })
    }

    /// Un compte vient de s'authentifier.
    pub fn note(self: &Arc<Self>, hex: &str) {
        if !self.seen.lock().unwrap_or_else(|e| e.into_inner()).insert(hex.to_owned()) {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        let (db, hex) = (self.db.clone(), hex.to_owned());
        rt.spawn(async move {
            let at = now();
            let r = db
                .writer()
                .execute(
                    "INSERT OR IGNORE INTO kv(scope, key, value, updated_at) SELECT ?1, ?2, ?3, ?3 WHERE EXISTS (SELECT 1 FROM kv WHERE scope = ?4 AND key = ?2)",
                    vec![PROOF_SCOPE.to_owned().into(), hex.into(), at.into(), BORN_SCOPE.to_owned().into()],
                )
                .await;
            if let Err(e) = r {
                eprintln!("preuve de réception des clés non notée : {e}");
            }
        });
    }
}

/// Où en est un compte : `None` pour un compte qui n'a pas été créé ici.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Onboarding {
    /// Création du compte.
    pub born_at: i64,
    /// Première connexion authentifiée.
    pub proved_at: Option<i64>,
    /// État de la primale (`sending`, `paid`, `unknown`).
    pub primo: Option<String>,
}

impl Onboarding {
    /// Éjectable : aucune preuve, aucune primale, et plus de 24 h d'ancienneté.
    pub fn ejectable(&self, at: i64) -> bool {
        self.proved_at.is_none() && self.primo.is_none() && at.saturating_sub(self.born_at) >= EJECT_AFTER_SECS
    }

    /// Résumé JSON pour l'écran Station.
    pub fn to_json(&self, at: i64) -> serde_json::Value {
        json!({ "born_at": self.born_at, "proved_at": self.proved_at, "primo": self.primo, "ejectable": self.ejectable(at) })
    }
}

/// Lecture synchrone (dans une transaction de lecture) de l'accueil d'un compte.
pub fn onboarding(c: &rusqlite::Connection, hex: &str) -> rusqlite::Result<Option<Onboarding>> {
    let get = |scope: &str| -> rusqlite::Result<Option<String>> {
        rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![scope, hex], |r| r.get::<_, String>(0)))
    };
    let Some(born) = get(BORN_SCOPE)?.and_then(|v| v.parse().ok()) else { return Ok(None) };
    Ok(Some(Onboarding { born_at: born, proved_at: get(PROOF_SCOPE)?.and_then(|v| v.parse().ok()), primo: get(PRIMO_SCOPE)? }))
}

/// Compte en attente de sa primale : clé, portefeuille Ğ1, email.
type Pending = (String, Option<String>, Option<String>);

/// Bilan d'un passage.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Primales envoyées.
    pub paid: u32,
    /// Déjà dotés de 1 Ğ1 ou plus.
    pub already: u32,
    /// En attente : maître trop pauvre ou plafond du jour atteint.
    pub waiting: u32,
    /// Issues inconnues (jamais rejouées).
    pub unknown: u32,
    /// Nœud injoignable : au prochain passage.
    pub retry: u32,
}

/// Envoi des primales des MULTIPASS qui ont prouvé la réception de leurs clés.
pub struct MemberPrimo<C: Chain> {
    db: Arc<Db>,
    chain: Arc<C>,
    wallets: Arc<Wallets>,
    activity: ActivityBus,
}

impl<C: Chain> MemberPrimo<C> {
    /// Primales depuis le maître de `wallets`.
    pub fn new(db: Arc<Db>, chain: Arc<C>, wallets: Arc<Wallets>, activity: ActivityBus) -> Self {
        Self { db, chain, wallets, activity }
    }

    async fn set(&self, hex: &str, value: Option<&str>) {
        let w = self.db.writer();
        let r = match value {
            Some(v) => w.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![PRIMO_SCOPE.to_owned().into(), hex.to_owned().into(), v.to_owned().into(), now().into()]).await,
            None => w.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![PRIMO_SCOPE.to_owned().into(), hex.to_owned().into()]).await,
        };
        if let Err(e) = r {
            self.activity.publish_to(Audience::Captain, "alert", "primal", format!("primale de {} : marque non écrite ({e})", hex.get(..8).unwrap_or(hex)), None);
        }
    }

    /// Un passage.
    pub async fn run_once(&self, at: u64) -> Result<Report, String> {
        let mut report = Report::default();
        let since = i64::try_from(at).unwrap_or(0) - 86_400;
        let (todo, sent_today): (Vec<Pending>, i64) = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT i.hex, i.g1pub_nostr, i.email FROM identities i \
                     JOIN kv p ON p.scope = 'member_proof' AND p.key = i.hex \
                     WHERE i.hosted = 1 AND i.role IN ('multipass', 'captain') \
                       AND NOT EXISTS (SELECT 1 FROM kv m WHERE m.scope = 'member_primo' AND m.key = i.hex) \
                     ORDER BY p.value LIMIT 100",
                )?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
                let n = c.query_row("SELECT COUNT(*) FROM journal WHERE stream = 'member_primo' AND at >= ?1", [since], |r| r.get(0))?;
                Ok((rows, n))
            })
            .await
            .map_err(|e| e.to_string())?;
        if todo.is_empty() {
            return Ok(report);
        }
        let master = self.wallets.master().await?;
        let mut spendable = match self.chain.balance(&master.account()).await {
            Ok(b) => b.spendable(Amount(RESERVE_G1_CENTS)).0,
            Err(_) => {
                report.retry += 1;
                return Ok(report);
            }
        };
        let mut budget = (MAX_PER_DAY - sent_today).max(0);
        for (hex, g1, email) in todo {
            let who = email.unwrap_or_else(|| hex.get(..12).unwrap_or(&hex).to_owned());
            let Some(account) = g1.as_deref().and_then(|g| Account::from_str(g).ok()) else {
                self.activity.publish_to(Audience::Captain, "alert", "primal", format!("primale de {who} : portefeuille Ğ1 inconnu"), None);
                continue;
            };
            match self.chain.balance(&account).await {
                Ok(b) if b.total.0 >= INIT_G1_CENTS => {
                    self.set(&hex, Some("paid")).await;
                    report.already += 1;
                    continue;
                }
                Ok(_) => {}
                Err(_) => {
                    report.retry += 1;
                    continue;
                }
            }
            if budget == 0 || spendable < INIT_G1_CENTS {
                report.waiting += 1;
                continue;
            }
            self.set(&hex, Some("sending")).await;
            let memo = "UPLANET:PRIMAL:MULTIPASS";
            match self.chain.transfer(&master, &account, Amount(INIT_G1_CENTS), TransferMode::KeepAlive, Some(memo)).await {
                Ok(out) => {
                    self.set(&hex, Some("paid")).await;
                    spendable -= INIT_G1_CENTS;
                    budget -= 1;
                    report.paid += 1;
                    let record = json!({ "hex": hex, "to": account.ss58(), "g1_cents": INIT_G1_CENTS, "block": hex::encode(out.block_hash) });
                    let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('member_primo', ?1, ?2)", vec![i64::try_from(at).unwrap_or(0).into(), record.to_string().into()]).await;
                    self.activity.publish_to(Audience::Captain, "system", "primal", format!("{who} a reçu sa transaction primale (1 Ğ1) : clés bien reçues"), None);
                }
                Err(ChainError::Connection(_)) => {
                    self.set(&hex, None).await; // perdu AVANT l'envoi : réessayer est sans danger
                    report.retry += 1;
                }
                Err(ChainError::UnknownOutcome(why)) => {
                    self.set(&hex, Some("unknown")).await;
                    report.unknown += 1;
                    self.activity.publish_to(Audience::Captain, "alert", "primal", format!("primale de {who} : issue inconnue ({why}) — vérifier son solde avant de rejouer"), None);
                }
                Err(other) => {
                    self.set(&hex, None).await;
                    report.retry += 1;
                    self.activity.publish_to(Audience::Captain, "alert", "primal", format!("primale de {who} refusée : {other}"), None);
                }
            }
        }
        Ok(report)
    }

    /// Boucle de fond : toutes les 5 minutes (une preuve arrive à la première connexion du membre).
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                () = tokio::time::sleep(EVERY) => {}
                _ = stop.changed() => return,
            }
            let Ok(at) = astro_core::solar::system_now_checked() else { continue };
            match self.run_once(at).await {
                Ok(r) if r.paid + r.unknown > 0 => println!("Primales MULTIPASS : {} envoyée(s), {} incertaine(s), {} en attente", r.paid, r.unknown, r.waiting),
                Ok(_) => {}
                Err(why) => {
                    self.activity.publish_to(Audience::Captain, "alert", "primal", why, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_duniter::{Balance, Ed25519Signer, TransferEvent, TxOutcome};

    use super::*;
    use crate::coop_wallets::Coop;

    struct Ledger {
        balances: Mutex<std::collections::HashMap<Account, u64>>,
        sent: Mutex<Vec<(Account, u64)>>,
    }

    impl Chain for Ledger {
        async fn balance(&self, a: &Account) -> Result<Balance, ChainError> {
            let t = self.balances.lock().unwrap().get(a).copied().unwrap_or(0);
            Ok(Balance { total: Amount(t), transferable: Amount(t), unclaimed_uds: Amount(0) })
        }
        async fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, _: TransferMode, _: Option<&str>) -> Result<TxOutcome, ChainError> {
            let mut b = self.balances.lock().unwrap();
            *b.entry(from.account()).or_default() -= amount.0;
            *b.entry(*to).or_default() += amount.0;
            self.sent.lock().unwrap().push((*to, amount.0));
            Ok(TxOutcome { block_hash: [9; 32], transferred: amount })
        }
        async fn watch_transfers(&self, _: tokio::sync::mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
            std::future::pending().await
        }
        fn existential_deposit(&self) -> Amount {
            Amount(100)
        }
    }

    async fn kv(db: &Db, scope: &str, key: &str, value: &str) {
        db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, 0)", vec![scope.to_owned().into(), key.to_owned().into(), value.to_owned().into()]).await.unwrap();
    }

    #[tokio::test]
    async fn the_primal_waits_for_proof_then_is_sent_once() {
        let dir = std::env::temp_dir().join(format!("astroportd-member-primo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let master_signer = Ed25519Signer::from_seed([0x01; 32]);
        let master = master_signer.account();
        let coop = Coop { node: Account([1; 32]), captain: Account([2; 32]), impot: Account([3; 32]), treasury: Account([4; 32]), rnd: Account([5; 32]), assets: Account([6; 32]) };
        let wallets = Arc::new(Wallets::fixed(db.clone(), Ed25519Signer::from_seed([0x44; 32]), coop).with_master(master_signer));
        let member = Account([7; 32]);
        let (hex, old) = ("ab".repeat(32), "cd".repeat(32));
        for (h, g) in [(&hex, member), (&old, Account([8; 32]))] {
            db.writer().execute("INSERT INTO identities(hex, role, email, g1pub_nostr, hosted) VALUES (?1, 'multipass', ?2, ?3, 1)", vec![h.clone().into(), format!("{}@ex.org", &h[..4]).into(), g.ss58().into()]).await.unwrap();
        }
        let chain = Arc::new(Ledger { balances: Mutex::new(std::collections::HashMap::from([(master, 100 + 500)])), sent: Mutex::new(Vec::new()) });
        let primo = MemberPrimo::new(db.clone(), chain.clone(), wallets, ActivityBus::new());
        kv(&db, BORN_SCOPE, &hex, "1000").await;

        // Sans preuve : rien. Le compte ancien (sans marque de naissance) n'est jamais concerné, même connecté.
        let proofs = Proofs::new(db.clone());
        proofs.note(&old);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(primo.run_once(2_000).await.unwrap(), Report::default());
        let ob = db.readers().read({ let h = hex.clone(); move |c| Ok(onboarding(c, &h)?) }).await.unwrap().unwrap();
        assert!(!ob.ejectable(1_000 + 3_600) && ob.ejectable(1_000 + EJECT_AFTER_SECS), "éjectable après 24 h sans preuve");

        // Première connexion : la preuve est notée, la primale part, une seule fois.
        proofs.note(&hex);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(primo.run_once(3_000).await.unwrap().paid, 1);
        assert_eq!(primo.run_once(3_300).await.unwrap(), Report::default());
        assert_eq!(chain.sent.lock().unwrap().clone(), vec![(member, 100)]);
        let ob = db.readers().read({ let h = hex.clone(); move |c| Ok(onboarding(c, &h)?) }).await.unwrap().unwrap();
        assert!(ob.proved_at.is_some() && ob.primo.as_deref() == Some("paid") && !ob.ejectable(i64::MAX / 2));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
