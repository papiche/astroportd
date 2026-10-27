//! Traduction des décisions comptables en requêtes SQL pour `astro-db` (journal, garde d'idempotence, `love_ledger`).
//!
//! Le module ne touche pas la base : il fabrique des [`Statement`] à passer au `Writer`. La garde `done` est le
//! verrou hebdomadaire : une même semaine n'est jamais ventilée deux fois, même si la clôture est relancée.

use astro_db::Statement;
use rusqlite::types::Value;

use crate::love::{Attestation, VolunteerWeek, WeekOutcome};
use crate::split::Allocation;

/// Clé de la garde d'idempotence d'une semaine.
pub fn week_key(week: &str) -> String {
    format!("economy:close:{week}")
}

/// `SELECT 1` : la semaine est-elle déjà clôturée ?
pub fn week_is_closed() -> &'static str {
    "SELECT 1 FROM done WHERE key = ?1"
}

/// Requêtes à appliquer **dans une seule transaction** pour acter une clôture. La première pose la garde :
/// `INSERT OR IGNORE` + [`Statement`] suivants conditionnés par la lecture préalable de [`week_is_closed`].
pub fn close_statements(week: &str, outcome: &WeekOutcome, captain_hex: &str, at: u64) -> Vec<Statement> {
    let mut stmts = vec![Statement::new(
        "INSERT OR IGNORE INTO done(key, value, at) VALUES (?1, ?2, ?3)",
        vec![Value::Text(week_key(week)), Value::Text(kind_of(outcome).into()), Value::Integer(at as i64)],
    )];
    let (stream, data) = match outcome {
        WeekOutcome::Split(a) => ("economy.3x13", a.to_json()),
        WeekOutcome::Drawdown { gap, .. } => ("economy.drawdown", serde_json::json!({ "week": week, "gap": gap.to_string() })),
        WeekOutcome::Volunteer(v) => ("economy.volunteer", serde_json::json!({ "week": week, "deficit": v.deficit.to_string() })),
    };
    stmts.push(Statement::new("INSERT INTO journal(stream, at, data) VALUES (?1, ?2, ?3)", vec![Value::Text(stream.into()), Value::Integer(at as i64), Value::Text(data.to_string())]));
    if let WeekOutcome::Volunteer(v) = outcome {
        stmts.push(love_entry(v, captain_hex, at, None));
    }
    stmts
}

/// Apport du Capitaine dans `love_ledger` (`src_event` = identifiant de l'attestation 30305 une fois signée).
pub fn love_entry(v: &VolunteerWeek, captain_hex: &str, at: u64, attestation_event: Option<&str>) -> Statement {
    let amount = i64::try_from(v.deficit.0).unwrap_or(i64::MAX);
    Statement::new(
        "INSERT INTO love_ledger(at, kind, amount_cents, detail, src_event) VALUES (?1, 'captain_contribution', ?2, ?3, ?4)",
        vec![
            Value::Integer(at as i64),
            Value::Integer(amount),
            Value::Text(serde_json::json!({ "week": v.week, "captain": captain_hex }).to_string()),
            attestation_event.map_or(Value::Null, |e| Value::Text(e.to_owned())),
        ],
    )
}

/// Relie l'attestation signée à la ligne du Love Ledger de cette semaine.
pub fn link_attestation(att: &Attestation, event_id: &str) -> Statement {
    Statement::new(
        "UPDATE love_ledger SET src_event = ?1 WHERE kind = 'captain_contribution' AND json_extract(detail, '$.week') = ?2 AND src_event IS NULL",
        vec![Value::Text(event_id.to_owned()), Value::Text(att.week.clone())],
    )
}

/// Total donné (en centimes) et nombre de semaines de bénévolat, pour les écrans et les attestations.
pub fn love_totals() -> &'static str {
    "SELECT COALESCE(SUM(amount_cents), 0), COUNT(DISTINCT json_extract(detail, '$.week')) FROM love_ledger WHERE kind = 'captain_contribution'"
}

fn kind_of(o: &WeekOutcome) -> &'static str {
    match o {
        WeekOutcome::Split(_) => "split",
        WeekOutcome::Drawdown { .. } => "drawdown",
        WeekOutcome::Volunteer(_) => "volunteer",
    }
}

/// Part ASSETS d'une ventilation, à reporter sur le registre du capital (`capital_assets`) par l'appelant.
pub fn assets_share(a: &Allocation) -> u64 {
    a.assets.0
}
