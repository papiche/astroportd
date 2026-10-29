//! Services de la station partagés par le bus de commandes de BRO, la console web et le tableau de bord :
//! solde (Duniter), matériel (Power-Score), dépôt de documents.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use astro_bro::bus::Services;
use astro_core::ActivityBus;
use astro_db::Db;
use astro_duniter::{Account, Chain, RpcPool, SubxtConnector};
use astro_p2p::P2pHandle;
use astro_p2p::routing::Hardware;
use rusqlite::types::Value;

/// Réserve non dépensable d'un compte, en centimes de Ğ1 (le dépôt existentiel de 1 Ğ1 n'est pas du Ẑen).
pub const RESERVE_G1_CENTS: u64 = 100;
/// 1 Ğ1 = 10 Ẑen (mode UPlanet ORIGIN) : un centime de Ğ1 vaut 10 centimes de Ẑen.
pub const ZEN_PER_G1: u64 = 10;
/// Délai d'une lecture de solde.
const BALANCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Identité publique de la station.
pub struct StationInfo {
    /// Clé Nostr du NODE.
    pub node_hex: String,
    /// PeerID IPFS/libp2p.
    pub peer_id: Option<String>,
    /// Nom de domaine principal.
    pub domain: Option<String>,
    /// Démarrage du démon.
    pub started: Instant,
}

/// Solde d'un compte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalanceView {
    /// Centimes de Ğ1.
    pub g1_cents: u64,
    /// Centimes de Ẑen : `(Ğ1 − 1) × 10`.
    pub zen_cents: u64,
}

/// Ẑen d'un solde Ğ1 : on retire la réserve de 1 Ğ1, puis 1 Ğ1 = 10 Ẑen. Entiers uniquement.
pub fn zen_of_g1(g1_cents: u64) -> u64 {
    g1_cents.saturating_sub(RESERVE_G1_CENTS).saturating_mul(ZEN_PER_G1)
}

/// Éléments nécessaires à [`StationServices::new`].
pub struct StationDeps {
    /// Base `uplanet.db`.
    pub db: Arc<Db>,
    /// Identité publique.
    pub info: StationInfo,
    /// Dossier des comptes.
    pub nostr_dir: Option<PathBuf>,
    /// Nœuds Duniter.
    pub chain: Option<Arc<RpcPool<SubxtConnector>>>,
    /// Matériel mesuré.
    pub hardware: Hardware,
    /// Maillage P2P.
    pub p2p: Option<P2pHandle>,
    /// Loyer hebdomadaire (centimes de Ẑen).
    pub weekly_rent_cents: u64,
    /// Bus d'activité.
    pub activity: ActivityBus,
    /// Dossier des `12345.json` des pairs (`~/.zen/tmp/swarm`), pour la vue Constellation.
    pub swarm_dir: Option<PathBuf>,
    /// Coordonnées de cette station (`--lat`/`--lon`), sinon celles de son NODE en base.
    pub origin: Option<(f64, f64)>,
    /// Services exposés dans le maillage (`--p2p-expose`).
    pub exposed: Vec<String>,
}

/// Contexte partagé.
pub struct StationServices {
    /// Base `uplanet.db`.
    pub db: Arc<Db>,
    /// Identité publique.
    pub info: StationInfo,
    /// Dossier des comptes (`<game>/nostr`) : `<email>/G1PUBNOSTR` donne le portefeuille Ğ1 d'un MULTIPASS.
    pub nostr_dir: Option<PathBuf>,
    /// Nœuds Duniter ; `None` : soldes indisponibles.
    pub chain: Option<Arc<RpcPool<SubxtConnector>>>,
    /// Matériel mesuré au démarrage.
    pub hardware: Hardware,
    /// Maillage P2P.
    pub p2p: Option<P2pHandle>,
    /// Loyer hebdomadaire d'un MULTIPASS, en centimes de Ẑen (NCARD = 1 Ẑen par semaine).
    pub weekly_rent_cents: u64,
    /// Bus d'activité.
    pub activity: ActivityBus,
    /// Dossier des `12345.json` des pairs.
    pub swarm_dir: Option<PathBuf>,
    /// Coordonnées de cette station.
    pub origin: Option<(f64, f64)>,
    /// Services exposés dans le maillage.
    pub exposed: Vec<String>,
    /// Dernier appel Duniter : `(quand, réussi)`, pour `/health` sans appel réseau.
    chain_seen: Mutex<Option<(Instant, bool)>>,
}

impl StationServices {
    /// Assemble le contexte.
    pub fn new(d: StationDeps) -> Self {
        Self { db: d.db, info: d.info, nostr_dir: d.nostr_dir, chain: d.chain, hardware: d.hardware, p2p: d.p2p, weekly_rent_cents: d.weekly_rent_cents, activity: d.activity, swarm_dir: d.swarm_dir, origin: d.origin, exposed: d.exposed, chain_seen: Mutex::new(None) }
    }

    /// Résultat du dernier appel Duniter (`None` : jamais sollicité).
    pub fn last_chain_call(&self) -> Option<(Duration, bool)> {
        self.chain_seen.lock().unwrap_or_else(|e| e.into_inner()).map(|(t, ok)| (t.elapsed(), ok))
    }

    /// Portefeuille Ğ1 d'un MULTIPASS : `g1pub_nostr` du DID, sinon le fichier `G1PUBNOSTR` du compte.
    async fn wallet_of(&self, owner: &str) -> Result<Account, String> {
        let hex = owner.to_owned();
        let (email, did): (Option<String>, Option<String>) = self
            .db
            .readers()
            .read(move |c| {
                let row = c.query_row("SELECT email, g1pub_nostr FROM identities WHERE hex = ?1", [hex], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)));
                Ok(row.unwrap_or((None, None)))
            })
            .await
            .map_err(|e| format!("base : {e}"))?;
        let from_file = || -> Option<String> {
            let path = self.nostr_dir.as_ref()?.join(email.as_deref()?).join("G1PUBNOSTR");
            std::fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
        };
        let text = did.filter(|s| !s.is_empty()).or_else(from_file).ok_or("portefeuille Ğ1 de ce compte introuvable (ni DID, ni fichier G1PUBNOSTR)")?;
        Account::from_str(&text).map_err(|e| e.to_string())
    }

    /// Solde du compte `owner`. L'erreur est lisible par l'humain (nœud injoignable, portefeuille inconnu…).
    pub async fn balance_of(&self, owner: &str) -> Result<BalanceView, String> {
        let chain = self.chain.as_ref().ok_or("nœud Duniter non configuré (--duniter-rpc)")?;
        let account = self.wallet_of(owner).await?;
        let result = tokio::time::timeout(BALANCE_TIMEOUT, chain.balance(&account)).await;
        let ok = matches!(result, Ok(Ok(_)));
        *self.chain_seen.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), ok));
        match result {
            Ok(Ok(b)) => Ok(BalanceView { g1_cents: b.total.0, zen_cents: zen_of_g1(b.total.0) }),
            Ok(Err(e)) => {
                self.activity.publish_to(astro_core::Audience::Owner(owner.to_owned()), "economy", "station:balance", format!("lecture du solde impossible : {e}"), None);
                Err(format!("lecture du solde impossible : {e}"))
            }
            Err(_) => Err(format!("le nœud Duniter ne répond pas ({} s)", BALANCE_TIMEOUT.as_secs())),
        }
    }
}

fn amount(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Texte lisible du matériel.
pub fn hardware_text(h: &Hardware, peers: Option<usize>) -> String {
    let mut s = format!("Power-Score {:.1} ({}) : GPU {:.0} Go · {} cœurs · {:.0} Go de RAM", h.score(), h.tier().label(), h.vram_gb, h.cpu_cores, h.ram_gb);
    if let Some(n) = peers {
        s.push_str(&format!(" · {n} pair(s) P2P"));
    }
    s
}

impl Services for StationServices {
    async fn balance(&self, owner_hex: &str) -> Result<String, String> {
        let b = self.balance_of(owner_hex).await?;
        Ok(format!("Solde : {} Ẑen ({} Ğ1)", amount(b.zen_cents), amount(b.g1_cents)))
    }

    async fn hardware(&self) -> Result<String, String> {
        Ok(hardware_text(&self.hardware, self.p2p.as_ref().map(|p| p.connected_peers().len())))
    }

    /// Met le document en file (`tasks`, `bro.upload`). Le traitement (récupération, chiffrement, ajout à l'uCloud)
    /// reste à brancher : la demande est tracée et dédoublonnée, jamais perdue ni exécutée à moitié.
    async fn upload(&self, owner_hex: &str, source: &str, name: Option<&str>) -> Result<String, String> {
        let is_url = source.starts_with("https://") || source.starts_with("http://");
        if !(is_url || astro_dav::blob::is_cid(source)) || source.len() > 2048 {
            return Err("source invalide : un CID IPFS v1 (bafy…/bafk…) ou une URL http(s) est attendu".into());
        }
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        if name.is_some_and(|n| n.chars().count() > 120 || n.chars().any(char::is_control)) {
            return Err("nom invalide (120 caractères, sans caractère de contrôle)".into());
        }
        let payload = serde_json::json!({ "owner": owner_hex, "source": source, "name": name }).to_string();
        let found = self
            .db
            .readers()
            .read({
                let payload = payload.clone();
                move |c| Ok(c.query_row("SELECT id FROM tasks WHERE kind = 'bro.upload' AND payload = ?1 AND status IN ('pending', 'running')", [payload], |r| r.get::<_, i64>(0)).ok())
            })
            .await
            .map_err(|e| format!("base : {e}"))?;
        if let Some(id) = found {
            return Ok(format!("Déjà en file (n° {id})."));
        }
        let out = self
            .db
            .writer()
            .execute("INSERT INTO tasks(kind, payload, status, created_at) VALUES ('bro.upload', ?1, 'pending', ?2)", vec![Value::Text(payload), Value::Integer(now_secs())])
            .await
            .map_err(|e| format!("base : {e}"))?;
        self.activity.publish_to(astro_core::Audience::Owner(owner_hex.to_owned()), "bro", "bro:upload", format!("document mis en file (n° {})", out.last_insert_rowid), None);
        Ok(match name {
            Some(n) => format!("Document « {n} » mis en file (n° {}).", out.last_insert_rowid),
            None => format!("Document mis en file (n° {}).", out.last_insert_rowid),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zen_is_g1_minus_the_reserve_times_ten_in_integers() {
        assert_eq!(zen_of_g1(0), 0);
        assert_eq!(zen_of_g1(100), 0, "la réserve de 1 Ğ1 n'est pas du Ẑen");
        assert_eq!(zen_of_g1(350), 2_500, "3,50 Ğ1 → 25,00 Ẑen");
        assert_eq!(amount(2_500), "25.00");
        assert_eq!(amount(7), "0.07");
    }

    #[test]
    fn hardware_text_mentions_tier_and_peers() {
        let h = Hardware { vram_gb: 24.0, cpu_cores: 16, ram_gb: 64.0 };
        let t = hardware_text(&h, Some(3));
        assert!(t.contains("Brain") && t.contains("160.0") && t.contains("3 pair(s)"), "{t}");
    }
}
