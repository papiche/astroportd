//! Reconstruction du statut des comptes après un `replay`.
//!
//! L'état de cycle de vie (`kv`, scope `lifecycle`) est une **projection** : le `Projector` relève, pour chaque
//! MULTIPASS hébergé, les loyers constatés dans les kinds 30852 et 30851 (`rent_payments`) ; ce module en déduit le
//! statut. La règle est la même que celle de [`crate::state::decide`], appliquée à l'historique :
//! dernier loyer + 7 jours = échéance ; échéance dépassée = grâce 7 jours ; au-delà = suspension. Jamais de destruction.
//!
//! Un compte sans loyer ni date de création connue est considéré comme neuf (à jour) : une donnée manquante ne
//! suspend jamais personne.

use astro_db::{Db, DbError, Statement};
use rusqlite::types::Value;

use crate::state::{AccountState, CYCLE_SECS, Contract, GRACE_SECS, Status};
use crate::store;

/// Statut d'un compte à l'instant `now`, d'après son dernier loyer constaté.
///
/// Pour un sociétaire, la validité des parts n'est pas visible dans Nostr : le compte est rétabli à jour, et la
/// prochaine évaluation hebdomadaire corrige si besoin.
pub fn rebuild_state(contract: Contract, created_at: u64, last_rent: Option<u64>, now: u64) -> AccountState {
    let mut s = AccountState::new(contract, created_at);
    s.last_ok_at = last_rent;
    if contract == Contract::CooperativeMember {
        return s;
    }
    let due = last_rent.unwrap_or(created_at).saturating_add(CYCLE_SECS);
    if now < due {
        return s;
    }
    if now < due.saturating_add(GRACE_SECS) {
        s.status = Status::GracePeriod;
        s.status_since = due;
    } else {
        s.status = Status::Suspended;
        s.status_since = due.saturating_add(GRACE_SECS);
    }
    s
}

/// Ligne lue pour un compte hébergé.
struct Row {
    hex: String,
    contract: Contract,
    created_at: Option<u64>,
    last_rent: Option<u64>,
}

/// Reconstruit l'état de tous les MULTIPASS hébergés dont le contrat est connu et qui n'ont pas déjà un état.
/// Retourne le nombre de comptes rétablis.
pub async fn rebuild_all(db: &Db, now: u64) -> Result<usize, DbError> {
    let rows = db
        .readers()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT i.hex, i.contract, i.created_at, (SELECT MAX(at) FROM rent_payments r WHERE r.owner_hex = i.hex) \
                 FROM identities i \
                 WHERE i.hosted = 1 AND i.role = 'multipass' AND i.contract IS NOT NULL \
                   AND NOT EXISTS (SELECT 1 FROM kv WHERE scope = 'lifecycle' AND key = i.hex)",
            )?;
            let found = stmt
                .query_map([], |r| {
                    let contract: String = r.get(1)?;
                    Ok(Row {
                        hex: r.get(0)?,
                        contract: if contract == "active_rental" { Contract::ActiveRental } else { Contract::CooperativeMember },
                        created_at: r.get::<_, Option<i64>>(2)?.and_then(|n| u64::try_from(n).ok()),
                        last_rent: r.get::<_, Option<i64>>(3)?.and_then(|n| u64::try_from(n).ok()),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(found)
        })
        .await?;
    let mut stmts: Vec<Statement> = Vec::new();
    for r in &rows {
        let state = rebuild_state(r.contract, r.created_at.or(r.last_rent).unwrap_or(now), r.last_rent, now);
        stmts.push(store::save_state(&r.hex, &state, now));
        stmts.push(store::set_contract(&r.hex, &state));
        stmts.push(Statement::new(
            "UPDATE identities SET status = ?2 WHERE hex = ?1 AND status IS NOT ?2",
            vec![Value::Text(r.hex.clone()), Value::Text(state.status.as_str().to_owned())],
        ));
    }
    if !stmts.is_empty() {
        db.writer().batch(stmts).await?;
    }
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const T0: u64 = 1_800_000_000;

    #[test]
    fn a_paid_account_stays_in_good_standing_until_the_next_due_date() {
        let s = rebuild_state(Contract::ActiveRental, T0, Some(T0 + 7 * DAY), T0 + 13 * DAY);
        assert_eq!((s.status, s.last_ok_at), (Status::ActiveRental, Some(T0 + 7 * DAY)));
    }

    #[test]
    fn a_missed_rent_goes_grace_then_suspended_like_the_live_machine() {
        let paid = Some(T0 + 7 * DAY);
        let g = rebuild_state(Contract::ActiveRental, T0, paid, T0 + 15 * DAY);
        assert_eq!((g.status, g.status_since), (Status::GracePeriod, T0 + 14 * DAY));
        let s = rebuild_state(Contract::ActiveRental, T0, paid, T0 + 21 * DAY);
        assert_eq!((s.status, s.status_since), (Status::Suspended, T0 + 21 * DAY));
    }

    #[test]
    fn an_account_that_never_paid_is_judged_from_its_creation() {
        assert_eq!(rebuild_state(Contract::ActiveRental, T0, None, T0 + 6 * DAY).status, Status::ActiveRental);
        assert_eq!(rebuild_state(Contract::ActiveRental, T0, None, T0 + 8 * DAY).status, Status::GracePeriod);
    }

    #[test]
    fn members_are_restored_in_good_standing() {
        assert_eq!(rebuild_state(Contract::CooperativeMember, T0, None, T0 + 400 * DAY).status, Status::CooperativeMember);
    }
}
