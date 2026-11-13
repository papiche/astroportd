//! Ğ1Billet : vérifier la valeur d'un billet papier et la transférer vers un autre compte de l'UPlanet.
//!
//! Reprend `billet.html` / `/scan` d'UPassport. Un billet est une clé Ğ1 **jetable** dont le seed (32 octets) est découpé en 3 parts
//! de Shamir (2 suffisent, GF(2⁸), polynôme `0x11B`, abscisses 1, 2, 3 : le procédé de `billet_gen.sh` et de TrocZen, pour que
//! les billets déjà imprimés restent encaissables). P1 est imprimée en clair, P2 sous le pli, P3 n'est pas imprimée.
//!
//! **Circuit d'encaissement** (inchangé) : le billet est vidé vers `UPLANETNAME_G1`, qui renvoie `solde − 1 Ğ1` au destinataire.
//! Ce « − 1 Ğ1 » est le dépôt d'activation que `UPLANETNAME_G1` avait avancé au billet : il le récupère, le porteur reçoit la valeur
//! annoncée. Avant tout virement, deux contrôles : le billet est bien **de cette UPlanet** (son primal est `UPLANETNAME_G1`) et
//! le destinataire l'est aussi (règle primale unique à tous les portefeuilles). Sans indexeur pour lire les primals, on refuse :
//! ne jamais sortir du circuit coopératif par prudence.
//!
//! **Reprise** : l'état de chaque billet est écrit **avant** chaque virement. Si le second virement échoue après le vidage, les
//! fonds sont sur `UPLANETNAME_G1` et la ligne reste « à terminer » : la station la reprend seule, et le Capitaine est prévenu.
//! Une issue inconnue du vidage n'est jamais rejouée.
//!
//! Le billet est un titre au porteur : l'encaissement n'exige pas de compte, mais il est limité en débit par adresse. Les parts
//! ne sont jamais journalisées.
//!
//! **Validité** : un billet peut porter une date de fin, écrite dans un événement signé par la clé de la coopérative
//! (kind 31903, `d = billet:<adresse>`, tag `u` = date de fin en secondes Unix : seules les étiquettes d'une lettre sont indexées). Passée cette date, le porteur ne l'encaisse plus.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_duniter::{Account, Amount, Chain, ChainError, Ed25519Signer, TransferMode, TxOutcome};
use astro_nostr_proto::UnsignedEvent;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::coop_wallets::Wallets;
use crate::funds::PrimalSource;
use crate::register::PublishFn;
use crate::station::{RESERVE_G1_CENTS, zen_of_g1};

const SCOPE: &str = "billet";
const RETRY_EVERY: Duration = Duration::from_secs(5 * 60);
/// Requêtes de lecture par adresse et par minute ; encaissements par adresse et par dix minutes.
/// Valeur maximale d'un billet, en ẑ.
pub const MAX_ZEN_PER_BILLET: u64 = 10_000;
const READS_PER_MIN: usize = 40;
const REDEEMS_PER_10_MIN: usize = 6;

/// Refus d'une demande, rangé par nature pour que l'interface réponde juste.
#[derive(Debug, thiserror::Error)]
pub enum BilletError {
    /// Entrée mal formée.
    #[error("{1}")]
    Invalid(&'static str, String),
    /// Refusé : billet d'une autre UPlanet, expiré, vide, destinataire inconnu…
    #[error("{1}")]
    Refused(&'static str, String),
    /// Trop de demandes depuis cette adresse.
    #[error("{1}")]
    Limited(&'static str, String),
    /// Service incomplet (chaîne ou indexeur absent).
    #[error("{1}")]
    Unavailable(&'static str, String),
    /// Défaillance interne ou de la chaîne (le détail est dans le message, jamais un secret).
    #[error("{1}")]
    Failed(&'static str, String),
}

impl BilletError {
    /// Code stable (`billet_expired`…) : l'interface traduit d'après lui, sans analyser le message.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(c, _) | Self::Refused(c, _) | Self::Limited(c, _) | Self::Unavailable(c, _) | Self::Failed(c, _) => c,
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Partage de Shamir 2-sur-3 du seed, compatible avec `billet_gen.sh` (GF(2⁸), `0x11B`, abscisses 1, 2, 3).
pub mod shamir {
    fn mul(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0;
        for _ in 0..8 {
            if b & 1 == 1 {
                p ^= a;
            }
            let hi = a & 0x80;
            a <<= 1;
            if hi != 0 {
                a ^= 0x1B;
            }
            b >>= 1;
        }
        p
    }

    fn inv(a: u8) -> u8 {
        (0..254).fold(1, |r, _| mul(r, a))
    }

    /// Découpe `secret` en trois parts ; `coef` donne l'octet aléatoire du coefficient de chaque octet.
    pub fn split(secret: &[u8; 32], mut coef: impl FnMut() -> u8) -> [[u8; 32]; 3] {
        let mut out = [[0u8; 32]; 3];
        for (i, &a0) in secret.iter().enumerate() {
            let a1 = coef();
            for (k, share) in out.iter_mut().enumerate() {
                share[i] = a0 ^ mul(a1, k as u8 + 1);
            }
        }
        out
    }

    /// Reconstruit le secret à partir de deux parts `(abscisse, valeur)` d'abscisses différentes.
    pub fn combine((x1, y1): (u8, &[u8; 32]), (x2, y2): (u8, &[u8; 32])) -> Option<[u8; 32]> {
        let denom = x1 ^ x2;
        if denom == 0 || x1 == 0 || x2 == 0 {
            return None;
        }
        let (l1, l2) = (mul(x2, inv(denom)), mul(x1, inv(denom)));
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = mul(y1[i], l1) ^ mul(y2[i], l2);
        }
        Some(out)
    }
}

/// Une part du seed, effacée de la mémoire à la fin.
type Part = Zeroizing<[u8; 32]>;

/// Lit une part : 64 hexadécimaux (casse et espaces autour libres).
pub fn parse_part(text: &str) -> Result<Part, BilletError> {
    let t = text.trim();
    let mut out = Zeroizing::new([0u8; 32]);
    if t.len() != 64 || hex::decode_to_slice(t, &mut *out).is_err() {
        return Err(BilletError::Invalid("billet_bad_part", "part illisible : 64 caractères hexadécimaux attendus".into()));
    }
    Ok(out)
}

/// Les six lectures possibles de deux parts : on ne sait pas laquelle est P1, P2 ou P3 (le QR ne le dit pas), donc on essaie
/// chaque attribution d'abscisses et on garde celle dont la clé porte un billet.
fn seeds(a: &[u8; 32], b: &[u8; 32]) -> Vec<Zeroizing<[u8; 32]>> {
    let mut out = Vec::new();
    for xa in 1..=3u8 {
        for xb in 1..=3u8 {
            if xa != xb {
                if let Some(seed) = shamir::combine((xa, a), (xb, b)) {
                    out.push(Zeroizing::new(seed));
                }
            }
        }
    }
    out
}

/// Témoin d'un billet (voir [`Billets::witness`]).
struct Witness {
    issuer: String,
    amount_zen: u64,
    expires: Option<u64>,
    ret: bool,
    content: String,
}

/// Un billet reconnu : sa clé (jamais rendue) et sa description.
struct Found {
    signer: Ed25519Signer,
    view: Value,
    total: u64,
}

/// Le service des billets.
pub struct Billets<C: Chain, P: PrimalSource> {
    db: Arc<Db>,
    chain: Arc<C>,
    wallets: Arc<Wallets>,
    primal: Option<Arc<P>>,
    uplanet_name: String,
    activity: ActivityBus,
    recent: Mutex<HashMap<(IpAddr, bool), Vec<u64>>>,
    inflight: Mutex<HashSet<[u8; 32]>>,
    issuing: Option<Issuing>,
    images: Option<Arc<crate::profile_image::PublicImages>>,
    retry_delay: Duration,
}

/// Ce qu'il faut pour **créer** des billets : les comptes des émetteurs (leur portefeuille finance le billet), la clé du NODE (qui
/// chiffre P2 et P3 gardées pour le retour automatique) et de quoi publier le témoin.
pub struct Issuing {
    /// Dossier des comptes (`~/.zen/game/nostr`) : le `.secret.dunikey` de chaque MULTIPASS.
    pub accounts_dir: PathBuf,
    /// Clé du NODE : elle seule déchiffre les parts gardées.
    pub node_secret: Zeroizing<[u8; 32]>,
    /// Adresse publique de la station (`https://…`) pour le QR « vérifier » ; à défaut, celle que la page indique.
    pub public_base: Option<String>,
    /// Publication du témoin.
    pub publish: PublishFn,
}

impl<C: Chain, P: PrimalSource> Billets<C, P> {
    /// Service pour la constellation `uplanet_name` ; `primal` : indexeur qui lit le premier émetteur d'un portefeuille.
    pub fn new(db: Arc<Db>, chain: Arc<C>, wallets: Arc<Wallets>, primal: Option<Arc<P>>, uplanet_name: String, activity: ActivityBus) -> Self {
        Self { db, chain, wallets, primal, uplanet_name, activity, recent: Mutex::new(HashMap::new()), inflight: Mutex::new(HashSet::new()), issuing: None, images: None, retry_delay: Duration::from_secs(15) }
    }

    /// Active la création de billets.
    #[must_use]
    pub fn with_issuing(mut self, issuing: Issuing) -> Self {
        self.issuing = Some(issuing);
        self
    }

    /// Images publiques (blobs) où lire le fond et l'icône de l'émetteur, ici ou chez les pairs.
    #[must_use]
    pub fn with_public_images(mut self, images: Arc<crate::profile_image::PublicImages>) -> Self {
        self.images = Some(images);
        self
    }

    /// Attente entre deux essais d'un virement refusé pour cause de nonce (« Invalid Transaction (1010) »).
    #[must_use]
    pub fn with_retry_delay(mut self, d: Duration) -> Self {
        self.retry_delay = d;
        self
    }

    fn throttle(&self, ip: Option<IpAddr>, redeem: bool) -> Result<(), BilletError> {
        let Some(ip) = ip else { return Ok(()) };
        let (max, window) = if redeem { (REDEEMS_PER_10_MIN, 600) } else { (READS_PER_MIN, 60) };
        let at = now();
        let mut g = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let v = g.entry((ip, redeem)).or_default();
        v.retain(|t| at.saturating_sub(*t) < window);
        if v.len() >= max {
            return Err(BilletError::Limited("billet_rate_limited", format!("trop de demandes : réessayez dans {} min", window / 60 + 1)));
        }
        v.push(at);
        if g.len() > 4096 {
            g.retain(|_, v| v.iter().any(|t| at.saturating_sub(*t) < 600));
        }
        Ok(())
    }

    async fn master(&self) -> Result<Arc<Ed25519Signer>, BilletError> {
        self.wallets.master().await.map_err(|e| BilletError::Unavailable("billet_unavailable", format!("portefeuille maître indisponible : {e}")))
    }

    /// Le portefeuille est-il de cette UPlanet : le maître lui-même, ou un compte dont le premier virement reçu vient du maître ?
    /// `Ok(None)` : impossible à dire (pas d'indexeur, ou indexeur muet).
    async fn of_this_uplanet(&self, who: &Account, master: &Account) -> Option<bool> {
        if who == master {
            return Some(true);
        }
        let primal = self.primal.as_ref()?.primal_of(&who.ss58()).await.ok()?;
        Some(primal.and_then(|p| p.parse::<Account>().ok()).as_ref() == Some(master))
    }

    async fn state(&self, account: &Account) -> Option<String> {
        let key = account.ss58();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, key], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten()
    }

    async fn set_state(&self, account: &Account, value: &str) {
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![SCOPE.to_owned().into(), account.ss58().into(), value.to_owned().into(), (now() as i64).into()]).await;
    }

    /// Témoin d'un billet : l'événement signé par la clé de la coopérative qui dit qui l'a créé, pour combien, jusqu'à quand, et si
    /// la valeur revient à l'émetteur. Un témoin signé par une autre clé ne compte pas.
    async fn witness(&self, account: &Account) -> Option<Witness> {
        let (_, coop_hex) = crate::coop_config::coop_keys(&self.uplanet_name).ok()?;
        let d = format!("billet:{}", account.ss58());
        self.db
            .readers()
            .read(move |c| {
                let tag = |n: &str| format!("(SELECT value FROM nostr_tags WHERE event_id = e.seq AND name = '{n}' LIMIT 1)");
                let sql = format!("SELECT {}, {}, {}, {}, e.content FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'd' AND t.value = ?2 WHERE e.kind = 31903 AND e.pubkey = ?1 ORDER BY e.created_at DESC LIMIT 1", tag("p"), tag("a"), tag("u"), tag("r"));
                Ok(c.query_row(&sql, rusqlite::params![coop_hex, d], |r| {
                    Ok(Witness { issuer: r.get::<_, Option<String>>(0)?.unwrap_or_default(), amount_zen: r.get::<_, Option<String>>(1)?.and_then(|v| v.parse().ok()).unwrap_or(0), expires: r.get::<_, Option<String>>(2)?.and_then(|v| v.parse().ok()), ret: r.get::<_, Option<String>>(3)?.as_deref() == Some("1"), content: r.get(4)? })
                })
                .optional()?)
            })
            .await
            .ok()
            .flatten()
    }

    async fn expiry(&self, account: &Account) -> Option<u64> {
        self.witness(account).await.and_then(|w| w.expires)
    }

    /// Virement avec nouvelle tentative : deux virements d'un même compte à la suite sont parfois refusés (« Invalid Transaction (1010) »,
    /// nonce pas encore vu de tous les nœuds) ; une attente suffit. Toute autre erreur est rendue telle quelle.
    async fn transfer_retry(&self, from: &Ed25519Signer, to: &Account, amount: u64, mode: TransferMode, memo: &str) -> Result<TxOutcome, ChainError> {
        let mut last = None;
        for attempt in 0..4 {
            if attempt > 0 {
                tokio::time::sleep(self.retry_delay).await;
            }
            match self.chain.transfer(from, to, Amount(amount), mode, Some(memo)).await {
                Err(ChainError::Rejected(m)) if m.contains("1010") => last = Some(ChainError::Rejected(m)),
                other => return other,
            }
        }
        Err(last.unwrap_or_else(|| ChainError::Rejected("virement refusé".into())))
    }

    /// Description d'un billet : valeur, authenticité, validité, état.
    async fn describe(&self, account: &Account) -> Result<(Value, u64), BilletError> {
        let master = self.master().await?.account();
        let balance = self.chain.balance(account).await.map_err(|e| BilletError::Failed("billet_chain_error", format!("solde illisible : {e}")))?;
        let (total, value) = (balance.total.0, balance.spendable(Amount(RESERVE_G1_CENTS)).0);
        let (authentic, expires, state) = (self.of_this_uplanet(account, &master).await, self.expiry(account).await, self.state(account).await);
        let expired = expires.is_some_and(|t| t <= now());
        let status = match state.as_deref().and_then(|s| s.split('|').next()) {
            Some("done") => "redeemed",
            Some("returned") => "returned",
            Some("drained" | "draining") => "pending",
            _ if value == 0 => "empty",
            _ if authentic == Some(false) => "foreign",
            _ if expired => "expired",
            _ => "ready",
        };
        Ok((json!({ "g1pub": account.ss58(), "g1_cents": total, "value_g1_cents": value, "zen_cents": zen_of_g1(total), "authentic": authentic, "expires_at": expires, "expired": expired, "status": status }), total))
    }

    /// Billet reconnu à partir de deux parts.
    async fn identify(&self, a: &[u8; 32], b: &[u8; 32], expect: Option<&Account>) -> Result<Found, BilletError> {
        if a == b {
            return Err(BilletError::Invalid("billet_same_part", "ce sont deux fois la même part : lisez l'autre QR du billet".into()));
        }
        let mut hits = Vec::new();
        for seed in seeds(a, b) {
            let signer = Ed25519Signer::from_seed(*seed);
            let account = signer.account();
            if expect.is_some_and(|e| *e != account) {
                continue;
            }
            let total = self.chain.balance(&account).await.map_err(|e| BilletError::Failed("billet_chain_error", format!("solde illisible : {e}")))?.total.0;
            if total > 0 || expect.is_some() || self.state(&account).await.is_some() {
                hits.push((signer, account, total));
            }
        }
        match hits.len() {
            0 => Err(BilletError::Refused("billet_no_match", "ces deux parts ne correspondent à aucun billet : vérifiez qu'elles viennent du même billet".into())),
            1 => {
                let (signer, account, _) = hits.remove(0);
                let (view, total) = self.describe(&account).await?;
                Ok(Found { signer, view, total })
            }
            _ => Err(BilletError::Failed("billet_ambiguous", "lecture ambiguë des parts : réessayez en lisant P1 puis P2".into())),
        }
    }

    fn account_of(text: &str) -> Result<Account, BilletError> {
        text.trim().parse::<Account>().map_err(|_| BilletError::Invalid("billet_bad_address", "adresse Ğ1 illisible".into()))
    }

    /// Le destinataire est-il reconnu par l'UPlanet (le maître, ou un compte marqué par lui) ?
    async fn destination_ok(&self, dest: &Account) -> Result<bool, BilletError> {
        let master = self.master().await?.account();
        if self.primal.is_none() && *dest != master {
            return Err(BilletError::Unavailable("billet_no_indexer", "cette station n'a pas d'indexeur Ğ1 : elle ne peut pas vérifier le destinataire".into()));
        }
        self.of_this_uplanet(dest, &master).await.ok_or_else(|| BilletError::Unavailable("billet_indexer_down", "indexeur Ğ1 injoignable : impossible de vérifier le destinataire pour l'instant".into()))
    }

    /// Point d'entrée des routes publiques : `op` ∈ `check` (adresse), `identify` (deux parts), `destination`, `redeem`.
    pub async fn handle(&self, ip: Option<IpAddr>, op: &str, body: &Value) -> Result<Value, BilletError> {
        let field = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("");
        let parts = || -> Result<(Part, Part), BilletError> {
            let list = body.get("parts").and_then(Value::as_array).filter(|p| p.len() == 2).ok_or_else(|| BilletError::Invalid("billet_two_parts", "deux parts attendues".into()))?;
            Ok((parse_part(list[0].as_str().unwrap_or(""))?, parse_part(list[1].as_str().unwrap_or(""))?))
        };
        match op {
            "check" => {
                self.throttle(ip, false)?;
                Ok(self.describe(&Self::account_of(field("g1pub"))?).await?.0)
            }
            "identify" => {
                self.throttle(ip, false)?;
                let (a, b) = parts()?;
                Ok(self.identify(&a, &b, None).await?.view)
            }
            "destination" => {
                self.throttle(ip, false)?;
                let dest = Self::account_of(field("g1pub"))?;
                Ok(json!({ "ok": self.destination_ok(&dest).await?, "g1pub": dest.ss58() }))
            }
            "redeem" => {
                self.throttle(ip, true)?;
                let (a, b) = parts()?;
                self.redeem(&a, &b, &Self::account_of(field("dest"))?).await
            }
            // Opérations d'un compte authentifié : `_who` est posé par l'interface après authentification, jamais par le client
            // (les routes publiques n'atteignent que les opérations ci-dessus).
            "is_issuer" | "set_issuer" | "issue" | "fund" | "delete" | "issued" => {
                let who = body.get("_who").ok_or_else(|| BilletError::Refused("billet_not_issuer", "compte inconnu".into()))?;
                let (hex, email, captain) = (who["hex"].as_str().unwrap_or(""), who["email"].as_str().unwrap_or(""), who["captain"].as_bool().unwrap_or(false));
                match op {
                    "is_issuer" => Ok(json!({ "issuer": self.issuing.is_some() && self.is_issuer(hex, captain).await })),
                    "set_issuer" => {
                        if !captain {
                            return Err(BilletError::Refused("billet_not_issuer", "réservé au Capitaine".into()));
                        }
                        let target = body.get("hex").and_then(Value::as_str).filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).ok_or_else(|| BilletError::Invalid("billet_bad_address", "compte invalide".into()))?;
                        self.set_issuer(target, body.get("allowed").and_then(Value::as_bool).unwrap_or(false)).await?;
                        Ok(json!({ "hex": target, "issuer": body.get("allowed").and_then(Value::as_bool).unwrap_or(false) }))
                    }
                    "issue" => self.issue(hex, email, captain, body).await,
                    "fund" => self.fund(hex, email, captain, field("g1pub")).await,
                    "delete" => self.delete(hex, captain, field("g1pub")).await,
                    _ => self.issued(hex).await,
                }
            }
            _ => Err(BilletError::Invalid("billet_bad_op", "opération inconnue".into())),
        }
    }

    async fn redeem(&self, a: &[u8; 32], b: &[u8; 32], dest: &Account) -> Result<Value, BilletError> {
        let master = self.master().await?;
        let found = self.identify(a, b, None).await?;
        let account = found.signer.account();
        match found.view["status"].as_str() {
            Some("ready") => {}
            Some("expired") => return Err(BilletError::Refused("billet_expired", "ce billet a expiré : sa valeur est revenue à son émetteur".into())),
            Some("redeemed") => return Err(BilletError::Refused("billet_redeemed", "ce billet a déjà été encaissé".into())),
            Some("returned") => return Err(BilletError::Refused("billet_expired", "ce billet a expiré : sa valeur est revenue à son émetteur".into())),
            Some("pending") => return Err(BilletError::Refused("billet_pending", "l'encaissement de ce billet est en cours : la station termine le transfert".into())),
            Some("foreign") => return Err(BilletError::Refused("billet_foreign", "ce billet n'a pas été émis par cette UPlanet".into())),
            _ => return Err(BilletError::Refused("billet_empty", "ce billet est vide".into())),
        }
        // Sans indexeur, l'origine du billet est inconnue : on ne fait pas entrer de fonds dans le circuit coopératif à l'aveugle.
        if found.view["authentic"] != json!(true) {
            return Err(BilletError::Unavailable("billet_no_indexer", "impossible de vérifier l'origine du billet pour l'instant (indexeur Ğ1)".into()));
        }
        if *dest == account {
            return Err(BilletError::Invalid("billet_dest_is_billet", "le destinataire est le billet lui-même".into()));
        }
        if !self.destination_ok(dest).await? {
            return Err(BilletError::Refused("billet_dest_unknown", "ce portefeuille n'est pas reconnu par l'UPlanet (primal incorrect) : le billet n'a pas été touché".into()));
        }
        let forward = found.total.saturating_sub(RESERVE_G1_CENTS);
        if forward == 0 {
            return Err(BilletError::Refused("billet_empty", "ce billet ne vaut rien : son solde ne couvre que le dépôt d'activation".into()));
        }
        self.settle(&found, &master, dest, forward).await
    }

    /// Règle un billet : le vide vers le maître, puis le maître envoie `forward` à `dest`. Un seul règlement à la fois par billet.
    async fn settle(&self, found: &Found, master: &Arc<Ed25519Signer>, dest: &Account, forward: u64) -> Result<Value, BilletError> {
        let account = found.signer.account();
        if !self.inflight.lock().unwrap_or_else(|e| e.into_inner()).insert(account.0) {
            return Err(BilletError::Refused("billet_pending", "l'encaissement de ce billet est déjà en cours".into()));
        }
        let out = self.run_redeem(found, master, dest, forward).await;
        self.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&account.0);
        out
    }

    async fn run_redeem(&self, found: &Found, master: &Arc<Ed25519Signer>, dest: &Account, forward: u64) -> Result<Value, BilletError> {
        let account = found.signer.account();
        let record = |step: &str| json!({ "billet": account.ss58(), "step": step, "dest": dest.ss58(), "g1_cents": forward });
        // L'état est écrit AVANT le virement : une coupure laisse une trace, jamais un billet vidé sans mémoire.
        self.set_state(&account, &format!("draining|{}|{forward}|{}", dest.ss58(), now())).await;
        if let Err(e) = self.transfer_retry(&found.signer, &master.account(), found.total, TransferMode::AllowDeath, "UPLANET:BILLET:REDEEM:DRAIN").await {
            return match e {
                // Issue inconnue : le billet est peut-être vidé. On ne rejoue pas, on prévient.
                astro_duniter::ChainError::UnknownOutcome(why) => {
                    self.alert(format!("Billet {} : issue inconnue du vidage ({why}) — vérifier son solde avant tout nouvel essai", account.ss58()));
                    Err(BilletError::Failed("billet_unknown_outcome", "l'issue du transfert est inconnue : ne réessayez pas, le Capitaine est prévenu".into()))
                }
                other => {
                    // Rien n'est parti : le billet reste valable.
                    self.clear_state(&account).await;
                    Err(BilletError::Failed("billet_drain_failed", format!("le vidage du billet a échoué : {other}")))
                }
            };
        }
        self.set_state(&account, &format!("drained|{}|{forward}|{}", dest.ss58(), now())).await;
        self.audit(record("drained")).await;
        match self.transfer_retry(master, dest, forward, TransferMode::KeepAlive, "UPLANET:BILLET:REDEEM:FORWARD").await {
            Ok(_) => {
                self.set_state(&account, &format!("done|{}|{forward}|{}", dest.ss58(), now())).await;
                self.audit(record("done")).await;
                Ok(json!({ "ok": true, "status": "done", "g1_cents": forward, "zen_cents": forward.saturating_mul(crate::station::ZEN_PER_G1), "dest": dest.ss58() }))
            }
            Err(e) => {
                self.alert(format!("Billet {} vidé mais le transfert final a échoué ({e}) : les fonds sont sur UPLANETNAME_G1, la station réessaie", account.ss58()));
                Ok(json!({ "ok": true, "status": "pending", "g1_cents": forward, "zen_cents": forward.saturating_mul(crate::station::ZEN_PER_G1), "dest": dest.ss58() }))
            }
        }
    }

    // ── Création de billets (réservée aux émetteurs choisis par le Capitaine) ─────────────────────────────────────────────

    fn issuing(&self) -> Result<&Issuing, BilletError> {
        self.issuing.as_ref().ok_or_else(|| BilletError::Unavailable("billet_unavailable", "la création de billets n'est pas activée sur cette station".into()))
    }

    /// Le compte `hex` peut-il créer des billets ? Le Capitaine toujours ; les MULTIPASS que le Capitaine a autorisés.
    pub async fn is_issuer(&self, hex: &str, captain: bool) -> bool {
        captain || self.db.readers().read({ let h = hex.to_owned(); move |c| Ok(c.query_row("SELECT 1 FROM kv WHERE scope = 'billet_issuer' AND key = ?1", [&h], |_| Ok(())).optional()?.is_some()) }).await.unwrap_or(false)
    }

    /// Autorise (ou retire) un MULTIPASS à créer des billets. Le Capitaine n'a pas besoin d'être listé.
    pub async fn set_issuer(&self, hex: &str, allowed: bool) -> Result<(), BilletError> {
        let w = self.db.writer();
        let out = if allowed {
            w.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES ('billet_issuer', ?1, '1', ?2)", vec![hex.to_owned().into(), (now() as i64).into()]).await
        } else {
            w.execute("DELETE FROM kv WHERE scope = 'billet_issuer' AND key = ?1", vec![hex.to_owned().into()]).await
        };
        out.map(|_| ()).map_err(|e| BilletError::Failed("billet_internal", e.to_string()))
    }

    /// Portefeuille Ğ1 d'un MULTIPASS hébergé : sa clé est dans le dossier du compte.
    fn wallet_of(&self, email: &str) -> Result<Ed25519Signer, BilletError> {
        let dir = &self.issuing()?.accounts_dir;
        let text = std::fs::read_to_string(dir.join(email).join(".secret.dunikey")).map_err(|_| BilletError::Unavailable("billet_no_wallet", "portefeuille Ğ1 du compte introuvable sur cette station".into()))?;
        let dk = astro_lifecycle::dunikey::Dunikey::parse(&text).map_err(|_| BilletError::Unavailable("billet_no_wallet", "portefeuille Ğ1 du compte illisible".into()))?;
        Ok(Ed25519Signer::from_seed(*dk.ed25519_seed()))
    }

    /// Crée `count` billets : clés jetables, parts, témoin signé par la coopérative. **Rien n'est financé** : chaque billet se finance
    /// ensuite à part ([`Self::fund`]), un échec n'affecte pas les autres. P1 et P2 ne sont rendues qu'ici, une seule fois, pour
    /// l'impression ; la station ne garde rien, sauf avec le retour automatique : P2 et P3 chiffrées pour le NODE dans le témoin.
    pub async fn issue(&self, issuer_hex: &str, issuer_email: &str, captain: bool, body: &Value) -> Result<Value, BilletError> {
        let iss = self.issuing()?;
        if !self.is_issuer(issuer_hex, captain).await {
            return Err(BilletError::Refused("billet_not_issuer", "le Capitaine ne vous a pas autorisé à créer des billets".into()));
        }
        let num = |k: &str| body.get(k).and_then(Value::as_u64);
        let count = num("count").unwrap_or(1);
        let amount_zen = num("amount_zen").unwrap_or(0);
        if !(1..=6).contains(&count) {
            return Err(BilletError::Invalid("billet_bad_count", "de 1 à 6 billets par planche".into()));
        }
        if amount_zen > MAX_ZEN_PER_BILLET {
            return Err(BilletError::Invalid("billet_bad_amount", format!("au plus {MAX_ZEN_PER_BILLET} ẑ par billet")));
        }
        let valid_days = match body.get("valid_days") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_u64().filter(|d| (1..=3650).contains(d)).ok_or_else(|| BilletError::Invalid("billet_bad_days", "durée de validité : de 1 à 3650 jours".into()))?),
        };
        let ret = body.get("return_to_issuer").and_then(Value::as_bool).unwrap_or(false);
        if ret && valid_days.is_none() {
            return Err(BilletError::Invalid("billet_return_needs_days", "le retour automatique demande une durée de validité".into()));
        }
        let origin = iss.public_base.clone().or_else(|| body.get("origin").and_then(Value::as_str).filter(|o| (o.starts_with("https://") || o.starts_with("http://")) && !o[o.find("//").unwrap_or(0) + 2..].contains(['/', ' ', '?', '#'])).map(str::to_owned));
        let issuer_wallet = self.wallet_of(issuer_email)?.account();
        let (coop_secret, _) = crate::coop_config::coop_keys(&self.uplanet_name).map_err(|e| BilletError::Unavailable("billet_unavailable", e))?;
        let node_hex = astro_nostr_proto::pubkey_of(&iss.node_secret).map_err(|e| BilletError::Failed("billet_internal", e.to_string()))?;
        let expires = valid_days.map(|d| now() + d * 86_400);
        let mut out = Vec::new();
        for _ in 0..count {
            let mut seed = Zeroizing::new([0u8; 32]);
            let mut coef = Zeroizing::new([0u8; 32]);
            getrandom::fill(&mut *seed).and_then(|()| getrandom::fill(&mut *coef)).map_err(|e| BilletError::Failed("billet_internal", format!("aléa indisponible : {e}")))?;
            let mut it = coef.iter().copied();
            let parts = Zeroizing::new(shamir::split(&seed, || it.next().unwrap_or(0)));
            let account = Ed25519Signer::from_seed(*seed).account();
            // Avec le retour automatique, la station garde P2 et P3 (chiffrées pour le NODE) et l'adresse où renvoyer la valeur.
            let content = if ret {
                let plain = json!({ "p2": hex::encode(parts[1]), "p3": hex::encode(parts[2]), "to": issuer_wallet.ss58() }).to_string();
                astro_crypto::nip44::encrypt_to_self(&iss.node_secret, &node_hex, &plain).map_err(|e| BilletError::Failed("billet_internal", e.to_string()))?
            } else {
                String::new()
            };
            let mut tags = vec![vec!["d".into(), format!("billet:{}", account.ss58())], vec!["p".into(), issuer_hex.to_owned()], vec!["a".into(), amount_zen.to_string()]];
            if let Some(e) = expires {
                tags.push(vec!["u".into(), e.to_string()]);
            }
            if ret {
                tags.push(vec!["r".into(), "1".into()]);
            }
            let event = UnsignedEvent { created_at: now(), kind: astro_nostr_proto::kind::APP_PARAMS, tags, content }.sign(&coop_secret).map_err(|e| BilletError::Failed("billet_internal", e.to_string()))?;
            (iss.publish)(event).await.map_err(|e| BilletError::Failed("billet_internal", format!("témoin non publié : {e}")))?;
            let (p1, p2) = (hex::encode(parts[0]), hex::encode(parts[1]));
            let link = origin.as_ref().map(|o| format!("{o}/scan?g1pub={}", account.ss58()));
            let qr = |t: &str| astro_ui::qr::svg(t);
            out.push(json!({ "g1pub": account.ss58(), "p1": p1, "p2": p2, "qr_p1": qr(&p1), "qr_p2": qr(&p2), "verify_url": link, "qr_verify": link.as_deref().and_then(qr), "amount_zen": amount_zen, "expires_at": expires, "return_to_issuer": ret }));
            self.audit(json!({ "billet": account.ss58(), "step": "issued", "issuer": issuer_hex, "amount_zen": amount_zen, "expires_at": expires, "return": ret })).await;
        }
        Ok(json!({ "billets": out, "issuer_wallet": issuer_wallet.ss58(), "style": self.issuer_style(issuer_hex).await }))
    }

    /// Fond et icône des billets : le **bandeau** (`banner`) et l'**image** (`picture`) du profil (kind 0) de l'émetteur, réduits en `data:` URI
    /// pour qu'ils passent la CSP et partent à l'impression. Un fond manquant ou illisible n'empêche jamais de créer des billets.
    async fn issuer_style(&self, hex: &str) -> Value {
        let h = hex.to_owned();
        let profile: Value = self.db.readers().read(move |c| Ok(c.query_row("SELECT content FROM nostr_events WHERE kind = 0 AND pubkey = ?1 ORDER BY created_at DESC LIMIT 1", [&h], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten().and_then(|c| serde_json::from_str(&c).ok()).unwrap_or(Value::Null);
        let load = |field: &'static str, kind: crate::profile_image::Kind| {
            let url = profile[field].as_str().unwrap_or("").to_owned();
            let images = self.images.clone();
            async move { if url.is_empty() { None } else { crate::profile_image::data_uri(images.as_deref(), &url, kind).await } }
        };
        json!({ "background": load("banner", crate::profile_image::Kind::Banner).await, "icon": load("picture", crate::profile_image::Kind::Picture).await })
    }

    /// Finance un billet créé par `issuer_hex` : 1 Ğ1 d'activation du maître si le billet est vide, puis le montant, du portefeuille
    /// de l'émetteur. Sûr à rappeler : un billet déjà au bon solde ne reçoit rien de plus. Seul un billet que **cet émetteur** a créé
    /// (témoin signé par la coopérative) peut recevoir l'activation gratuite du maître.
    pub async fn fund(&self, issuer_hex: &str, issuer_email: &str, captain: bool, g1pub: &str) -> Result<Value, BilletError> {
        self.issuing()?;
        if !self.is_issuer(issuer_hex, captain).await {
            return Err(BilletError::Refused("billet_not_issuer", "le Capitaine ne vous a pas autorisé à créer des billets".into()));
        }
        let account = Self::account_of(g1pub)?;
        let w = self.witness(&account).await.filter(|w| w.issuer == issuer_hex).ok_or_else(|| BilletError::Refused("billet_not_yours", "ce billet n'a pas été créé par vous".into()))?;
        if w.amount_zen == 0 {
            return Err(BilletError::Refused("billet_blank", "billet vierge : rien à financer".into()));
        }
        let want = w.amount_zen * 100 / crate::station::ZEN_PER_G1;
        let target = RESERVE_G1_CENTS + want;
        let balance = |a: Account| async move { self.chain.balance(&a).await.map(|b| b.total.0).map_err(|e| BilletError::Failed("billet_chain_error", format!("solde illisible : {e}"))) };
        let have = balance(account).await?;
        if have >= target {
            self.mark_funded(&account, want).await;
            return Ok(json!({ "ok": true, "already": true, "g1_cents": have }));
        }
        let master = self.master().await?;
        if have < RESERVE_G1_CENTS {
            self.transfer_retry(&master, &account, RESERVE_G1_CENTS, TransferMode::KeepAlive, "UPLANET:BILLET:PRIMO").await.map_err(|e| BilletError::Failed("billet_fund_failed", format!("dépôt d'activation refusé : {e}")))?;
        }
        let issuer = self.wallet_of(issuer_email)?;
        let poor = balance(issuer.account()).await? < want + RESERVE_G1_CENTS;
        if poor {
            return Err(BilletError::Refused("billet_issuer_poor", "le solde de votre portefeuille ne couvre pas ce billet".into()));
        }
        self.transfer_retry(&issuer, &account, want, TransferMode::KeepAlive, &format!("UPLANET:BILLET:{}ZEN", w.amount_zen)).await.map_err(|e| BilletError::Failed("billet_fund_failed", format!("financement refusé : {e}")))?;
        self.mark_funded(&account, want).await;
        self.audit(json!({ "billet": account.ss58(), "step": "funded", "issuer": issuer_hex, "g1_cents": want })).await;
        Ok(json!({ "ok": true, "already": false, "g1_cents": target }))
    }

    /// Supprime un billet **pas encore financé** : le témoin est retiré de Nostr (NIP-09 : événement `kind 5` signé par la coopérative,
    /// qui a signé le témoin, citant son adresse), donc de cette station et de ceux qui le suivent. Réservé à son émetteur (ou au
    /// Capitaine). Un billet qui porte de la valeur ne se supprime pas : il s'encaisse, ou revient à l'émetteur à sa date de fin.
    /// Le dépôt d'activation de 1 Ğ1 que le billet a pu recevoir du maître y reste : la station ne garde pas la clé d'un billet.
    pub async fn delete(&self, issuer_hex: &str, captain: bool, g1pub: &str) -> Result<Value, BilletError> {
        let iss = self.issuing()?;
        if !self.is_issuer(issuer_hex, captain).await {
            return Err(BilletError::Refused("billet_not_issuer", "le Capitaine ne vous a pas autorisé à créer des billets".into()));
        }
        let account = Self::account_of(g1pub)?;
        let w = self.witness(&account).await.ok_or_else(|| BilletError::Refused("billet_unknown", "billet inconnu de cette station".into()))?;
        if w.issuer != issuer_hex && !captain {
            return Err(BilletError::Refused("billet_not_yours", "ce billet n'a pas été créé par vous".into()));
        }
        let (view, _) = self.describe(&account).await?;
        if view["status"] != "empty" {
            return Err(BilletError::Refused("billet_not_empty", "ce billet porte de la valeur ou a déjà servi : il ne peut plus être supprimé (encaissez-le, ou attendez sa date de fin)".into()));
        }
        let (coop_secret, coop_hex) = crate::coop_config::coop_keys(&self.uplanet_name).map_err(|e| BilletError::Unavailable("billet_unavailable", e))?;
        let address = format!("{}:{coop_hex}:billet:{}", astro_nostr_proto::kind::APP_PARAMS, account.ss58());
        let event = UnsignedEvent { created_at: now(), kind: astro_nostr_proto::kind::DELETION, tags: vec![vec!["a".into(), address]], content: "billet supprimé avant financement".into() }
            .sign(&coop_secret)
            .map_err(|e| BilletError::Failed("billet_internal", e.to_string()))?;
        (iss.publish)(event).await.map_err(|e| BilletError::Failed("billet_internal", format!("suppression non publiée : {e}")))?;
        self.audit(json!({ "billet": account.ss58(), "step": "deleted", "issuer": issuer_hex, "g1_cents": view["g1_cents"] })).await;
        Ok(json!({ "ok": true, "g1pub": account.ss58(), "g1_cents_left": view["g1_cents"] }))
    }

    /// Suivi des billets de cette UPlanet, pour tous les membres (aucun détail personnel) : émis, financés, en circulation et leur valeur,
    /// encaissés, renvoyés à l'émetteur, expirés. Lu dans les témoins (signés par la coopérative) et l'état local de la station.
    pub async fn stats(&self) -> Value {
        let Ok((_, coop_hex)) = crate::coop_config::coop_keys(&self.uplanet_name) else { return Value::Null };
        let rows: Vec<(String, Option<u64>, Option<u64>)> = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT d.value, (SELECT value FROM nostr_tags WHERE event_id = e.seq AND name = 'a' LIMIT 1), (SELECT value FROM nostr_tags WHERE event_id = e.seq AND name = 'u' LIMIT 1) FROM nostr_events e JOIN nostr_tags d ON d.event_id = e.seq AND d.name = 'd' AND d.value LIKE 'billet:%' WHERE e.kind = 31903 AND e.pubkey = ?1 LIMIT 5000")?;
                let rows = st.query_map([&coop_hex], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.and_then(|v| v.parse().ok()), r.get::<_, Option<String>>(2)?.and_then(|v| v.parse().ok()))))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .unwrap_or_default();
        let kv = |scope: &'static str| {
            let db = self.db.clone();
            async move {
                db.readers().read(move |c| {
                    let mut st = c.prepare("SELECT key, value FROM kv WHERE scope = ?1")?;
                    let m = st.query_map([scope], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<HashMap<_, _>, _>>()?;
                    Ok(m)
                }).await.unwrap_or_default()
            }
        };
        let (states, funded) = (kv(SCOPE).await, kv("billet_funded").await);
        let at = now();
        let (mut issued, mut n_funded, mut circulating, mut redeemed, mut returned, mut expired, mut pending, mut value) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        for (d, amount, until) in rows {
            let key = d.strip_prefix("billet:").unwrap_or(&d).to_owned();
            issued += 1;
            if !funded.contains_key(&key) {
                continue;
            }
            n_funded += 1;
            match states.get(&key).and_then(|v| v.split('|').next()) {
                Some("done") => redeemed += 1,
                Some("returned") => returned += 1,
                Some(_) => pending += 1,
                None if until.is_some_and(|u| u <= at) => expired += 1,
                None => {
                    circulating += 1;
                    value += amount.unwrap_or(0);
                }
            }
        }
        json!({ "issued": issued, "funded": n_funded, "circulating": circulating, "circulating_zen": value, "redeemed": redeemed, "returned": returned, "expired": expired, "pending": pending })
    }

    /// Adresses des billets connus (témoins signés par la coopérative) : la carte les regroupe en un point « billets ».
    pub async fn addresses(&self) -> HashSet<String> {
        let Ok((_, coop_hex)) = crate::coop_config::coop_keys(&self.uplanet_name) else { return HashSet::new() };
        self.db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT substr(d.value, 8) FROM nostr_events e JOIN nostr_tags d ON d.event_id = e.seq AND d.name = 'd' AND d.value LIKE 'billet:%' WHERE e.kind = 31903 AND e.pubkey = ?1 LIMIT 20000")?;
                let set = st.query_map([&coop_hex], |r| r.get::<_, String>(0))?.collect::<Result<HashSet<_>, _>>()?;
                Ok(set)
            })
            .await
            .unwrap_or_default()
    }

    /// Note que le billet a reçu sa valeur (le suivi de l'UPlanet ne compte « en circulation » que les billets financés).
    async fn mark_funded(&self, account: &Account, g1_cents: u64) {
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES ('billet_funded', ?1, ?2, ?3)", vec![account.ss58().into(), g1_cents.to_string().into(), (now() as i64).into()]).await;
    }

    /// Les billets créés par `issuer_hex`, du plus récent au plus ancien, avec leur état.
    pub async fn issued(&self, issuer_hex: &str) -> Result<Value, BilletError> {
        let (_, coop_hex) = crate::coop_config::coop_keys(&self.uplanet_name).map_err(|e| BilletError::Unavailable("billet_unavailable", e))?;
        let issuer = issuer_hex.to_owned();
        let rows: Vec<(String, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT d.value, e.created_at FROM nostr_events e JOIN nostr_tags d ON d.event_id = e.seq AND d.name = 'd' AND d.value LIKE 'billet:%' JOIN nostr_tags p ON p.event_id = e.seq AND p.name = 'p' AND p.value = ?2 WHERE e.kind = 31903 AND e.pubkey = ?1 ORDER BY e.created_at DESC LIMIT 30")?;
                let rows = st.query_map(rusqlite::params![coop_hex, issuer], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| BilletError::Failed("billet_internal", e.to_string()))?;
        let mut list = Vec::new();
        for (d, created) in rows {
            let Some(account) = d.strip_prefix("billet:").and_then(|a| a.parse::<Account>().ok()) else { continue };
            let (view, _) = self.describe(&account).await?;
            let w = self.witness(&account).await;
            list.push(json!({ "g1pub": account.ss58(), "created_at": created, "amount_zen": w.as_ref().map(|w| w.amount_zen), "return_to_issuer": w.as_ref().is_some_and(|w| w.ret), "expires_at": view["expires_at"], "status": view["status"], "zen_cents": view["zen_cents"] }));
        }
        Ok(json!({ "billets": list }))
    }

    /// Retour automatique : les billets dont la date est passée, choisis « retour à l'émetteur » et non encaissés, sont vidés vers le
    /// maître puis envoyés au portefeuille de leur émetteur, avec P2 et P3 gardées dans le témoin. Rend le nombre de billets renvoyés.
    pub async fn sweep_once(&self) -> Result<usize, BilletError> {
        let Some(iss) = &self.issuing else { return Ok(0) };
        let (_, coop_hex) = crate::coop_config::coop_keys(&self.uplanet_name).map_err(|e| BilletError::Unavailable("billet_unavailable", e))?;
        let now_s = now();
        let due: Vec<String> = self
            .db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT d.value FROM nostr_events e JOIN nostr_tags d ON d.event_id = e.seq AND d.name = 'd' AND d.value LIKE 'billet:%' JOIN nostr_tags r ON r.event_id = e.seq AND r.name = 'r' AND r.value = '1' JOIN nostr_tags u ON u.event_id = e.seq AND u.name = 'u' AND CAST(u.value AS INTEGER) <= ?2 WHERE e.kind = 31903 AND e.pubkey = ?1")?;
                let rows = st.query_map(rusqlite::params![coop_hex, now_s as i64], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| BilletError::Failed("billet_internal", e.to_string()))?;
        let master = self.master().await?;
        let node_hex = astro_nostr_proto::pubkey_of(&iss.node_secret).map_err(|e| BilletError::Failed("billet_internal", e.to_string()))?;
        let mut returned = 0;
        for d in due {
            let Some(account) = d.strip_prefix("billet:").and_then(|a| a.parse::<Account>().ok()) else { continue };
            if self.state(&account).await.is_some() {
                continue; // déjà encaissé, vidé ou en cours
            }
            let Some(w) = self.witness(&account).await.filter(|w| w.ret) else { continue };
            let plain = match astro_crypto::nip44::decrypt_from_self(&iss.node_secret, &node_hex, &w.content) {
                Ok(p) => p,
                Err(e) => {
                    self.alert(format!("Billet {} : parts gardées illisibles ({e}), retour automatique impossible", account.ss58()));
                    continue;
                }
            };
            let kept: Value = serde_json::from_str(&plain).unwrap_or(Value::Null);
            let (Ok(p2), Ok(p3), Some(to)) = (parse_part(kept["p2"].as_str().unwrap_or("")), parse_part(kept["p3"].as_str().unwrap_or("")), kept["to"].as_str().and_then(|t| t.parse::<Account>().ok())) else { continue };
            let Some(seed) = shamir::combine((2, &p2), (3, &p3)) else { continue };
            let signer = Ed25519Signer::from_seed(seed);
            if signer.account() != account {
                self.alert(format!("Billet {} : les parts gardées ne redonnent pas sa clé", account.ss58()));
                continue;
            }
            let (total, forward) = match self.chain.balance(&account).await {
                Ok(b) => (b.total.0, b.spendable(Amount(RESERVE_G1_CENTS)).0),
                Err(_) => continue,
            };
            if forward == 0 {
                self.set_state(&account, &format!("done|{}|0|{}", to.ss58(), now())).await; // déjà vide : rien à renvoyer
                continue;
            }
            if !self.destination_ok(&to).await.is_ok_and(|ok| ok) {
                self.alert(format!("Billet {} expiré : le portefeuille de l'émetteur n'est pas reconnu par l'UPlanet, valeur non renvoyée", account.ss58()));
                continue;
            }
            let found = Found { signer, view: Value::Null, total };
            if self.settle(&found, &master, &to, forward).await.is_ok() {
                // Se distingue d'un encaissement par le porteur : le suivi de l'UPlanet compte les deux à part.
                self.set_state(&account, &format!("returned|{}|{forward}|{}", to.ss58(), now())).await;
                returned += 1;
            }
        }
        Ok(returned)
    }

    async fn clear_state(&self, account: &Account) {
        let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SCOPE.to_owned().into(), account.ss58().into()]).await;
    }

    /// Journal d'audit : jamais de part ni de seed.
    async fn audit(&self, record: Value) {
        let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('billet', ?1, ?2)", vec![(now() as i64).into(), record.to_string().into()]).await;
    }

    fn alert(&self, text: String) {
        self.activity.publish_to(Audience::Captain, "alert", "billet", text, None);
    }

    /// Termine les encaissements dont le vidage a réussi et le transfert final non. Rend le nombre de billets terminés.
    pub async fn resume_once(&self) -> Result<usize, BilletError> {
        let rows: Vec<(String, String)> = self
            .db
            .readers()
            .read(|c| {
                let mut st = c.prepare("SELECT key, value FROM kv WHERE scope = ?1 AND value LIKE 'drained|%'")?;
                let rows = st.query_map(rusqlite::params![SCOPE], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(|e| BilletError::Failed("billet_internal", e.to_string()))?;
        let master = self.master().await?;
        let mut done = 0;
        for (key, value) in rows {
            let mut f = value.split('|').skip(1);
            let (Some(dest), Some(cents)) = (f.next().and_then(|d| d.parse::<Account>().ok()), f.next().and_then(|c| c.parse::<u64>().ok())) else { continue };
            let Ok(account) = key.parse::<Account>() else { continue };
            if self.transfer_retry(&master, &dest, cents, TransferMode::KeepAlive, "UPLANET:BILLET:REDEEM:FORWARD").await.is_ok() {
                self.set_state(&account, &format!("done|{}|{cents}|{}", dest.ss58(), now())).await;
                self.audit(json!({ "billet": key, "step": "done", "dest": dest.ss58(), "g1_cents": cents, "resumed": true })).await;
                done += 1;
            }
        }
        Ok(done)
    }

    /// Reprise périodique des encaissements à terminer, jusqu'à l'arrêt. (Le retour des billets expirés n'est **pas** ici : il se fait
    /// une fois par jour, à 20h12 solaire, par l'orchestrateur de nuit.)
    pub async fn run(self: Arc<Self>, mut stop: tokio::sync::watch::Receiver<bool>) {
        loop {
            tokio::select! {
                () = tokio::time::sleep(RETRY_EVERY) => {}
                _ = stop.changed() => return,
            }
            match self.resume_once().await {
                Ok(n) if n > 0 => println!("billets : {n} encaissement(s) terminé(s)"),
                Ok(_) => {}
                Err(e) => eprintln!("billets : reprise des encaissements : {e}"),
            }

        }
    }
}

#[cfg(test)]
mod tests {
    use astro_duniter::{Balance, ChainError, TransferEvent, TxOutcome};
    use astro_nostr_proto::UnsignedEvent;

    use super::*;
    use crate::coop_wallets::Coop;

    const UP: &str = "UPlanetTestName";

    /// Chaîne factice : soldes, virements enregistrés, échec d'un émetteur donné.
    struct Ledger {
        balances: Mutex<HashMap<Account, u64>>,
        sent: Mutex<Vec<(Account, Account, u64)>>,
        fail_from: Mutex<Option<Account>>,
        nonce_failures: Mutex<u32>,
    }

    impl Chain for Ledger {
        async fn balance(&self, a: &Account) -> Result<Balance, ChainError> {
            { let t = self.balances.lock().unwrap().get(a).copied().unwrap_or(0); Ok(Balance { total: Amount(t), transferable: Amount(t), unclaimed_uds: Amount(0) }) }
        }
        async fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, _: TransferMode, _: Option<&str>) -> Result<TxOutcome, ChainError> {
            {
                let mut n = self.nonce_failures.lock().unwrap();
                if *n > 0 {
                    *n -= 1;
                    return Err(ChainError::Rejected("RPC error: User error: Invalid Transaction (1010)".into()));
                }
            }
            if *self.fail_from.lock().unwrap() == Some(from.account()) {
                return Err(ChainError::Rejected("nœud hors service".into()));
            }
            let mut b = self.balances.lock().unwrap();
            *b.entry(from.account()).or_default() -= amount.0;
            *b.entry(*to).or_default() += amount.0;
            self.sent.lock().unwrap().push((from.account(), *to, amount.0));
            Ok(TxOutcome { block_hash: [9; 32], transferred: amount })
        }
        async fn watch_transfers(&self, _: tokio::sync::mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
            std::future::pending().await
        }
        fn existential_deposit(&self) -> Amount {
            Amount(100)
        }
    }

    /// Indexeur factice : primal par adresse SS58.
    struct Primals(Mutex<HashMap<String, Option<String>>>);

    impl PrimalSource for Primals {
        async fn primal_of(&self, ss58: &str) -> Result<Option<String>, String> {
            self.0.lock().unwrap().get(ss58).cloned().ok_or_else(|| "inconnu".to_owned())
        }
    }

    /// Seed et parts produits par l'algorithme de `billet_gen.sh` (coefficients `(7i+3) mod 256`, seed `1..=32`).
    const SEED: [u8; 32] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32];
    const P1: &str = "0208121c1a202a3c3248425c5a506a7c6268929c9a808abcb2a8a2dcdad0cafc";
    const P2: &str = "071621343b4a5d607f8e99aca3b2c5c8f7e60a1f1021364b546572878899ae83";
    const P3: &str = "041c302c246c705444ccd0fcf4eca0a4849c8b979fb7abefffd7cb474f577b5f";

    struct Rig {
        dir: std::path::PathBuf,
        svc: Billets<Ledger, Primals>,
        chain: Arc<Ledger>,
        primals: Arc<Primals>,
        master: Account,
        billet: Account,
        dest: Account,
        db: Arc<Db>,
    }

    async fn rig(tag: &str, billet_g1_cents: u64) -> Rig {
        let dir = std::env::temp_dir().join(format!("astroportd-billet-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let master_signer = Ed25519Signer::from_seed([0x01; 32]);
        let master = master_signer.account();
        let coop = Coop { node: Account([1; 32]), captain: Account([2; 32]), impot: Account([3; 32]), treasury: Account([4; 32]), rnd: Account([5; 32]), assets: Account([6; 32]) };
        let wallets = Arc::new(Wallets::fixed(db.clone(), Ed25519Signer::from_seed([0x44; 32]), coop).with_master(master_signer));
        let (billet, dest) = (Ed25519Signer::from_seed(SEED).account(), Account([0xD5; 32]));
        let chain = Arc::new(Ledger { balances: Mutex::new(HashMap::from([(billet, billet_g1_cents), (master, 10_000)])), sent: Mutex::new(Vec::new()), fail_from: Mutex::new(None), nonce_failures: Mutex::new(0) });
        let primals = Arc::new(Primals(Mutex::new(HashMap::from([(billet.ss58(), Some(master.ss58())), (dest.ss58(), Some(master.ss58()))]))));
        let svc = Billets::new(db.clone(), chain.clone(), wallets, Some(primals.clone()), UP.into(), ActivityBus::new());
        Rig { dir, svc, chain, primals, master, billet, dest, db }
    }

    fn body(parts: [&str; 2], dest: &Account) -> Value {
        json!({ "parts": parts, "dest": dest.ss58() })
    }

    #[test]
    fn shares_match_what_billet_gen_prints_and_any_two_rebuild_the_seed() {
        let coefs: Vec<u8> = (0..32u16).map(|i| ((i * 7 + 3) & 0xFF) as u8).collect();
        let mut it = coefs.into_iter();
        let parts = shamir::split(&SEED, || it.next().unwrap());
        assert_eq!([hex::encode(parts[0]), hex::encode(parts[1]), hex::encode(parts[2])], [P1, P2, P3]);
        for (i, j) in [(0, 1), (0, 2), (1, 2), (1, 0), (2, 0), (2, 1)] {
            assert_eq!(shamir::combine((i as u8 + 1, &parts[i]), (j as u8 + 1, &parts[j])), Some(SEED), "parts {i} et {j}");
        }
        assert_eq!(shamir::combine((1, &parts[0]), (1, &parts[1])), None, "même abscisse");
        assert!(parse_part("zz").is_err() && parse_part(&"ab".repeat(31)).is_err());
    }

    #[tokio::test]
    async fn a_billet_is_checked_then_redeemed_through_the_master_wallet_whatever_the_order_of_the_parts() {
        let r = rig("ok", 600).await;
        // P3 puis P1 : le QR ne dit pas quelle part il porte, la station retrouve l'ordre.
        let v = r.svc.handle(None, "identify", &json!({ "parts": [P3, P1] })).await.unwrap();
        assert_eq!((v["status"].as_str(), v["value_g1_cents"].as_u64(), v["zen_cents"].as_u64(), v["authentic"].as_bool()), (Some("ready"), Some(500), Some(5000), Some(true)), "{v}");
        assert_eq!(v["g1pub"], r.billet.ss58());
        let c = r.svc.handle(None, "check", &json!({ "g1pub": r.billet.ss58() })).await.unwrap();
        assert_eq!(c["value_g1_cents"], 500, "la valeur se lit aussi avec la seule adresse du billet");

        let out = r.svc.handle(None, "redeem", &body([P3, P1], &r.dest)).await.unwrap();
        assert_eq!((out["status"].as_str(), out["g1_cents"].as_u64()), (Some("done"), Some(500)), "{out}");
        assert_eq!(*r.chain.sent.lock().unwrap(), vec![(r.billet, r.master, 600), (r.master, r.dest, 500)], "billet → maître, puis maître → destinataire de (solde − 1 Ğ1)");
        assert_eq!(r.chain.balances.lock().unwrap()[&r.dest], 500);
        let again = r.svc.handle(None, "redeem", &body([P1, P2], &r.dest)).await.unwrap_err().to_string();
        assert!(again.contains("aucun billet") || again.contains("déjà"), "{again}");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_destination_unknown_to_the_uplanet_leaves_the_billet_untouched() {
        let r = rig("dest", 600).await;
        let stranger = Account([0x77; 32]);
        r.primals.0.lock().unwrap().insert(stranger.ss58(), Some(Account([0x99; 32]).ss58()));
        let err = r.svc.handle(None, "redeem", &body([P1, P2], &stranger)).await.unwrap_err();
        assert!(matches!(err, BilletError::Refused("billet_dest_unknown", _)), "{err}");
        assert!(r.chain.sent.lock().unwrap().is_empty(), "aucun virement");
        assert_eq!(r.svc.handle(None, "destination", &json!({ "g1pub": r.dest.ss58() })).await.unwrap()["ok"], true);
        assert_eq!(r.svc.handle(None, "destination", &json!({ "g1pub": stranger.ss58() })).await.unwrap()["ok"], false);
        // Une adresse jamais créditée n'a pas de primal : refusée par prudence.
        r.primals.0.lock().unwrap().insert(stranger.ss58(), None);
        assert_eq!(r.svc.handle(None, "destination", &json!({ "g1pub": stranger.ss58() })).await.unwrap()["ok"], false);
        // Sans indexeur on ne devine pas.
        let blind = Billets::<Ledger, Primals>::new(r.db.clone(), r.chain.clone(), r.svc.wallets.clone(), None, UP.into(), ActivityBus::new());
        assert!(matches!(blind.handle(None, "redeem", &body([P1, P2], &r.dest)).await, Err(BilletError::Unavailable(..))));
        assert!(r.chain.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_billet_from_another_uplanet_or_an_empty_one_is_refused() {
        let r = rig("foreign", 600).await;
        r.primals.0.lock().unwrap().insert(r.billet.ss58(), Some(Account([0x55; 32]).ss58()));
        let v = r.svc.handle(None, "check", &json!({ "g1pub": r.billet.ss58() })).await.unwrap();
        assert_eq!(v["status"], "foreign");
        assert!(matches!(r.svc.handle(None, "redeem", &body([P1, P2], &r.dest)).await, Err(BilletError::Refused(..))));
        let empty = rig("empty", 100).await;
        let v = empty.svc.handle(None, "check", &json!({ "g1pub": empty.billet.ss58() })).await.unwrap();
        assert_eq!((v["status"].as_str(), v["value_g1_cents"].as_u64()), (Some("empty"), Some(0)), "le dépôt d'activation seul ne vaut rien");
        let _ = std::fs::remove_dir_all(r.dir);
        let _ = std::fs::remove_dir_all(empty.dir);
    }

    #[tokio::test]
    async fn a_failed_final_transfer_is_resumed_by_the_station() {
        let r = rig("resume", 600).await;
        *r.chain.fail_from.lock().unwrap() = Some(r.master);
        let out = r.svc.handle(None, "redeem", &body([P1, P2], &r.dest)).await.unwrap();
        assert_eq!(out["status"], "pending", "le porteur est informé que le transfert se termine seul");
        assert_eq!(r.chain.balances.lock().unwrap()[&r.billet], 0, "billet vidé");
        let v = r.svc.handle(None, "check", &json!({ "g1pub": r.billet.ss58() })).await.unwrap();
        assert_eq!(v["status"], "pending");
        assert_eq!(r.svc.resume_once().await.unwrap(), 0, "chaîne encore en panne : rien de perdu, on réessaiera");
        *r.chain.fail_from.lock().unwrap() = None;
        assert_eq!(r.svc.resume_once().await.unwrap(), 1);
        assert_eq!(r.chain.balances.lock().unwrap()[&r.dest], 500);
        assert_eq!(r.svc.resume_once().await.unwrap(), 0, "une seule fois");
        assert_eq!(r.svc.handle(None, "check", &json!({ "g1pub": r.billet.ss58() })).await.unwrap()["status"], "redeemed");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_failed_drain_keeps_the_billet_valid() {
        let r = rig("drain", 600).await;
        *r.chain.fail_from.lock().unwrap() = Some(r.billet);
        let err = r.svc.handle(None, "redeem", &body([P1, P2], &r.dest)).await.unwrap_err();
        assert!(matches!(err, BilletError::Failed(..)));
        *r.chain.fail_from.lock().unwrap() = None;
        assert_eq!(r.svc.handle(None, "redeem", &body([P1, P2], &r.dest)).await.unwrap()["status"], "done", "un nouvel essai reste possible");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn an_expired_billet_cannot_be_redeemed_by_the_bearer() {
        let r = rig("exp", 600).await;
        let (secret, coop_hex) = crate::coop_config::coop_keys(UP).unwrap();
        let tags = |exp: u64| vec![vec!["d".into(), format!("billet:{}", r.billet.ss58())], vec!["u".into(), exp.to_string()]];
        let put = |exp: u64, at: u64| {
            let ev = UnsignedEvent { created_at: at, kind: 31903, tags: tags(exp), content: String::new() }.sign(&secret).unwrap();
            let db = r.db.clone();
            async move { db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &ev).map(|_| ())).await.unwrap() }
        };
        put(now() + 3600, 10).await;
        let v = r.svc.handle(None, "check", &json!({ "g1pub": r.billet.ss58() })).await.unwrap();
        assert_eq!((v["status"].as_str(), v["expired"].as_bool()), (Some("ready"), Some(false)));
        assert!(v["expires_at"].as_u64().unwrap() > now());
        put(now() - 10, 20).await;
        assert_eq!(r.svc.handle(None, "check", &json!({ "g1pub": r.billet.ss58() })).await.unwrap()["status"], "expired");
        let err = r.svc.handle(None, "redeem", &body([P1, P2], &r.dest)).await.unwrap_err().to_string();
        assert!(err.contains("expiré"), "{err}");
        assert!(r.chain.sent.lock().unwrap().is_empty());
        // Une date signée par un autre que la coopérative ne compte pas.
        let other = [9u8; 32];
        let forged = UnsignedEvent { created_at: 30, kind: 31903, tags: tags(now() + 99_999), content: String::new() }.sign(&other).unwrap();
        r.db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &forged).map(|_| ())).await.unwrap();
        assert_eq!(r.svc.handle(None, "check", &json!({ "g1pub": r.billet.ss58() })).await.unwrap()["status"], "expired", "seule la clé de la coopérative fait foi ({coop_hex})");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn requests_are_limited_per_address_and_bad_input_is_named() {
        let r = rig("limit", 600).await;
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        for _ in 0..READS_PER_MIN {
            r.svc.handle(Some(ip), "check", &json!({ "g1pub": r.billet.ss58() })).await.unwrap();
        }
        assert!(matches!(r.svc.handle(Some(ip), "check", &json!({ "g1pub": r.billet.ss58() })).await, Err(BilletError::Limited(..))));
        assert!(r.svc.handle(None, "check", &json!({ "g1pub": "pas une adresse" })).await.is_err());
        assert!(matches!(r.svc.handle(None, "identify", &json!({ "parts": [P1, P1] })).await, Err(BilletError::Invalid(..))), "deux fois la même part");
        assert!(matches!(r.svc.handle(None, "identify", &json!({ "parts": [P1, "xyz"] })).await, Err(BilletError::Invalid(..))));
        assert!(matches!(r.svc.handle(None, "identify", &json!({ "parts": [P1, &"ab".repeat(32)] })).await, Err(BilletError::Refused(..))), "deux parts d'aucun billet");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    const ISSUER_HEX: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ISSUER_EMAIL: &str = "emetteur@exemple.org";

    /// Service avec création de billets : un MULTIPASS émetteur (5 Ğ1), une clé NODE, un témoin rangé dans la base.
    async fn issuing_rig(tag: &str) -> (Rig, Billets<Ledger, Primals>, Account, Arc<Mutex<Vec<astro_nostr_proto::Event>>>) {
        let r = rig(tag, 0).await;
        let dk = r.dir.join("nostr").join(ISSUER_EMAIL);
        std::fs::create_dir_all(&dk).unwrap();
        std::fs::write(dk.join(".secret.dunikey"), astro_lifecycle::dunikey::render(&[0x61; 32])).unwrap();
        let wallet = Ed25519Signer::from_seed([0x61; 32]).account();
        r.chain.balances.lock().unwrap().insert(wallet, 5_000);
        r.primals.0.lock().unwrap().insert(wallet.ss58(), Some(r.master.ss58()));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (db, sent2) = (r.db.clone(), sent.clone());
        let publish: PublishFn = Arc::new(move |e| {
            let (db, sent) = (db.clone(), sent2.clone());
            Box::pin(async move {
                sent.lock().unwrap().push(e.clone());
                db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &e).map(|_| ())).await.map_err(|x| x.to_string())
            })
        });
        let svc = Billets::new(r.db.clone(), r.chain.clone(), r.svc.wallets.clone(), Some(r.primals.clone()), UP.into(), ActivityBus::new())
            .with_issuing(Issuing { accounts_dir: r.dir.join("nostr"), node_secret: Zeroizing::new([0x71; 32]), public_base: Some("https://station.exemple".into()), publish })
            .with_retry_delay(Duration::ZERO);
        (r, svc, wallet, sent)
    }

    fn sheet_part(b: &Value, k: &str) -> String {
        b[k].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn only_the_issuers_the_captain_chose_can_create_billets() {
        let (r, svc, _, _) = issuing_rig("perm").await;
        let ask = json!({ "count": 1, "amount_zen": 50 });
        let err = svc.issue(ISSUER_HEX, ISSUER_EMAIL, false, &ask).await.unwrap_err();
        assert!(matches!(err, BilletError::Refused("billet_not_issuer", _)), "{err}");
        assert!(svc.issue("bb".repeat(32).as_str(), "autre@exemple.org", true, &ask).await.is_err(), "le Capitaine est autorisé mais son portefeuille manque ici");
        svc.set_issuer(ISSUER_HEX, true).await.unwrap();
        assert!(svc.is_issuer(ISSUER_HEX, false).await);
        svc.issue(ISSUER_HEX, ISSUER_EMAIL, false, &ask).await.unwrap();
        svc.set_issuer(ISSUER_HEX, false).await.unwrap();
        assert!(!svc.is_issuer(ISSUER_HEX, false).await, "l'autorisation se retire");
        assert!(matches!(svc.fund(ISSUER_HEX, ISSUER_EMAIL, false, &r.billet.ss58()).await, Err(BilletError::Refused("billet_not_issuer", _))));
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn an_unfunded_billet_is_deleted_from_nostr_but_never_a_funded_one() {
        let (r, svc, _, sent) = issuing_rig("delete").await;
        svc.set_issuer(ISSUER_HEX, true).await.unwrap();
        let out = svc.issue(ISSUER_HEX, ISSUER_EMAIL, false, &json!({ "count": 2, "amount_zen": 50 })).await.unwrap();
        let (a, b): (Account, Account) = (out["billets"][0]["g1pub"].as_str().unwrap().parse().unwrap(), out["billets"][1]["g1pub"].as_str().unwrap().parse().unwrap());
        assert_eq!(svc.issued(ISSUER_HEX).await.unwrap()["billets"].as_array().unwrap().len(), 2);
        // Un autre émetteur ne supprime pas le billet d'autrui ; un inconnu non plus.
        svc.set_issuer(&"cc".repeat(32), true).await.unwrap();
        assert!(matches!(svc.delete(&"cc".repeat(32), false, &a.ss58()).await, Err(BilletError::Refused("billet_not_yours", _))));
        assert!(matches!(svc.delete(ISSUER_HEX, false, &Account([9; 32]).ss58()).await, Err(BilletError::Refused("billet_unknown", _))));
        assert!(matches!(svc.delete("dd".repeat(32).as_str(), false, &a.ss58()).await, Err(BilletError::Refused("billet_not_issuer", _))));
        // Non financé : supprimé, un kind 5 signé par la coopérative cite son adresse, le témoin disparaît (l'autre reste).
        let done = svc.delete(ISSUER_HEX, false, &a.ss58()).await.unwrap();
        assert_eq!((done["ok"].as_bool(), done["g1_cents_left"].as_u64()), (Some(true), Some(0)));
        let del = sent.lock().unwrap().last().unwrap().clone();
        assert_eq!(del.kind, 5);
        assert!(del.tag_value("a").unwrap().ends_with(&format!(":billet:{}", a.ss58())), "{:?}", del.tags);
        let left: Vec<String> = svc.issued(ISSUER_HEX).await.unwrap()["billets"].as_array().unwrap().iter().map(|x| x["g1pub"].as_str().unwrap().to_owned()).collect();
        assert_eq!(left, vec![b.ss58()], "seul le billet supprimé a disparu");
        assert!(!svc.addresses().await.contains(&a.ss58()));
        // Financé : refusé, rien n'est publié.
        svc.fund(ISSUER_HEX, ISSUER_EMAIL, false, &b.ss58()).await.unwrap();
        let before = sent.lock().unwrap().len();
        assert!(matches!(svc.delete(ISSUER_HEX, false, &b.ss58()).await, Err(BilletError::Refused("billet_not_empty", _))));
        assert_eq!(sent.lock().unwrap().len(), before);
        // Le Capitaine peut supprimer un billet non financé d'un autre émetteur.
        let c = svc.issue(ISSUER_HEX, ISSUER_EMAIL, false, &json!({ "count": 1, "amount_zen": 10 })).await.unwrap();
        let c: Account = c["billets"][0]["g1pub"].as_str().unwrap().parse().unwrap();
        assert!(svc.delete("ee".repeat(32).as_str(), true, &c.ss58()).await.is_ok());
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn a_billet_is_issued_funded_once_and_then_cashed_with_its_printed_parts() {
        let (r, svc, wallet, sent) = issuing_rig("issue").await;
        svc.set_issuer(ISSUER_HEX, true).await.unwrap();
        let out = svc.issue(ISSUER_HEX, ISSUER_EMAIL, false, &json!({ "count": 2, "amount_zen": 50 })).await.unwrap();
        let billets = out["billets"].as_array().unwrap();
        assert_eq!(billets.len(), 2);
        let b = &billets[0];
        let (p1, p2) = (sheet_part(b, "p1"), sheet_part(b, "p2"));
        assert!(b["qr_p1"].as_str().unwrap().contains("<svg") && b["qr_verify"].as_str().unwrap().contains("<svg"));
        assert!(b["verify_url"].as_str().unwrap().starts_with("https://station.exemple/scan?g1pub=g1"));
        assert_eq!(out["style"], json!({ "background": null, "icon": null }), "pas de profil : pas de fond");
        assert_eq!(b["return_to_issuer"], false);
        // Sans retour automatique, le témoin ne contient rien de secret.
        let ev = sent.lock().unwrap()[0].clone();
        assert!(ev.content.is_empty() && ev.tag_value("p") == Some(ISSUER_HEX) && ev.tag_value("a") == Some("50") && ev.tag_value("u").is_none());
        assert!(!serde_json::to_string(&*sent.lock().unwrap()).unwrap().contains(&p2), "P2 n'est jamais publiée");
        let g1pub: Account = b["g1pub"].as_str().unwrap().parse().unwrap();
        let st = svc.stats().await;
        assert_eq!((st["issued"].as_u64(), st["funded"].as_u64(), st["circulating"].as_u64()), (Some(2), Some(0), Some(0)), "créés mais pas financés : pas en circulation");

        // Financement : 1 Ğ1 d'activation du maître, puis 5 Ğ1 (50 ẑ) du portefeuille de l'émetteur ; un nonce refusé est réessayé.
        *r.chain.nonce_failures.lock().unwrap() = 1;
        let f = svc.fund(ISSUER_HEX, ISSUER_EMAIL, false, &g1pub.ss58()).await.unwrap();
        assert_eq!((f["ok"].as_bool(), f["already"].as_bool(), f["g1_cents"].as_u64()), (Some(true), Some(false), Some(600)), "{f}");
        assert_eq!(*r.chain.sent.lock().unwrap(), vec![(r.master, g1pub, 100), (wallet, g1pub, 500)]);
        assert_eq!(svc.fund(ISSUER_HEX, ISSUER_EMAIL, false, &g1pub.ss58()).await.unwrap()["already"], true, "rappelable sans rien renvoyer");
        assert_eq!(r.chain.sent.lock().unwrap().len(), 2);
        let st = svc.stats().await;
        assert_eq!((st["funded"].as_u64(), st["circulating"].as_u64(), st["circulating_zen"].as_u64()), (Some(1), Some(1), Some(50)), "{st}");
        assert!(svc.addresses().await.contains(&g1pub.ss58()), "la carte reconnaît les billets");
        // Un autre émetteur ne peut pas financer (ni déclencher l'activation gratuite) d'un billet qui n'est pas le sien.
        svc.set_issuer(&"cc".repeat(32), true).await.unwrap();
        assert!(matches!(svc.fund(&"cc".repeat(32), ISSUER_EMAIL, false, &g1pub.ss58()).await, Err(BilletError::Refused("billet_not_yours", _))));
        assert!(matches!(svc.fund(ISSUER_HEX, ISSUER_EMAIL, false, &Account([9; 32]).ss58()).await, Err(BilletError::Refused("billet_not_yours", _))), "billet inconnu");

        // Le porteur l'encaisse avec les deux QR imprimés.
        r.primals.0.lock().unwrap().insert(g1pub.ss58(), Some(r.master.ss58()));
        let out = svc.handle(None, "redeem", &json!({ "parts": [p2, p1], "dest": r.dest.ss58() })).await.unwrap();
        assert_eq!((out["status"].as_str(), out["g1_cents"].as_u64()), (Some("done"), Some(500)), "{out}");
        let st = svc.stats().await;
        assert_eq!((st["redeemed"].as_u64(), st["circulating"].as_u64()), (Some(1), Some(0)), "{st}");
        let list = svc.issued(ISSUER_HEX).await.unwrap();
        assert_eq!(list["billets"].as_array().unwrap().len(), 2);
        assert!(list["billets"].as_array().unwrap().iter().any(|x| x["status"] == "redeemed" && x["amount_zen"] == 50));
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn the_return_option_keeps_p2_and_p3_encrypted_and_sends_the_value_back_when_the_billet_expires() {
        let (r, svc, wallet, sent) = issuing_rig("return").await;
        svc.set_issuer(ISSUER_HEX, true).await.unwrap();
        assert!(matches!(svc.issue(ISSUER_HEX, ISSUER_EMAIL, false, &json!({ "amount_zen": 50, "return_to_issuer": true })).await, Err(BilletError::Invalid("billet_return_needs_days", _))));
        let out = svc.issue(ISSUER_HEX, ISSUER_EMAIL, false, &json!({ "count": 1, "amount_zen": 50, "valid_days": 30, "return_to_issuer": true })).await.unwrap();
        let b = &out["billets"][0];
        let (p1, p2, g1pub): (String, String, Account) = (sheet_part(b, "p1"), sheet_part(b, "p2"), b["g1pub"].as_str().unwrap().parse().unwrap());
        let ev = sent.lock().unwrap()[0].clone();
        assert_eq!((ev.tag_value("r"), ev.tag_value("u").is_some()), (Some("1"), true));
        assert!(!ev.content.is_empty() && !ev.content.contains(&p2), "P2 et P3 sont chiffrées dans le témoin");
        assert!(astro_crypto::nip44::decrypt_from_self(&[0x99; 32], &astro_nostr_proto::pubkey_of(&[0x99; 32]).unwrap(), &ev.content).is_err(), "seul le NODE les déchiffre");
        svc.fund(ISSUER_HEX, ISSUER_EMAIL, false, &g1pub.ss58()).await.unwrap();
        r.primals.0.lock().unwrap().insert(g1pub.ss58(), Some(r.master.ss58()));
        assert_eq!(svc.sweep_once().await.unwrap(), 0, "pas avant la date de fin");
        let ex = svc.handle(None, "check", &json!({ "g1pub": g1pub.ss58() })).await.unwrap();
        assert_eq!((ex["status"].as_str(), ex["expires_at"].as_u64().is_some()), (Some("ready"), true));

        // La date de fin passe : le porteur ne peut plus encaisser, la station renvoie la valeur à l'émetteur.
        r.db.writer().execute("UPDATE nostr_tags SET value = '1' WHERE name = 'u'", vec![]).await.unwrap();
        let err = svc.handle(None, "redeem", &json!({ "parts": [p1, p2], "dest": r.dest.ss58() })).await.unwrap_err();
        assert!(matches!(err, BilletError::Refused("billet_expired", _)), "{err}");
        let before = r.chain.balances.lock().unwrap()[&wallet];
        assert_eq!(svc.sweep_once().await.unwrap(), 1);
        assert_eq!(r.chain.balances.lock().unwrap()[&wallet], before + 500, "l'émetteur retrouve exactement la valeur du billet");
        assert_eq!(r.chain.balances.lock().unwrap()[&g1pub], 0);
        assert_eq!(svc.sweep_once().await.unwrap(), 0, "une seule fois");
        let st = svc.stats().await;
        assert_eq!((st["returned"].as_u64(), st["redeemed"].as_u64(), st["circulating"].as_u64()), (Some(1), Some(0), Some(0)), "renvoyé à l'émetteur, pas encaissé : {st}");
        assert_eq!(svc.handle(None, "check", &json!({ "g1pub": g1pub.ss58() })).await.unwrap()["status"], "returned");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn the_issuers_profile_images_become_the_background_and_icon() {
        let (r, svc, _, _) = issuing_rig("style").await;
        svc.set_issuer(ISSUER_HEX, true).await.unwrap();
        let img = |w, h| {
            let mut out = Vec::new();
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 255) as u8, (y % 255) as u8, 9]))).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
            out
        };
        use crate::profile_image::{Kind, PublicImages, normalize, url_for};
        let secret = [0xAA; 32];
        let hex = astro_nostr_proto::pubkey_of(&secret).unwrap();
        let images = Arc::new(PublicImages::new(r.db.clone(), Arc::new(astro_dav::FsBlobStore::open(r.dir.join("blobs")).unwrap()), None, None));
        let svc = svc.with_public_images(images.clone());
        let banner = images.put(&hex, normalize(&img(1600, 500), Kind::Banner).unwrap()).await.unwrap();
        let icon = images.put(&hex, normalize(&img(200, 200), Kind::Picture).unwrap()).await.unwrap();
        // Le profil (kind 0) de l'émetteur pointe vers ces deux images (blobs publics, adressées par leur CID).
        let profile = json!({ "name": "Émetteur", "banner": url_for("https://station.exemple", &banner), "picture": url_for("https://station.exemple", &icon) });
        let ev = UnsignedEvent { created_at: 10, kind: 0, tags: vec![], content: profile.to_string() }.sign(&secret).unwrap();
        r.db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &ev).map(|_| ())).await.unwrap();
        let style = svc.issuer_style(&hex).await;
        assert!(style["background"].as_str().unwrap().starts_with("data:image/jpeg;base64,") && style["icon"].as_str().unwrap().starts_with("data:image/jpeg;base64,"), "{style}");
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[tokio::test]
    async fn bad_sheet_requests_are_named() {
        let (r, svc, _, _) = issuing_rig("bad").await;
        svc.set_issuer(ISSUER_HEX, true).await.unwrap();
        for (body, code) in [(json!({ "count": 0 }), "billet_bad_count"), (json!({ "count": 7 }), "billet_bad_count"), (json!({ "amount_zen": MAX_ZEN_PER_BILLET + 1 }), "billet_bad_amount"), (json!({ "valid_days": 0 }), "billet_bad_days")] {
            let err = svc.issue(ISSUER_HEX, ISSUER_EMAIL, false, &body).await.unwrap_err();
            assert!(matches!(&err, BilletError::Invalid(c, _) if *c == code), "{body} → {err}");
        }
        let _ = std::fs::remove_dir_all(r.dir);
    }

    #[test]
    fn every_error_code_of_the_station_is_translated_by_the_scan_page() {
        let (code, fr) = (include_str!("billet.rs"), include_str!("../crates/astro-ui/web/scan-i18n.js"));
        let (en, es) = (include_str!("../crates/astro-ui/web/scan-lang-en.js"), include_str!("../crates/astro-ui/web/scan-lang-es.js"));
        let body = code.split("#[cfg(test)]").next().unwrap();
        let mut seen = 0;
        for part in body.split("\"billet_").skip(1) {
            let name = part.split('"').next().unwrap();
            // Les codes de la création de billets s'affichent dans l'application des membres (qui traduit le message), pas sur /scan.
            const ISSUER_SIDE: [&str; 13] = ["funded", "no_wallet", "not_issuer", "bad_count", "bad_amount", "bad_days", "return_needs_days", "not_yours", "blank", "issuer_poor", "fund_failed", "unknown", "not_empty"];
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c == '_') || ISSUER_SIDE.contains(&name) {
                continue;
            }
            seen += 1;
            for (lang, text) in [("fr", fr), ("en", en), ("es", es)] {
                assert_eq!(text.matches(&format!("billet_{name}:")).count(), 1, "le code billet_{name} doit être traduit ({lang})");
            }
        }
        assert!(seen >= 20, "codes trouvés : {seen}");
    }
}
