//! Machine d'état hebdomadaire d'un MULTIPASS.
//!
//! Chaque semaine (7 jours après le dernier prélèvement, ou la création) :
//!
//! - `active_rental` : prélèvement de la redevance Opex en ẐEN ;
//! - `cooperative_member` : validation des parts sociétaires (ZenCard Capex).
//!
//! Si le prélèvement ou la validation échoue, le compte entre en `grace_period` pour 7 jours, puis passe en
//! `suspended`. **Aucune destruction automatique** : un compte suspendu reste intact (clés, données, identité) et se
//! réactive dès que le prélèvement ou la validation réussit ; il peut aussi déménager (`migration`).
//!
//! Le legacy ne connaît ni `grace_period` ni `suspended` pour les comptes en défaut de paiement : il les détruit après 7 jours.

use astro_core::Amount;

/// Durée d'un cycle.
pub const CYCLE_SECS: u64 = 7 * 86_400;
/// Durée de la période de grâce.
pub const GRACE_SECS: u64 = 7 * 86_400;

/// Nature du contrat (colonne `identities.contract`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contract {
    /// Locataire : redevance hebdomadaire.
    ActiveRental,
    /// Sociétaire : parts validées.
    CooperativeMember,
}

/// Statut du compte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Locataire à jour.
    ActiveRental,
    /// Sociétaire à jour.
    CooperativeMember,
    /// Défaut constaté, délai de régularisation en cours.
    GracePeriod,
    /// Délai écoulé : services coupés, compte conservé.
    Suspended,
}

impl Status {
    /// Nom stocké (`active_rental`, `cooperative_member`, `grace_period`, `suspended`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ActiveRental => "active_rental",
            Self::CooperativeMember => "cooperative_member",
            Self::GracePeriod => "grace_period",
            Self::Suspended => "suspended",
        }
    }

    /// Statut d'un compte à jour pour ce contrat.
    pub fn good_standing(c: Contract) -> Self {
        match c {
            Contract::ActiveRental => Self::ActiveRental,
            Contract::CooperativeMember => Self::CooperativeMember,
        }
    }
}

/// État persistant d'un compte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountState {
    /// Contrat.
    pub contract: Contract,
    /// Statut courant.
    pub status: Status,
    /// Création du compte.
    pub created_at: u64,
    /// Dernier prélèvement réussi, ou dernière validation des parts.
    pub last_ok_at: Option<u64>,
    /// Début du statut courant (pour la grâce).
    pub status_since: u64,
}

impl AccountState {
    /// Compte neuf : à jour, premier prélèvement dans 7 jours.
    pub fn new(contract: Contract, created_at: u64) -> Self {
        Self { contract, status: Status::good_standing(contract), created_at, last_ok_at: None, status_since: created_at }
    }

    /// JSON stocké dans `kv` (scope `lifecycle`).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "contract": match self.contract { Contract::ActiveRental => "active_rental", Contract::CooperativeMember => "cooperative_member" },
            "status": self.status.as_str(), "created_at": self.created_at, "last_ok_at": self.last_ok_at, "status_since": self.status_since,
        })
    }
}

/// Constat de la semaine, fourni par l'appelant (soldes lus sur la chaîne, parts lues dans la ZenCard).
#[derive(Debug, Clone, Copy)]
pub struct Cycle {
    /// Redevance hebdomadaire due (locataire).
    pub rent_due: Amount,
    /// Solde ẐEN disponible du MULTIPASS.
    pub balance: Amount,
    /// Les parts sociétaires sont valides (sociétaire).
    pub shares_valid: bool,
}

/// Ce que la machine ordonne.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Rien à faire cette semaine.
    NotDue,
    /// Prélever la redevance.
    Charge(Amount),
    /// Parts validées.
    SharesValidated,
    /// Défaut constaté : ouverture de la période de grâce (prévenir le titulaire).
    EnteredGrace,
    /// Toujours en grâce, rien ne change.
    StillInGrace,
    /// Grâce écoulée : suspension (prévenir le titulaire).
    Suspended,
    /// Toujours suspendu.
    StillSuspended,
    /// Régularisé : compte réactivé (avec prélèvement des arriérés d'une semaine pour un locataire).
    Reactivated(Option<Amount>),
}

/// Décision : nouvel état et action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// État après la décision.
    pub state: AccountState,
    /// Action ordonnée.
    pub action: Action,
}

/// Évalue un compte à l'instant `now`.
pub fn decide(state: &AccountState, c: &Cycle, now: u64) -> Decision {
    let mut next = state.clone();
    let ok = match state.contract {
        Contract::ActiveRental => c.balance >= c.rent_due,
        Contract::CooperativeMember => c.shares_valid,
    };
    let settle = |next: &mut AccountState| -> Action {
        next.status = Status::good_standing(state.contract);
        next.status_since = now;
        next.last_ok_at = Some(now);
        match state.contract {
            Contract::ActiveRental => Action::Charge(c.rent_due),
            Contract::CooperativeMember => Action::SharesValidated,
        }
    };
    let action = match state.status {
        Status::Suspended if ok => match settle(&mut next) {
            Action::Charge(a) => Action::Reactivated(Some(a)),
            _ => Action::Reactivated(None),
        },
        Status::Suspended => Action::StillSuspended,
        Status::GracePeriod if ok => match settle(&mut next) {
            Action::Charge(a) => Action::Reactivated(Some(a)),
            _ => Action::Reactivated(None),
        },
        Status::GracePeriod if now >= state.status_since + GRACE_SECS => {
            next.status = Status::Suspended;
            next.status_since = now;
            Action::Suspended
        }
        Status::GracePeriod => Action::StillInGrace,
        Status::ActiveRental | Status::CooperativeMember => {
            let since = state.last_ok_at.unwrap_or(state.created_at);
            if now < since + CYCLE_SECS {
                Action::NotDue
            } else if ok {
                settle(&mut next)
            } else {
                next.status = Status::GracePeriod;
                next.status_since = now;
                Action::EnteredGrace
            }
        }
    };
    Decision { state: next, action }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const T0: u64 = 1_800_000_000;

    fn cycle(balance: u64) -> Cycle {
        Cycle { rent_due: Amount(5_000), balance: Amount(balance), shares_valid: true }
    }

    #[test]
    fn a_new_account_is_charged_after_seven_days_not_before() {
        let s = AccountState::new(Contract::ActiveRental, T0);
        assert_eq!(decide(&s, &cycle(9_999), T0 + 6 * DAY).action, Action::NotDue);
        let d = decide(&s, &cycle(9_999), T0 + 7 * DAY);
        assert_eq!(d.action, Action::Charge(Amount(5_000)));
        assert_eq!((d.state.status, d.state.last_ok_at), (Status::ActiveRental, Some(T0 + 7 * DAY)));
        // Le cycle suivant repart du prélèvement.
        assert_eq!(decide(&d.state, &cycle(9_999), T0 + 13 * DAY).action, Action::NotDue);
        assert_eq!(decide(&d.state, &cycle(9_999), T0 + 14 * DAY).action, Action::Charge(Amount(5_000)));
    }

    #[test]
    fn unpaid_rent_goes_grace_then_suspended_and_nothing_is_destroyed() {
        let s = AccountState::new(Contract::ActiveRental, T0);
        let g = decide(&s, &cycle(4_999), T0 + 7 * DAY);
        assert_eq!((g.action.clone(), g.state.status), (Action::EnteredGrace, Status::GracePeriod));
        assert_eq!(decide(&g.state, &cycle(0), T0 + 10 * DAY).action, Action::StillInGrace);
        let susp = decide(&g.state, &cycle(0), T0 + 14 * DAY);
        assert_eq!((susp.action, susp.state.status), (Action::Suspended, Status::Suspended));
        assert_eq!(decide(&susp.state, &cycle(0), T0 + 60 * DAY).action, Action::StillSuspended);
        assert_eq!(susp.state.contract, Contract::ActiveRental, "le contrat survit à la suspension");
    }

    #[test]
    fn paying_during_grace_or_after_suspension_reactivates_the_account() {
        let s = AccountState::new(Contract::ActiveRental, T0);
        let g = decide(&s, &cycle(0), T0 + 7 * DAY).state;
        let r = decide(&g, &cycle(5_000), T0 + 9 * DAY);
        assert_eq!((r.action, r.state.status), (Action::Reactivated(Some(Amount(5_000))), Status::ActiveRental));
        let susp = decide(&g, &cycle(0), T0 + 14 * DAY).state;
        let r = decide(&susp, &cycle(8_000), T0 + 90 * DAY);
        assert_eq!((r.action, r.state.status, r.state.last_ok_at), (Action::Reactivated(Some(Amount(5_000))), Status::ActiveRental, Some(T0 + 90 * DAY)));
    }

    #[test]
    fn cooperative_members_are_validated_on_shares_not_on_balance() {
        let s = AccountState::new(Contract::CooperativeMember, T0);
        let broke_but_member = Cycle { rent_due: Amount(5_000), balance: Amount(0), shares_valid: true };
        let d = decide(&s, &broke_but_member, T0 + 7 * DAY);
        assert_eq!((d.action, d.state.status), (Action::SharesValidated, Status::CooperativeMember));
        let no_shares = Cycle { shares_valid: false, balance: Amount(1_000_000), ..broke_but_member };
        let g = decide(&d.state, &no_shares, T0 + 14 * DAY);
        assert_eq!((g.action.clone(), g.state.status), (Action::EnteredGrace, Status::GracePeriod));
        let back = decide(&g.state, &broke_but_member, T0 + 16 * DAY);
        assert_eq!((back.action, back.state.status), (Action::Reactivated(None), Status::CooperativeMember));
    }

    #[test]
    fn states_serialize_with_the_documented_status_names() {
        let j = AccountState::new(Contract::CooperativeMember, T0).to_json();
        assert_eq!((j["status"].as_str(), j["contract"].as_str()), (Some("cooperative_member"), Some("cooperative_member")));
        assert_eq!(Status::GracePeriod.as_str(), "grace_period");
        assert_eq!(Status::Suspended.as_str(), "suspended");
    }
}
