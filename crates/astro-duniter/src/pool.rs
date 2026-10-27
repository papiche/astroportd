//! Pool de nœuds RPC avec bascule automatique.
//!
//! - Un nœud en panne est mis de côté pendant un délai de grâce, puis réessayé.
//! - Les lectures et la surveillance rebasculent sans condition.
//! - Un **virement n'est jamais rejoué** une fois envoyé : seul un échec *avant* l'envoi (connexion, lecture du nonce)
//!   passe au nœud suivant. Après l'envoi, une perte de connexion est une issue inconnue à vérifier par le solde.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::chain::{Balance, Chain, ChainError, SubxtChain, TransferEvent, TransferMode, TxOutcome};
use crate::{Account, Amount, Ed25519Signer};

/// Fabrique de connexions (permet de simuler des nœuds dans les tests).
pub trait Connector: Send + Sync + 'static {
    /// Type de connexion produite.
    type Chain: Chain;
    /// Ouvre une connexion vers `url`.
    fn connect(&self, url: &str) -> impl Future<Output = Result<Self::Chain, ChainError>> + Send;
}

/// Connecteur `subxt` réel.
#[derive(Debug, Default, Clone, Copy)]
pub struct SubxtConnector;

impl Connector for SubxtConnector {
    type Chain = SubxtChain;

    async fn connect(&self, url: &str) -> Result<SubxtChain, ChainError> {
        SubxtChain::connect(url).await
    }
}

/// Réglages du pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Nœuds, par ordre de préférence.
    pub urls: Vec<String>,
    /// Durée pendant laquelle un nœud en panne n'est pas réessayé.
    pub cooldown: Duration,
    /// Pause avant de rebasculer quand la surveillance perd sa connexion.
    pub reconnect_delay: Duration,
}

impl PoolConfig {
    /// Nœuds publics de la Ğ1 (liste de `gcli`, `data.rs`), avec 60 s de grâce.
    pub fn default_g1() -> Self {
        let urls = ["wss://g1.axiom-team.fr/ws", "wss://g1.p2p.legal/ws", "wss://g1.asycn.io/ws/", "wss://g1.coinduf.eu", "wss://g1.gyroi.de", "wss://archive-rpc.g1.brussels.ovh", "wss://g1.rendall.fr/ws"];
        Self { urls: urls.iter().map(|u| (*u).to_owned()).collect(), cooldown: Duration::from_secs(60), reconnect_delay: Duration::from_secs(2) }
    }
}

struct State<C> {
    active: Option<(usize, Arc<C>)>,
    down_until: Vec<Option<Instant>>,
}

/// Pool de nœuds : se comporte comme une [`Chain`] unique et résiliente.
pub struct RpcPool<K: Connector> {
    connector: K,
    cfg: PoolConfig,
    state: Mutex<State<K::Chain>>,
    ed: AtomicU64,
}

impl<K: Connector> RpcPool<K> {
    /// Crée le pool (aucune connexion avant le premier appel).
    pub fn new(connector: K, cfg: PoolConfig) -> Self {
        let n = cfg.urls.len();
        Self { connector, cfg, state: Mutex::new(State { active: None, down_until: vec![None; n] }), ed: AtomicU64::new(100) }
    }

    fn mark_down(&self, idx: usize) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.active.as_ref().is_some_and(|(i, _)| *i == idx) {
            st.active = None;
        }
        if let Some(slot) = st.down_until.get_mut(idx) {
            *slot = Some(Instant::now() + self.cfg.cooldown);
        }
    }

    /// Connexion active, ou première connexion réussie parmi les nœuds non mis de côté.
    async fn acquire(&self) -> Result<(usize, Arc<K::Chain>), ChainError> {
        if let Some((i, c)) = self.state.lock().unwrap_or_else(|e| e.into_inner()).active.clone() {
            return Ok((i, c));
        }
        let mut last = ChainError::Connection("aucun nœud configuré".into());
        let mut tried_any = false;
        for (idx, url) in self.cfg.urls.iter().enumerate() {
            let available = {
                let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                st.down_until.get(idx).copied().flatten().is_none_or(|t| Instant::now() >= t)
            };
            if !available {
                continue;
            }
            tried_any = true;
            match self.connector.connect(url).await {
                Ok(chain) => {
                    self.ed.store(chain.existential_deposit().0, Ordering::Relaxed);
                    let chain = Arc::new(chain);
                    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.down_until[idx] = None;
                    st.active = Some((idx, chain.clone()));
                    return Ok((idx, chain));
                }
                Err(e) => {
                    self.mark_down(idx);
                    last = e;
                }
            }
        }
        if !tried_any {
            return Err(ChainError::Connection("tous les nœuds sont en pause après des échecs récents".into()));
        }
        Err(last)
    }

    /// Exécute une lecture avec bascule : chaque nœud est tenté au plus une fois par appel.
    async fn read<T, F, Fut>(&self, op: F) -> Result<T, ChainError>
    where
        F: Fn(Arc<K::Chain>) -> Fut,
        Fut: Future<Output = Result<T, ChainError>>,
    {
        let mut last = ChainError::Connection("aucun nœud".into());
        for _ in 0..self.cfg.urls.len().max(1) {
            let (idx, chain) = self.acquire().await?;
            match op(chain).await {
                Err(ChainError::Connection(e)) => {
                    self.mark_down(idx);
                    last = ChainError::Connection(e);
                }
                other => return other,
            }
        }
        Err(last)
    }
}

impl<K: Connector> Chain for RpcPool<K> {
    async fn balance(&self, who: &Account) -> Result<Balance, ChainError> {
        self.read(|c| async move { c.balance(who).await }).await
    }

    async fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, mode: TransferMode, memo: Option<&str>) -> Result<TxOutcome, ChainError> {
        // Seule une erreur `Connection` (avant envoi) autorise de passer au nœud suivant.
        let mut last = ChainError::Connection("aucun nœud".into());
        for _ in 0..self.cfg.urls.len().max(1) {
            let (idx, chain) = self.acquire().await?;
            match chain.transfer(from, to, amount, mode, memo).await {
                Err(ChainError::Connection(e)) => {
                    self.mark_down(idx);
                    last = ChainError::Connection(e);
                }
                other => return other,
            }
        }
        Err(last)
    }

    async fn watch_transfers(&self, out: mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
        loop {
            if out.is_closed() {
                return Ok(());
            }
            // Nœud injoignable ou flux coupé : on le met de côté et on rebascule après une pause.
            if let Ok((idx, chain)) = self.acquire().await {
                if chain.watch_transfers(out.clone()).await.is_ok() {
                    return Ok(());
                }
                self.mark_down(idx);
            }
            tokio::time::sleep(self.cfg.reconnect_delay).await;
        }
    }

    fn existential_deposit(&self) -> Amount {
        Amount(self.ed.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// Comportement d'un faux nœud.
    #[derive(Clone)]
    enum Node {
        Down,
        Up { transfer: Result<TxOutcome, ChainError>, watch_fails: bool },
    }

    #[derive(Default)]
    struct Log {
        connects: Mutex<Vec<String>>,
        transfers: AtomicUsize,
    }

    struct MockChain {
        node: Node,
        log: Arc<Log>,
    }

    impl Chain for MockChain {
        async fn balance(&self, _: &Account) -> Result<Balance, ChainError> {
            Ok(Balance { total: Amount(500), transferable: Amount(400), unclaimed_uds: Amount(0) })
        }

        async fn transfer(&self, _: &Ed25519Signer, _: &Account, _: Amount, _: TransferMode, _: Option<&str>) -> Result<TxOutcome, ChainError> {
            self.log.transfers.fetch_add(1, Ordering::SeqCst);
            match &self.node {
                Node::Up { transfer, .. } => transfer.clone(),
                Node::Down => Err(ChainError::Connection("down".into())),
            }
        }

        async fn watch_transfers(&self, out: mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
            match &self.node {
                Node::Up { watch_fails: false, .. } => {
                    let ev = TransferEvent { block_number: 1, block_hash: [0; 32], from: Account([1; 32]), to: Account([2; 32]), amount: Amount(5) };
                    out.send(ev).await.map_err(|e| ChainError::Invalid(e.to_string()))?;
                    Ok(())
                }
                _ => Err(ChainError::Connection("flux coupé".into())),
            }
        }

        fn existential_deposit(&self) -> Amount {
            Amount(100)
        }
    }

    struct MockConnector {
        nodes: Mutex<HashMap<String, Node>>,
        log: Arc<Log>,
    }

    impl Connector for MockConnector {
        type Chain = MockChain;

        async fn connect(&self, url: &str) -> Result<MockChain, ChainError> {
            self.log.connects.lock().unwrap().push(url.to_owned());
            match self.nodes.lock().unwrap().get(url).cloned() {
                Some(Node::Down) | None => Err(ChainError::Connection(format!("{url} injoignable"))),
                Some(node) => Ok(MockChain { node, log: self.log.clone() }),
            }
        }
    }

    fn ok_tx() -> Result<TxOutcome, ChainError> {
        Ok(TxOutcome { block_hash: [9; 32], transferred: Amount(100) })
    }

    fn pool(nodes: &[(&str, Node)]) -> (RpcPool<MockConnector>, Arc<Log>) {
        let log = Arc::new(Log::default());
        let connector = MockConnector { nodes: Mutex::new(nodes.iter().map(|(u, n)| ((*u).to_owned(), n.clone())).collect()), log: log.clone() };
        let cfg = PoolConfig { urls: nodes.iter().map(|(u, _)| (*u).to_owned()).collect(), cooldown: Duration::from_millis(80), reconnect_delay: Duration::from_millis(5) };
        (RpcPool::new(connector, cfg), log)
    }

    fn signer() -> Ed25519Signer {
        Ed25519Signer::from_seed([1; 32])
    }

    #[tokio::test]
    async fn a_dead_first_node_fails_over_to_the_next_for_reads_and_transfers() {
        let (p, log) = pool(&[("a", Node::Down), ("b", Node::Up { transfer: ok_tx(), watch_fails: false })]);
        assert_eq!(p.balance(&Account([1; 32])).await.unwrap().transferable, Amount(400));
        let out = p.transfer(&signer(), &Account([2; 32]), Amount(100), TransferMode::KeepAlive, None).await.unwrap();
        assert_eq!(out.transferred, Amount(100));
        assert_eq!(log.transfers.load(Ordering::SeqCst), 1);
        assert_eq!(*log.connects.lock().unwrap(), ["a", "b"], "nœud actif réutilisé, nœud mort mis de côté");
        assert_eq!(p.existential_deposit(), Amount(100));
    }

    #[tokio::test]
    async fn a_transfer_whose_connection_dies_before_sending_is_replayed_once_on_another_node() {
        let (p, log) = pool(&[("a", Node::Up { transfer: Err(ChainError::Connection("nonce".into())), watch_fails: false }), ("b", Node::Up { transfer: ok_tx(), watch_fails: false })]);
        p.transfer(&signer(), &Account([2; 32]), Amount(100), TransferMode::KeepAlive, None).await.unwrap();
        assert_eq!(log.transfers.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_transfer_is_never_replayed_after_submission() {
        for err in [ChainError::UnknownOutcome("perdu".into()), ChainError::Rejected("fonds".into())] {
            let (p, log) = pool(&[("a", Node::Up { transfer: Err(err.clone()), watch_fails: false }), ("b", Node::Up { transfer: ok_tx(), watch_fails: false })]);
            let got = p.transfer(&signer(), &Account([2; 32]), Amount(100), TransferMode::KeepAlive, None).await;
            assert_eq!(got, Err(err));
            assert_eq!(log.transfers.load(Ordering::SeqCst), 1, "un seul envoi : pas de double paiement");
        }
    }

    #[tokio::test]
    async fn all_nodes_down_is_a_connection_error_and_a_cooling_node_is_retried_later() {
        let (p, _) = pool(&[("a", Node::Down), ("b", Node::Down)]);
        assert!(matches!(p.balance(&Account([1; 32])).await, Err(ChainError::Connection(_))));
        assert!(matches!(p.balance(&Account([1; 32])).await, Err(ChainError::Connection(m)) if m.contains("en pause")));
        tokio::time::sleep(Duration::from_millis(100)).await;
        p.connector.nodes.lock().unwrap().insert("a".into(), Node::Up { transfer: ok_tx(), watch_fails: false });
        assert!(p.balance(&Account([1; 32])).await.is_ok(), "après la grâce, le nœud est réessayé");
    }

    #[tokio::test]
    async fn the_watch_reconnects_to_another_node_and_feeds_the_notifier() {
        let (p, _) = pool(&[("a", Node::Up { transfer: ok_tx(), watch_fails: true }), ("b", Node::Up { transfer: ok_tx(), watch_fails: false })]);
        let notifier = crate::FundsNotifier::new([Account([2; 32])]);
        let mut rx = notifier.subscribe();
        tokio::time::timeout(Duration::from_secs(5), notifier.run(&p)).await.unwrap().unwrap();
        assert_eq!(rx.try_recv().unwrap().amount, Amount(5));
    }
}
