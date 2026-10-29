//! Réaction Nostr **kind 7 `+N`** : « je te donne N Ẑen ». Le paiement est un virement Ğ1 (Duniter v2s) du portefeuille du
//! MULTIPASS **hébergé ici** de celui qui réagit vers le portefeuille du MULTIPASS de l'auteur réagi.
//!
//! Règles (décidées par le Capitaine de la constellation) :
//! - qui signe : le portefeuille Ğ1 du MULTIPASS hébergé de **celui qui réagit** ; une réaction d'un compte hébergé
//!   ailleurs est payée par SA station (jamais deux fois) ;
//! - le destinataire doit avoir un MULTIPASS dont le portefeuille a le **même primal** que la constellation (contrôle
//!   primal : voir [`crate::funds`]) ; sans indexeur pour le vérifier, on ne paie pas, on le dit ;
//! - 1 Ẑen = 0,1 Ğ1 ; plafonds par réaction et par jour pour chaque donateur ; solde suffisant (hors réserve de 1 Ğ1) ;
//! - **jamais dans le chemin d'écriture du relai** : la réaction est déjà acceptée et diffusée quand ce module la voit ;
//! - **idempotent** : une réaction n'est traitée qu'une fois, et une issue inconnue après envoi (connexion perdue) n'est
//!   JAMAIS rejouée — le Capitaine est prévenu de vérifier le solde.
//!
//! Le donateur est informé du résultat (flux personnel et message privé de BRO) ; le destinataire l'est par l'écoute des
//! virements entrants ([`crate::funds`]).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_duniter::{Account, Amount, Chain, ChainError, Ed25519Signer, TransferMode};
use astro_lifecycle::dunikey::Dunikey;
use astro_nostr_proto::Event;
use rusqlite::OptionalExtension;
use serde_json::json;
use tokio::sync::{broadcast, watch};

use crate::funds::PrimalSource;
use crate::station::{RESERVE_G1_CENTS, ZEN_PER_G1};
use crate::upload_worker::Notify;

/// Espace des réactions traitées dans `claimed` et des états d'envoi dans `kv`.
const CLAIM: &str = "zen_react";
/// Essais quand le nœud Duniter est injoignable (avant tout envoi).
const MAX_RETRIES: u32 = 5;
/// Pause entre deux essais.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Plafonds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZenLimits {
    /// Ẑen au plus par réaction.
    pub per_reaction: u64,
    /// Ẑen au plus par donateur et par période de 24 h.
    pub per_day: u64,
}

impl Default for ZenLimits {
    fn default() -> Self {
        Self { per_reaction: 100, per_day: 500 }
    }
}

/// Issue du traitement d'une réaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Pas un paiement (autre kind, contenu sans `+N`, donateur hébergé ailleurs) : rien à faire.
    NotAPayment,
    /// Déjà traitée.
    Duplicate,
    /// Virement effectué.
    Paid {
        /// Ẑen envoyés.
        zen: u64,
        /// Adresse Ğ1 du destinataire.
        to: String,
    },
    /// Pas de paiement, avec la raison (dite au donateur).
    Refused(String),
    /// Nœud injoignable AVANT l'envoi : à réessayer.
    Retry(String),
    /// Issue inconnue APRÈS l'envoi : jamais rejoué, à vérifier par le solde.
    Unknown(String),
}

/// Montant d'une réaction `+N` : un entier strictement positif de 1 à 6 chiffres.
pub fn parse_amount(content: &str) -> Option<u64> {
    let digits = content.trim().strip_prefix('+')?;
    (!digits.is_empty() && digits.len() <= 6 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok()).flatten().filter(|n| *n > 0)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Paiement des réactions.
pub struct ZenReactions<C: Chain, P: PrimalSource> {
    db: Arc<Db>,
    chain: Arc<C>,
    accounts_dir: PathBuf,
    primal: Option<Arc<P>>,
    master: String,
    activity: ActivityBus,
    notify: Option<Notify>,
    limits: ZenLimits,
}

impl<C: Chain, P: PrimalSource> ZenReactions<C, P> {
    /// `accounts_dir` : dossier des comptes (`~/.zen/game/nostr`), où se trouve le `.secret.dunikey` de chaque MULTIPASS.
    /// `master` : portefeuille `UPLANETNAME_G1` (le primal attendu des destinataires).
    #[allow(clippy::too_many_arguments)]
    pub fn new(db: Arc<Db>, chain: Arc<C>, accounts_dir: PathBuf, primal: Option<Arc<P>>, master: String, activity: ActivityBus, notify: Option<Notify>, limits: ZenLimits) -> Self {
        Self { db, chain, accounts_dir, primal, master, activity, notify, limits }
    }

    async fn tell(&self, owner: &str, text: String) {
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "economy", "zen:react", text.clone(), None);
        if let Some(n) = &self.notify {
            n(owner.to_owned(), text).await;
        }
    }

    async fn label(&self, hex: &str) -> String {
        let h = hex.to_owned();
        let found: Option<String> = self
            .db
            .readers()
            .read(move |c| {
                let name = c.query_row("SELECT json_extract(content, '$.name') FROM nostr_events WHERE pubkey = ?1 AND kind = 0 ORDER BY created_at DESC LIMIT 1", [&h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
                let email = c.query_row("SELECT email FROM identities WHERE hex = ?1", [&h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
                Ok(name.filter(|n| !n.trim().is_empty()).or_else(|| email.map(|e| e.split('@').next().unwrap_or_default().to_owned())))
            })
            .await
            .ok()
            .flatten();
        found.filter(|l| !l.is_empty()).unwrap_or_else(|| hex.chars().take(8).collect())
    }

    /// Le donateur : `(email, portefeuille Ğ1)` s'il est hébergé ici.
    async fn donor(&self, hex: &str) -> Option<(String, String)> {
        let h = hex.to_owned();
        self.db
            .readers()
            .read(move |c| {
                Ok(c.query_row("SELECT email, g1pub_nostr FROM identities WHERE hex = ?1 AND hosted = 1 AND role IN ('multipass', 'captain') AND email IS NOT NULL AND g1pub_nostr IS NOT NULL", [&h], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).optional()?)
            })
            .await
            .ok()
            .flatten()
    }

    /// Auteur réagi : le dernier tag `p` (NIP-25), sinon l'auteur de l'événement du tag `e`.
    async fn author(&self, e: &Event) -> Option<String> {
        if let Some(p) = e.tags.iter().rev().find(|t| t.first().map(String::as_str) == Some("p")).and_then(|t| t.get(1)).filter(|p| astro_nostr_proto::is_hex64(p)) {
            return Some(p.clone());
        }
        let target = e.tags.iter().rev().find(|t| t.first().map(String::as_str) == Some("e")).and_then(|t| t.get(1))?.clone();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT pubkey FROM nostr_events WHERE id = ?1", [&target], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten()
    }

    async fn claim(&self, id: &str) -> bool {
        self.db.writer().execute("INSERT OR IGNORE INTO claimed(owner, event_id, at) VALUES (?1, ?2, ?3)", vec![CLAIM.to_owned().into(), id.to_owned().into(), now().into()]).await.is_ok_and(|r| r.changes > 0)
    }

    async fn release(&self, id: &str) {
        let _ = self.db.writer().execute("DELETE FROM claimed WHERE owner = ?1 AND event_id = ?2", vec![CLAIM.to_owned().into(), id.to_owned().into()]).await;
    }

    async fn paid_last_day(&self, donor: &str) -> u64 {
        let (d, since) = (donor.to_owned(), now() - 86_400);
        self.db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT COALESCE(SUM(json_extract(data, '$.zen')), 0) FROM journal WHERE stream = 'zen_react' AND at > ?1 AND json_extract(data, '$.donor') = ?2", rusqlite::params![since, d], |r| r.get::<_, i64>(0))?))
            .await
            .map_or(0, |n| u64::try_from(n).unwrap_or(0))
    }

    /// Traite une réaction. Idempotent.
    pub async fn handle(&self, e: &Event) -> Outcome {
        let Some(zen) = (e.kind == 7).then(|| parse_amount(&e.content)).flatten() else { return Outcome::NotAPayment };
        // Un donateur hébergé ailleurs est payé par sa propre station.
        let Some((email, from_wallet)) = self.donor(&e.pubkey).await else { return Outcome::NotAPayment };
        if !self.claim(&e.id).await {
            return Outcome::Duplicate;
        }
        match self.pay(e, zen, &email, &from_wallet).await {
            Outcome::Retry(why) => {
                self.release(&e.id).await; // avant tout envoi : on peut réessayer proprement
                Outcome::Retry(why)
            }
            Outcome::Refused(why) => {
                self.tell(&e.pubkey, format!("❌ Votre réaction +{zen} n'a pas envoyé de Ẑen : {why}.")).await;
                Outcome::Refused(why)
            }
            other => other,
        }
    }

    async fn pay(&self, e: &Event, zen: u64, email: &str, from_wallet: &str) -> Outcome {
        let refuse = |why: &str| Outcome::Refused(why.to_owned());
        if zen > self.limits.per_reaction {
            return refuse(&format!("au plus {} Ẑen par réaction", self.limits.per_reaction));
        }
        let Some(author) = self.author(e).await else { return refuse("destinataire introuvable (ni tag p, ni événement réagi connu)") };
        if author == e.pubkey {
            return refuse("on ne se paie pas soi-même");
        }
        let a = author.clone();
        let wallet: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT g1pub_nostr FROM identities WHERE hex = ?1 AND g1pub_nostr IS NOT NULL", [&a], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten();
        let label = self.label(&author).await;
        let Some(to) = wallet.as_deref().and_then(|w| Account::from_str(w).ok()) else { return refuse(&format!("{label} n'a pas de MULTIPASS Ğ1 connu de cette constellation")) };
        // Primal identique : le portefeuille du destinataire doit venir de cette UPlanet.
        let Some(primal) = &self.primal else { return refuse("origine du portefeuille non vérifiable : aucun indexeur Duniter joint") };
        match primal.primal_of(&to.ss58()).await {
            Ok(Some(p)) if p == self.master => {}
            Ok(Some(_)) => return refuse(&format!("le portefeuille de {label} ne vient pas de cette UPlanet (primal différent)")),
            Ok(None) => return refuse(&format!("le portefeuille de {label} n'a encore rien reçu : primal inconnu")),
            Err(why) => return Outcome::Retry(format!("indexeur : {why}")),
        }
        let used = self.paid_last_day(&e.pubkey).await;
        if used + zen > self.limits.per_day {
            return refuse(&format!("plafond de {} Ẑen par jour atteint ({used} déjà envoyés)", self.limits.per_day));
        }
        // La clé Ğ1 du donateur : celle de son MULTIPASS, vérifiée contre son portefeuille connu.
        let key = std::fs::read_to_string(self.accounts_dir.join(email).join(".secret.dunikey")).ok().and_then(|t| Dunikey::parse(&t).ok());
        let Some(key) = key else { return refuse("clé Ğ1 de votre MULTIPASS introuvable sur cette station") };
        let signer = Ed25519Signer::from_seed(*key.ed25519_seed());
        if Account::from_str(from_wallet).ok() != Some(signer.account()) {
            return refuse("la clé Ğ1 du compte ne correspond pas à son portefeuille");
        }
        let g1_cents = zen * 100 / ZEN_PER_G1;
        let balance = match self.chain.balance(&signer.account()).await {
            Ok(b) => b,
            Err(ChainError::Connection(why)) => return Outcome::Retry(format!("nœud Duniter : {why}")),
            Err(other) => return refuse(&format!("solde illisible ({other})")),
        };
        let spendable = balance.total.0.saturating_sub(RESERVE_G1_CENTS);
        if spendable < g1_cents {
            return refuse(&format!("solde insuffisant ({} Ẑen disponibles)", spendable * ZEN_PER_G1 / 100));
        }
        // Point de non-retour : on note l'envoi AVANT de signer. Une reprise après un arrêt brutal ne rejoue jamais.
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES ('zen_react', ?1, 'sending', ?2)", vec![e.id.clone().into(), now().into()]).await;
        let memo = format!("UPLANET:{}:ZEN:{}", &self.master[..self.master.len().min(8)], &e.id[..16]);
        match self.chain.transfer(&signer, &to, Amount(g1_cents), TransferMode::KeepAlive, Some(&memo)).await {
            Ok(out) => {
                let _ = self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES ('zen_react', ?1, 'paid', ?2)", vec![e.id.clone().into(), now().into()]).await;
                let record = json!({ "donor": e.pubkey, "author": author, "event": e.id, "zen": zen, "g1_cents": g1_cents, "block": hex::encode(out.block_hash) });
                let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('zen_react', ?1, ?2)", vec![now().into(), record.to_string().into()]).await;
                self.tell(&e.pubkey, format!("💸 {zen} Ẑen envoyés à {label} ({}.{:02} Ğ1).", g1_cents / 100, g1_cents % 100)).await;
                Outcome::Paid { zen, to: to.ss58() }
            }
            // Perdu AVANT l'envoi (nonce, connexion) : réessayer est sans danger.
            Err(ChainError::Connection(why)) => {
                let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = 'zen_react' AND key = ?1", vec![e.id.clone().into()]).await;
                Outcome::Retry(format!("nœud Duniter : {why}"))
            }
            Err(ChainError::UnknownOutcome(why)) => {
                self.activity.publish_to(Audience::Captain, "alert", "zen:react", format!("virement d'une réaction {} : issue inconnue ({why}) — vérifier le solde du donateur avant de rejouer", &e.id[..8]), None);
                self.tell(&e.pubkey, format!("⚠️ Votre {zen} Ẑen à {label} : l'issue du virement est incertaine, le Capitaine vérifie. Ne renvoyez pas.")).await;
                Outcome::Unknown(why)
            }
            Err(other) => {
                let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = 'zen_react' AND key = ?1", vec![e.id.clone().into()]).await;
                refuse(&other.to_string())
            }
        }
    }

    /// Consomme le flux des événements acceptés par le relai : chaque réaction `+N` d'un donateur hébergé ici est payée,
    /// hors du chemin d'écriture, avec jusqu'à [`MAX_RETRIES`] nouveaux essais si le nœud Duniter est injoignable.
    pub async fn run(self: Arc<Self>, mut live: broadcast::Receiver<Arc<Event>>, mut stop: watch::Receiver<bool>) {
        let mut retry: VecDeque<(Arc<Event>, u32, tokio::time::Instant)> = VecDeque::new();
        loop {
            let due = retry.front().map(|(_, _, at)| *at);
            tokio::select! {
                got = live.recv() => match got {
                    Ok(e) => self.process(e, 0, &mut retry).await,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        self.activity.publish_to(Audience::Captain, "alert", "zen:react", format!("{n} réaction(s) du relai non vues (trop d'événements d'un coup) : leurs paiements éventuels n'ont pas été traités"), None);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                () = async { match due { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {
                    if let Some((e, tries, _)) = retry.pop_front() {
                        self.process(e, tries, &mut retry).await;
                    }
                }
                _ = stop.changed() => return,
            }
        }
    }

    async fn process(&self, e: Arc<Event>, tries: u32, retry: &mut VecDeque<(Arc<Event>, u32, tokio::time::Instant)>) {
        if let Outcome::Retry(why) = self.handle(&e).await {
            if tries + 1 >= MAX_RETRIES {
                self.tell(&e.pubkey, format!("❌ Votre réaction +{} n'a pas pu être payée ({why}). Renvoyez-la plus tard.", parse_amount(&e.content).unwrap_or(0))).await;
            } else {
                retry.push_back((e, tries + 1, tokio::time::Instant::now() + RETRY_AFTER));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use astro_duniter::{Balance, TransferEvent, TxOutcome};
    use astro_nostr_proto::{UnsignedEvent, pubkey_of};

    use super::*;

    const ALICE: [u8; 32] = [0xA1; 32]; // donatrice, MULTIPASS hébergé ici
    const BOB: [u8; 32] = [0xB2; 32]; // destinataire
    const REMOTE: [u8; 32] = [0xC3; 32]; // donateur hébergé ailleurs

    fn hex(s: &[u8; 32]) -> String {
        pubkey_of(s).unwrap()
    }

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

    struct Primals(HashMap<String, Result<Option<String>, String>>);

    impl PrimalSource for Primals {
        async fn primal_of(&self, ss58: &str) -> Result<Option<String>, String> {
            self.0.get(ss58).cloned().unwrap_or(Ok(None))
        }
    }

    struct Rig {
        dir: PathBuf,
        db: Arc<Db>,
        node: Arc<Node>,
        bus: ActivityBus,
        told: Arc<Mutex<Vec<String>>>,
        zen: ZenReactions<Node, Primals>,
        bob_wallet: Account,
    }

    async fn rig(tag: &str, bob_primal: Result<Option<String>, String>) -> Rig {
        let dir = std::env::temp_dir().join(format!("astroportd-zenreact-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("accounts/alice@example.org")).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        // Le portefeuille d'Alice est celui de sa clé Ğ1 ; celui de Bob est un compte quelconque.
        let alice_signer = Ed25519Signer::from_seed([0x11; 32]);
        std::fs::write(dir.join("accounts/alice@example.org/.secret.dunikey"), astro_lifecycle::dunikey::render(&[0x11; 32])).unwrap();
        let bob_wallet = Account([0x22; 32]);
        for (hex, email, wallet, hosted) in [(hex(&ALICE), "alice@example.org", alice_signer.account().ss58(), 1), (hex(&BOB), "bob@example.org", bob_wallet.ss58(), 0), (hex(&REMOTE), "remote@example.org", Account([0x33; 32]).ss58(), 0)] {
            db.writer().execute("INSERT INTO identities(hex, role, email, g1pub_nostr, hosted) VALUES (?1, 'multipass', ?2, ?3, ?4)", vec![hex.into(), email.to_owned().into(), wallet.into(), i64::from(hosted).into()]).await.unwrap();
        }
        let node = Arc::new(Node { total: Mutex::new(100 + 2_000), sent: Mutex::new(Vec::new()), fail: Mutex::new(None) }); // 1 Ğ1 de réserve + 20 Ğ1
        let master = Account([0x01; 32]).ss58();
        let primal = Primals(HashMap::from([(bob_wallet.ss58(), bob_primal)]));
        let bus = ActivityBus::new();
        let told = Arc::new(Mutex::new(Vec::new()));
        let log = told.clone();
        let notify: Notify = Arc::new(move |_, text| {
            let log = log.clone();
            Box::pin(async move { log.lock().unwrap().push(text) })
        });
        let zen = ZenReactions::new(db.clone(), node.clone(), dir.join("accounts"), Some(Arc::new(primal)), master, bus.clone(), Some(notify), ZenLimits { per_reaction: 100, per_day: 150 });
        Rig { dir, db, node, bus, told, zen, bob_wallet }
    }

    fn like(from: &[u8; 32], content: &str, about: &[u8; 32], at: u64) -> Event {
        UnsignedEvent { created_at: at, kind: 7, tags: vec![vec!["e".into(), "ab".repeat(32)], vec!["p".into(), hex(about)]], content: content.into() }.sign(from).unwrap()
    }

    fn master_of(r: &Rig) -> String {
        r.zen.master.clone()
    }

    #[test]
    fn only_plus_n_is_a_payment() {
        assert_eq!((parse_amount("+5"), parse_amount(" +120 "), parse_amount("+")), (Some(5), Some(120), None));
        for bad in ["5", "+0", "-3", "+1.5", "+abc", "❤️", "+", "", "+1234567", "++2"] {
            assert_eq!(parse_amount(bad), None, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_plus_n_reaction_pays_the_author_once_from_the_donors_wallet_and_tells_the_donor() {
        let r = rig("ok", Ok(None)).await;
        // Le primal du destinataire est celui de la constellation.
        let master = master_of(&r);
        let zen = ZenReactions::new(r.db.clone(), r.node.clone(), r.dir.join("accounts"), Some(Arc::new(Primals(HashMap::from([(r.bob_wallet.ss58(), Ok(Some(master.clone())))])))), master, r.bus.clone(), None, ZenLimits::default());
        let e = like(&ALICE, "+5", &BOB, 1_800_000_000);
        assert_eq!(zen.handle(&e).await, Outcome::Paid { zen: 5, to: r.bob_wallet.ss58() });
        assert_eq!(zen.handle(&e).await, Outcome::Duplicate, "idempotent : jamais deux paiements pour la même réaction");
        let sent = r.node.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].0, sent[0].1), (r.bob_wallet, 50), "5 Ẑen = 0,5 Ğ1 = 50 centimes");
        assert!(sent[0].2.starts_with("UPLANET:") && sent[0].2.contains(&e.id[..16]), "{}", sent[0].2);
        let mine = r.bus.recent(10).into_iter().filter(|a| a.audience == Audience::Owner(hex(&ALICE)) && a.message.contains("5 Ẑen")).count();
        assert_eq!(mine, 1);
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn nothing_is_paid_when_the_primal_differs_is_unknown_or_the_indexer_is_missing_and_the_donor_is_told_why() {
        for (tag, primal, expect) in [("foreign", Ok(Some(Account([0x99; 32]).ss58())), "primal différent"), ("empty", Ok(None), "primal inconnu")] {
            let r = rig(tag, primal).await;
            let out = r.zen.handle(&like(&ALICE, "+5", &BOB, 1_800_000_000)).await;
            assert!(matches!(&out, Outcome::Refused(why) if why.contains(expect)), "{out:?}");
            assert!(r.node.sent.lock().unwrap().is_empty());
            assert!(r.told.lock().unwrap()[0].contains(expect), "{:?}", r.told.lock().unwrap());
            let _ = std::fs::remove_dir_all(&r.dir);
        }
        let r = rig("noidx", Ok(None)).await;
        let zen: ZenReactions<Node, Primals> = ZenReactions::new(r.db.clone(), r.node.clone(), r.dir.join("accounts"), None, master_of(&r), r.bus.clone(), None, ZenLimits::default());
        assert!(matches!(zen.handle(&like(&ALICE, "+5", &BOB, 1_800_000_001)).await, Outcome::Refused(why) if why.contains("indexeur")));
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn limits_balance_self_payment_unknown_recipients_and_remote_donors_are_handled() {
        let r = rig("limits", Ok(None)).await;
        let master = master_of(&r);
        let zen = ZenReactions::new(r.db.clone(), r.node.clone(), r.dir.join("accounts"), Some(Arc::new(Primals(HashMap::from([(r.bob_wallet.ss58(), Ok(Some(master.clone())))])))), master, r.bus.clone(), None, ZenLimits { per_reaction: 100, per_day: 150 });
        // Hors sujet : une réaction ordinaire, un autre kind, un donateur hébergé ailleurs.
        assert_eq!(zen.handle(&like(&ALICE, "+", &BOB, 10)).await, Outcome::NotAPayment);
        assert_eq!(zen.handle(&like(&REMOTE, "+5", &BOB, 11)).await, Outcome::NotAPayment, "payé par SA station");
        // Plafonds.
        assert!(matches!(zen.handle(&like(&ALICE, "+101", &BOB, 12)).await, Outcome::Refused(w) if w.contains("par réaction")));
        assert!(matches!(zen.handle(&like(&ALICE, "+100", &BOB, 13)).await, Outcome::Paid { .. }));
        assert!(matches!(zen.handle(&like(&ALICE, "+60", &BOB, 14)).await, Outcome::Refused(w) if w.contains("plafond")), "100 + 60 > 150 par jour");
        assert!(matches!(zen.handle(&like(&ALICE, "+50", &BOB, 15)).await, Outcome::Paid { .. }), "100 + 50 = 150 : exactement le plafond");
        // Soi-même et destinataire sans portefeuille.
        assert!(matches!(zen.handle(&like(&ALICE, "+1", &ALICE, 16)).await, Outcome::Refused(w) if w.contains("soi-même")));
        assert!(matches!(zen.handle(&like(&ALICE, "+1", &[0x77; 32], 17)).await, Outcome::Refused(w) if w.contains("MULTIPASS")));
        // Solde insuffisant (hors réserve de 1 Ğ1).
        *r.node.total.lock().unwrap() = 100 + 5;
        let broke = ZenReactions::new(r.db.clone(), r.node.clone(), r.dir.join("accounts"), Some(Arc::new(Primals(HashMap::from([(r.bob_wallet.ss58(), Ok(Some(master_of(&r))))])))), master_of(&r), r.bus.clone(), None, ZenLimits { per_reaction: 100, per_day: 10_000 });
        assert!(matches!(broke.handle(&like(&ALICE, "+10", &BOB, 18)).await, Outcome::Refused(w) if w.contains("solde insuffisant")));
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[tokio::test]
    async fn a_lost_connection_before_sending_is_retried_but_an_unknown_outcome_after_sending_never_is() {
        let r = rig("chain", Ok(None)).await;
        let master = master_of(&r);
        let zen = ZenReactions::new(r.db.clone(), r.node.clone(), r.dir.join("accounts"), Some(Arc::new(Primals(HashMap::from([(r.bob_wallet.ss58(), Ok(Some(master.clone())))])))), master, r.bus.clone(), None, ZenLimits::default());
        let e = like(&ALICE, "+5", &BOB, 1_800_000_000);
        *r.node.fail.lock().unwrap() = Some(ChainError::Connection("nœud muet".into()));
        assert!(matches!(zen.handle(&e).await, Outcome::Retry(_)));
        *r.node.fail.lock().unwrap() = None;
        assert!(matches!(zen.handle(&e).await, Outcome::Paid { .. }), "le même événement est rejoué proprement après un échec avant envoi");
        // Issue inconnue : jamais rejouée, le Capitaine est alerté.
        let e2 = like(&ALICE, "+6", &BOB, 1_800_000_001);
        *r.node.fail.lock().unwrap() = Some(ChainError::UnknownOutcome("connexion perdue après envoi".into()));
        assert!(matches!(zen.handle(&e2).await, Outcome::Unknown(_)));
        *r.node.fail.lock().unwrap() = None;
        assert_eq!(zen.handle(&e2).await, Outcome::Duplicate, "jamais deux virements");
        assert_eq!(r.node.sent.lock().unwrap().len(), 1);
        let alerts: Vec<String> = r.bus.recent(20).into_iter().filter(|a| a.audience == Audience::Captain).map(|a| a.message.clone()).collect();
        assert!(alerts.iter().any(|m| m.contains("issue inconnue")), "{alerts:?}");
        let _ = std::fs::remove_dir_all(&r.dir);
    }
}
