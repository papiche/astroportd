//! Moteur comptable coopératif : registre Ğ1-N², règle des 3×1/3, Love Ledger.
//!
//! - [`ledger`] : validation intégrale du kind 30852 (signatures, chaînage `prev`, anti-double dépense, pas de découvert) ;
//! - [`lease`] : régie des communs — tarifs du nœud (disque, GPU 1/24) et baux temporels payés en Ẑen ;
//! - [`split`] : ventilation d'un surplus en `IMPOT` / prime du Capitaine / `CASH` / `RnD` / `ASSETS` ;
//! - [`love`] : clôture hebdomadaire et bascule en *Bénévolat Actif* quand la trésorerie ne couvre plus les charges ;
//! - [`store`] : traduction en requêtes SQL (garde d'idempotence `done`, `love_ledger`, `journal`).
//!
//! Tous les montants sont des centimes entiers ([`Amount`]), jamais des flottants.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod ledger;
pub mod lease;
pub mod love;
pub mod split;
pub mod store;

pub use astro_core::{Amount, AmountError};
pub use ledger::{Applied, Ledger, LedgerConfig, LedgerError, Outcome, ReplayReport, Tx};
pub use love::{Attestation, VolunteerWeek, WeekInput, WeekOutcome, close_week};
pub use split::{Allocation, SplitConfig, SplitError};
