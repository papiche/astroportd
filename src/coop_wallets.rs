//! Portefeuilles Ğ1 de l'économie coopérative d'une station, dérivés de `UPLANETNAME` (formule `CoopPrefix` :
//! sel = poivre = `<UPLANETNAME>.<suffixe>`).
//!
//! - **compte technique d'hébergement** (`HOSTING.<PeerID du NODE>`) : reçoit les prix d'hébergement des MULTIPASS
//!   hébergés, puis le règlement hebdomadaire en redistribue le contenu. Sa clé est signée par la station, comme celle
//!   des MULTIPASS hébergés. Le sel dépend du NODE, jamais de l'email du Capitaine
//!   (qui peut changer sans déplacer le compte) ; il est distinct de celui du legacy (`<UPLANETNAME>.<email>`,
//!   `CAPTAIN_DEDICATED`) : aucun mélange avec les anciens flux ;
//! - destinations du règlement : NODE (`PAF`), MULTIPASS du Capitaine (`2×PAF` et prime), puis `IMPOT`, `TREASURY`,
//!   `RND`, `ASSETS` (les mêmes sels que les scripts d'Astroport.ONE : mêmes portefeuilles).
//!
//! La dérivation (scrypt) est lente : elle se fait une fois, hors du fil asynchrone, et le résultat est gardé.

use std::str::FromStr;
use std::sync::Arc;

use astro_core::formula::{FormulaInput, KeyFormula};
use astro_db::Db;
use astro_duniter::{Account, Ed25519Signer};
use rusqlite::OptionalExtension;
use tokio::sync::OnceCell;

/// Destinations du règlement hebdomadaire.
#[derive(Debug, Clone)]
pub struct Coop {
    /// Portefeuille du NODE (Armateur) : reçoit 1×PAF.
    pub node: Account,
    /// MULTIPASS du Capitaine : reçoit 2×PAF et la prime de gestion.
    pub captain: Account,
    /// Provision d'impôt sur les sociétés.
    pub impot: Account,
    /// Trésorerie (`CASH`).
    pub treasury: Account,
    /// Recherche et développement.
    pub rnd: Account,
    /// Actifs durables.
    pub assets: Account,
}

/// Portefeuilles de la station, calculés à la demande puis gardés.
pub struct Wallets {
    uplanet_name: String,
    db: Arc<Db>,
    tech: OnceCell<Arc<Ed25519Signer>>,
    coop: OnceCell<Arc<Coop>>,
    master: OnceCell<Arc<Ed25519Signer>>,
    targets: OnceCell<Arc<Vec<(String, Account)>>>,
}

/// Portefeuilles coopératifs d'une UPlanet à marquer par transaction primale : nom et sel (`<UPLANETNAME>.<sel>`) ; le
/// premier n'a pas de suffixe. Les mêmes que `tools/my.sh` d'Astroport.ONE (le portefeuille maître `G1` les finance).
const PRIMAL_SUFFIXES: [(&str, Option<&str>); 9] = [
    ("UPLANETNAME", None),
    ("SOCIETY", Some("SOCIETY")),
    ("TREASURY", Some("TREASURY")),
    ("RND", Some("RND")),
    ("ASSETS", Some("ASSETS")),
    ("IMPOT", Some("IMPOT")),
    ("INTRUSION", Some("INTRUSION")),
    ("CAPITAL", Some("CAPITAL")),
    ("AMORTISSEMENT", Some("AMORTISSEMENT")),
];

/// Compte `<UPLANETNAME>.<suffixe>` : (signataire, adresse).
fn derive(uplanet_name: &str, suffix: &str) -> Result<(Ed25519Signer, Account), String> {
    let input = FormulaInput { salt: "", pepper: "", uplanet_name, coop_suffix: Some(suffix) };
    let seed = KeyFormula::CoopPrefix.credentials(&input).map_err(|e| e.to_string())?.seed().map_err(|e| e.to_string())?;
    let signer = Ed25519Signer::from_seed(*seed.ed25519_secret_seed());
    let account = signer.account();
    Ok((signer, account))
}

/// Adresse du portefeuille `<UPLANETNAME>[.<suffixe>]`.
fn derive_bare(uplanet_name: &str, suffix: Option<&str>) -> Result<Account, String> {
    let input = FormulaInput { salt: "", pepper: "", uplanet_name, coop_suffix: suffix };
    let seed = KeyFormula::CoopPrefix.credentials(&input).map_err(|e| e.to_string())?.seed().map_err(|e| e.to_string())?;
    Account::from_str(&seed.g1_ss58().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

impl Wallets {
    /// Portefeuilles de la constellation `uplanet_name` ; l'email du Capitaine et les adresses du NODE et du Capitaine
    /// sont lus dans `db`.
    pub fn new(uplanet_name: String, db: Arc<Db>) -> Self {
        Self { uplanet_name, db, tech: OnceCell::new(), coop: OnceCell::new(), master: OnceCell::new(), targets: OnceCell::new() }
    }

    /// Portefeuilles déjà connus (tests) : aucune dérivation.
    pub fn fixed(db: Arc<Db>, tech: Ed25519Signer, coop: Coop) -> Self {
        let (t, c, g) = (OnceCell::new(), OnceCell::new(), OnceCell::new());
        let targets = vec![("HOSTING".to_owned(), tech.account()), ("NODE".to_owned(), coop.node), ("IMPOT".to_owned(), coop.impot), ("TREASURY".to_owned(), coop.treasury), ("RND".to_owned(), coop.rnd), ("ASSETS".to_owned(), coop.assets)];
        let _ = (t.set(Arc::new(tech)), c.set(Arc::new(coop)), g.set(Arc::new(targets)));
        Self { uplanet_name: String::new(), db, tech: t, coop: c, master: OnceCell::new(), targets: g }
    }

    /// Ajoute un portefeuille coopératif nommé (tests : `INTRUSION`, `SOCIETY`…).
    pub fn with_target(mut self, name: &str, account: Account) -> Self {
        let mut list = self.targets.get().map(|t| t.as_ref().clone()).unwrap_or_default();
        list.push((name.to_owned(), account));
        self.targets = OnceCell::new();
        let _ = self.targets.set(Arc::new(list));
        self
    }

    /// Portefeuille maître déjà connu (tests).
    pub fn with_master(self, master: Ed25519Signer) -> Self {
        let _ = self.master.set(Arc::new(master));
        self
    }

    /// Signataire d'un portefeuille coopératif par son nom (`SOCIETY`, `ASSETS`, `UPLANETNAME`…, ou `HOSTING`) : sert à
    /// corriger une intrusion reçue par ce portefeuille. Jamais `INTRUSION` ni le maître `G1`.
    pub async fn signer_of(&self, label: &str) -> Result<Arc<Ed25519Signer>, String> {
        if label == "HOSTING" {
            return self.tech().await;
        }
        let (_, suffix) = PRIMAL_SUFFIXES.iter().find(|(l, _)| *l == label && *l != "INTRUSION").ok_or_else(|| format!("portefeuille coopératif inconnu : {label}"))?;
        let (name, suffix) = (self.uplanet_name.clone(), suffix.map(str::to_owned));
        let signer = tokio::task::spawn_blocking(move || -> Result<Ed25519Signer, String> {
            let input = FormulaInput { salt: "", pepper: "", uplanet_name: &name, coop_suffix: suffix.as_deref() };
            let seed = KeyFormula::CoopPrefix.credentials(&input).map_err(|e| e.to_string())?.seed().map_err(|e| e.to_string())?;
            Ok(Ed25519Signer::from_seed(*seed.ed25519_secret_seed()))
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(Arc::new(signer))
    }

    /// Portefeuille maître `UPLANETNAME_G1` : il finance les transactions primales.
    pub async fn master(&self) -> Result<Arc<Ed25519Signer>, String> {
        self.master
            .get_or_try_init(|| async {
                let name = self.uplanet_name.clone();
                let (signer, _) = tokio::task::spawn_blocking(move || derive(&name, "G1")).await.map_err(|e| e.to_string())??;
                Ok(Arc::new(signer))
            })
            .await
            .cloned()
    }

    /// Tous les portefeuilles à marquer par transaction primale : les coopératifs, le compte technique d'hébergement et le NODE.
    pub async fn targets(&self) -> Result<Arc<Vec<(String, Account)>>, String> {
        let tech = self.tech().await?.account();
        let coop = self.coop().await?;
        self.targets
            .get_or_try_init(|| async {
                let name = self.uplanet_name.clone();
                let mut out = tokio::task::spawn_blocking(move || -> Result<Vec<(String, Account)>, String> {
                    PRIMAL_SUFFIXES.iter().map(|(label, suffix)| derive_bare(&name, *suffix).map(|a| ((*label).to_owned(), a))).collect()
                })
                .await
                .map_err(|e| e.to_string())??;
                out.push(("HOSTING".to_owned(), tech));
                out.push(("NODE".to_owned(), coop.node));
                Ok(Arc::new(out))
            })
            .await
            .cloned()
    }

    async fn node_id(&self) -> Result<String, String> {
        self.db
            .readers()
            .read(|c| Ok(c.query_row("SELECT peer_id FROM identities WHERE role = 'node' AND hosted = 1 AND peer_id IS NOT NULL LIMIT 1", [], |r| r.get::<_, String>(0)).optional()?))
            .await
            .map_err(|e| format!("base illisible : {e}"))?
            .ok_or_else(|| "NODE introuvable : aucun compte technique d'hébergement".to_owned())
    }

    /// Compte technique d'hébergement (signataire).
    pub async fn tech(&self) -> Result<Arc<Ed25519Signer>, String> {
        self.tech
            .get_or_try_init(|| async {
                let node_id = self.node_id().await?;
                let name = self.uplanet_name.clone();
                let (signer, _) = tokio::task::spawn_blocking(move || derive(&name, &format!("HOSTING.{node_id}"))).await.map_err(|e| e.to_string())??;
                Ok(Arc::new(signer))
            })
            .await
            .cloned()
    }

    /// Destinations du règlement.
    pub async fn coop(&self) -> Result<Arc<Coop>, String> {
        self.coop
            .get_or_try_init(|| async {
                let (node, captain): (Option<String>, Option<String>) = self
                    .db
                    .readers()
                    .read(|c| {
                        let node = c.query_row("SELECT g1pub FROM identities WHERE role = 'node' AND hosted = 1 AND g1pub IS NOT NULL LIMIT 1", [], |r| r.get::<_, String>(0)).optional()?;
                        let captain = c.query_row("SELECT g1pub_nostr FROM identities WHERE role = 'captain' AND hosted = 1 AND g1pub_nostr IS NOT NULL LIMIT 1", [], |r| r.get::<_, String>(0)).optional()?;
                        Ok((node, captain))
                    })
                    .await
                    .map_err(|e| format!("base illisible : {e}"))?;
                let node = node.and_then(|w| Account::from_str(&w).ok()).ok_or("portefeuille Ğ1 du NODE introuvable")?;
                let captain = captain.and_then(|w| Account::from_str(&w).ok()).ok_or("portefeuille Ğ1 du MULTIPASS du Capitaine introuvable")?;
                let name = self.uplanet_name.clone();
                let [impot, treasury, rnd, assets] = tokio::task::spawn_blocking(move || -> Result<[Account; 4], String> {
                    let pick = |suffix: &str| derive(&name, suffix).map(|(_, a)| a);
                    Ok([pick("IMPOT")?, pick("TREASURY")?, pick("RND")?, pick("ASSETS")?])
                })
                .await
                .map_err(|e| e.to_string())??;
                Ok(Arc::new(Coop { node, captain, impot, treasury, rnd, assets }))
            })
            .await
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wallets_are_deterministic_and_distinct_per_suffix() {
        let a = derive("UPlanetTestName", "TREASURY").unwrap().1;
        assert_eq!(derive("UPlanetTestName", "TREASURY").unwrap().1, a);
        assert_ne!(derive("UPlanetTestName", "RND").unwrap().1, a);
        assert_ne!(derive("AutreUPlanet", "TREASURY").unwrap().1, a, "une autre constellation, d'autres portefeuilles");
        assert_ne!(derive("UPlanetTestName", "HOSTING.12D3KooWx").unwrap().1, derive("UPlanetTestName", "cap@example.org").unwrap().1, "le compte technique n'est pas le CAPTAIN_DEDICATED du legacy");
    }
}
