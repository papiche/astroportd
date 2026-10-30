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
    /// Supprimé par son auteur (NIP-09) : non réinséré.
    Deleted,
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

    // NIP-09 : un événement (ou une version d'adresse) supprimé par son auteur ne revient pas.
    let address = format!("{}:{}:{}", e.kind, e.pubkey, tag_value(e, "d").unwrap_or(""));
    let address = if class == KindClass::Replaceable { format!("{}:{}:", e.kind, e.pubkey) } else { address };
    let deleted: Option<i64> = tx
        .query_row("SELECT 1 FROM nostr_deleted WHERE pubkey = ?1 AND (ref = ?2 OR (ref = ?3 AND until >= ?4))", params![e.pubkey, e.id, address, i64::try_from(e.created_at).unwrap_or(i64::MAX)], |r| r.get(0))
        .optional()?;
    if deleted.is_some() && e.kind != 5 {
        return Ok(InsertOutcome::Deleted);
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

/// NIP-09 : un événement kind 5 supprime les événements cités par `e` (identifiant) ou `a` (adresse `kind:pubkey:d`, toutes
/// les versions jusqu'à la date de la suppression), s'ils ont le même auteur. Une trace reste (`nostr_deleted`) pour qu'ils
/// ne soient pas réinsérés. Un kind 5 n'en supprime jamais un autre.
fn apply_deletion(tx: &Transaction<'_>, deletion: &Event) -> Result<(), DbError> {
    let at = i64::try_from(deletion.created_at).unwrap_or(i64::MAX);
    for t in &deletion.tags {
        let (Some(name), Some(value)) = (t.first().map(String::as_str), t.get(1)) else { continue };
        match name {
            "e" => {
                tx.execute("DELETE FROM nostr_events WHERE id = ?1 AND pubkey = ?2 AND kind <> 5", params![value, deletion.pubkey])?;
                tx.execute("INSERT OR IGNORE INTO nostr_deleted(pubkey, ref, until) VALUES (?1, ?2, ?3)", params![deletion.pubkey, value, i64::MAX])?;
            }
            "a" => {
                let mut parts = value.splitn(3, ':');
                let (Some(kind), Some(author), Some(d)) = (parts.next().and_then(|k| k.parse::<u32>().ok()), parts.next(), parts.next()) else { continue };
                if author != deletion.pubkey || kind == 5 {
                    continue;
                }
                match classify(kind) {
                    KindClass::Replaceable => tx.execute("DELETE FROM nostr_events WHERE pubkey = ?1 AND kind = ?2 AND created_at <= ?3", params![author, kind, at])?,
                    KindClass::ParameterizedReplaceable => tx.execute("DELETE FROM nostr_events WHERE pubkey = ?1 AND kind = ?2 AND d_tag = ?3 AND created_at <= ?4", params![author, kind, d, at])?,
                    _ => continue,
                };
                tx.execute("INSERT INTO nostr_deleted(pubkey, ref, until) VALUES (?1, ?2, ?3) ON CONFLICT(pubkey, ref) DO UPDATE SET until = MAX(until, excluded.until)", params![author, value, at])?;
            }
            _ => {}
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
    // NIP-50 : chaque mot doit commencer un mot du contenu (index FTS5, insensible à la casse et aux accents), les
    // résultats sont classés par pertinence (BM25) puis du plus récent au plus ancien.
    let mut order = "e.created_at DESC, e.id ASC";
    if let Some(query) = filter.search.as_deref().and_then(search_match) {
        let Some(query) = query else { return Ok(Vec::new()) };
        sql = sql.replacen("FROM nostr_events e WHERE 1=1", "FROM nostr_events e JOIN nostr_fts_ids m ON m.id = e.id JOIN nostr_fts f ON f.rowid = m.rowid AND nostr_fts MATCH ? WHERE 1=1", 1);
        args.insert(0, Value::Text(query));
        order = "f.rank, e.created_at DESC, e.id ASC";
    }
    sql += " AND (e.expires_at IS NULL OR e.expires_at > ?)";
    args.push(Value::Integer(i64::try_from(now).unwrap_or(i64::MAX)));
    sql += &format!(" ORDER BY {order} LIMIT ?");
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

/// Requête FTS5 d'une recherche NIP-50. `None` : aucun mot à chercher (extensions `clé:valeur` seules) ; `Some(None)` :
/// un mot sans lettre ni chiffre ne peut rien trouver. Chaque mot devient une expression `"mot"*` (préfixe, ET implicite) :
/// les guillemets sont doublés, aucun opérateur FTS5 n'est donc interprétable depuis l'extérieur.
fn search_match(search: &str) -> Option<Option<String>> {
    let terms = astro_nostr_proto::filter::search_terms(search);
    if terms.is_empty() {
        return None;
    }
    if terms.iter().any(|t| !t.chars().any(char::is_alphanumeric)) {
        return Some(None);
    }
    Some(Some(terms.iter().map(|t| format!("\"{}\"*", t.replace('"', "\"\""))).collect::<Vec<_>>().join(" ")))
}

/// Reconstruit l'index de recherche NIP-50 depuis `nostr_events` (base ancienne, ou index à refaire).
pub fn rebuild_search_index(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "DELETE FROM nostr_fts; DELETE FROM nostr_fts_ids; \
         INSERT INTO nostr_fts_ids(id) SELECT id FROM nostr_events WHERE kind NOT IN (4, 1059); \
         INSERT INTO nostr_fts(rowid, content) SELECT m.rowid, e.content FROM nostr_fts_ids m JOIN nostr_events e ON e.id = m.id;",
    )?;
    Ok(())
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
    async fn search_uses_the_fts_index_and_follows_every_change() {
        let tmp = TempDir::new("ev-fts");
        let db = Db::open(&tmp.path().join("u.db"), 2).unwrap();
        let search = |q: &str| Filter { search: Some(q.into()), ..Filter::default() };
        put(&db, &make(7, 1, 100, vec![], "Un café sur la place")).await;
        put(&db, &make(7, 1, 200, vec![], "café café café, le meilleur café")).await;
        put(&db, &make(7, 1, 300, vec![], "du thé")).await;
        put(&db, &make(7, 1, 400, vec![], "100% sûr")).await;
        assert_eq!(ids(&db, search("CAFE")).await, ["café café café, le meilleur café", "Un café sur la place"], "accents et casse ignorés, plus pertinent d'abord");
        assert_eq!(ids(&db, search("caf")).await.len(), 2, "préfixe");
        assert_eq!(ids(&db, search("café place")).await, ["Un café sur la place"], "tous les mots");
        assert_eq!(ids(&db, search("language:fr thé")).await, ["du thé"], "extensions ignorées");
        assert_eq!(ids(&db, search("100%")).await, ["100% sûr"]);
        assert!(ids(&db, search("!!!")).await.is_empty(), "un mot sans lettre ne trouve rien");
        // Aucun opérateur FTS5 n'est interprétable.
        for hostile in ["café OR thé", "\"café", "café*", "NEAR(café thé)", "content:café", "-café"] {
            let _ = ids(&db, search(hostile)).await;
        }
        assert!(ids(&db, search("café OR thé")).await.is_empty(), "OR est un mot, pas un opérateur");
        // Les messages chiffrés ne sont pas indexés.
        put(&db, &make(7, 1059, 500, vec![], "secretgift")).await;
        assert!(ids(&db, search("secretgift")).await.is_empty());
        // Remplacement : l'ancien texte disparaît de l'index.
        put(&db, &make(8, 0, 100, vec![], "ancien profil")).await;
        put(&db, &make(8, 0, 200, vec![], "nouveau profil")).await;
        assert_eq!(ids(&db, search("profil")).await, ["nouveau profil"]);
        assert!(ids(&db, search("ancien")).await.is_empty());
        // Suppression NIP-09.
        let doomed = make(9, 1, 100, vec![], "éphémère");
        put(&db, &doomed).await;
        assert_eq!(ids(&db, search("ephemere")).await.len(), 1);
        put(&db, &make(9, 5, 150, vec![vec!["e", &doomed.id]], "")).await;
        assert!(ids(&db, search("ephemere")).await.is_empty());
        // Reconstruction : même résultat qu'avant.
        db.writer().transaction(|tx| rebuild_search_index(tx)).await.unwrap();
        assert_eq!(ids(&db, search("CAFE")).await.len(), 2);
        assert_eq!(ids(&db, search("profil")).await, ["nouveau profil"]);
    }

    #[tokio::test]
    async fn an_old_database_gets_its_events_indexed_on_open() {
        let tmp = TempDir::new("ev-fts-old");
        let path = tmp.path().join("u.db");
        {
            let db = Db::open(&path, 1).unwrap();
            put(&db, &make(7, 1, 100, vec![], "vieux message")).await;
        }
        // Simule une base d'avant l'index : on le vide, puis on rouvre.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch("DELETE FROM nostr_fts; DELETE FROM nostr_fts_ids;").unwrap();
        }
        let db = Db::open(&path, 1).unwrap();
        assert_eq!(ids(&db, Filter { search: Some("vieux".into()), ..Filter::default() }).await, ["vieux message"]);
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
