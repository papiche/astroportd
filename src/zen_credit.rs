//! Mouvements de Ẑen entre les portefeuilles de la coopérative et les MULTIPASS hébergés ici : le `UPLANET.official.sh` du legacy.
//!
//! Un Ẑen n'est jamais créé : il est adossé à de la Ğ1 réellement détenue (1 Ğ1 = 10 Ẑen, comme partout dans la station). Chaque
//! mouvement est un **plan** de virements, exécuté pas à pas et rejouable sans doublon.
//!
//! | genre | qui | plan |
//! |---|---|---|
//! | `usage` | Capitaine | maître → `UPLANETNAME` → MULTIPASS (le `-l` : la primale du MULTIPASS reste le maître) |
//! | `satellite`, `constellation` | Capitaine, OpenCollective | maître → `SOCIETY` → MULTIPASS 33 % · R&D 33 % · ASSETS 33 % · Capitaine (le reste), **sans** étape par la ZenCard |
//! | `labo` | OpenCollective | maître → R&D (dons fléchés) |
//! | `cash` | tout membre, pour lui-même | MULTIPASS → TREASURY (liquidité apportée à la coopérative) |
//! | `member` | tout membre | MULTIPASS → MULTIPASS d'un proche réciproque hébergé ici |
//!
//! **Garde-fous** : montant plafonné à [`MAX_ZEN_PER_CREDIT`] ẑ ; l'écran montre d'abord le devis (`preview`) puis exige une
//! confirmation explicite (`send` avec `confirm`) ; un seul mouvement à la fois ; refusé si le portefeuille débité ne couvre pas
//! le montant. Les références des virements du Capitaine gardent la forme legacy (`UPLANET:<8 car.>:ZENCOIN:<email>`) ; celles des
//! membres ne portent **que des préfixes de clés** : la chaîne est publique, l'adresse e-mail d'un membre n'a rien à y faire.
//!
//! **Reprise** : l'état est écrit avant chaque virement (`kv`, portée `zen_credit`) : `run|…|i` (le virement `i` est parti, issue
//! inconnue), `ok|…|i` (les virements avant `i` sont faits), `done`. Rejouer le même identifiant termine un plan à moitié fait
//! sans jamais payer deux fois ; une issue **inconnue** n'est jamais rejouée : il faut vérifier les soldes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_duniter::{Account, Amount, Chain, ChainError, Ed25519Signer, TransferMode};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::billet::BilletError;
use crate::coop_wallets::Wallets;
use crate::station::{RESERVE_G1_CENTS, zen_of_g1};

const SCOPE: &str = "zen_credit";
/// Plafond d'un mouvement, en ẑ entiers (même borne que `MAX_ZEN_PER_BILLET` et `MAX_WEEKLY_ZEN`).
pub const MAX_ZEN_PER_CREDIT: u64 = 10_000;
const MAX_ZEN_CENTS: u64 = MAX_ZEN_PER_CREDIT * 100;
/// Un centime de Ğ1 vaut 10 centimes de Ẑen.
const ZEN_CENTS_PER_G1_CENT: u64 = crate::station::ZEN_PER_G1;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Genre de mouvement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Crédit d'usage d'un MULTIPASS.
    Usage,
    /// Part sociale « satellite » (infrastructure sans IA).
    Satellite,
    /// Part sociale « constellation » (infrastructure avec IA).
    Constellation,
    /// Don fléché vers le portefeuille R&D.
    Labo,
    /// Un membre apporte de la liquidité à la trésorerie.
    Cash,
    /// Un membre envoie à un proche.
    Member,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Usage => "usage",
            Kind::Satellite => "satellite",
            Kind::Constellation => "constellation",
            Kind::Labo => "labo",
            Kind::Cash => "cash",
            Kind::Member => "member",
        }
    }

    /// Genre d'après son nom.
    pub fn parse(s: &str) -> Option<Self> {
        [Kind::Usage, Kind::Satellite, Kind::Constellation, Kind::Labo, Kind::Cash, Kind::Member].into_iter().find(|k| k.tag() == s)
    }

    fn society(self) -> Option<&'static str> {
        matches!(self, Kind::Satellite | Kind::Constellation).then(|| self.tag())
    }
}

/// Portefeuille qui signe un virement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Payer {
    Master,
    Usage,
    Society,
    Member,
}

struct Step {
    from: Payer,
    to: Account,
    g1: u64,
    memo: String,
}

/// Un MULTIPASS hébergé ici.
#[derive(Clone)]
struct Party {
    hex: String,
    email: String,
    account: Account,
}

struct Quote {
    kind: Kind,
    to: Option<Party>,
    zc: u64,
    g1: u64,
    steps: Vec<Step>,
    split: Vec<(&'static str, u64)>,
    payer_total: u64,
    /// Le MULTIPASS destinataire est vide : le maître lui avance d'abord son 1 Ğ1 d'activation (sa transaction primale).
    activation: bool,
}

/// Le service de mouvements.
pub struct ZenCredit<C: Chain> {
    db: Arc<Db>,
    chain: Arc<C>,
    wallets: Arc<Wallets>,
    accounts_dir: PathBuf,
    activity: ActivityBus,
    gate: tokio::sync::Mutex<()>,
    retry_delay: Duration,
}

type R<T> = Result<T, BilletError>;

impl<C: Chain> ZenCredit<C> {
    /// `accounts_dir` : dossier des comptes (`~/.zen/game/nostr`), où se trouve le `.secret.dunikey` de chaque MULTIPASS.
    pub fn new(db: Arc<Db>, chain: Arc<C>, wallets: Arc<Wallets>, accounts_dir: PathBuf, activity: ActivityBus) -> Self {
        Self { db, chain, wallets, accounts_dir, activity, gate: tokio::sync::Mutex::new(()), retry_delay: Duration::from_secs(15) }
    }

    #[cfg(test)]
    fn with_retry_delay(mut self, d: Duration) -> Self {
        self.retry_delay = d;
        self
    }

    // ── Points d'entrée de l'interface ──────────────────────────────────────────────────────────────

    /// Opérations du **Capitaine** : `recipients`, `preview`, `send` (genres `usage`, `satellite`, `constellation`).
    pub async fn handle(&self, op: &str, body: &Value) -> R<Value> {
        if !body["_who"]["captain"].as_bool().unwrap_or(false) {
            return Err(BilletError::Refused("zen_credit_not_captain", "réservé au Capitaine".into()));
        }
        let actor = body["_who"]["hex"].as_str().unwrap_or("");
        if op == "recipients" {
            return self.recipients(actor).await;
        }
        let kind = Kind::parse(body.get("kind").and_then(Value::as_str).unwrap_or("usage")).filter(|k| matches!(k, Kind::Usage | Kind::Satellite | Kind::Constellation)).ok_or_else(|| BilletError::Invalid("zen_credit_bad_kind", "genre de versement inconnu".into()))?;
        let to = Self::hex_of(body)?;
        let zc = Self::zen_cents_of(body)?;
        match op {
            "preview" => {
                let to = self.party(&to).await?;
                Ok(self.view(&self.quote(kind, Some(to), None, zc, true).await?))
            }
            "send" => self.send(actor, body, kind, Some(to.as_str()), None, zc).await,
            _ => Err(BilletError::Invalid("zen_credit_bad_op", "opération inconnue".into())),
        }
    }

    /// Opérations d'un **membre** sur son propre portefeuille : `recipients` (la trésorerie et ses proches réciproques
    /// hébergés ici), `preview`, `send`. `to` = `cash` ou la clé d'un proche.
    pub async fn handle_member(&self, op: &str, body: &Value) -> R<Value> {
        let (hex, email) = (body["_who"]["hex"].as_str().unwrap_or(""), body["_who"]["email"].as_str().unwrap_or(""));
        let me = self.party(hex).await.map_err(|_| BilletError::Refused("zen_credit_no_wallet", "votre MULTIPASS n'est pas hébergé sur cette station".into()))?;
        if !me.email.eq_ignore_ascii_case(email) {
            return Err(BilletError::Refused("zen_credit_no_wallet", "compte inconnu".into()));
        }
        if op == "recipients" {
            let rows = self.friends(hex).await?;
            let mut list = vec![json!({ "to": "cash", "label": "cash" })];
            list.extend(rows.into_iter().map(|(h, e)| json!({ "to": h, "label": e })));
            return Ok(json!({ "recipients": list, "max_zen": MAX_ZEN_PER_CREDIT }));
        }
        let zc = Self::zen_cents_of(body)?;
        let target = body.get("to").and_then(Value::as_str).unwrap_or("");
        let (kind, to) = if target == "cash" {
            (Kind::Cash, None)
        } else {
            let h = Self::hex_of(body)?;
            if !self.friends(hex).await?.iter().any(|(f, _)| *f == h) {
                return Err(BilletError::Refused("zen_credit_not_friend", "ce destinataire n'est pas un proche réciproque hébergé ici".into()));
            }
            (Kind::Member, Some(self.party(&h).await?))
        };
        match op {
            "preview" => Ok(self.view(&self.quote(kind, to, Some(me), zc, true).await?)),
            "send" => {
                let to_hex = to.as_ref().map(|p| p.hex.clone());
                self.send(hex, body, kind, to_hex.as_deref(), Some(me), zc).await
            }
            _ => Err(BilletError::Invalid("zen_credit_bad_op", "opération inconnue".into())),
        }
    }

    /// Crédite un don (OpenCollective) : `kind` et `email` du donateur, `zen_cents` à verser. `id` rend l'appel rejouable.
    pub async fn credit_donation(&self, id: &str, kind: Kind, email: &str, zen_cents: u64) -> R<Value> {
        let to = match kind {
            Kind::Labo => None,
            Kind::Usage | Kind::Satellite | Kind::Constellation => Some(self.party_by_email(email).await?),
            _ => return Err(BilletError::Invalid("zen_credit_bad_kind", "genre de don inconnu".into())),
        };
        let to_hex = to.as_ref().map(|p| p.hex.clone());
        self.run("oc", id, kind, to_hex.as_deref(), None, zen_cents, email).await
    }

    /// Le versement `id` demandé par `actor` est-il terminé ?
    pub async fn is_done(&self, actor: &str, id: &str) -> bool {
        self.state(&format!("{}:{id}", actor.chars().take(16).collect::<String>())).await.is_some_and(|s| s.starts_with("done|"))
    }

    // ── Lecture ─────────────────────────────────────────────────────────────────────────────────────

    fn hex_of(body: &Value) -> R<String> {
        body.get("to").and_then(Value::as_str).filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_owned).ok_or_else(|| BilletError::Invalid("zen_credit_bad_recipient", "destinataire invalide".into()))
    }

    /// Montant en centimes de Ẑen, d'après `amount_zen` (ẑ entiers).
    fn zen_cents_of(body: &Value) -> R<u64> {
        let zen = body.get("amount_zen").and_then(Value::as_u64).ok_or_else(|| BilletError::Invalid("zen_credit_bad_amount", "montant en ẑ entiers attendu".into()))?;
        if zen == 0 || zen > MAX_ZEN_PER_CREDIT {
            return Err(BilletError::Invalid("zen_credit_bad_amount", format!("de 1 à {MAX_ZEN_PER_CREDIT} ẑ par versement (plusieurs versements au-delà)")));
        }
        Ok(zen * 100)
    }

    async fn read<T: Send + 'static>(&self, f: impl FnOnce(&rusqlite::Connection) -> Result<T, astro_db::DbError> + Send + 'static) -> R<T> {
        self.db.readers().read(f).await.map_err(|e| BilletError::Failed("zen_credit_internal", format!("base illisible : {e}")))
    }

    /// MULTIPASS hébergés ici, à qui le Capitaine peut verser (le sien en premier).
    async fn recipients(&self, my_hex: &str) -> R<Value> {
        let me = my_hex.to_owned();
        let rows: Vec<(String, String)> = self
            .read(move |c| {
                let mut stmt = c.prepare("SELECT hex, email FROM identities WHERE hosted = 1 AND role = 'multipass' AND email IS NOT NULL ORDER BY (hex = ?1) DESC, lower(email)")?;
                Ok(stmt.query_map([&me], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await?;
        let list: Vec<Value> = rows.into_iter().map(|(hex, email)| json!({ "hex": hex, "email": email, "me": hex == my_hex })).collect();
        Ok(json!({ "recipients": list, "max_zen": MAX_ZEN_PER_CREDIT, "kinds": ["usage", "satellite", "constellation"] }))
    }

    /// Proches **réciproques** (ils se suivent mutuellement) d'un MULTIPASS, hébergés ici : `(clé, email)`.
    async fn friends(&self, hex: &str) -> R<Vec<(String, String)>> {
        let me = hex.to_owned();
        self.read(move |c| {
            let mut stmt = c.prepare(
                "SELECT i.hex, i.email FROM identities i \
                 WHERE i.hosted = 1 AND i.role = 'multipass' AND i.email IS NOT NULL AND i.hex <> ?1 \
                   AND EXISTS (SELECT 1 FROM social_graph g WHERE g.owner_hex = ?1 AND g.pubkey = i.hex AND g.level = 1) \
                   AND EXISTS (SELECT 1 FROM social_graph g WHERE g.owner_hex = i.hex AND g.pubkey = ?1 AND g.level = 1) \
                 ORDER BY lower(i.email)",
            )?;
            Ok(stmt.query_map([&me], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// Portefeuille Ğ1 (adresse seulement) d'un MULTIPASS hébergé : sa clé est dans le dossier du compte.
    fn signer_of_email(&self, email: &str) -> R<Ed25519Signer> {
        let text = std::fs::read_to_string(self.accounts_dir.join(email).join(".secret.dunikey")).map_err(|_| BilletError::Unavailable("zen_credit_no_wallet", "portefeuille Ğ1 du compte introuvable sur cette station".into()))?;
        let dk = astro_lifecycle::dunikey::Dunikey::parse(&text).map_err(|_| BilletError::Unavailable("zen_credit_no_wallet", "portefeuille Ğ1 du compte illisible".into()))?;
        Ok(Ed25519Signer::from_seed(*dk.ed25519_seed()))
    }

    async fn party(&self, hex: &str) -> R<Party> {
        let h = hex.to_owned();
        let email: String = self
            .read(move |c| Ok(c.query_row("SELECT email FROM identities WHERE hex = ?1 AND hosted = 1 AND role = 'multipass' AND email IS NOT NULL", [&h], |r| r.get::<_, String>(0)).optional()?))
            .await?
            .ok_or_else(|| BilletError::Refused("zen_credit_unknown_recipient", "ce MULTIPASS n'est pas hébergé sur cette station".into()))?;
        let account = self.signer_of_email(&email)?.account();
        Ok(Party { hex: hex.to_owned(), email, account })
    }

    async fn party_by_email(&self, email: &str) -> R<Party> {
        let e = email.trim().to_ascii_lowercase();
        let hex: String = self
            .read(move |c| Ok(c.query_row("SELECT hex FROM identities WHERE lower(email) = ?1 AND hosted = 1 AND role = 'multipass'", [&e], |r| r.get::<_, String>(0)).optional()?))
            .await?
            .ok_or_else(|| BilletError::Refused("zen_credit_unknown_recipient", format!("aucun MULTIPASS hébergé ici pour {email}")))?;
        self.party(&hex).await
    }

    async fn balance_of(&self, a: &Account) -> R<astro_duniter::Balance> {
        self.chain.balance(a).await.map_err(|e| BilletError::Failed("zen_credit_chain_error", format!("solde illisible : {e}")))
    }

    // ── Devis ───────────────────────────────────────────────────────────────────────────────────────

    /// Construit le plan. `fresh` : vérifie aussi les soldes (pas à la reprise d'un plan déjà entamé).
    async fn quote(&self, kind: Kind, to: Option<Party>, from: Option<Party>, zc: u64, fresh: bool) -> R<Quote> {
        self.quote_for(kind, to, from, zc, fresh, "", None).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn quote_for(&self, kind: Kind, to: Option<Party>, from: Option<Party>, zc: u64, fresh: bool, donor: &str, forced_activation: Option<bool>) -> R<Quote> {
        if !(ZEN_CENTS_PER_G1_CENT..=MAX_ZEN_CENTS).contains(&zc) {
            return Err(BilletError::Invalid("zen_credit_bad_amount", format!("de 0,10 à {MAX_ZEN_PER_CREDIT} ẑ par versement")));
        }
        let g1 = zc / ZEN_CENTS_PER_G1_CENT;
        let unavailable = |what: &str, e: String| BilletError::Unavailable("zen_credit_unavailable", format!("{what} indisponible : {e}"));
        let usage = self.wallets.signer_of("UPLANETNAME").await.map_err(|e| unavailable("portefeuille UPLANETNAME", e))?.account();
        let p8: String = usage.ss58().chars().take(8).collect();
        let coop = if kind == Kind::Usage || kind == Kind::Member { None } else { Some(self.wallets.coop().await.map_err(|e| unavailable("portefeuilles de la coopérative", e))?) };
        let email = to.as_ref().map_or_else(|| donor.to_owned(), |p| p.email.clone());
        let mut steps = Vec::new();
        let mut split = Vec::new();
        let mut empty_check: Option<(Account, u64)> = None;
        let master = self.wallets.master().await.map_err(|e| unavailable("portefeuille maître", e))?.account();
        let payer = match kind {
            Kind::Cash | Kind::Member => from.as_ref().map(|p| p.account).ok_or_else(|| BilletError::Refused("zen_credit_no_wallet", "compte inconnu".into()))?,
            _ => master,
        };
        match kind {
            Kind::Usage => {
                let t = to.clone().ok_or_else(|| BilletError::Invalid("zen_credit_bad_recipient", "destinataire invalide".into()))?;
                let memo = format!("UPLANET:{p8}:ZENCOIN:{email}");
                steps.push(Step { from: Payer::Master, to: usage, g1, memo: memo.clone() });
                steps.push(Step { from: Payer::Usage, to: t.account, g1, memo });
                empty_check = Some((t.account, g1));
            }
            Kind::Satellite | Kind::Constellation => {
                let (ty, t, coop) = (kind.society().unwrap_or("satellite"), to.clone().ok_or_else(|| BilletError::Invalid("zen_credit_bad_recipient", "destinataire invalide".into()))?, coop.as_ref().ok_or_else(|| unavailable("portefeuilles de la coopérative", "absents".into()))?);
                let society = self.wallets.signer_of("SOCIETY").await.map_err(|e| unavailable("portefeuille SOCIETY", e))?.account();
                // 33 % + 33 % + 33 %, le reste (1 % et les arrondis) revient au Capitaine, comme dans le legacy.
                let part = g1 * 33 / 100;
                steps.push(Step { from: Payer::Master, to: society, g1, memo: format!("UPLANET:{p8}:SOCIETY:{email}:{ty}") });
                let legs = [("MULTIPASS", t.account, part, format!("UPLANET:{p8}:ZENCOIN:{email}")), ("R&D", coop.rnd, part, format!("UPLANET:{p8}:RnD:{email}:{ty}")), ("ASSETS", coop.assets, part, format!("UPLANET:{p8}:ASSETS:{email}:{ty}")), ("Capitaine", coop.captain, g1 - 3 * part, format!("UPLANET:{p8}:CAPTAIN:{email}:{ty}"))];
                for (label, dest, amount, memo) in legs {
                    if amount > 0 {
                        steps.push(Step { from: Payer::Society, to: dest, g1: amount, memo });
                        split.push((label, amount * ZEN_CENTS_PER_G1_CENT));
                    }
                }
                empty_check = Some((t.account, part));
            }
            Kind::Labo => {
                let coop = coop.as_ref().ok_or_else(|| unavailable("portefeuilles de la coopérative", "absents".into()))?;
                steps.push(Step { from: Payer::Master, to: coop.rnd, g1, memo: format!("UPLANET:{p8}:RnD:OC-DON:{email}") });
            }
            Kind::Cash => {
                let (coop, f) = (coop.as_ref().ok_or_else(|| unavailable("portefeuilles de la coopérative", "absents".into()))?, from.as_ref().ok_or_else(|| BilletError::Refused("zen_credit_no_wallet", "compte inconnu".into()))?);
                steps.push(Step { from: Payer::Member, to: coop.treasury, g1, memo: format!("UPLANET:{p8}:CASH:{}", &f.hex[..8]) });
            }
            Kind::Member => {
                let (t, f) = (to.clone().ok_or_else(|| BilletError::Invalid("zen_credit_bad_recipient", "destinataire invalide".into()))?, from.as_ref().ok_or_else(|| BilletError::Refused("zen_credit_no_wallet", "compte inconnu".into()))?);
                if t.hex == f.hex {
                    return Err(BilletError::Invalid("zen_credit_self", "ce serait un virement vers vous-même".into()));
                }
                steps.push(Step { from: Payer::Member, to: t.account, g1, memo: format!("UPLANET:{p8}:TRANSFER:{}:{}", &f.hex[..8], &t.hex[..8]) });
                empty_check = Some((t.account, g1));
            }
        }
        // Un MULTIPASS encore vide reçoit d'abord son 1 Ğ1 d'activation DU MAÎTRE : c'est sa transaction primale, celle que la
        // vigilance des portefeuilles exige. Passer par UPLANETNAME ou SOCIETY d'abord ferait de lui un intrus.
        let activation = match forced_activation {
            Some(a) => a,
            None => fresh && matches!(kind, Kind::Usage | Kind::Satellite | Kind::Constellation) && match &to {
                Some(t) => self.balance_of(&t.account).await?.total.0 == 0,
                None => false,
            },
        };
        if activation {
            if let Some(t) = &to {
                steps.insert(0, Step { from: Payer::Master, to: t.account, g1: RESERVE_G1_CENTS, memo: "UPLANET:PRIMAL:MULTIPASS".into() });
                empty_check = None;
            }
        }
        let need = g1 + if activation { RESERVE_G1_CENTS } else { 0 };
        let payer_total = if fresh {
            let bal = self.balance_of(&payer).await?;
            let spendable = bal.spendable(Amount(RESERVE_G1_CENTS)).0;
            if need > spendable {
                return Err(if matches!(kind, Kind::Cash | Kind::Member) {
                    BilletError::Refused("zen_credit_poor", "votre solde ne couvre pas ce montant (1 Ğ1 reste toujours sur votre portefeuille)".into())
                } else {
                    BilletError::Refused("zen_credit_master_poor", format!("le portefeuille maître ne couvre pas ce montant ({} Ğ1 de plus qu'il n'en a de disponibles) : alimentez-le d'abord", fmt_g1(need - spendable)))
                });
            }
            // Un compte vide ne peut pas recevoir moins que le dépôt existentiel.
            if let Some((acc, amount)) = empty_check.filter(|(_, a)| *a < RESERVE_G1_CENTS) {
                if self.balance_of(&acc).await?.total.0 < RESERVE_G1_CENTS {
                    return Err(BilletError::Invalid("zen_credit_too_small", format!("ce portefeuille est vide : il faut au moins {} ẑ pour l'activer (ici {})", RESERVE_G1_CENTS * ZEN_CENTS_PER_G1_CENT / 100, amount * ZEN_CENTS_PER_G1_CENT / 100)));
                }
            }
            bal.total.0
        } else {
            0
        };
        Ok(Quote { kind, to, zc, g1, steps, split, payer_total, activation })
    }

    fn view(&self, q: &Quote) -> Value {
        let after = q.payer_total.saturating_sub(q.g1 + if q.activation { RESERVE_G1_CENTS } else { 0 });
        json!({
            "kind": q.kind.tag(), "payer": if matches!(q.kind, Kind::Cash | Kind::Member) { "member" } else { "master" },
            "to": q.to.as_ref().map(|p| p.hex.clone()), "email": q.to.as_ref().map(|p| p.email.clone()), "address": q.to.as_ref().map(|p| p.account.ss58()),
            "amount_zen_cents": q.zc, "g1_cents": q.g1 + if q.activation { RESERVE_G1_CENTS } else { 0 }, "activation_g1_cents": if q.activation { RESERVE_G1_CENTS } else { 0 },
            "payer_g1_cents": q.payer_total, "payer_after_g1_cents": after, "payer_after_zen_cents": zen_of_g1(after),
            "split": q.split.iter().map(|(l, c)| json!({ "label": l, "zen_cents": c })).collect::<Vec<_>>(),
            "max_zen": MAX_ZEN_PER_CREDIT,
        })
    }

    // ── Exécution ───────────────────────────────────────────────────────────────────────────────────

    async fn state(&self, key: &str) -> Option<String> {
        let key = key.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, key], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten()
    }

    async fn set_state(&self, key: &str, value: &str) {
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![SCOPE.to_owned().into(), key.to_owned().into(), value.to_owned().into(), now().into()]).await;
    }

    async fn clear_state(&self, key: &str) {
        let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SCOPE.to_owned().into(), key.to_owned().into()]).await;
    }

    async fn audit(&self, record: Value) {
        let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('zen_credit', ?1, ?2)", vec![now().into(), record.to_string().into()]).await;
    }

    /// Virement avec nouvelle tentative sur « Invalid Transaction (1010) » (nonce pas encore vu de tous les nœuds).
    async fn transfer_retry(&self, from: &Ed25519Signer, to: &Account, amount: u64, memo: &str) -> Result<(), ChainError> {
        let mut last = None;
        for attempt in 0..4 {
            if attempt > 0 {
                tokio::time::sleep(self.retry_delay).await;
            }
            match self.chain.transfer(from, to, Amount(amount), TransferMode::KeepAlive, Some(memo)).await {
                Err(ChainError::Rejected(m)) if m.contains("1010") => last = Some(ChainError::Rejected(m)),
                Ok(_) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| ChainError::Rejected("virement refusé".into())))
    }

    async fn send(&self, actor: &str, body: &Value, kind: Kind, to: Option<&str>, from: Option<Party>, zc: u64) -> R<Value> {
        if body.get("confirm").and_then(Value::as_bool) != Some(true) {
            return Err(BilletError::Invalid("zen_credit_unconfirmed", "versement non confirmé".into()));
        }
        let id = body.get("credit_id").and_then(Value::as_str).filter(|s| (8..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')).ok_or_else(|| BilletError::Invalid("zen_credit_bad_id", "identifiant de versement invalide".into()))?;
        self.run(actor, id, kind, to, from, zc, "").await
    }

    /// Exécute (ou reprend) un plan. `actor` : qui le demande (clé du compte, ou `oc`) ; il fait partie de la clé d'état.
    #[allow(clippy::too_many_arguments)]
    async fn run(&self, actor: &str, id: &str, kind: Kind, to: Option<&str>, from: Option<Party>, zc: u64, donor: &str) -> R<Value> {
        let _guard = self.gate.try_lock().map_err(|_| BilletError::Limited("zen_credit_busy", "un versement est déjà en cours : réessayez dans un instant".into()))?;
        let key = format!("{}:{id}", actor.chars().take(16).collect::<String>());
        let base = format!("{}|{}|{zc}", kind.tag(), to.unwrap_or("-"));
        let mut forced = None;
        let mut done_steps = 0usize;
        let mut fresh = true;
        if let Some(state) = self.state(&key).await {
            let v: Vec<&str> = state.splitn(6, '|').collect();
            match v.as_slice() {
                ["done", ..] => return Ok(json!({ "ok": true, "already": true })),
                ["run", ..] => return Err(BilletError::Refused("zen_credit_unknown_outcome", "l'issue d'un virement est inconnue : vérifiez les soldes des portefeuilles concernés dans « Ma UPlanet » avant de recommencer, avec un nouveau versement".into())),
                ["ok", k, t, z, a, i] => {
                    if format!("{k}|{t}|{z}") != base {
                        return Err(BilletError::Refused("zen_credit_id_reused", "cet identifiant de versement appartient à une autre demande".into()));
                    }
                    done_steps = i.parse().unwrap_or(0);
                    fresh = done_steps == 0;
                    forced = Some(*a == "1");
                }
                _ => {}
            }
        }
        let recipient = match to {
            Some(h) => Some(self.party(h).await?),
            None => None,
        };
        let q = self.quote_for(kind, recipient, from.clone(), zc, fresh, donor, forced).await?;
        let tail = format!("{base}|{}", u8::from(q.activation));
        let label = q.to.as_ref().map_or_else(|| donor.to_owned(), |p| p.email.clone());
        for (idx, step) in q.steps.iter().enumerate().skip(done_steps) {
            // L'état est écrit AVANT chaque virement, irréversible.
            self.set_state(&key, &format!("run|{tail}|{idx}")).await;
            let signer = match step.from {
                Payer::Master => self.wallets.master().await.map_err(|e| BilletError::Unavailable("zen_credit_unavailable", format!("portefeuille maître indisponible : {e}")))?,
                Payer::Usage => self.wallets.signer_of("UPLANETNAME").await.map_err(|e| BilletError::Unavailable("zen_credit_unavailable", format!("portefeuille UPLANETNAME indisponible : {e}")))?,
                Payer::Society => self.wallets.signer_of("SOCIETY").await.map_err(|e| BilletError::Unavailable("zen_credit_unavailable", format!("portefeuille SOCIETY indisponible : {e}")))?,
                Payer::Member => Arc::new(self.signer_of_email(&from.as_ref().map(|p| p.email.clone()).unwrap_or_default())?),
            };
            match self.transfer_retry(&signer, &step.to, step.g1, &step.memo).await {
                Ok(()) => self.set_state(&key, &format!("ok|{tail}|{}", idx + 1)).await,
                Err(e) => return Err(self.step_failed(&key, &tail, idx, &e, &label, q.zc).await),
            }
        }
        self.set_state(&key, &format!("done|{tail}|{}", q.steps.len())).await;
        self.audit(json!({ "id": id, "actor": actor, "kind": kind.tag(), "to": to, "label": label, "zen_cents": q.zc, "g1_cents": q.g1, "resumed": done_steps > 0 })).await;
        // Le Capitaine est prévenu de ses propres versements et des dons ; les mouvements privés des membres ne sont pas diffusés.
        if !matches!(kind, Kind::Cash | Kind::Member) {
            self.activity.publish_to(Audience::Captain, "economy", "zen_credit", format!("{} ẑ ({}) : {label}", fmt_g1(q.zc), kind.tag()), None);
        }
        Ok(json!({ "ok": true, "already": false, "kind": kind.tag(), "to": to, "email": q.to.as_ref().map(|p| p.email.clone()), "amount_zen_cents": q.zc, "g1_cents": q.g1 }))
    }

    /// Échec d'un virement : refus net de la chaîne ou nœud injoignable = ce virement n'est pas parti ; issue inconnue = l'état
    /// `run` reste, on ne rejoue jamais.
    async fn step_failed(&self, key: &str, tail: &str, idx: usize, e: &ChainError, label: &str, zc: u64) -> BilletError {
        if matches!(e, ChainError::UnknownOutcome(_)) {
            self.activity.publish_to(Audience::Captain, "alert", "zen_credit", format!("Versement {key} : issue inconnue du virement {idx}, vérifiez les soldes ({e})"), None);
            return BilletError::Failed("zen_credit_unknown_outcome", format!("issue inconnue d'un virement : vérifiez les soldes avant de recommencer ({e})"));
        }
        if idx == 0 {
            self.clear_state(key).await;
            return BilletError::Failed("zen_credit_failed", format!("virement refusé, rien n'a été envoyé : {e}"));
        }
        self.set_state(key, &format!("ok|{tail}|{idx}")).await;
        self.activity.publish_to(Audience::Captain, "alert", "zen_credit", format!("Versement de {} ẑ ({label}) à moitié fait : relancez le même versement ({e})", fmt_g1(zc)), None);
        BilletError::Failed("zen_credit_second_leg", format!("le versement s'est arrêté après {idx} virement(s) ({e}) : relancez le même versement, il reprendra là où il s'est arrêté"))
    }
}

/// Centimes en texte « 12,50 ».
fn fmt_g1(cents: u64) -> String {
    format!("{},{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use astro_duniter::{Balance, TransferEvent, TxOutcome};

    use super::*;

    const UP: &str = "UPlanetTestName";
    const CAPTAIN: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    pub(crate) const MEMBER: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const OTHER: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    pub(crate) struct Ledger {
        pub(crate) balances: Mutex<HashMap<Account, u64>>,
        pub(crate) sent: Mutex<Vec<(Account, Account, u64, String)>>,
        /// Refuse les virements issus de ce compte.
        fail_from: Mutex<Option<Account>>,
        /// Les virements issus de ce compte ont une issue inconnue.
        unknown_from: Mutex<Option<Account>>,
    }

    impl Chain for Ledger {
        async fn balance(&self, a: &Account) -> Result<Balance, ChainError> {
            let t = self.balances.lock().unwrap().get(a).copied().unwrap_or(0);
            Ok(Balance { total: Amount(t), transferable: Amount(t), unclaimed_uds: Amount(0) })
        }
        async fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, _: TransferMode, memo: Option<&str>) -> Result<TxOutcome, ChainError> {
            if *self.fail_from.lock().unwrap() == Some(from.account()) {
                return Err(ChainError::Rejected("nœud hors service".into()));
            }
            if *self.unknown_from.lock().unwrap() == Some(from.account()) {
                return Err(ChainError::UnknownOutcome("délai".into()));
            }
            let mut b = self.balances.lock().unwrap();
            *b.entry(from.account()).or_default() -= amount.0;
            *b.entry(*to).or_default() += amount.0;
            self.sent.lock().unwrap().push((from.account(), *to, amount.0, memo.unwrap_or("").to_owned()));
            Ok(TxOutcome { block_hash: [9; 32], transferred: amount })
        }
        async fn watch_transfers(&self, _: tokio::sync::mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
            std::future::pending().await
        }
        fn existential_deposit(&self) -> Amount {
            Amount(100)
        }
    }

    pub(crate) struct Rig {
        pub(crate) dir: PathBuf,
        pub(crate) db: Arc<Db>,
        pub(crate) chain: Arc<Ledger>,
        pub(crate) master: Account,
        pub(crate) usage: Account,
        pub(crate) society: Account,
        pub(crate) member: Account,
        pub(crate) captain: Account,
        pub(crate) coop: Arc<crate::coop_wallets::Coop>,
        pub(crate) svc: Arc<ZenCredit<Ledger>>,
    }

    pub(crate) async fn rig(tag: &str, master_g1: u64) -> Rig {
        let dir = std::env::temp_dir().join(format!("astroportd-zencredit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (email, seed) in [("capitaine@exemple.org", 0x61u8), ("membre@exemple.org", 0x62), ("autre@exemple.org", 0x63)] {
            std::fs::create_dir_all(dir.join("nostr").join(email)).unwrap();
            std::fs::write(dir.join("nostr").join(email).join(".secret.dunikey"), astro_lifecycle::dunikey::render(&[seed; 32])).unwrap();
        }
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let (member, captain, other) = (Ed25519Signer::from_seed([0x62; 32]).account(), Ed25519Signer::from_seed([0x61; 32]).account(), Ed25519Signer::from_seed([0x63; 32]).account());
        for (hex, email) in [(CAPTAIN, "capitaine@exemple.org"), (MEMBER, "membre@exemple.org"), (OTHER, "autre@exemple.org")] {
            db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex.to_owned().into(), email.to_owned().into()]).await.unwrap();
        }
        // De quoi lire les portefeuilles coopératifs : le NODE et le MULTIPASS du Capitaine.
        db.writer().execute("INSERT INTO identities(hex, role, hosted, g1pub) VALUES (?1, 'node', 1, ?2)", vec!["11".repeat(32).into(), Account([1; 32]).ss58().into()]).await.unwrap();
        db.writer().execute("INSERT INTO identities(hex, role, hosted, g1pub_nostr) VALUES (?1, 'captain', 1, ?2)", vec!["22".repeat(32).into(), captain.ss58().into()]).await.unwrap();
        let wallets = Arc::new(Wallets::new(UP.into(), db.clone()));
        let master = wallets.master().await.unwrap().account();
        let usage = wallets.signer_of("UPLANETNAME").await.unwrap().account();
        let society = wallets.signer_of("SOCIETY").await.unwrap().account();
        let coop = wallets.coop().await.unwrap();
        let chain = Arc::new(Ledger { balances: Mutex::new(HashMap::from([(master, master_g1), (usage, 100), (society, 100), (member, 1_100), (other, 100), (captain, 100)])), sent: Mutex::new(Vec::new()), fail_from: Mutex::new(None), unknown_from: Mutex::new(None) });
        let svc = Arc::new(ZenCredit::new(db.clone(), chain.clone(), wallets, dir.join("nostr"), ActivityBus::new()).with_retry_delay(Duration::ZERO));
        Rig { dir, db, chain, master, usage, society, member, captain, coop, svc }
    }

    fn body(captain: bool, extra: Value) -> Value {
        let mut b = extra;
        b["_who"] = json!({ "hex": CAPTAIN, "email": "capitaine@exemple.org", "captain": captain });
        b
    }

    fn send_body(id: &str, zen: u64) -> Value {
        body(true, json!({ "to": MEMBER, "amount_zen": zen, "credit_id": id, "confirm": true }))
    }

    fn member_body(extra: Value) -> Value {
        let mut b = extra;
        b["_who"] = json!({ "hex": MEMBER, "email": "membre@exemple.org", "captain": false });
        b
    }

    async fn befriend(r: &Rig, a: &str, b: &str) {
        for (o, p) in [(a, b), (b, a)] {
            r.db.writer().execute("INSERT OR REPLACE INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![o.to_owned().into(), p.to_owned().into()]).await.unwrap();
        }
    }

    #[tokio::test]
    async fn only_the_captain_can_use_the_captain_operations() {
        let r = rig("captain", 10_000).await;
        for op in ["recipients", "preview", "send"] {
            assert!(matches!(r.svc.handle(op, &body(false, json!({ "to": MEMBER, "amount_zen": 10 }))).await, Err(BilletError::Refused("zen_credit_not_captain", _))), "{op}");
        }
        assert!(r.chain.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn the_preview_shows_the_debit_and_what_is_left_without_sending_anything() {
        let r = rig("preview", 10_000).await;
        let v = r.svc.handle("preview", &body(true, json!({ "to": MEMBER, "amount_zen": 50 }))).await.unwrap();
        assert_eq!((v["g1_cents"].as_u64(), v["payer_after_g1_cents"].as_u64(), v["payer_after_zen_cents"].as_u64()), (Some(500), Some(9_500), Some(94_000)), "50 ẑ = 5 Ğ1 ; reste 95 Ğ1 = 940 ẑ (94 000 centimes) hors réserve de 1 Ğ1");
        assert_eq!((v["email"].as_str(), v["amount_zen_cents"].as_u64(), v["payer"].as_str()), (Some("membre@exemple.org"), Some(5_000), Some("master")));
        assert!(r.chain.sent.lock().unwrap().is_empty(), "un devis ne bouge rien");
        let list = r.svc.handle("recipients", &body(true, json!({}))).await.unwrap();
        assert_eq!(list["recipients"][0]["hex"], CAPTAIN, "le MULTIPASS du Capitaine d'abord");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_usage_credit_goes_master_then_usage_wallet_then_the_multipass_with_the_zencoin_reference() {
        let r = rig("send", 10_000).await;
        let out = r.svc.handle("send", &send_body("credit-0001", 50)).await.unwrap();
        assert_eq!((out["ok"].as_bool(), out["amount_zen_cents"].as_u64()), (Some(true), Some(5_000)));
        let sent = r.chain.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert_eq!((sent[0].0, sent[0].1, sent[0].2), (r.master, r.usage, 500));
        assert_eq!((sent[1].0, sent[1].1, sent[1].2), (r.usage, r.member, 500));
        assert!(sent.iter().all(|t| t.3.contains(":ZENCOIN:membre@exemple.org") && t.3.starts_with("UPLANET:")), "{sent:?}");
        let bal = r.chain.balances.lock().unwrap().clone();
        assert_eq!((bal[&r.master], bal[&r.usage], bal[&r.member]), (9_500, 100, 1_600), "UPLANETNAME ne garde rien au passage");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_share_skips_the_zencard_and_splits_a_third_each_with_the_rest_to_the_captain() {
        let r = rig("society", 10_000).await;
        let mut b = send_body("credit-0010", 100);
        b["kind"] = json!("constellation");
        r.svc.handle("send", &b).await.unwrap();
        let sent = r.chain.sent.lock().unwrap().clone();
        let legs: Vec<(Account, Account, u64)> = sent.iter().map(|t| (t.0, t.1, t.2)).collect();
        assert_eq!(legs, [(r.master, r.society, 1_000), (r.society, r.member, 330), (r.society, r.coop.rnd, 330), (r.society, r.coop.assets, 330), (r.society, r.captain, 10)], "maître → SOCIETY, puis 33 % MULTIPASS, R&D, ASSETS, et le reste au Capitaine");
        assert!(sent[0].3.contains(":SOCIETY:membre@exemple.org:constellation") && sent[2].3.contains(":RnD:") && sent[3].3.contains(":ASSETS:"), "{sent:?}");
        let bal = r.chain.balances.lock().unwrap().clone();
        assert_eq!(bal[&r.society], 100, "SOCIETY ne garde rien au passage");
        let p = r.svc.handle("preview", &{ let mut b = body(true, json!({ "to": MEMBER, "amount_zen": 100 })); b["kind"] = json!("satellite"); b }).await.unwrap();
        assert_eq!(p["split"].as_array().unwrap().len(), 4);
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn replaying_a_credit_never_pays_twice() {
        let r = rig("replay", 10_000).await;
        r.svc.handle("send", &send_body("credit-0002", 20)).await.unwrap();
        let again = r.svc.handle("send", &send_body("credit-0002", 20)).await.unwrap();
        assert_eq!(again["already"], true);
        assert_eq!(r.chain.sent.lock().unwrap().len(), 2, "deux virements en tout, pas quatre");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn the_cap_the_confirmation_and_the_master_balance_are_enforced() {
        let r = rig("limits", 10_000).await;
        for zen in [0, MAX_ZEN_PER_CREDIT + 1] {
            assert!(matches!(r.svc.handle("send", &send_body("credit-0003", zen)).await, Err(BilletError::Invalid("zen_credit_bad_amount", _))), "{zen}");
        }
        let mut unconfirmed = send_body("credit-0003", 10);
        unconfirmed["confirm"] = json!(false);
        assert!(matches!(r.svc.handle("send", &unconfirmed).await, Err(BilletError::Invalid("zen_credit_unconfirmed", _))));
        assert!(matches!(r.svc.handle("send", &send_body("x", 10)).await, Err(BilletError::Invalid("zen_credit_bad_id", _))));
        // 100 Ğ1 au maître, 1 Ğ1 de réserve : 990 ẑ au plus.
        assert!(matches!(r.svc.handle("send", &send_body("credit-0003", 1_000)).await, Err(BilletError::Refused("zen_credit_master_poor", _))));
        assert!(r.svc.handle("send", &send_body("credit-0004", 990)).await.is_ok());
        assert!(matches!(r.svc.handle("preview", &body(true, json!({ "to": "ee".repeat(32).replace("ee", "ab"), "amount_zen": 10 }))).await, Err(BilletError::Refused("zen_credit_unknown_recipient", _))));
        let mut bad = send_body("credit-0005", 10);
        bad["kind"] = json!("cash");
        assert!(matches!(r.svc.handle("send", &bad).await, Err(BilletError::Invalid("zen_credit_bad_kind", _))), "le Capitaine n'a pas la main sur les mouvements des membres");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_second_leg_failure_is_resumed_by_replaying_the_same_credit() {
        let r = rig("resume", 10_000).await;
        *r.chain.fail_from.lock().unwrap() = Some(r.usage);
        let err = r.svc.handle("send", &send_body("credit-0006", 30)).await.unwrap_err();
        assert!(matches!(err, BilletError::Failed("zen_credit_second_leg", _)), "{err}");
        assert_eq!(r.chain.balances.lock().unwrap()[&r.usage], 400, "les 3 Ğ1 attendent sur UPLANETNAME");
        *r.chain.fail_from.lock().unwrap() = None;
        r.svc.handle("send", &send_body("credit-0006", 30)).await.unwrap();
        let bal = r.chain.balances.lock().unwrap().clone();
        assert_eq!((bal[&r.master], bal[&r.usage], bal[&r.member]), (9_700, 100, 1_400), "le maître n'est débité qu'une fois");
        assert_eq!(r.chain.sent.lock().unwrap().len(), 2);
        // Une autre demande avec le même identifiant n'est pas une reprise.
        let mut other = send_body("credit-0006", 31);
        other["amount_zen"] = json!(31);
        assert_eq!(r.svc.handle("send", &other).await.unwrap()["already"], true, "terminé : rien de plus");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_refused_first_leg_can_be_retried_but_an_unknown_one_is_never_replayed() {
        let r = rig("first", 10_000).await;
        *r.chain.fail_from.lock().unwrap() = Some(r.master);
        assert!(matches!(r.svc.handle("send", &send_body("credit-0007", 10)).await, Err(BilletError::Failed("zen_credit_failed", _))));
        *r.chain.fail_from.lock().unwrap() = None;
        assert!(r.svc.handle("send", &send_body("credit-0007", 10)).await.is_ok(), "refus net : rien n'était parti, on recommence");
        *r.chain.unknown_from.lock().unwrap() = Some(r.master);
        assert!(matches!(r.svc.handle("send", &send_body("credit-0008", 10)).await, Err(BilletError::Failed("zen_credit_unknown_outcome", _))));
        *r.chain.unknown_from.lock().unwrap() = None;
        assert!(matches!(r.svc.handle("send", &send_body("credit-0008", 10)).await, Err(BilletError::Refused("zen_credit_unknown_outcome", _))), "issue inconnue : jamais rejoué");
        assert_eq!(r.chain.sent.lock().unwrap().len(), 2, "un seul versement abouti (2 virements)");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn an_empty_multipass_is_first_activated_by_the_master_so_its_primal_stays_the_master() {
        let r = rig("empty", 10_000).await;
        r.chain.balances.lock().unwrap().insert(r.member, 0);
        let v = r.svc.handle("preview", &body(true, json!({ "to": MEMBER, "amount_zen": 5 }))).await.unwrap();
        assert_eq!((v["activation_g1_cents"].as_u64(), v["g1_cents"].as_u64(), v["payer_after_g1_cents"].as_u64()), (Some(100), Some(150), Some(9_850)), "5 ẑ = 0,5 Ğ1, plus 1 Ğ1 d'activation");
        r.svc.handle("send", &send_body("credit-0020", 5)).await.unwrap();
        let sent = r.chain.sent.lock().unwrap().clone();
        let legs: Vec<(Account, Account, u64)> = sent.iter().map(|t| (t.0, t.1, t.2)).collect();
        assert_eq!(legs, [(r.master, r.member, 100), (r.master, r.usage, 50), (r.usage, r.member, 50)], "la première transaction reçue par le MULTIPASS vient du maître");
        assert_eq!(sent[0].3, "UPLANET:PRIMAL:MULTIPASS");
        // Un compte déjà actif ne reçoit pas de seconde activation.
        r.svc.handle("send", &send_body("credit-0021", 5)).await.unwrap();
        assert_eq!(r.chain.sent.lock().unwrap().len(), 5);
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn an_activated_credit_that_stops_midway_resumes_without_a_second_activation() {
        let r = rig("empty-resume", 10_000).await;
        r.chain.balances.lock().unwrap().insert(r.member, 0);
        *r.chain.fail_from.lock().unwrap() = Some(r.usage);
        assert!(matches!(r.svc.handle("send", &send_body("credit-0022", 5)).await, Err(BilletError::Failed("zen_credit_second_leg", _))));
        *r.chain.fail_from.lock().unwrap() = None;
        // Le compte n'est plus vide (il a reçu l'activation) : la reprise ne doit pas changer le plan.
        r.svc.handle("send", &send_body("credit-0022", 5)).await.unwrap();
        let sent = r.chain.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 3, "activation, maître → UPLANETNAME, UPLANETNAME → MULTIPASS : chacun une seule fois");
        assert_eq!(r.chain.balances.lock().unwrap()[&r.member], 150);
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_member_brings_cash_to_the_treasury_with_a_memo_that_hides_the_email() {
        let r = rig("cash", 10_000).await;
        let ask = member_body(json!({ "to": "cash", "amount_zen": 20, "credit_id": "cash-0001", "confirm": true }));
        let p = r.svc.handle_member("preview", &ask).await.unwrap();
        assert_eq!((p["payer"].as_str(), p["payer_g1_cents"].as_u64(), p["payer_after_g1_cents"].as_u64()), (Some("member"), Some(1_100), Some(900)));
        r.svc.handle_member("send", &ask).await.unwrap();
        let sent = r.chain.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].0, sent[0].1, sent[0].2), (r.member, r.coop.treasury, 200));
        assert!(sent[0].3.contains(":CASH:dddddddd") && !sent[0].3.contains('@'), "{}", sent[0].3);
        // Trop cher pour son solde : 11 Ğ1 dont 1 de réserve = 100 ẑ au plus.
        let big = member_body(json!({ "to": "cash", "amount_zen": 101, "credit_id": "cash-0002", "confirm": true }));
        assert!(matches!(r.svc.handle_member("send", &big).await, Err(BilletError::Refused("zen_credit_poor", _))));
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_member_can_only_send_to_a_reciprocal_friend_hosted_here() {
        let r = rig("friend", 10_000).await;
        let ask = member_body(json!({ "to": OTHER, "amount_zen": 10, "credit_id": "friend-0001", "confirm": true }));
        assert!(matches!(r.svc.handle_member("send", &ask).await, Err(BilletError::Refused("zen_credit_not_friend", _))), "ni suivi ni suivant");
        r.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![MEMBER.to_owned().into(), OTHER.to_owned().into()]).await.unwrap();
        assert!(matches!(r.svc.handle_member("send", &ask).await, Err(BilletError::Refused("zen_credit_not_friend", _))), "un suivi à sens unique ne suffit pas");
        befriend(&r, MEMBER, OTHER).await;
        let list = r.svc.handle_member("recipients", &member_body(json!({}))).await.unwrap();
        assert_eq!(list["recipients"].as_array().unwrap().len(), 2, "la trésorerie et un proche");
        r.svc.handle_member("send", &ask).await.unwrap();
        let sent = r.chain.sent.lock().unwrap().clone();
        assert_eq!((sent[0].0, sent[0].1, sent[0].2), (r.member, Ed25519Signer::from_seed([0x63; 32]).account(), 100));
        assert!(!sent[0].3.contains('@'), "la chaîne est publique : jamais d'email d'un membre");
        let myself = member_body(json!({ "to": MEMBER, "amount_zen": 10, "credit_id": "friend-0002", "confirm": true }));
        assert!(r.svc.handle_member("send", &myself).await.is_err());
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn donations_are_credited_once_and_a_labo_gift_goes_straight_to_rnd() {
        let r = rig("donation", 10_000).await;
        let out = r.svc.credit_donation("oc-aaaa0001", Kind::Usage, "Membre@Exemple.org", 850).await.unwrap();
        assert_eq!(out["amount_zen_cents"], 850);
        assert_eq!(r.chain.sent.lock().unwrap().iter().map(|t| t.2).collect::<Vec<_>>(), [85, 85], "8,50 ẑ = 0,85 Ğ1");
        assert_eq!(r.svc.credit_donation("oc-aaaa0001", Kind::Usage, "membre@exemple.org", 850).await.unwrap()["already"], true);
        r.svc.credit_donation("oc-aaaa0002", Kind::Labo, "donateur@exemple.org", 5_000).await.unwrap();
        let last = r.chain.sent.lock().unwrap().last().cloned().unwrap();
        assert_eq!((last.0, last.1, last.2), (r.master, r.coop.rnd, 500));
        assert!(last.3.contains(":RnD:OC-DON:donateur@exemple.org"));
        assert!(matches!(r.svc.credit_donation("oc-aaaa0003", Kind::Usage, "inconnu@exemple.org", 1_000).await, Err(BilletError::Refused("zen_credit_unknown_recipient", _))));
        let _ = std::fs::remove_dir_all(r.dir);
    }
}
