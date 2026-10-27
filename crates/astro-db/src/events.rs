//! Stockage des événements Nostr : insertion avec sémantique de remplacement (NIP-01, NIP-09, NIP-40) et requêtes par filtre.
//!
//! Source unique partagée par le relai (`astro-relay`) et le moteur de synchronisation (`astro-sync`).

use astro_nostr_proto::{Event, Filter};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};

use crate::DbError;

/// Classe d'un kind selon NIP-01.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindClass {
    /// Stocké, jamais remplacé.
    Regular,
    /// Un seul événement par `(pubkey, kind)` : kinds 0, 3 et 10000-19999.
    Replaceable,
    /// Diffusé mais jamais stocké : 20000-29999.
    Ephemeral,
    /// Un seul événement par `(pubkey, kind, d)` : 30000-39999.
    ParameterizedReplaceable,
}

/// Classe un kind.
pub fn classify(kind: u32) -> KindClass {
    match kind {
        0 | 3 | 10_000..=19_999 => KindClass::Replaceable,
        20_000..=29_999 => KindClass::Ephemeral,
        30_000..=39_999 => KindClass::ParameterizedReplaceable,
        _ => KindClass::Regular,
    }
}

/// Résultat d'une insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    /// Nouvel événement stocké.
    Inserted,
    /// Identifiant déjà connu.
    Duplicate,
    /// Un événement remplaçable plus récent existe déjà.
    Superseded,
    /// Kind éphémère : non stocké.
    Ephemeral,
}

/// Valeur du premier tag `name`.
fn tag_value<'a>(e: &'a Event, name: &str) -> Option<&'a str> {
    e.tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).map(String::as_str)
}

/// Longueur maximale d'une valeur de tag indexée.
const MAX_INDEXED_TAG_VALUE: usize = 1024;

/// Insère `e` dans `nostr_events` / `nostr_tags` en appliquant les règles de remplacement.
///
/// À appeler dans une transaction du [`crate::Writer`]. N'effectue aucune validation de signature.
pub fn insert_event(tx: &Transaction<'_>, e: &Event) -> Result<InsertOutcome, DbError> {
    let class = classify(e.kind);
    if class == KindClass::Ephemeral {
        return Ok(InsertOutcome::Ephemeral);
    }
    let known: Option<i64> = tx.query_row("SELECT 1 FROM nostr_events WHERE id = ?1", params![e.id], |r| r.get(0)).optional()?;
    if known.is_some() {
        return Ok(InsertOutcome::Duplicate);
    }

    let d_tag = (class == KindClass::ParameterizedReplaceable).then(|| tag_value(e, "d").unwrap_or("").to_owned());
    if class != KindClass::Regular {
        // Remplaçable : garder le plus récent ; à égalité d'horodatage, l'identifiant le plus petit l'emporte.
        let existing: Option<(String, i64)> = match &d_tag {
            Some(d) => tx
                .query_row("SELECT id, created_at FROM nostr_events WHERE pubkey=?1 AND kind=?2 AND d_tag=?3", params![e.pubkey, e.kind, d], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?,
            None => tx
                .query_row("SELECT id, created_at FROM nostr_events WHERE pubkey=?1 AND kind=?2", params![e.pubkey, e.kind], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?,
        };
        if let Some((old_id, old_ts)) = existing {
            let new_ts = i64::try_from(e.created_at).unwrap_or(i64::MAX);
            if old_ts > new_ts || (old_ts == new_ts && old_id < e.id) {
                return Ok(InsertOutcome::Superseded);
            }
            tx.execute("DELETE FROM nostr_events WHERE id = ?1", params![old_id])?;
        }
    }

    let tags_json = serde_json::to_string(&e.tags).map_err(|x| DbError::Corrupt(x.to_string()))?;
    let expires_at = tag_value(e, "expiration").and_then(|v| v.parse::<i64>().ok());
    tx.execute(
        "INSERT INTO nostr_events(id, pubkey, kind, created_at, content, sig, tags, d_tag, expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![e.id, e.pubkey, e.kind, i64::try_from(e.created_at).unwrap_or(i64::MAX), e.content, e.sig, tags_json, d_tag, expires_at],
    )?;
    for t in &e.tags {
        if let (Some(name), Some(value)) = (t.first(), t.get(1)) {
            if name.len() == 1 && value.len() <= MAX_INDEXED_TAG_VALUE {
                tx.execute("INSERT OR IGNORE INTO nostr_tags(event_id, name, value) VALUES (?1,?2,?3)", params![e.id, name, value])?;
            }
        }
    }
    if e.kind == 5 {
        apply_deletion(tx, e)?;
    }
    Ok(InsertOutcome::Inserted)
}

/// NIP-09 : un événement kind 5 supprime les événements `e` cités, s'ils ont le même auteur.
fn apply_deletion(tx: &Transaction<'_>, deletion: &Event) -> Result<(), DbError> {
    for t in &deletion.tags {
        if t.first().map(String::as_str) == Some("e") {
            if let Some(id) = t.get(1) {
                tx.execute("DELETE FROM nostr_events WHERE id = ?1 AND pubkey = ?2 AND kind <> 5", params![id, deletion.pubkey])?;
            }
        }
    }
    Ok(())
}

/// Événements correspondant à `filter`, du plus récent au plus ancien, au plus `cap` (ou `filter.limit` s'il est plus petit).
///
/// Les événements expirés (NIP-40) à `now` sont exclus.
pub fn query_events(conn: &Connection, filter: &Filter, now: u64, cap: usize) -> Result<Vec<Event>, DbError> {
    if [filter.ids.as_ref().map(Vec::len), filter.authors.as_ref().map(Vec::len), filter.kinds.as_ref().map(Vec::len)].contains(&Some(0))
        || filter.tags.values().any(Vec::is_empty)
    {
        return Ok(Vec::new());
    }
    let mut sql = String::from("SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e WHERE 1=1");
    let mut args: Vec<Value> = Vec::new();
    let marks = |n: usize| vec!["?"; n].join(",");
    if let Some(ids) = &filter.ids {
        sql += &format!(" AND e.id IN ({})", marks(ids.len()));
        args.extend(ids.iter().cloned().map(Value::Text));
    }
    if let Some(authors) = &filter.authors {
        sql += &format!(" AND e.pubkey IN ({})", marks(authors.len()));
        args.extend(authors.iter().cloned().map(Value::Text));
    }
    if let Some(kinds) = &filter.kinds {
        sql += &format!(" AND e.kind IN ({})", marks(kinds.len()));
        args.extend(kinds.iter().map(|k| Value::Integer(i64::from(*k))));
    }
    if let Some(since) = filter.since {
        sql += " AND e.created_at >= ?";
        args.push(Value::Integer(i64::try_from(since).unwrap_or(i64::MAX)));
    }
    if let Some(until) = filter.until {
        sql += " AND e.created_at <= ?";
        args.push(Value::Integer(i64::try_from(until).unwrap_or(i64::MAX)));
    }
    for (name, values) in &filter.tags {
        sql += &format!(" AND EXISTS (SELECT 1 FROM nostr_tags t WHERE t.event_id = e.id AND t.name = ? AND t.value IN ({}))", marks(values.len()));
        args.push(Value::Text(name.to_string()));
        args.extend(values.iter().cloned().map(Value::Text));
    }
    sql += " AND (e.expires_at IS NULL OR e.expires_at > ?)";
    args.push(Value::Integer(i64::try_from(now).unwrap_or(i64::MAX)));
    sql += " ORDER BY e.created_at DESC, e.id ASC LIMIT ?";
    let limit = filter.limit.map_or(cap, |l| l.min(cap));
    args.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(args.iter()))?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(row_to_event(r)?);
    }
    Ok(out)
}

/// Reconstruit un événement depuis une ligne `id, pubkey, kind, created_at, content, sig, tags`.
pub fn row_to_event(r: &rusqlite::Row<'_>) -> Result<Event, DbError> {
    let tags_json: String = r.get(6)?;
    let tags = serde_json::from_str(&tags_json).map_err(|e| DbError::Corrupt(format!("tags : {e}")))?;
    Ok(Event {
        id: r.get(0)?,
        pubkey: r.get(1)?,
        kind: r.get(2)?,
        created_at: u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
        content: r.get(4)?,
        sig: r.get(5)?,
        tags,
    })
}

#[cfg(test)]
mod tests {
    use astro_nostr_proto::UnsignedEvent;

    use super::*;
    use crate::{Db, testutil::TempDir};

    fn make(secret: u8, kind: u32, ts: u64, tags: Vec<Vec<&str>>, content: &str) -> Event {
        UnsignedEvent {
            created_at: ts,
            kind,
            tags: tags.into_iter().map(|t| t.into_iter().map(String::from).collect()).collect(),
            content: content.into(),
        }
        .sign(&[secret; 32])
        .unwrap()
    }

    async fn put(db: &Db, e: &Event) -> InsertOutcome {
        let e = e.clone();
        db.writer().transaction(move |tx| insert_event(tx, &e)).await.unwrap()
    }

    async fn ids(db: &Db, f: Filter) -> Vec<String> {
        db.readers().read(move |c| Ok(query_events(c, &f, 1_800_000_000, 500)?.into_iter().map(|e| e.content).collect())).await.unwrap()
    }

    #[test]
    fn kinds_are_classified_per_nip01() {
        assert_eq!(classify(0), KindClass::Replaceable);
        assert_eq!(classify(3), KindClass::Replaceable);
        assert_eq!(classify(10_002), KindClass::Replaceable);
        assert_eq!(classify(1), KindClass::Regular);
        assert_eq!(classify(4), KindClass::Regular);
        assert_eq!(classify(24_315), KindClass::Ephemeral);
        assert_eq!(classify(30_800), KindClass::ParameterizedReplaceable);
        assert_eq!(classify(40_000), KindClass::Regular);
    }

    #[tokio::test]
    async fn regular_events_dedupe_and_query() {
        let tmp = TempDir::new("ev1");
        let db = Db::open(&tmp.path().join("u.db"), 2).unwrap();
        let a = make(1, 1, 100, vec![vec!["t", "x"]], "a");
        let b = make(1, 1, 200, vec![vec!["t", "y"]], "b");
        assert_eq!(put(&db, &a).await, InsertOutcome::Inserted);
        assert_eq!(put(&db, &a).await, InsertOutcome::Duplicate);
        assert_eq!(put(&db, &b).await, InsertOutcome::Inserted);
        assert_eq!(ids(&db, Filter::default()).await, ["b", "a"], "plus récent d'abord");
        let by_tag = Filter { tags: [('t', vec!["x".to_string()])].into(), ..Filter::default() };
        assert_eq!(ids(&db, by_tag).await, ["a"]);
        let limited = Filter { limit: Some(1), ..Filter::default() };
        assert_eq!(ids(&db, limited).await, ["b"]);
        let empty = Filter { authors: Some(vec![]), ..Filter::default() };
        assert!(ids(&db, empty).await.is_empty());
    }

    #[tokio::test]
    async fn replaceable_keeps_only_the_newest() {
        let tmp = TempDir::new("ev2");
        let db = Db::open(&tmp.path().join("u.db"), 2).unwrap();
        let old = make(2, 0, 100, vec![], "old");
        let new = make(2, 0, 200, vec![], "new");
        assert_eq!(put(&db, &new).await, InsertOutcome::Inserted);
        assert_eq!(put(&db, &old).await, InsertOutcome::Superseded);
        assert_eq!(ids(&db, Filter::default()).await, ["new"]);
        let newer = make(2, 0, 300, vec![], "newer");
        assert_eq!(put(&db, &newer).await, InsertOutcome::Inserted);
        assert_eq!(ids(&db, Filter::default()).await, ["newer"]);
        // Un autre auteur n'est pas affecté.
        assert_eq!(put(&db, &make(3, 0, 50, vec![], "other")).await, InsertOutcome::Inserted);
        assert_eq!(ids(&db, Filter::default()).await.len(), 2);
    }

    #[tokio::test]
    async fn parameterized_replaceable_is_keyed_by_d_tag() {
        let tmp = TempDir::new("ev3");
        let db = Db::open(&tmp.path().join("u.db"), 2).unwrap();
        assert_eq!(put(&db, &make(4, 30_078, 100, vec![vec!["d", "a"]], "a1")).await, InsertOutcome::Inserted);
        assert_eq!(put(&db, &make(4, 30_078, 100, vec![vec!["d", "b"]], "b1")).await, InsertOutcome::Inserted);
        assert_eq!(put(&db, &make(4, 30_078, 200, vec![vec!["d", "a"]], "a2")).await, InsertOutcome::Inserted);
        let mut got = ids(&db, Filter::default()).await;
        got.sort();
        assert_eq!(got, ["a2", "b1"]);
        let by_d = Filter { tags: [('d', vec!["a".to_string()])].into(), ..Filter::default() };
        assert_eq!(ids(&db, by_d).await, ["a2"]);
    }

    #[tokio::test]
    async fn equal_timestamp_keeps_the_lowest_id() {
        let tmp = TempDir::new("ev4");
        let db = Db::open(&tmp.path().join("u.db"), 1).unwrap();
        let x = make(5, 0, 100, vec![], "x");
        let y = make(5, 0, 100, vec![], "y");
        let (low, high) = if x.id < y.id { (x, y) } else { (y, x) };
        assert_eq!(put(&db, &high).await, InsertOutcome::Inserted);
        assert_eq!(put(&db, &low).await, InsertOutcome::Inserted);
        assert_eq!(put(&db, &high).await, InsertOutcome::Superseded);
        assert_eq!(ids(&db, Filter::default()).await, std::slice::from_ref(&low.content));
    }

    #[tokio::test]
    async fn ephemeral_events_are_not_stored() {
        let tmp = TempDir::new("ev5");
        let db = Db::open(&tmp.path().join("u.db"), 1).unwrap();
        assert_eq!(put(&db, &make(6, 24_315, 100, vec![], "presence")).await, InsertOutcome::Ephemeral);
        assert!(ids(&db, Filter::default()).await.is_empty());
    }

    #[tokio::test]
    async fn deletion_only_removes_own_events() {
        let tmp = TempDir::new("ev6");
        let db = Db::open(&tmp.path().join("u.db"), 1).unwrap();
        let mine = make(7, 1, 100, vec![], "mine");
        let theirs = make(8, 1, 100, vec![], "theirs");
        put(&db, &mine).await;
        put(&db, &theirs).await;
        let del = make(7, 5, 200, vec![vec!["e", &mine.id], vec!["e", &theirs.id]], "");
        assert_eq!(put(&db, &del).await, InsertOutcome::Inserted);
        let got = ids(&db, Filter { kinds: Some(vec![1]), ..Filter::default() }).await;
        assert_eq!(got, ["theirs"]);
    }

    #[tokio::test]
    async fn expired_events_are_hidden() {
        let tmp = TempDir::new("ev7");
        let db = Db::open(&tmp.path().join("u.db"), 1).unwrap();
        put(&db, &make(9, 1, 100, vec![vec!["expiration", "1700000000"]], "gone")).await;
        put(&db, &make(9, 1, 101, vec![vec!["expiration", "1900000000"]], "kept")).await;
        assert_eq!(ids(&db, Filter::default()).await, ["kept"]);
    }
}
