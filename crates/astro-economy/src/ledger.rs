//! Registre Ğ1-N² (kind 30852) : validation intégrale, indépendante de l'ordre d'arrivée.
//!
//! Chaque événement est un virement `auteur → p` de `amount` ẐEN. Règles (toutes appliquées ici, pas seulement
//! structurellement comme dans le projecteur) :
//!
//! 1. id et signature BIP-340 valides ; horodatage pas dans le futur (tolérance [`LedgerConfig::max_future_secs`]) ;
//! 2. forme : `d`, `p` (≠ auteur), `amount` (décimal exact, > 0, 2 décimales max), `prev`, `t=g1-n2` — chacun une seule fois ;
//! 3. chaînage : `prev` = id du dernier événement accepté de l'auteur, ou `genesis` pour le premier ;
//! 4. anti-double dépense : un `d` ne sert qu'une fois par auteur, et deux enfants du même `prev` gèlent l'auteur (fourche) ;
//! 5. pas de découvert : le solde de l'auteur couvre le montant, sauf `t=mint` d'une autorité de création monétaire.
//!
//! Les doublons exacts (même id) sont idempotents. Un événement dont le `prev` n'est pas encore connu est
//! [`LedgerError::OutOfOrder`] : [`Ledger::replay`] le retente jusqu'au point fixe, donc la convergence ne dépend
//! pas de l'ordre de réception.

use std::collections::{HashMap, HashSet};

use astro_core::{Amount, AmountError};
use astro_nostr_proto::{Event, is_hex64, kind};

/// Réglages du validateur.
#[derive(Debug, Clone)]
pub struct LedgerConfig {
    /// Clés (64 hex) autorisées à créer de la monnaie (`t=mint`) : Capitaine, clé LOVE, ZenCard coopérative…
    pub mint_authorities: HashSet<String>,
    /// Avance maximale tolérée d'un horodatage sur l'horloge locale, en secondes.
    pub max_future_secs: u64,
}

impl LedgerConfig {
    /// Configuration avec les autorités données et 10 minutes de tolérance d'horloge.
    pub fn with_mint_authorities(auth: impl IntoIterator<Item = String>) -> Self {
        Self { mint_authorities: auth.into_iter().collect(), max_future_secs: 600 }
    }
}

/// Pourquoi un événement est refusé.
#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum LedgerError {
    /// Ce n'est pas un kind 30852.
    #[error("kind {0} ≠ 30852")]
    WrongKind(u32),
    /// Identifiant ou signature invalide.
    #[error("signature ou identifiant invalide")]
    BadSignature,
    /// Tag absent, en double ou mal formé.
    #[error("forme invalide : {0}")]
    Malformed(&'static str),
    /// Montant illisible ou nul.
    #[error("montant invalide : {0}")]
    BadAmount(String),
    /// Horodatage trop loin dans le futur.
    #[error("horodatage dans le futur")]
    FromFuture,
    /// L'auteur est gelé après une fourche : plus aucun de ses événements n'est accepté.
    #[error("compte gelé (fourche détectée)")]
    Frozen,
    /// Double dépense : `d` réutilisé, ou deux enfants du même `prev`.
    #[error("double dépense : {0}")]
    DoubleSpend(&'static str),
    /// `prev` inconnu : l'événement précédent n'est pas encore arrivé (à rejouer plus tard).
    #[error("chaînage : prev inconnu")]
    OutOfOrder,
    /// `t=mint` par une clé non autorisée.
    #[error("création monétaire non autorisée")]
    NotMintAuthority,
    /// Solde insuffisant.
    #[error("découvert interdit : solde {have}, besoin {need}")]
    Overdraft {
        /// Solde de l'auteur.
        have: Amount,
        /// Montant demandé.
        need: Amount,
    },
    /// Dépassement arithmétique.
    #[error("dépassement de capacité")]
    Overflow,
}

impl From<AmountError> for LedgerError {
    fn from(_: AmountError) -> Self {
        Self::Overflow
    }
}

/// Virement extrait d'un événement valide en forme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tx {
    /// Identifiant de l'événement.
    pub id: String,
    /// Émetteur (64 hex).
    pub author: String,
    /// Destinataire (64 hex).
    pub dest: String,
    /// Montant.
    pub amount: Amount,
    /// Événement précédent de l'émetteur, `None` pour `genesis`.
    pub prev: Option<String>,
    /// Identifiant de transaction unique par émetteur.
    pub d: String,
    /// Création monétaire.
    pub is_mint: bool,
    /// Horodatage.
    pub created_at: u64,
}

impl Tx {
    /// Extrait le virement d'un événement (forme seulement : ni signature, ni solde).
    pub fn from_event(e: &Event) -> Result<Self, LedgerError> {
        if e.kind != kind::LEDGER_N2 {
            return Err(LedgerError::WrongKind(e.kind));
        }
        let one = |name: &'static str| -> Result<&str, LedgerError> {
            let mut it = e.tags.iter().filter(|t| t.first().map(String::as_str) == Some(name)).filter_map(|t| t.get(1));
            match (it.next(), it.next()) {
                (Some(v), None) => Ok(v.as_str()),
                (None, _) => Err(LedgerError::Malformed(name)),
                _ => Err(LedgerError::Malformed("tag en double")),
            }
        };
        let (d, dest, amount, prev) = (one("d")?, one("p")?, one("amount")?, one("prev")?);
        if d.is_empty() {
            return Err(LedgerError::Malformed("d vide"));
        }
        if !is_hex64(dest) || dest == e.pubkey {
            return Err(LedgerError::Malformed("destinataire invalide"));
        }
        if prev != "genesis" && !is_hex64(prev) {
            return Err(LedgerError::Malformed("prev invalide"));
        }
        let t_has = |v: &str| e.tags.iter().any(|t| t.first().map(String::as_str) == Some("t") && t.get(1).map(String::as_str) == Some(v));
        if !t_has("g1-n2") {
            return Err(LedgerError::Malformed("t=g1-n2 absent"));
        }
        let amount: Amount = amount.parse().map_err(|_| LedgerError::BadAmount(amount.to_owned()))?;
        if amount == Amount::ZERO {
            return Err(LedgerError::BadAmount("nul".into()));
        }
        Ok(Self {
            id: e.id.clone(),
            author: e.pubkey.clone(),
            dest: dest.to_owned(),
            amount,
            prev: (prev != "genesis").then(|| prev.to_owned()),
            d: d.to_owned(),
            is_mint: t_has("mint"),
            created_at: e.created_at,
        })
    }
}

/// Virement accepté et soldes résultants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Virement.
    pub tx: Tx,
    /// Solde de l'émetteur après application.
    pub author_balance: Amount,
    /// Solde du destinataire après application.
    pub dest_balance: Amount,
}

/// Issue d'un [`Ledger::apply`] réussi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nouvel événement accepté.
    Applied(Applied),
    /// Déjà accepté (idempotent).
    Duplicate,
}

/// Bilan d'un [`Ledger::replay`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReplayReport {
    /// Événements acceptés, dans l'ordre d'application.
    pub applied: Vec<String>,
    /// Doublons ignorés.
    pub duplicates: usize,
    /// Refusés définitivement (id, cause).
    pub rejected: Vec<(String, LedgerError)>,
    /// Toujours en attente d'un `prev` qui n'est jamais arrivé.
    pub orphans: Vec<String>,
}

#[derive(Default)]
struct Account {
    balance: Amount,
    head: Option<String>,
}

/// État du registre : soldes, tête de chaîne de chaque auteur, comptes gelés.
pub struct Ledger {
    cfg: LedgerConfig,
    accounts: HashMap<String, Account>,
    seen: HashSet<String>,
    used_d: HashSet<(String, String)>,
    children: HashMap<(String, Option<String>), String>,
    frozen: HashSet<String>,
}

impl Ledger {
    /// Registre vide.
    pub fn new(cfg: LedgerConfig) -> Self {
        Self { cfg, accounts: HashMap::new(), seen: HashSet::new(), used_d: HashSet::new(), children: HashMap::new(), frozen: HashSet::new() }
    }

    /// Solde d'un compte (0 s'il est inconnu).
    pub fn balance(&self, hex: &str) -> Amount {
        self.accounts.get(hex).map_or(Amount::ZERO, |a| a.balance)
    }

    /// Dernier événement accepté de l'auteur.
    pub fn head(&self, hex: &str) -> Option<&str> {
        self.accounts.get(hex).and_then(|a| a.head.as_deref())
    }

    /// Valeur à mettre dans `prev` pour le prochain événement de `hex`.
    pub fn next_prev(&self, hex: &str) -> String {
        self.head(hex).unwrap_or("genesis").to_owned()
    }

    /// Compte gelé après fourche.
    pub fn is_frozen(&self, hex: &str) -> bool {
        self.frozen.contains(hex)
    }

    /// Valide et applique un événement. `now` : horloge locale en secondes UNIX.
    pub fn apply(&mut self, e: &Event, now: u64) -> Result<Outcome, LedgerError> {
        let tx = Tx::from_event(e)?;
        e.verify().map_err(|_| LedgerError::BadSignature)?;
        if self.seen.contains(&tx.id) {
            return Ok(Outcome::Duplicate);
        }
        if tx.created_at > now.saturating_add(self.cfg.max_future_secs) {
            return Err(LedgerError::FromFuture);
        }
        if self.frozen.contains(&tx.author) {
            return Err(LedgerError::Frozen);
        }
        if self.used_d.contains(&(tx.author.clone(), tx.d.clone())) {
            self.frozen.insert(tx.author.clone());
            return Err(LedgerError::DoubleSpend("d déjà utilisé"));
        }
        let slot = (tx.author.clone(), tx.prev.clone());
        if self.children.contains_key(&slot) {
            self.frozen.insert(tx.author.clone());
            return Err(LedgerError::DoubleSpend("deux enfants du même prev"));
        }
        if self.head(&tx.author) != tx.prev.as_deref() {
            return Err(LedgerError::OutOfOrder);
        }
        if tx.is_mint {
            if !self.cfg.mint_authorities.contains(&tx.author) {
                return Err(LedgerError::NotMintAuthority);
            }
        } else {
            let have = self.balance(&tx.author);
            if have < tx.amount {
                return Err(LedgerError::Overdraft { have, need: tx.amount });
            }
        }
        let dest_new = self.balance(&tx.dest).checked_add(tx.amount)?;
        // Tous les contrôles sont passés : mutations (aucune erreur possible au-delà de ce point).
        if !tx.is_mint {
            let a = self.accounts.entry(tx.author.clone()).or_default();
            a.balance = a.balance.checked_sub(tx.amount).unwrap_or_default();
        }
        self.accounts.entry(tx.dest.clone()).or_default().balance = dest_new;
        self.accounts.entry(tx.author.clone()).or_default().head = Some(tx.id.clone());
        self.seen.insert(tx.id.clone());
        self.used_d.insert((tx.author.clone(), tx.d.clone()));
        self.children.insert(slot, tx.id.clone());
        let (author_balance, dest_balance) = (self.balance(&tx.author), self.balance(&tx.dest));
        Ok(Outcome::Applied(Applied { tx, author_balance, dest_balance }))
    }

    /// Rejoue un lot d'événements dans un ordre déterministe (`created_at`, `id`), en retentant les orphelins
    /// jusqu'au point fixe : le résultat ne dépend pas de l'ordre d'entrée.
    pub fn replay(&mut self, mut events: Vec<Event>, now: u64) -> ReplayReport {
        events.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        let mut report = ReplayReport::default();
        let mut pending = events;
        loop {
            let before = pending.len();
            let mut still = Vec::new();
            for e in pending {
                match self.apply(&e, now) {
                    Ok(Outcome::Applied(a)) => report.applied.push(a.tx.id),
                    Ok(Outcome::Duplicate) => report.duplicates += 1,
                    Err(LedgerError::OutOfOrder) => still.push(e),
                    Err(err) => report.rejected.push((e.id, err)),
                }
            }
            if still.is_empty() || still.len() == before {
                report.orphans = still.into_iter().map(|e| e.id).collect();
                return report;
            }
            pending = still;
        }
    }
}

#[cfg(test)]
mod tests {
    use astro_nostr_proto::{UnsignedEvent, pubkey_of};

    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn key(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn hex_of(n: u8) -> String {
        pubkey_of(&key(n)).unwrap()
    }

    fn ev(from: u8, to: u8, amount: &str, prev: &str, d: &str, mint: bool, at: u64) -> Event {
        let mut tags = vec![vec!["d".into(), d.into()], vec!["p".into(), hex_of(to)], vec!["amount".into(), amount.into()], vec!["prev".into(), prev.into()], vec!["t".into(), "g1-n2".into()]];
        if mint {
            tags.push(vec!["t".into(), "mint".into()]);
        }
        UnsignedEvent { created_at: at, kind: kind::LEDGER_N2, tags, content: String::new() }.sign(&key(from)).unwrap()
    }

    fn ledger() -> Ledger {
        Ledger::new(LedgerConfig::with_mint_authorities([hex_of(1)]))
    }

    fn applied(l: &mut Ledger, e: &Event) -> Applied {
        let Outcome::Applied(a) = l.apply(e, NOW).unwrap() else { unreachable!("doublon inattendu") };
        a
    }

    #[test]
    fn mint_then_transfers_chain_and_balances_follow_in_exact_centimes() {
        let mut l = ledger();
        let m = ev(1, 2, "100.50", "genesis", "m1", true, NOW - 100);
        let a = applied(&mut l, &m);
        assert_eq!((l.balance(&hex_of(1)), a.dest_balance), (Amount(0), Amount(10_050)), "le mint ne débite pas l'autorité");
        let t = ev(2, 3, "0.05", "genesis", "t1", false, NOW - 90);
        let a = applied(&mut l, &t);
        assert_eq!((a.author_balance, a.dest_balance), (Amount(10_045), Amount(5)));
        let t2 = ev(2, 3, "100.45", &t.id, "t2", false, NOW - 80);
        applied(&mut l, &t2);
        assert_eq!((l.balance(&hex_of(2)), l.balance(&hex_of(3))), (Amount(0), Amount(10_050)));
        assert_eq!(l.head(&hex_of(2)), Some(t2.id.as_str()));
        assert_eq!(l.next_prev(&hex_of(9)), "genesis");
    }

    #[test]
    fn overdraft_is_forbidden_to_the_centime_and_changes_nothing() {
        let mut l = ledger();
        applied(&mut l, &ev(1, 2, "10", "genesis", "m", true, NOW - 10));
        let e = ev(2, 3, "10.01", "genesis", "t", false, NOW - 5);
        assert_eq!(l.apply(&e, NOW), Err(LedgerError::Overdraft { have: Amount(1000), need: Amount(1001) }));
        assert_eq!((l.balance(&hex_of(2)), l.balance(&hex_of(3)), l.head(&hex_of(2))), (Amount(1000), Amount(0), None));
        applied(&mut l, &ev(2, 3, "10.00", "genesis", "t", false, NOW - 5));
    }

    #[test]
    fn only_authorities_mint_and_forged_or_tampered_events_are_refused() {
        let mut l = ledger();
        assert_eq!(l.apply(&ev(2, 3, "5", "genesis", "m", true, NOW - 1), NOW), Err(LedgerError::NotMintAuthority));
        let mut forged = ev(1, 2, "5", "genesis", "m", true, NOW - 1);
        forged.tags[2][1] = "5000".into();
        assert_eq!(l.apply(&forged, NOW), Err(LedgerError::BadSignature), "montant modifié après signature");
        let mut other_key = ev(1, 2, "5", "genesis", "m", true, NOW - 1);
        other_key.pubkey = hex_of(3);
        assert_eq!(l.apply(&other_key, NOW), Err(LedgerError::BadSignature), "auteur usurpé");
        assert_eq!(l.balance(&hex_of(2)), Amount(0));
    }

    #[test]
    fn malformed_events_are_refused_with_a_reason() {
        let mut l = ledger();
        let bad = |amount: &str| ev(1, 2, amount, "genesis", "x", true, NOW - 1);
        for a in ["0", "0.00", "-1", "1.234", "1e3", "abc", ".5", "1."] {
            assert!(matches!(l.apply(&bad(a), NOW), Err(LedgerError::BadAmount(_))), "{a}");
        }
        assert_eq!(l.apply(&ev(1, 1, "1", "genesis", "x", true, NOW - 1), NOW), Err(LedgerError::Malformed("destinataire invalide")));
        assert_eq!(l.apply(&ev(1, 2, "1", "pas-un-hash", "x", true, NOW - 1), NOW), Err(LedgerError::Malformed("prev invalide")));
        let mut no_t = ev(1, 2, "1", "genesis", "x", true, NOW - 1);
        no_t.tags.retain(|t| t[0] != "t" || t[1] != "g1-n2");
        let no_t = UnsignedEvent { created_at: no_t.created_at, kind: no_t.kind, tags: no_t.tags, content: String::new() }.sign(&key(1)).unwrap();
        assert_eq!(l.apply(&no_t, NOW), Err(LedgerError::Malformed("t=g1-n2 absent")));
        let wrong = UnsignedEvent { created_at: NOW, kind: 1, tags: vec![], content: String::new() }.sign(&key(1)).unwrap();
        assert_eq!(l.apply(&wrong, NOW), Err(LedgerError::WrongKind(1)));
        assert_eq!(l.apply(&ev(1, 2, "1", "genesis", "x", true, NOW + 10_000), NOW), Err(LedgerError::FromFuture));
    }

    #[test]
    fn double_spend_by_forking_the_chain_freezes_the_author() {
        let mut l = ledger();
        applied(&mut l, &ev(1, 2, "10", "genesis", "m", true, NOW - 50));
        let first = ev(2, 3, "10", "genesis", "a", false, NOW - 40);
        applied(&mut l, &first);
        // Même prev, destinataire différent : tentative de dépenser deux fois le même solde.
        let second = ev(2, 4, "10", "genesis", "b", false, NOW - 39);
        assert_eq!(l.apply(&second, NOW), Err(LedgerError::DoubleSpend("deux enfants du même prev")));
        assert!(l.is_frozen(&hex_of(2)));
        assert_eq!(l.apply(&ev(2, 3, "1", &first.id, "c", false, NOW - 30), NOW), Err(LedgerError::Frozen));
        assert_eq!(l.balance(&hex_of(4)), Amount(0));
        assert_eq!(l.apply(&first, NOW), Ok(Outcome::Duplicate), "le rejeu exact reste idempotent");
    }

    #[test]
    fn reusing_a_d_tag_is_a_double_spend_even_with_a_valid_chain() {
        let mut l = ledger();
        let m = ev(1, 2, "10", "genesis", "m", true, NOW - 50);
        applied(&mut l, &m);
        let again = ev(1, 3, "10", &m.id, "m", true, NOW - 40);
        assert_eq!(l.apply(&again, NOW), Err(LedgerError::DoubleSpend("d déjà utilisé")));
    }

    #[test]
    fn replay_converges_whatever_the_arrival_order_and_reports_orphans() {
        let m = ev(1, 2, "30", "genesis", "m", true, NOW - 100);
        let t1 = ev(2, 3, "10", "genesis", "t1", false, NOW - 90);
        let t2 = ev(2, 3, "10", &t1.id, "t2", false, NOW - 80);
        let t3 = ev(2, 4, "10", &t2.id, "t3", false, NOW - 70);
        let ghost = ev(5, 3, "1", &"a".repeat(64), "g", false, NOW - 60);
        let batch = vec![t3.clone(), ghost.clone(), t1.clone(), m.clone(), t2.clone(), t1.clone()];
        let mut l = ledger();
        let r = l.replay(batch.clone(), NOW);
        assert_eq!(r.applied, [m.id.clone(), t1.id.clone(), t2.id.clone(), t3.id.clone()]);
        assert_eq!((r.duplicates, r.orphans.clone()), (1, vec![ghost.id.clone()]));
        assert_eq!((l.balance(&hex_of(2)), l.balance(&hex_of(3)), l.balance(&hex_of(4))), (Amount(0), Amount(2000), Amount(1000)));
        let mut rev = batch;
        rev.reverse();
        let mut l2 = ledger();
        l2.replay(rev, NOW);
        for n in 1..=5 {
            assert_eq!(l.balance(&hex_of(n)), l2.balance(&hex_of(n)));
        }
    }

    #[test]
    fn replay_resolves_a_fork_the_same_way_in_any_order() {
        let m = ev(1, 2, "10", "genesis", "m", true, NOW - 100);
        let a = ev(2, 3, "10", "genesis", "a", false, NOW - 50);
        let b = ev(2, 4, "10", "genesis", "b", false, NOW - 40);
        for order in [vec![m.clone(), a.clone(), b.clone()], vec![b.clone(), a.clone(), m.clone()]] {
            let mut l = ledger();
            let r = l.replay(order, NOW);
            assert_eq!(r.applied, [m.id.clone(), a.id.clone()], "le plus ancien gagne");
            assert_eq!(r.rejected, vec![(b.id.clone(), LedgerError::DoubleSpend("deux enfants du même prev"))]);
            assert!(l.is_frozen(&hex_of(2)));
        }
    }
}
