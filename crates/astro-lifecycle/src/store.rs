//! Traduction en requêtes SQL pour `astro-db` : identités hébergées, état de cycle de vie, garde hebdomadaire.
//!
//! Le module ne touche pas la base : il fabrique des [`Statement`] que l'appelant passe au `Writer`.
//! `identities.hosted` est un état local, posé ici et jamais écrasé par un DID (voir `Projector`).

use astro_db::Statement;
use rusqlite::types::Value;

use crate::discovery::Discovered;
use crate::state::AccountState;

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// Inscrit les identités découvertes : NODE (`node`), Capitaine (`captain`), autres MULTIPASS (`multipass`), tous `hosted = 1`.
/// La formule de clés n'est écrite que si `.keyformula` la déclare : sinon la valeur existante est conservée.
pub fn identity_statements(d: &Discovered) -> Vec<Statement> {
    let mut out = vec![Statement::new(
        "INSERT INTO identities(hex, role, g1pub, ipns, peer_id, hosted) VALUES (?1, 'node', ?2, ?3, ?3, 1) \
         ON CONFLICT(hex) DO UPDATE SET role = 'node', g1pub = excluded.g1pub, ipns = excluded.ipns, peer_id = excluded.peer_id, hosted = 1",
        vec![text(&d.node.nostr_hex), text(&d.node.g1_pub), text(&d.node.peer_id)],
    )];
    let captain = d.captain().map(|m| m.hex.as_str());
    for m in &d.multipasses {
        let role = if Some(m.hex.as_str()) == captain { "captain" } else { "multipass" };
        let sql = if m.key_formula.is_some() {
            "INSERT INTO identities(hex, role, email, key_formula, hosted) VALUES (?1, ?2, ?3, ?4, 1) \
             ON CONFLICT(hex) DO UPDATE SET role = excluded.role, email = excluded.email, key_formula = excluded.key_formula, hosted = 1"
        } else {
            "INSERT INTO identities(hex, role, email, hosted) VALUES (?1, ?2, ?3, 1) \
             ON CONFLICT(hex) DO UPDATE SET role = excluded.role, email = excluded.email, hosted = 1"
        };
        let mut params = vec![text(&m.hex), text(role), text(&m.email)];
        if let Some(f) = m.key_formula {
            params.push(text(f.as_str()));
        }
        out.push(Statement::new(sql, params));
    }
    out
}

/// État de cycle de vie d'un compte, dans `kv` (scope `lifecycle`).
pub fn save_state(hex: &str, state: &AccountState, at: u64) -> Statement {
    Statement::new(
        "INSERT INTO kv(scope, key, value, updated_at) VALUES ('lifecycle', ?1, ?2, ?3) \
         ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        vec![text(hex), Value::Text(state.to_json().to_string()), Value::Integer(at as i64)],
    )
}

/// Lecture de l'état d'un compte.
pub fn load_state_sql() -> &'static str {
    "SELECT value FROM kv WHERE scope = 'lifecycle' AND key = ?1"
}

/// Clé de la garde hebdomadaire d'un compte : une semaine ISO n'est évaluée qu'une fois par compte.
pub fn week_key(hex: &str, week: &str) -> String {
    format!("lifecycle:{week}:{hex}")
}

/// Pose la garde hebdomadaire (`changes == 0` si elle existait déjà).
pub fn claim_week(hex: &str, week: &str, at: u64) -> Statement {
    Statement::new("INSERT OR IGNORE INTO done(key, value, at) VALUES (?1, 'evaluated', ?2)", vec![Value::Text(week_key(hex, week)), Value::Integer(at as i64)])
}

/// Reflète le statut (hors DID) pour les écrans : `identities.contract` suit le contrat du compte.
pub fn set_contract(hex: &str, state: &AccountState) -> Statement {
    let contract = match state.contract {
        crate::state::Contract::ActiveRental => "active_rental",
        crate::state::Contract::CooperativeMember => "cooperative_member",
    };
    Statement::new("UPDATE identities SET contract = ?2 WHERE hex = ?1", vec![text(hex), text(contract)])
}
