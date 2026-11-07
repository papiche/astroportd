//! Écoute permanente des virements Ğ1 (ẐEN) reçus par les comptes de la station.
//!
//! Le titulaire est prévenu **dès le bloc** : flux d'activité personnel (jamais vu des autres membres) et message
//! privé de BRO s'il est hors ligne. Chaque virement est aussi inscrit au journal (`journal`, flux `funds`), une seule
//! fois même si la chaîne rejoue un bloc après une reconnexion.
//!
//! **Contrôle primal** (équivalent de `tools/primal_wallet_control.sh`) : le « primal » d'un portefeuille est l'émetteur
//! de son tout premier virement reçu. Un virement est de **cette UPlanet** quand son expéditeur, ou le primal de celui-ci,
//! est le portefeuille maître `UPLANETNAME_G1` (ou un portefeuille coopératif de la station). Sinon il vient de l'extérieur :
//! il n'est pas crédité en Ẑen, il est signalé au titulaire et au Capitaine et, quand la primale étrangère est **prouvée**
//! (accord de deux indexeurs), il est **renvoyé vers `INTRUSION`** ([`crate::intrusion_guard`], branché par
//! [`FundsWatch::with_guard`]) pour que les comptes en Ẑen restent justes. Les portefeuilles coopératifs de la station
//! (sauf `INTRUSION`, le maître et le NODE) sont surveillés aussi.
//! L'historique d'un portefeuille n'est pas dans le nœud Duniter : le primal se lit auprès d'un indexeur (squid,
//! `--squid-url`). Sans indexeur, le virement est annoncé « origine non vérifiée », jamais déclaré valide.

use std::collections::HashMap;
use std::future::Future;
use std::str::FromStr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use astro_core::formula::{FormulaInput, KeyFormula};
use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_duniter::{Account, Chain, ChainError, FundsNotifier, IncomingFunds};
use rusqlite::types::Value;
use serde_json::json;
use tokio::sync::{broadcast, watch};

use crate::fetch::{FetchLimits, post_json};
use crate::station::ZEN_PER_G1;
use crate::upload_worker::Notify;

/// Montant (en centimes de Ğ1) du virement de validation envoyé par un membre de la toile Ğ1 (UPassport).
pub const WOT_VALIDATION_CENTS: u64 = 1;
/// Période de relecture des comptes à surveiller.
const REFRESH: Duration = Duration::from_secs(30);
/// Période de la sonde du nœud Duniter.
const PROBE: Duration = Duration::from_secs(60);
/// Réponse de l'indexeur : 256 Ko au plus.
const SQUID_MAX_BYTES: u64 = 256 * 1024;

/// Portefeuille maître de la constellation : `UPLANETNAME_G1`, clé coopérative dont le sel est `<UPLANETNAME>.G1`.
pub fn master_wallet(uplanet_name: &str) -> Result<String, String> {
    let input = FormulaInput { salt: "", pepper: "", uplanet_name, coop_suffix: Some("G1") };
    let credentials = KeyFormula::CoopPrefix.credentials(&input).map_err(|e| e.to_string())?;
    credentials.seed().map_err(|e| e.to_string())?.g1_ss58().map_err(|e| e.to_string())
}

/// Source du « primal » d'un portefeuille (émetteur de son premier virement reçu).
pub trait PrimalSource: Send + Sync + 'static {
    /// `Ok(None)` : le portefeuille n'a jamais rien reçu.
    fn primal_of(&self, ss58: &str) -> impl Future<Output = Result<Option<String>, String>> + Send;
}

/// Indexeurs squid (GraphQL) d'un nœud Duniter v2s : le premier qui répond sert, les suivants prennent le relais.
/// La liste peut être remplacée en marche (découverte périodique, voir [`crate::duniter_nodes`]).
pub struct SquidPrimal {
    urls: Arc<RwLock<Vec<String>>>,
    limits: FetchLimits,
}

impl SquidPrimal {
    /// Indexeurs à `urls` (`https://…/v1/graphql`), par ordre de préférence.
    pub fn new(urls: Vec<String>) -> Self {
        let mut limits = FetchLimits::new(SQUID_MAX_BYTES);
        limits.timeout = Duration::from_secs(15);
        Self { urls: Arc::new(RwLock::new(urls)), limits }
    }

    /// Liste partagée : y écrire la remplace pour les prochains appels.
    pub fn urls_handle(&self) -> Arc<RwLock<Vec<String>>> {
        self.urls.clone()
    }
}

/// Corps de la requête : la première transaction reçue par `wallet`.
pub fn primal_query(wallet: &str) -> String {
    json!({
        // Deux virements : s'ils sont dans le même bloc et viennent de deux émetteurs, la primale est ambiguë.
        "query": "query($w:String!){transfers(condition:{toId:$w},orderBy:BLOCK_NUMBER_ASC,first:2){nodes{fromId blockNumber}}}",
        "variables": { "w": wallet },
    })
    .to_string()
}

/// Cache des primales **vérifiées par accord d'indexeurs** (immuables : l'émetteur d'un premier virement ne change jamais).
/// L'ancien cache `primal`, rempli sur la foi d'un seul indexeur, n'est plus cru.
pub const PRIMAL_SCOPE: &str = "primal_q";

/// Lit la réponse de l'indexeur. Une erreur GraphQL est une erreur, pas « aucun primal » ; deux émetteurs différents dans le
/// premier bloc rendent la primale ambiguë (erreur : jamais tranché au hasard).
pub fn parse_primal(body: &[u8]) -> Result<Option<String>, String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| format!("réponse illisible : {e}"))?;
    if let Some(msg) = v["errors"][0]["message"].as_str() {
        return Err(format!("erreur de l'indexeur : {msg}"));
    }
    let nodes = v["data"]["transfers"]["nodes"].as_array().ok_or("réponse sans data.transfers.nodes")?;
    let first = nodes.first().and_then(|n| n["fromId"].as_str()).filter(|s| !s.is_empty()).map(str::to_owned);
    if let (Some(a), Some(b)) = (nodes.first(), nodes.get(1)) {
        if a["blockNumber"] == b["blockNumber"] && a["fromId"] != b["fromId"] {
            return Err(format!("primale ambiguë : deux émetteurs dans le premier bloc ({})", a["blockNumber"]));
        }
    }
    Ok(first)
}

/// Primale de `wallet` tenue pour sûre : **au moins deux indexeurs** donnent la même réponse et aucun ne la contredit (un seul
/// suffit s'il n'y en a qu'un de configuré). Un indexeur en retard (« aucun virement ») ou incomplet contredit les autres : le
/// résultat est alors une erreur — « non vérifié » —, jamais une primale fausse gardée pour toujours.
pub async fn primal_consensus(urls: &[String], limits: &FetchLimits, wallet: &str) -> Result<Option<String>, String> {
    if urls.is_empty() {
        return Err("aucun indexeur disponible".into());
    }
    let need = if urls.len() == 1 { 1 } else { 2 };
    let query = primal_query(wallet);
    let (mut answers, mut errors): (Vec<Option<String>>, Vec<String>) = (Vec::new(), Vec::new());
    for url in urls {
        match post_json(url, &query, limits).await.map_err(|e| e.to_string()).and_then(|b| parse_primal(&b)) {
            Ok(a) => {
                if answers.iter().any(|x| *x != a) {
                    return Err(format!("indexeurs en désaccord sur la primale de {} : {:?} / {:?}", wallet.chars().take(8).collect::<String>(), answers[0], a));
                }
                answers.push(a);
                if answers.len() >= need {
                    return Ok(answers.swap_remove(0));
                }
            }
            Err(e) => errors.push(format!("{url} : {e}")),
        }
    }
    Err(format!("primale non confirmée : {} réponse(s) sur {need} exigées ({})", answers.len(), errors.join(" ; ")))
}

impl PrimalSource for SquidPrimal {
    async fn primal_of(&self, ss58: &str) -> Result<Option<String>, String> {
        let urls = self.urls.read().unwrap_or_else(|e| e.into_inner()).clone();
        primal_consensus(&urls, &self.limits, ss58).await
    }
}

/// Origine d'un virement reçu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// De cette UPlanet : l'expéditeur, ou son primal, est le portefeuille maître.
    SameUplanet,
    /// 0,01 Ğ1 : validation du portefeuille par un membre de la toile Ğ1. Invisible dans le solde Ẑen.
    WotValidation,
    /// D'une autre origine : non crédité en Ẑen.
    Foreign(Option<String>),
    /// Contrôle impossible (indexeur absent ou injoignable) : raison.
    Unverified(String),
}

impl Verdict {
    fn label(&self) -> &'static str {
        match self {
            Self::SameUplanet => "same_uplanet",
            Self::WotValidation => "wot_validation",
            Self::Foreign(_) => "foreign",
            Self::Unverified(_) => "unverified",
        }
    }
}

/// Qui reçoit : le titulaire d'un portefeuille surveillé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// Clé Nostr du compte.
    pub hex: String,
    /// Adresse du compte, si connue.
    pub email: Option<String>,
    /// `multipass` ou `zencard`.
    pub role: String,
}

/// Surveillance des virements entrants.
pub struct FundsWatch<P: PrimalSource> {
    db: Arc<Db>,
    activity: ActivityBus,
    notifier: Arc<FundsNotifier>,
    holders: RwLock<HashMap<Account, Holder>>,
    master: String,
    primal: Option<Arc<P>>,
    notify: Option<Notify>,
    nostr_dir: Option<std::path::PathBuf>,
    bad_wallets: Mutex<usize>,
    guard: Option<(Arc<crate::coop_wallets::Wallets>, crate::intrusion_guard::FixFn)>,
    coop: RwLock<HashMap<Account, String>>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn amount(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

fn short(ss58: &str) -> String {
    let n = ss58.chars().count();
    if n <= 12 { ss58.to_owned() } else { format!("{}…{}", ss58.chars().take(5).collect::<String>(), ss58.chars().skip(n - 4).collect::<String>()) }
}

impl<P: PrimalSource> FundsWatch<P> {
    /// Surveillance pour `master` (portefeuille `UPLANETNAME_G1`). `primal` : indexeur, s'il est configuré.
    pub fn new(db: Arc<Db>, activity: ActivityBus, master: String, primal: Option<Arc<P>>, notify: Option<Notify>, nostr_dir: Option<std::path::PathBuf>) -> Self {
        Self { db, activity, notifier: Arc::new(FundsNotifier::new([])), holders: RwLock::new(HashMap::new()), master, primal, notify, nostr_dir, bad_wallets: Mutex::new(0), guard: None, coop: RwLock::new(HashMap::new()) }
    }

    /// Surveille aussi les portefeuilles coopératifs de `wallets` et renvoie vers `INTRUSION`, par `fix`, tout virement dont
    /// la primale étrangère est prouvée.
    pub fn with_guard(mut self, wallets: Arc<crate::coop_wallets::Wallets>, fix: crate::intrusion_guard::FixFn) -> Self {
        self.guard = Some((wallets, fix));
        self
    }

    /// Titulaires actuellement surveillés.
    pub fn watched(&self) -> usize {
        self.holders.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Relit les comptes hébergés : Ğ1 du MULTIPASS (`g1pub_nostr`, sinon le fichier `G1PUBNOSTR`) et de la ZenCard (`g1pub`).
    pub async fn refresh_accounts(&self) -> Result<usize, String> {
        type Row = (String, String, Option<String>, Option<String>, Option<String>);
        let rows: Vec<Row> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT hex, role, email, g1pub_nostr, g1pub FROM identities WHERE hosted = 1 AND role IN ('multipass','zencard','captain')")?;
                Ok(stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(|e| format!("base : {e}"))?;
        let mut map = HashMap::new();
        let mut bad = 0usize;
        for (hex, role, email, g1_nostr, g1) in rows {
            let from_file = || -> Option<String> {
                let path = self.nostr_dir.as_ref()?.join(email.as_deref()?).join("G1PUBNOSTR");
                std::fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
            };
            let multipass = g1_nostr.filter(|s| !s.is_empty()).or_else(from_file);
            for (text, kind) in [(multipass, if role == "zencard" { "zencard" } else { "multipass" }), (g1.filter(|s| !s.is_empty()), "zencard")] {
                let Some(text) = text else { continue };
                match Account::from_str(&text) {
                    Ok(account) => {
                        map.insert(account, Holder { hex: hex.clone(), email: email.clone(), role: kind.to_owned() });
                    }
                    Err(_) => bad += 1,
                }
            }
        }
        {
            // Une adresse illisible ne doit pas passer sous silence : une seule alerte par changement du nombre.
            let mut last = self.bad_wallets.lock().unwrap_or_else(|e| e.into_inner());
            if bad != *last && bad > 0 {
                self.activity.publish_to(Audience::Captain, "alert", "funds", format!("{bad} portefeuille(s) Ğ1 illisible(s) dans les comptes hébergés : leurs virements ne sont pas surveillés"), None);
            }
            *last = bad;
        }
        // Portefeuilles coopératifs : surveillés comme les comptes (INTRUSION reçoit par construction de l'extérieur ; le
        // NODE n'a pas de signataire ici ; le maître n'est jamais dans la liste).
        let mut coop = HashMap::new();
        if let Some((wallets, _)) = &self.guard {
            match wallets.targets().await {
                Ok(targets) => {
                    for (label, account) in targets.iter() {
                        coop.insert(*account, label.clone());
                        if label != "INTRUSION" && label != "NODE" {
                            map.entry(*account).or_insert_with(|| Holder { hex: String::new(), email: Some(label.clone()), role: "coop".into() });
                        }
                    }
                }
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "funds", format!("portefeuilles coopératifs illisibles : {e} ; leurs virements ne sont pas surveillés"), None);
                }
            }
        }
        *self.coop.write().unwrap_or_else(|e| e.into_inner()) = coop;
        let n = map.len();
        self.notifier.set_accounts(map.keys().copied());
        *self.holders.write().unwrap_or_else(|e| e.into_inner()) = map;
        Ok(n)
    }

    /// Origine du virement.
    pub async fn classify(&self, from: &Account, amount_cents: u64) -> Verdict {
        if amount_cents == WOT_VALIDATION_CENTS {
            return Verdict::WotValidation;
        }
        if self.coop.read().unwrap_or_else(|e| e.into_inner()).contains_key(from) {
            return Verdict::SameUplanet; // virement interne à la coopérative
        }
        let from = from.ss58();
        if from == self.master {
            return Verdict::SameUplanet;
        }
        let Some(source) = &self.primal else { return Verdict::Unverified("aucun indexeur configuré (--squid-url)".into()) };
        if let Some(cached) = self.cached_primal(&from).await {
            return Self::compare(&self.master, Some(cached));
        }
        match source.primal_of(&from).await {
            Ok(primal) => {
                if let Some(p) = &primal {
                    self.cache_primal(&from, p).await; // l'émetteur d'un premier virement ne change jamais
                }
                Self::compare(&self.master, primal)
            }
            Err(e) => Verdict::Unverified(e),
        }
    }

    fn compare(master: &str, primal: Option<String>) -> Verdict {
        match primal {
            Some(p) if p == master => Verdict::SameUplanet,
            other => Verdict::Foreign(other),
        }
    }

    async fn cached_primal(&self, who: &str) -> Option<String> {
        let who = who.to_owned();
        self.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [PRIMAL_SCOPE, who.as_str()], |r| r.get::<_, String>(0)))?)).await.ok().flatten()
    }

    async fn cache_primal(&self, who: &str, primal: &str) {
        let _ = self
            .db
            .writer()
            .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO NOTHING", vec![PRIMAL_SCOPE.to_owned().into(), who.to_owned().into(), primal.to_owned().into(), now().into()])
            .await;
    }

    /// Traite un virement : journal (une seule fois), flux d'activité du titulaire, message privé.
    pub async fn handle(&self, funds: IncomingFunds) {
        let holder = self.holders.read().unwrap_or_else(|e| e.into_inner()).get(&funds.account).cloned();
        let Some(holder) = holder else { return };
        let (to, from) = (funds.account.ss58(), funds.from.ss58());
        let key = format!("{}:{from}:{to}:{}", funds.block_number, funds.amount.0);
        match self.db.writer().execute("INSERT OR IGNORE INTO claimed(owner, event_id, at) VALUES ('funds', ?1, ?2)", vec![key.into(), now().into()]).await {
            Ok(r) if r.changes == 0 => return, // bloc rejoué après une reconnexion
            Ok(_) => {}
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "funds", format!("virement reçu mais non journalisé ({e})"), None);
            }
        }
        let cents = funds.amount.0;
        let verdict = self.classify(&funds.from, cents).await;
        let zen_cents = if verdict == Verdict::SameUplanet { cents.saturating_mul(ZEN_PER_G1) } else { 0 };
        let record = json!({
            "owner": holder.hex, "wallet": to, "kind": holder.role, "from": from, "g1_cents": cents, "zen_cents": zen_cents,
            "block": funds.block_number, "verdict": verdict.label(), "detail": match &verdict { Verdict::Unverified(why) => json!(why), Verdict::Foreign(p) => json!(p), _ => json!(null) },
        });
        if let Err(e) = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('funds', ?1, ?2)", vec![Value::Integer(now()), record.to_string().into()]).await {
            self.activity.publish_to(Audience::Captain, "alert", "funds", format!("journal des virements : {e}"), None);
        }
        let g1 = amount(cents);
        let (text, alert) = match &verdict {
            Verdict::SameUplanet => (format!("💶 Vous avez reçu {} Ẑen ({g1} Ğ1) de {} — bloc {}.", amount(zen_cents), short(&from), funds.block_number), false),
            Verdict::WotValidation => (format!("🪪 Votre portefeuille a reçu une validation de {} ({g1} Ğ1) : elle n'apparaît pas dans votre solde Ẑen.", short(&from)), false),
            Verdict::Unverified(why) => (format!("💶 {g1} Ğ1 reçus de {} — origine non vérifiée ({why}), pas encore comptés en Ẑen.", short(&from)), true),
            Verdict::Foreign(_) => (format!("⚠️ {g1} Ğ1 reçus de {}, un portefeuille extérieur à cette UPlanet : non comptés en Ẑen. Le Capitaine en est informé.", short(&from)), true),
        };
        let data = json!({ "g1_cents": cents, "zen_cents": zen_cents, "from": from, "block": funds.block_number, "verdict": verdict.label() });
        // Primale étrangère PROUVÉE : renvoi vers INTRUSION (jamais sur une primale inconnue ou non vérifiée).
        if let (Verdict::Foreign(Some(primal)), Some((wallets, fix))) = (&verdict, &self.guard) {
            let label = holder.email.clone().unwrap_or_else(|| holder.hex.chars().take(12).collect());
            let signer = match holder.role.as_str() {
                "coop" => wallets.signer_of(&label).await,
                "zencard" => Err("clé de la ZenCard non tenue par astroportd".to_owned()),
                _ => self.member_signer(holder.email.as_deref(), &to),
            };
            match signer {
                Ok(signer) => fix(crate::intrusion_guard::Fix { label, signer, from: from.clone(), primal: primal.clone(), cents, block: funds.block_number }).await,
                Err(why) => {
                    self.activity.publish_to(Audience::Captain, "alert", "intrusion", format!("intrusion reçue par {label} ({g1} Ğ1 de {from}) : renvoi impossible ({why}), à faire à la main"), None);
                }
            }
        }
        if holder.role == "coop" {
            if alert {
                self.activity.publish_to(Audience::Captain, "alert", "funds:in", format!("portefeuille coopératif {} : virement {} de {g1} Ğ1 de {}", holder.email.as_deref().unwrap_or("?"), verdict.label(), short(&from)), Some(data));
            }
            return;
        }
        self.activity.publish_to(Audience::Owner(holder.hex.clone()), "economy", "funds:in", text.clone(), Some(data.clone()));
        if alert {
            let who = holder.email.clone().unwrap_or_else(|| holder.hex.chars().take(8).collect());
            self.activity.publish_to(Audience::Captain, "alert", "funds:in", format!("virement {} de {} Ğ1 pour {who} : {}", verdict.label(), g1, match &verdict { Verdict::Unverified(why) => why.clone(), _ => format!("primal {}", short(&from)) }), Some(data));
        }
        if verdict != Verdict::WotValidation {
            if let Some(n) = &self.notify {
                n(holder.hex, text).await;
            }
        }
    }

    /// Clé Ğ1 d'un MULTIPASS hébergé (`<nostr>/<email>/.secret.dunikey`), vérifiée contre l'adresse qui a reçu.
    fn member_signer(&self, email: Option<&str>, wallet: &str) -> Result<Arc<astro_duniter::Ed25519Signer>, String> {
        let path = self.nostr_dir.as_ref().zip(email).map(|(d, e)| d.join(e).join(".secret.dunikey")).ok_or("dossier du compte inconnu")?;
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{} : {e}", path.display()))?;
        let key = astro_lifecycle::dunikey::Dunikey::parse(&text).map_err(|e| e.to_string())?;
        let signer = astro_duniter::Ed25519Signer::from_seed(*key.ed25519_seed());
        if signer.account().ss58() != wallet {
            return Err("la clé du compte ne correspond pas au portefeuille qui a reçu".into());
        }
        Ok(Arc::new(signer))
    }

    /// Une sonde du nœud Duniter : le pool réessaie en silence une connexion qui échoue, la station ne le saurait pas. Le
    /// Capitaine est prévenu **une fois** quand le nœud devient injoignable, et une fois quand il revient. `up` : dernier
    /// état connu (`None` : jamais sondé).
    pub async fn probe<C: Chain>(&self, chain: &C, up: &mut Option<bool>) {
        let Ok(master) = Account::from_str(&self.master) else { return };
        let result = tokio::time::timeout(Duration::from_secs(20), chain.balance(&master)).await.unwrap_or_else(|_| Err(ChainError::Connection("délai dépassé".into())));
        match (&result, *up) {
            (Ok(_), Some(false)) => {
                self.activity.publish_to(Audience::Captain, "system", "funds", "nœud Duniter de nouveau joignable : l'écoute des virements a repris", None);
            }
            (Err(e), Some(true) | None) => {
                self.activity.publish_to(Audience::Captain, "alert", "funds", format!("nœud Duniter injoignable ({e}) : les virements reçus ne sont pas vus tant qu'il ne répond pas"), None);
            }
            _ => {}
        }
        *up = Some(result.is_ok());
    }

    /// Boucle : relit les comptes, branche le notifieur sur la chaîne et traite chaque virement, jusqu'à l'arrêt.
    /// Une chaîne qui se ferme ou échoue est rebranchée après une pause croissante (5 s à 60 s).
    pub async fn run<C: Chain>(self: Arc<Self>, chain: Arc<C>, mut stop: watch::Receiver<bool>) {
        let me = self.clone();
        let mut refresher_stop = stop.clone();
        let refresher = tokio::spawn(async move {
            loop {
                if let Err(e) = me.refresh_accounts().await {
                    me.activity.publish_to(Audience::Captain, "alert", "funds", format!("comptes à surveiller illisibles : {e}"), None);
                }
                tokio::select! {
                    () = tokio::time::sleep(REFRESH) => {}
                    _ = refresher_stop.changed() => return,
                }
            }
        });
        let (me, probe_chain, mut probe_stop) = (self.clone(), chain.clone(), stop.clone());
        let probe = tokio::spawn(async move {
            let mut up = None;
            loop {
                me.probe(probe_chain.as_ref(), &mut up).await;
                tokio::select! {
                    () = tokio::time::sleep(PROBE) => {}
                    _ = probe_stop.changed() => return,
                }
            }
        });
        let mut rx = self.notifier.subscribe();
        let me = self.clone();
        let pump = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(funds) => me.handle(funds).await,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        me.activity.publish_to(Audience::Captain, "alert", "funds", format!("{n} virement(s) non traités (trop de blocs d'un coup) : relire l'historique des comptes"), None);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        let mut pause = Duration::from_secs(5);
        loop {
            let notifier = self.notifier.clone();
            tokio::select! {
                res = async { notifier.run(chain.as_ref()).await } => {
                    let why = res.err().map_or_else(|| "flux fermé".to_owned(), |e| e.to_string());
                    self.activity.publish_to(Audience::Captain, "alert", "funds", format!("écoute des virements interrompue ({why}) : nouvel essai dans {} s", pause.as_secs()), None);
                }
                _ = stop.changed() => break,
            }
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                _ = stop.changed() => break,
            }
            pause = (pause * 2).min(Duration::from_secs(60));
        }
        refresher.abort();
        probe.abort();
        pump.abort();
    }
}

#[cfg(test)]
mod tests {

    /// Faux indexeur : répond `body` (JSON) à chaque requête HTTP.
    async fn fake_indexer(body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/graphql", l.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let _ = s.read(&mut buf).await;
                    let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                });
            }
        });
        url
    }

    #[tokio::test]
    async fn a_primal_is_only_trusted_when_two_indexers_agree() {
        let mut limits = FetchLimits::new(64 * 1024);
        limits.allow_private = true;
        const A: &str = r#"{"data":{"transfers":{"nodes":[{"fromId":"g1maitre","blockNumber":48203}]}}}"#;
        const B: &str = r#"{"data":{"transfers":{"nodes":[{"fromId":"g1autre","blockNumber":90000}]}}}"#;
        const EMPTY: &str = r#"{"data":{"transfers":{"nodes":[]}}}"#;
        let (a1, a2, b, empty) = (fake_indexer(A).await, fake_indexer(A).await, fake_indexer(B).await, fake_indexer(EMPTY).await);
        assert_eq!(primal_consensus(&[a1.clone(), a2.clone()], &limits, "g1x").await.unwrap(), Some("g1maitre".into()));
        // Un indexeur incomplet (historique partiel) ou menteur contredit : non vérifié, jamais une primale gardée à tort.
        assert!(primal_consensus(&[a1.clone(), b], &limits, "g1x").await.unwrap_err().contains("désaccord"));
        // Un indexeur en retard (« aucun virement ») contredit aussi.
        assert!(primal_consensus(&[empty, a1.clone()], &limits, "g1x").await.unwrap_err().contains("désaccord"));
        // Un seul répond sur deux configurés : pas assez.
        assert!(primal_consensus(&[a1.clone(), "http://127.0.0.1:9/v1/graphql".into()], &limits, "g1x").await.unwrap_err().contains("non confirmée"));
        // Un seul indexeur configuré : sa réponse suffit (rien de mieux possible).
        assert_eq!(primal_consensus(&[a2], &limits, "g1x").await.unwrap(), Some("g1maitre".into()));
    }

    use std::collections::HashMap as Map;

    use astro_duniter::Amount;

    use super::*;

    struct Primals(Mutex<Map<String, Result<Option<String>, String>>>, Mutex<usize>);

    impl PrimalSource for Primals {
        async fn primal_of(&self, ss58: &str) -> Result<Option<String>, String> {
            *self.1.lock().unwrap() += 1;
            self.0.lock().unwrap().get(ss58).cloned().unwrap_or(Ok(None))
        }
    }

    fn acct(n: u8) -> Account {
        Account([n; 32])
    }

    async fn setup(tag: &str, primals: Map<String, Result<Option<String>, String>>, notified: Arc<Mutex<Vec<(String, String)>>>) -> (std::path::PathBuf, Arc<Db>, Arc<FundsWatch<Primals>>, ActivityBus, Arc<Primals>) {
        let dir = std::env::temp_dir().join(format!("astroportd-funds-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let hex = "ab".repeat(32);
        let g1 = acct(7).ss58();
        db.writer()
            .execute("INSERT INTO identities(hex, role, email, g1pub_nostr, hosted) VALUES (?1, 'multipass', 'alice@exemple.org', ?2, 1)", vec![hex.into(), g1.into()])
            .await
            .unwrap();
        let bus = ActivityBus::new();
        let source = Arc::new(Primals(Mutex::new(primals), Mutex::new(0)));
        let notify: Notify = Arc::new(move |owner, text| {
            let log = notified.clone();
            Box::pin(async move { log.lock().unwrap().push((owner, text)) })
        });
        let w = Arc::new(FundsWatch::new(db.clone(), bus.clone(), acct(1).ss58(), Some(source.clone()), Some(notify), None));
        assert_eq!(w.refresh_accounts().await.unwrap(), 1);
        (dir, db, w, bus, source)
    }

    fn funds(from: u8, cents: u64, block: u64) -> IncomingFunds {
        IncomingFunds { account: acct(7), from: acct(from), amount: Amount(cents), block_number: block, block_hash: [0; 32] }
    }

    fn texts(bus: &ActivityBus, audience: &Audience) -> Vec<String> {
        bus.recent(50).into_iter().filter(|a| &a.audience == audience).map(|a| a.message.clone()).collect()
    }

    #[tokio::test]
    async fn a_proven_foreign_transfer_is_sent_to_the_guard_and_a_coop_sender_is_internal() {
        let dir = std::env::temp_dir().join(format!("astroportd-funds-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nostr/alice@exemple.org")).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let seed = [0x61; 32];
        std::fs::write(dir.join("nostr/alice@exemple.org/.secret.dunikey"), astro_lifecycle::dunikey::render(&seed)).unwrap();
        let alice = astro_duniter::Ed25519Signer::from_seed(seed).account();
        db.writer().execute("INSERT INTO identities(hex, role, email, g1pub_nostr, hosted) VALUES (?1, 'multipass', 'alice@exemple.org', ?2, 1)", vec!["ab".repeat(32).into(), alice.ss58().into()]).await.unwrap();
        let coop = crate::coop_wallets::Coop { node: acct(21), captain: acct(22), impot: acct(23), treasury: acct(24), rnd: acct(25), assets: acct(26) };
        let wallets = Arc::new(crate::coop_wallets::Wallets::fixed(db.clone(), astro_duniter::Ed25519Signer::from_seed([0x44; 32]), coop).with_target("INTRUSION", acct(29)));
        let fixes: Arc<Mutex<Vec<(String, String, u64)>>> = Arc::default();
        let log = fixes.clone();
        let fix: crate::intrusion_guard::FixFn = Arc::new(move |f| {
            let log = log.clone();
            Box::pin(async move { log.lock().unwrap().push((f.label, f.from, f.cents)) })
        });
        let primals = Map::from([(acct(9).ss58(), Ok(Some(acct(8).ss58()))), (acct(10).ss58(), Ok(None)), (acct(11).ss58(), Err("indexeurs en désaccord".into()))]);
        let source = Arc::new(Primals(Mutex::new(primals), Mutex::new(0)));
        let w = FundsWatch::new(db.clone(), ActivityBus::new(), acct(1).ss58(), Some(source), None, Some(dir.join("nostr"))).with_guard(wallets, fix);
        assert!(w.refresh_accounts().await.unwrap() > 1, "les portefeuilles coopératifs sont surveillés aussi");
        let incoming = |from: u8, cents: u64, block: u64| IncomingFunds { account: alice, from: acct(from), amount: Amount(cents), block_number: block, block_hash: [0; 32] };
        w.handle(incoming(9, 3_700, 1)).await; // primale étrangère prouvée → renvoi
        w.handle(incoming(10, 500, 2)).await; // primale inconnue → rien
        w.handle(incoming(11, 500, 3)).await; // non vérifiée → rien
        w.handle(incoming(26, 500, 4)).await; // ASSETS → interne
        assert_eq!(fixes.lock().unwrap().clone(), vec![("alice@exemple.org".to_owned(), acct(9).ss58(), 3_700)]);
        assert_eq!(w.classify(&acct(26), 500).await, Verdict::SameUplanet);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_master_wallet_is_the_cooperative_g1_key() {
        // Vecteur de astro-core (tests/legacy_keygen.rs : coop_g1, UPLANETNAME « UPlanetTestName »).
        assert_eq!(master_wallet("UPlanetTestName").unwrap(), Account::from_str("EgemnJRqh59pMp7fV4YT3cVWmiUXyR9Q8kAG2Ckvrxpi").unwrap().ss58());
    }

    #[test]
    fn the_indexer_answer_is_read_strictly() {
        assert_eq!(parse_primal(br#"{"data":{"transfers":{"nodes":[{"fromId":"g1abc","blockNumber":4}]}}}"#).unwrap(), Some("g1abc".into()));
        assert_eq!(parse_primal(br#"{"data":{"transfers":{"nodes":[]}}}"#).unwrap(), None);
        assert!(parse_primal(br#"{"errors":[{"message":"boom"}]}"#).unwrap_err().contains("boom"), "une erreur GraphQL n'est pas « aucun primal »");
        assert!(parse_primal(b"<html>").is_err() && parse_primal(br#"{"data":{}}"#).is_err());
        assert!(primal_query("g1x").contains("\"w\":\"g1x\""));
        // Deux émetteurs dans le premier bloc : ambigu, jamais tranché au hasard ; le même émetteur deux fois : sans ambiguïté.
        assert!(parse_primal(br#"{"data":{"transfers":{"nodes":[{"fromId":"g1a","blockNumber":4},{"fromId":"g1b","blockNumber":4}]}}}"#).unwrap_err().contains("ambiguë"));
        assert_eq!(parse_primal(br#"{"data":{"transfers":{"nodes":[{"fromId":"g1a","blockNumber":4},{"fromId":"g1b","blockNumber":9}]}}}"#).unwrap(), Some("g1a".into()));
    }

    #[tokio::test]
    async fn a_transfer_from_the_same_uplanet_is_credited_notified_and_journaled_once() {
        let told = Arc::new(Mutex::new(Vec::new()));
        let (dir, db, w, bus, _) = setup("ok", Map::from([(acct(9).ss58(), Ok(Some(acct(1).ss58())))]), told.clone()).await;
        w.handle(funds(9, 250, 42)).await;
        w.handle(funds(9, 250, 42)).await; // même bloc rejoué après une reconnexion
        let mine = Audience::Owner("ab".repeat(32));
        let said = texts(&bus, &mine);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("25.00 Ẑen") && said[0].contains("2.50 Ğ1"), "{}", said[0]);
        assert_eq!(told.lock().unwrap().len(), 1, "un seul message privé");
        assert!(texts(&bus, &Audience::Captain).is_empty(), "rien à signaler au Capitaine");
        let (n, data): (i64, String) = db.readers().read(|c| Ok(c.query_row("SELECT COUNT(*), MAX(data) FROM journal WHERE stream = 'funds'", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
        assert_eq!(n, 1);
        assert!(data.contains("\"zen_cents\":2500") && data.contains("same_uplanet"), "{data}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_foreign_primal_is_not_credited_and_alerts_the_captain() {
        let told = Arc::new(Mutex::new(Vec::new()));
        let (dir, db, w, bus, _) = setup("foreign", Map::from([(acct(9).ss58(), Ok(Some(acct(5).ss58())))]), told.clone()).await;
        w.handle(funds(9, 1000, 7)).await;
        let said = texts(&bus, &Audience::Owner("ab".repeat(32)));
        assert!(said[0].contains("extérieur") && said[0].contains("non comptés"), "{}", said[0]);
        assert_eq!(texts(&bus, &Audience::Captain).len(), 1);
        let data: String = db.readers().read(|c| Ok(c.query_row("SELECT data FROM journal WHERE stream = 'funds'", [], |r| r.get(0))?)).await.unwrap();
        assert!(data.contains("\"zen_cents\":0") && data.contains("foreign"), "{data}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_unreachable_indexer_is_reported_never_treated_as_valid() {
        let told = Arc::new(Mutex::new(Vec::new()));
        let (dir, _, w, bus, _) = setup("down", Map::from([(acct(9).ss58(), Err("indexeur injoignable".into()))]), told).await;
        w.handle(funds(9, 500, 8)).await;
        let said = texts(&bus, &Audience::Owner("ab".repeat(32)));
        assert!(said[0].contains("origine non vérifiée") && said[0].contains("indexeur injoignable"), "{}", said[0]);
        assert_eq!(texts(&bus, &Audience::Captain).len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_master_wallet_and_wot_validations_need_no_lookup_and_the_primal_is_cached() {
        let told = Arc::new(Mutex::new(Vec::new()));
        let (dir, _, w, _bus, source) = setup("direct", Map::from([(acct(9).ss58(), Ok(Some(acct(1).ss58())))]), told.clone()).await;
        w.handle(funds(1, 300, 1)).await; // émis par le portefeuille maître
        w.handle(funds(3, 1, 2)).await; // 0,01 Ğ1 : validation de la toile Ğ1
        assert_eq!(*source.1.lock().unwrap(), 0, "aucune requête à l'indexeur");
        assert_eq!(told.lock().unwrap().len(), 1, "la validation ne déclenche pas de message privé");
        w.handle(funds(9, 100, 3)).await;
        w.handle(funds(9, 100, 4)).await;
        assert_eq!(*source.1.lock().unwrap(), 1, "le primal d'un portefeuille est immuable : une seule lecture");
        let _ = std::fs::remove_dir_all(dir);
    }

    struct Node(Mutex<bool>);

    impl Chain for Node {
        async fn balance(&self, _: &Account) -> Result<astro_duniter::Balance, ChainError> {
            if *self.0.lock().unwrap() { Ok(astro_duniter::Balance { total: Amount(0), transferable: Amount(0), unclaimed_uds: Amount(0) }) } else { Err(ChainError::Connection("wss://127.0.0.1:1 : refusée".into())) }
        }
        async fn transfer(&self, _: &astro_duniter::Ed25519Signer, _: &Account, _: Amount, _: astro_duniter::TransferMode, _: Option<&str>) -> Result<astro_duniter::TxOutcome, ChainError> {
            Err(ChainError::Invalid("non utilisé".into()))
        }
        async fn watch_transfers(&self, _: tokio::sync::mpsc::Sender<astro_duniter::TransferEvent>) -> Result<(), ChainError> {
            std::future::pending().await
        }
        fn existential_deposit(&self) -> Amount {
            Amount(100)
        }
    }

    /// Nœud qui diffuse un virement vers le compte surveillé, deux fois (rejeu après reconnexion), puis se tait.
    struct Streaming;

    impl Chain for Streaming {
        async fn balance(&self, _: &Account) -> Result<astro_duniter::Balance, ChainError> {
            Ok(astro_duniter::Balance { total: Amount(0), transferable: Amount(0), unclaimed_uds: Amount(0) })
        }
        async fn transfer(&self, _: &astro_duniter::Ed25519Signer, _: &Account, _: Amount, _: astro_duniter::TransferMode, _: Option<&str>) -> Result<astro_duniter::TxOutcome, ChainError> {
            Err(ChainError::Invalid("non utilisé".into()))
        }
        async fn watch_transfers(&self, out: tokio::sync::mpsc::Sender<astro_duniter::TransferEvent>) -> Result<(), ChainError> {
            let ev = astro_duniter::TransferEvent { block_number: 77, block_hash: [3; 32], from: acct(1), to: acct(7), amount: Amount(400) };
            out.send(ev.clone()).await.map_err(|e| ChainError::Invalid(e.to_string()))?;
            out.send(ev).await.map_err(|e| ChainError::Invalid(e.to_string()))?;
            std::future::pending().await
        }
        fn existential_deposit(&self) -> Amount {
            Amount(100)
        }
    }

    #[tokio::test]
    async fn the_running_watcher_turns_a_block_event_into_one_personal_notification() {
        let told = Arc::new(Mutex::new(Vec::new()));
        let (dir, _, w, bus, _) = setup("run", Map::new(), told.clone()).await;
        let (stop, rx) = watch::channel(false);
        let task = tokio::spawn(w.clone().run(Arc::new(Streaming), rx));
        let mine = Audience::Owner("ab".repeat(32));
        for _ in 0..100 {
            if !texts(&bus, &mine).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await; // le rejeu du même bloc est ignoré
        let said = texts(&bus, &mine);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("40.00 Ẑen") && said[0].contains("4.00 Ğ1") && said[0].contains("bloc 77"), "{}", said[0]);
        assert_eq!(told.lock().unwrap().len(), 1);
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_unreachable_duniter_node_is_reported_once_then_its_return_is() {
        let told = Arc::new(Mutex::new(Vec::new()));
        let (dir, _, w, bus, _) = setup("probe", Map::new(), told).await;
        let node = Node(Mutex::new(false));
        let mut up = None;
        for _ in 0..3 {
            w.probe(&node, &mut up).await; // injoignable : une seule alerte, pas une par sonde
        }
        let alerts = texts(&bus, &Audience::Captain);
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert!(alerts[0].contains("injoignable") && alerts[0].contains("refusée"), "{}", alerts[0]);
        *node.0.lock().unwrap() = true;
        w.probe(&node, &mut up).await;
        w.probe(&node, &mut up).await;
        let alerts = texts(&bus, &Audience::Captain);
        assert_eq!(alerts.len(), 2, "{alerts:?}");
        assert!(alerts[1].contains("de nouveau joignable"));
        assert_eq!(up, Some(true));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn without_an_indexer_the_origin_is_announced_unverified() {
        let dir = std::env::temp_dir().join(format!("astroportd-funds-noidx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let w: FundsWatch<Primals> = FundsWatch::new(db, ActivityBus::new(), acct(1).ss58(), None, None, None);
        assert!(matches!(w.classify(&acct(9), 100).await, Verdict::Unverified(why) if why.contains("--squid-url")));
        assert_eq!(w.classify(&acct(1), 100).await, Verdict::SameUplanet);
        let _ = std::fs::remove_dir_all(dir);
    }
}
