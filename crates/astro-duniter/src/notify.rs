//! Notification instantanée des fonds reçus par les comptes de la station.

use std::collections::HashSet;
use std::sync::RwLock;

use tokio::sync::{broadcast, mpsc};

use crate::chain::{Chain, ChainError, TransferEvent};
use crate::{Account, Amount};

/// Fonds reçus par un compte surveillé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingFunds {
    /// Compte de la station crédité.
    pub account: Account,
    /// Émetteur.
    pub from: Account,
    /// Montant reçu.
    pub amount: Amount,
    /// Bloc d'inclusion.
    pub block_number: u64,
    /// Hash du bloc.
    pub block_hash: [u8; 32],
}

/// Filtre le flux `Balances::Transfer` de la chaîne sur les comptes de la station et le rediffuse.
pub struct FundsNotifier {
    /// Comptes surveillés : l'ensemble change quand un MULTIPASS naît, sans relancer la surveillance.
    accounts: RwLock<HashSet<Account>>,
    tx: broadcast::Sender<IncomingFunds>,
}

impl FundsNotifier {
    /// Surveille `accounts` ; chaque abonné reçoit les notifications dès l'observation du bloc.
    pub fn new(accounts: impl IntoIterator<Item = Account>) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { accounts: RwLock::new(accounts.into_iter().collect()), tx }
    }

    /// Remplace l'ensemble des comptes surveillés (nouveaux MULTIPASS, comptes détruits).
    pub fn set_accounts(&self, accounts: impl IntoIterator<Item = Account>) {
        *self.accounts.write().unwrap_or_else(|e| e.into_inner()) = accounts.into_iter().collect();
    }

    /// Nombre de comptes surveillés.
    pub fn watched(&self) -> usize {
        self.accounts.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Nouvel abonné.
    pub fn subscribe(&self) -> broadcast::Receiver<IncomingFunds> {
        self.tx.subscribe()
    }

    /// Notification pour un événement, si le destinataire est surveillé.
    pub fn handle(&self, ev: &TransferEvent) -> Option<IncomingFunds> {
        if !self.accounts.read().unwrap_or_else(|e| e.into_inner()).contains(&ev.to) {
            return None;
        }
        let note = IncomingFunds { account: ev.to, from: ev.from, amount: ev.amount, block_number: ev.block_number, block_hash: ev.block_hash };
        // Aucun abonné n'est pas une erreur : la notification est simplement perdue.
        let _ = self.tx.send(note.clone());
        Some(note)
    }

    /// Branche le notifieur sur `chain` jusqu'à la fermeture du flux (ou erreur de la chaîne).
    pub async fn run<C: Chain>(&self, chain: &C) -> Result<(), ChainError> {
        let (tx, mut rx) = mpsc::channel(256);
        let watch = chain.watch_transfers(tx);
        let pump = async {
            while let Some(ev) = rx.recv().await {
                self.handle(&ev);
            }
        };
        let (res, ()) = tokio::join!(watch, pump);
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(to: u8, amount: u64) -> TransferEvent {
        TransferEvent { block_number: 10, block_hash: [1; 32], from: Account([7; 32]), to: Account([to; 32]), amount: Amount(amount) }
    }

    #[test]
    fn the_watched_set_can_change_while_running() {
        let n = FundsNotifier::new([Account([1; 32])]);
        assert!(n.handle(&ev(2, 100)).is_none());
        n.set_accounts([Account([1; 32]), Account([2; 32])]);
        assert_eq!(n.watched(), 2);
        assert!(n.handle(&ev(2, 100)).is_some());
        n.set_accounts([Account([2; 32])]);
        assert!(n.handle(&ev(1, 100)).is_none());
    }

    #[test]
    fn only_watched_accounts_are_notified() {
        let n = FundsNotifier::new([Account([1; 32])]);
        let mut rx = n.subscribe();
        assert!(n.handle(&ev(2, 100)).is_none());
        let got = n.handle(&ev(1, 250)).unwrap();
        assert_eq!(got.amount, Amount(250));
        assert_eq!(rx.try_recv().unwrap(), got);
        assert!(rx.try_recv().is_err());
    }
}
