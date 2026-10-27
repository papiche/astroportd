//! Accès à la chaîne : soldes, virements, flux d'événements.

use std::future::Future;
use std::time::Duration;

use astro_g1_runtime::runtime;
use subxt::OnlineClient;
use subxt::config::PolkadotConfig;
use subxt::utils::MultiAddress;
use tokio::sync::mpsc;

use crate::{Account, Amount, Ed25519Signer};

/// Erreurs d'accès à la chaîne. La distinction compte : seules les erreurs `Connection` peuvent être rejouées sur un
/// autre nœud sans risque de double paiement.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ChainError {
    /// Nœud injoignable ou connexion perdue **avant** envoi : on peut réessayer ailleurs.
    #[error("connexion : {0}")]
    Connection(String),
    /// La chaîne a refusé l'opération (fonds insuffisants, sous le dépôt existentiel…).
    #[error("refusé par la chaîne : {0}")]
    Rejected(String),
    /// Transaction envoyée mais statut inconnu : vérifier le solde avant de réessayer.
    #[error("issue inconnue après envoi : {0}")]
    UnknownOutcome(String),
    /// Entrée invalide.
    #[error("entrée invalide : {0}")]
    Invalid(String),
}

/// Soldes d'un compte (centimes de Ğ1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Balance {
    /// Total, dépôt existentiel compris.
    pub total: Amount,
    /// Dépensable : le total moins le dépôt existentiel et les montants bloqués.
    pub transferable: Amount,
    /// Dividendes universels non réclamés.
    pub unclaimed_uds: Amount,
}

/// Politique de virement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    /// `transfer_allow_death` : peut vider le compte (équivalent du `DRAIN` legacy).
    AllowDeath,
    /// `transfer_keep_alive` : refuse de passer sous le dépôt existentiel.
    KeepAlive,
}

/// Virement confirmé dans un bloc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxOutcome {
    /// Hash du bloc d'inclusion.
    pub block_hash: [u8; 32],
    /// Montant effectivement transféré selon l'événement `Balances::Transfer` du bloc.
    pub transferred: Amount,
}

/// `Balances::Transfer` observé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferEvent {
    /// Numéro de bloc.
    pub block_number: u64,
    /// Hash du bloc.
    pub block_hash: [u8; 32],
    /// Émetteur.
    pub from: Account,
    /// Destinataire.
    pub to: Account,
    /// Montant.
    pub amount: Amount,
}

/// Accès à une chaîne Ğ1.
pub trait Chain: Send + Sync + 'static {
    /// Soldes d'un compte.
    fn balance(&self, who: &Account) -> impl Future<Output = Result<Balance, ChainError>> + Send;

    /// Virement. `memo` est ajouté en `system.remark_with_event` dans la **même** transaction (`utility.batch`).
    /// Ne rend `Ok` qu'une fois le virement inclus **et** son événement `Balances::Transfer` constaté :
    /// une soumission acceptée n'est pas un paiement (un premier virement sous le dépôt existentiel échoue en silence).
    fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, mode: TransferMode, memo: Option<&str>) -> impl Future<Output = Result<TxOutcome, ChainError>> + Send;

    /// Diffuse les `Balances::Transfer` des blocs au fil de l'eau jusqu'à erreur ou fermeture du canal.
    fn watch_transfers(&self, out: mpsc::Sender<TransferEvent>) -> impl Future<Output = Result<(), ChainError>> + Send;

    /// Dépôt existentiel.
    fn existential_deposit(&self) -> Amount;
}

/// Implémentation sur un nœud RPC (`wss://…` ou `ws://…`).
pub struct SubxtChain {
    client: OnlineClient<PolkadotConfig>,
    existential_deposit: Amount,
}

fn connection(e: impl std::fmt::Display) -> ChainError {
    ChainError::Connection(e.to_string())
}

impl SubxtChain {
    /// Se connecte au nœud (délai de 15 s).
    pub async fn connect(url: &str) -> Result<Self, ChainError> {
        let client = tokio::time::timeout(Duration::from_secs(15), OnlineClient::<PolkadotConfig>::from_url(url))
            .await
            .map_err(|_| connection("délai de connexion dépassé"))?
            .map_err(connection)?;
        // Dépôt existentiel lu dans le runtime, jamais codé en dur.
        let ed = client.constants().at(&runtime::constants().balances().existential_deposit()).map_err(connection)?;
        Ok(Self { client, existential_deposit: Amount(ed) })
    }
}

fn amount_of(v: u64) -> Result<Amount, ChainError> {
    Ok(Amount(v))
}

impl Chain for SubxtChain {
    async fn balance(&self, who: &Account) -> Result<Balance, ChainError> {
        let call = runtime::apis().universal_dividend_api().account_balances(who.account_id());
        let b = self.client.runtime_api().at_latest().await.map_err(connection)?.call(call).await.map_err(connection)?;
        Ok(Balance { total: amount_of(b.total)?, transferable: amount_of(b.transferable)?, unclaimed_uds: amount_of(b.unclaim_uds)? })
    }

    async fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, mode: TransferMode, memo: Option<&str>) -> Result<TxOutcome, ChainError> {
        let dest = MultiAddress::Id(to.account_id());
        // Étape 1, SANS envoi : lecture du nonce (RPC) et signature. Toute erreur ici est rejouable sur un autre nœud.
        let params = subxt::config::DefaultExtrinsicParamsBuilder::<PolkadotConfig>::new().build();
        let mut tx = self.client.tx();
        let signed = match (memo, mode) {
            (None, TransferMode::AllowDeath) => tx.create_signed(&runtime::tx().balances().transfer_allow_death(dest, amount.0), from, params).await,
            (None, TransferMode::KeepAlive) => tx.create_signed(&runtime::tx().balances().transfer_keep_alive(dest, amount.0), from, params).await,
            (Some(memo), mode) => tx.create_signed(&crate::offline::memo_batch(dest, amount, mode, memo), from, params).await,
        }
        .map_err(connection)?;
        // Étape 2 : envoi. Au-delà de ce point on ne rejoue plus jamais ailleurs (risque de double paiement).
        let progress = signed.submit_and_watch().await.map_err(|e| classify_submit(&e.to_string()))?;
        // À partir d'ici la transaction est partie : toute perte de statut est une issue INCONNUE.
        let in_block = tokio::time::timeout(Duration::from_secs(120), wait_in_block(progress)).await.map_err(|_| ChainError::UnknownOutcome("délai d'inclusion dépassé".into()))??;
        let block_hash = in_block.block_hash().0;
        let events = in_block.wait_for_success().await.map_err(|e| ChainError::Rejected(e.to_string()))?;
        let transfer = events
            .find::<runtime::balances::events::Transfer>()
            .filter_map(Result::ok)
            .find(|t| t.from == from.account().account_id() && t.to == to.account_id())
            .ok_or_else(|| ChainError::Rejected("aucun événement Balances::Transfer : le virement n'a pas eu lieu".into()))?;
        Ok(TxOutcome { block_hash, transferred: amount_of(transfer.amount)? })
    }

    async fn watch_transfers(&self, out: mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
        let mut blocks = self.client.blocks().subscribe_best().await.map_err(connection)?;
        while let Some(block) = blocks.next().await {
            let block = block.map_err(connection)?;
            let events = block.events().await.map_err(connection)?;
            for ev in events.find::<runtime::balances::events::Transfer>().filter_map(Result::ok) {
                let msg = TransferEvent { block_number: u64::from(block.number()), block_hash: block.hash().0, from: Account::from(&ev.from), to: Account::from(&ev.to), amount: amount_of(ev.amount)? };
                if out.send(msg).await.is_err() {
                    return Ok(()); // plus de lecteur : arrêt normal
                }
            }
        }
        Err(connection("flux de blocs terminé"))
    }

    fn existential_deposit(&self) -> Amount {
        self.existential_deposit
    }
}

/// Après l'envoi : rejet explicite de la chaîne, ou issue inconnue (jamais « rejouable »).
fn classify_submit(msg: &str) -> ChainError {
    let lower = msg.to_ascii_lowercase();
    let rejected = ["invalid transaction", "inability to pay", "bad proof", "stale", "future", "payment", "priority", "banned", "dispatch"];
    if rejected.iter().any(|k| lower.contains(k)) { ChainError::Rejected(msg.to_owned()) } else { ChainError::UnknownOutcome(msg.to_owned()) }
}

async fn wait_in_block(mut progress: subxt::tx::TxProgress<PolkadotConfig, OnlineClient<PolkadotConfig>>) -> Result<subxt::tx::TxInBlock<PolkadotConfig, OnlineClient<PolkadotConfig>>, ChainError> {
    use subxt::tx::TxStatus;
    while let Some(status) = progress.next().await {
        match status.map_err(|e| ChainError::UnknownOutcome(e.to_string()))? {
            TxStatus::InBestBlock(b) | TxStatus::InFinalizedBlock(b) => return Ok(b),
            TxStatus::Error { message } | TxStatus::Invalid { message } | TxStatus::Dropped { message } => return Err(ChainError::Rejected(message)),
            _ => {}
        }
    }
    Err(ChainError::UnknownOutcome("flux de statut terminé sans inclusion".into()))
}
