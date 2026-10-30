//! Bibliothèque de photos et fichiers du compte : requêtes de lecture de l'interface (déjà là, souvenirs, chronologie,
//! recherche à filtres, doublons). Tout est calculé en SQL sur `ucloud_entries`, par propriétaire : rien ne sort de la station
//! et rien n'est comparé entre comptes (la présence d'un contenu chez un autre compte ne doit jamais se deviner).

use rusqlite::{Connection, ToSql, params, params_from_iter};
use serde_json::{Value, json};

/// Colonnes d'un élément affiché : chemin, type MIME, taille, mtime, date de prise de vue, date retenue, miniature, lat, lon, empreinte.
const COLS: &str = "e.path, e.mime, e.size, e.mtime, e.taken_at, COALESCE(e.taken_at, e.mtime) AS t, json_extract(e.data, '$.thumb'), e.lat, e.lon, e.phash, e.sha_plain";
/// Fichiers réels d'un compte (pas les dossiers, pas les entrées sans contenu).
const FILES: &str = "e.owner_hex = ?1 AND e.type = 'file' AND e.cid IS NOT NULL";
/// Plafond d'empreintes comparées pour les doublons visuels (borne la mémoire et le temps).
const MAX_PHASH: usize = 30_000;
/// Distance de Hamming maximale entre deux empreintes pour parler de « même image ».
const NEAR: u32 = 6;
/// Nombre de SHA-256 acceptés par appel de [`have`].
pub const MAX_HAVE: usize = 500;

fn thumb(json: Option<String>) -> Value {
    let Some(v) = json.and_then(|j| serde_json::from_str::<Value>(&j).ok()) else { return Value::Null };
    json!({ "w": v["w"], "h": v["h"], "ow": v["ow"], "oh": v["oh"] })
}

fn item(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "path": r.get::<_, String>(0)?, "mime": r.get::<_, Option<String>>(1)?, "size": r.get::<_, Option<i64>>(2)?,
        "mtime": r.get::<_, Option<i64>>(3)?, "taken_at": r.get::<_, Option<i64>>(4)?, "t": r.get::<_, Option<i64>>(5)?,
        "thumb": thumb(r.get::<_, Option<String>>(6)?), "lat": r.get::<_, Option<f64>>(7)?, "lon": r.get::<_, Option<f64>>(8)?,
    }))
}

/// Parmi `shas` (SHA-256 du contenu clair, en hexadécimal), ceux que ce compte possède déjà : `[{ sha, path }]`.
/// Un client qui sauvegarde ses photos n'envoie ainsi que ce qui manque.
pub fn have(c: &Connection, owner: &str, shas: &[String]) -> rusqlite::Result<Vec<Value>> {
    let shas: Vec<String> = shas.iter().filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).map(|s| s.to_ascii_lowercase()).take(MAX_HAVE).collect();
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; shas.len()].join(",");
    let sql = format!("SELECT sha_plain, MIN(path) FROM ucloud_entries WHERE owner_hex = ? AND type = 'file' AND cid IS NOT NULL AND sha_plain IN ({marks}) GROUP BY sha_plain");
    let mut args: Vec<&dyn ToSql> = vec![&owner];
    args.extend(shas.iter().map(|s| s as &dyn ToSql));
    let mut stmt = c.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), |r| Ok(json!({ "sha": r.get::<_, String>(0)?, "path": r.get::<_, String>(1)? })))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Jours depuis 1970-01-01 pour une date civile (algorithme de H. Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// `"2026-10"` → `[début, fin[` en secondes Unix (UTC). `None` si le mois est mal formé.
pub fn month_bounds(ym: &str) -> Option<(i64, i64)> {
    let (y, m) = ym.split_once('-')?;
    let (y, m): (i64, i64) = (y.parse().ok()?, m.parse().ok()?);
    if !(1970..=2200).contains(&y) || !(1..=12).contains(&m) || ym.len() != 7 {
        return None;
    }
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    Some((days_from_civil(y, m, 1) * 86_400, days_from_civil(ny, nm, 1) * 86_400))
}

/// Chronologie : un compteur par mois (du plus récent au plus ancien) ; l'interface charge ensuite un mois à la demande.
pub fn timeline_buckets(c: &Connection, owner: &str) -> rusqlite::Result<Vec<Value>> {
    let sql = format!("SELECT strftime('%Y-%m', COALESCE(e.taken_at, e.mtime), 'unixepoch') AS ym, count(*) FROM ucloud_entries e WHERE {FILES} AND (e.mime LIKE 'image/%' OR e.mime LIKE 'video/%') AND COALESCE(e.taken_at, e.mtime) IS NOT NULL GROUP BY ym ORDER BY ym DESC");
    let mut stmt = c.prepare_cached(&sql)?;
    let rows = stmt.query_map([owner], |r| Ok(json!({ "ym": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)? })))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Les photos et vidéos d'un mois (`"2026-10"`), de la plus récente à la plus ancienne.
pub fn timeline_month(c: &Connection, owner: &str, ym: &str) -> rusqlite::Result<Option<Vec<Value>>> {
    let Some((start, end)) = month_bounds(ym) else { return Ok(None) };
    let sql = format!("SELECT {COLS} FROM ucloud_entries e WHERE {FILES} AND (e.mime LIKE 'image/%' OR e.mime LIKE 'video/%') AND COALESCE(e.taken_at, e.mtime) >= ?2 AND COALESCE(e.taken_at, e.mtime) < ?3 ORDER BY t DESC, e.path LIMIT 2000");
    let mut stmt = c.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![owner, start, end], item)?.collect::<Result<Vec<_>, _>>()?;
    Ok(Some(rows))
}

/// « Ce jour-là » : les photos et vidéos prises le même jour (`"10-31"`) les années précédentes, groupées par année décroissante.
pub fn memories(c: &Connection, owner: &str, month_day: &str, this_year: i64) -> rusqlite::Result<Vec<Value>> {
    if month_day.len() != 5 || !month_day.bytes().enumerate().all(|(i, b)| if i == 2 { b == b'-' } else { b.is_ascii_digit() }) {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT CAST(strftime('%Y', COALESCE(e.taken_at, e.mtime), 'unixepoch') AS INTEGER) AS y, {COLS} FROM ucloud_entries e \
         WHERE {FILES} AND (e.mime LIKE 'image/%' OR e.mime LIKE 'video/%') AND strftime('%m-%d', COALESCE(e.taken_at, e.mtime), 'unixepoch') = ?2 \
         AND e.taken_at IS NOT NULL ORDER BY y DESC, t, e.path"
    );
    let mut stmt = c.prepare_cached(&sql)?;
    let mut years: Vec<(i64, Vec<Value>)> = Vec::new();
    let mut rows = stmt.query(params![owner, month_day])?;
    while let Some(r) = rows.next()? {
        let y: i64 = r.get(0)?;
        // `item` lit les colonnes à partir de la 0 : on les relit décalées d'une.
        let it = json!({
            "path": r.get::<_, String>(1)?, "mime": r.get::<_, Option<String>>(2)?, "size": r.get::<_, Option<i64>>(3)?,
            "mtime": r.get::<_, Option<i64>>(4)?, "taken_at": r.get::<_, Option<i64>>(5)?, "t": r.get::<_, Option<i64>>(6)?,
            "thumb": thumb(r.get::<_, Option<String>>(7)?), "lat": r.get::<_, Option<f64>>(8)?, "lon": r.get::<_, Option<f64>>(9)?,
        });
        if y >= this_year {
            continue;
        }
        match years.last_mut() {
            Some((ly, items)) if *ly == y => {
                if items.len() < 12 {
                    items.push(it);
                }
            }
            _ => years.push((y, vec![it])),
        }
    }
    Ok(years.into_iter().map(|(y, items)| json!({ "year": y, "ago": this_year - y, "items": items })).collect())
}

/// Critères de [`search`].
#[derive(Debug, Default, Clone)]
pub struct Search {
    /// Mots (tous requis) cherchés dans le nom et la description, sans casse.
    pub q: Option<String>,
    /// Nom d'une personne reconnue (FaceCloud) présente sur la photo.
    pub person: Option<String>,
    /// Bornes de date (secondes Unix, `from` inclus, `to` exclu).
    pub from: Option<i64>,
    /// Fin de période.
    pub to: Option<i64>,
    /// Zone `(sud, ouest, nord, est)` en degrés.
    pub bbox: Option<(f64, f64, f64, f64)>,
    /// `image`, `video` ou `other`.
    pub kind: Option<String>,
    /// Curseur : `(date, chemin)` du dernier résultat de la page précédente.
    pub after: Option<(i64, String)>,
    /// Résultats par page (1 à 200).
    pub limit: usize,
}

fn like_escape(w: &str) -> String {
    format!("%{}%", w.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

/// Recherche à filtres combinés, paginée par curseur `(date, chemin)` (stable même si des fichiers arrivent pendant la lecture).
/// Rend `{ items, next }` ; `next` est `null` à la dernière page.
pub fn search(c: &Connection, owner: &str, s: &Search) -> rusqlite::Result<Value> {
    let mut sql = format!("SELECT {COLS} FROM ucloud_entries e WHERE {FILES}");
    let mut args: Vec<rusqlite::types::Value> = vec![owner.to_owned().into()];
    for w in s.q.as_deref().unwrap_or("").split_whitespace().take(8) {
        sql += " AND (lower(e.path) LIKE ? ESCAPE '\\' OR lower(COALESCE(e.desc_text, '')) LIKE ? ESCAPE '\\')";
        let pat = like_escape(&w.chars().take(64).collect::<String>());
        args.push(pat.clone().into());
        args.push(pat.into());
    }
    match s.kind.as_deref() {
        Some("image") => sql += " AND e.mime LIKE 'image/%'",
        Some("video") => sql += " AND e.mime LIKE 'video/%'",
        Some("other") => sql += " AND (e.mime IS NULL OR (e.mime NOT LIKE 'image/%' AND e.mime NOT LIKE 'video/%'))",
        _ => {}
    }
    if let Some(p) = s.person.as_deref().filter(|p| !p.is_empty()) {
        sql += " AND EXISTS (SELECT 1 FROM faces f WHERE f.owner_hex = e.owner_hex AND f.ucloud_path = e.path AND f.name = ?)";
        args.push(p.to_owned().into());
    }
    if let Some(from) = s.from {
        sql += " AND COALESCE(e.taken_at, e.mtime) >= ?";
        args.push(from.into());
    }
    if let Some(to) = s.to {
        sql += " AND COALESCE(e.taken_at, e.mtime) < ?";
        args.push(to.into());
    }
    if let Some((south, west, north, east)) = s.bbox {
        sql += " AND e.lat BETWEEN ? AND ?";
        args.extend([south.into(), north.into()]);
        // Une zone qui franchit l'antiméridien (ouest > est) se coupe en deux intervalles.
        if west <= east {
            sql += " AND e.lon BETWEEN ? AND ?";
        } else {
            sql += " AND (e.lon >= ? OR e.lon <= ?)";
        }
        args.extend([west.into(), east.into()]);
    }
    if let Some((t, path)) = &s.after {
        sql += " AND (COALESCE(e.taken_at, e.mtime) < ? OR (COALESCE(e.taken_at, e.mtime) = ? AND e.path > ?))";
        args.extend([(*t).into(), (*t).into(), path.clone().into()]);
    }
    let limit = s.limit.clamp(1, 200);
    sql += " ORDER BY t DESC, e.path ASC LIMIT ?";
    args.push(i64::try_from(limit + 1).unwrap_or(201).into());
    let mut stmt = c.prepare(&sql)?;
    let mut items = stmt.query_map(params_from_iter(args.iter()), item)?.collect::<Result<Vec<_>, _>>()?;
    let next = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|l| json!({ "t": l["t"], "path": l["path"] }))
    } else {
        None
    };
    Ok(json!({ "items": items, "next": next }))
}

/// Un doublon candidat.
struct Dup {
    path: String,
    sha: Option<String>,
    phash: i64,
    size: i64,
    scored: i64,
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Doublons du compte : `exact` (même contenu, même SHA-256 du clair) et `similar` (images presque identiques, distance de Hamming
/// des empreintes perceptuelles ≤ 6, contenus différents). Chaque groupe propose un `keep` : le plus gros fichier, puis celui qui a le plus
/// de métadonnées (date, position). **Rien n'est supprimé ici** : l'interface demande confirmation.
pub fn duplicates(c: &Connection, owner: &str) -> rusqlite::Result<Value> {
    // Exacts.
    let sql = format!(
        "SELECT {COLS} FROM ucloud_entries e WHERE {FILES} AND e.sha_plain IN (SELECT sha_plain FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL AND sha_plain IS NOT NULL GROUP BY sha_plain HAVING count(*) > 1) \
         ORDER BY e.sha_plain, e.size DESC, e.path LIMIT 5000"
    );
    let mut stmt = c.prepare(&sql)?;
    let mut exact: Vec<(String, Vec<Value>)> = Vec::new();
    let mut rows = stmt.query([owner])?;
    while let Some(r) = rows.next()? {
        let sha: String = r.get(10)?;
        let it = item(r)?;
        match exact.last_mut() {
            Some((s, items)) if *s == sha => items.push(it),
            _ => exact.push((sha, vec![it])),
        }
    }
    let exact: Vec<Value> = exact.into_iter().map(|(sha, items)| json!({ "sha": sha, "items": items, "keep": 0 })).collect();

    // Similaires : bandes de bits (au plus 6 bits diffèrent donc au moins une des 7 bandes de 9 bits est identique).
    let mut stmt = c.prepare(
        "SELECT path, sha_plain, phash, COALESCE(size, 0), (lat IS NOT NULL) + (taken_at IS NOT NULL) FROM ucloud_entries \
         WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL AND phash IS NOT NULL AND mime LIKE 'image/%' LIMIT ?2",
    )?;
    let dups: Vec<Dup> = stmt
        .query_map(params![owner, i64::try_from(MAX_PHASH).unwrap_or(i64::MAX)], |r| Ok(Dup { path: r.get(0)?, sha: r.get(1)?, phash: r.get(2)?, size: r.get(3)?, scored: r.get(4)? }))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut parent: Vec<usize> = (0..dups.len()).collect();
    let mut buckets: std::collections::HashMap<(u8, u16), Vec<usize>> = std::collections::HashMap::new();
    for (i, d) in dups.iter().enumerate() {
        let bits = u64::from_ne_bytes(d.phash.to_ne_bytes());
        for band in 0..7u8 {
            buckets.entry((band, ((bits >> (u32::from(band) * 9)) & 0x1ff) as u16)).or_default().push(i);
        }
    }
    for members in buckets.values().filter(|m| m.len() > 1 && m.len() <= 2000) {
        for (a, &i) in members.iter().enumerate() {
            for &j in &members[a + 1..] {
                if (dups[i].phash ^ dups[j].phash).count_ones() <= NEAR {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    if ri != rj {
                        parent[ri] = rj;
                    }
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for i in 0..dups.len() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    let mut similar = Vec::new();
    for members in groups.into_values().filter(|m| m.len() > 1) {
        // Un groupe dont tous les membres ont le même contenu est déjà dans `exact`.
        let shas: std::collections::BTreeSet<_> = members.iter().map(|&i| dups[i].sha.as_deref().unwrap_or("")).collect();
        if shas.len() < 2 {
            continue;
        }
        let keep = members.iter().enumerate().max_by_key(|&(_, &i)| (dups[i].size, dups[i].scored)).map_or(0, |(k, _)| k);
        let mut paths: Vec<&str> = members.iter().map(|&i| dups[i].path.as_str()).collect();
        let keep_path = paths[keep].to_owned();
        paths.sort_unstable();
        let items = paths
            .iter()
            .map(|p| c.query_row(&format!("SELECT {COLS} FROM ucloud_entries e WHERE e.owner_hex = ?1 AND e.path = ?2"), params![owner, p], item))
            .collect::<Result<Vec<_>, _>>()?;
        let keep = paths.iter().position(|p| **p == keep_path).unwrap_or(0);
        similar.push(json!({ "items": items, "keep": keep }));
        if similar.len() >= 500 {
            break;
        }
    }
    Ok(json!({ "exact": exact, "similar": similar }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "aa";
    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn db() -> Connection {
        astro_db::open_in_memory().unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn put(c: &Connection, owner: &str, path: &str, mime: &str, size: i64, taken: Option<i64>, sha: &str, phash: Option<i64>, desc: Option<&str>) {
        c.execute(
            "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, sha_plain, phash, desc_text, data) VALUES (?1, ?2, '/', 'file', ?3, ?4, ?5, 1000, ?6, ?7, ?8, ?9, '{}')",
            params![owner, path, format!("cid-{path}"), mime, size, taken, sha, phash, desc],
        )
        .unwrap();
    }

    // 2024-05-17 12:00 UTC et 2026-05-17 12:00 UTC.
    const T2024: i64 = 1_715_947_200;
    const T2026: i64 = 1_779_019_200;

    #[test]
    fn have_reports_only_what_this_account_owns() {
        let c = db();
        put(&c, ME, "/a.jpg", "image/jpeg", 10, None, SHA_A, None, None);
        put(&c, "bb", "/autre.jpg", "image/jpeg", 10, None, &"bb".repeat(32), None, None);
        let r = have(&c, ME, &[SHA_A.to_uppercase(), "bb".repeat(32), "pas-un-sha".into()]).unwrap();
        assert_eq!(r, vec![json!({ "sha": SHA_A, "path": "/a.jpg" })], "le contenu d'un autre compte ne se devine pas");
        assert!(have(&c, ME, &[]).unwrap().is_empty());
    }

    #[test]
    fn months_and_memories() {
        assert_eq!(month_bounds("2026-05"), Some((1_777_593_600, 1_780_272_000)));
        assert_eq!(month_bounds("2026-12").map(|b| b.1 - b.0), Some(31 * 86_400));
        assert_eq!(month_bounds("2026-13"), None);
        assert_eq!(month_bounds("26-05"), None);
        let c = db();
        put(&c, ME, "/2024.jpg", "image/jpeg", 1, Some(T2024), "", None, None);
        put(&c, ME, "/2026.jpg", "image/jpeg", 1, Some(T2026), "", None, None);
        put(&c, ME, "/doc.pdf", "application/pdf", 1, Some(T2024), "", None, None);
        let b = timeline_buckets(&c, ME).unwrap();
        assert_eq!(b, vec![json!({ "ym": "2026-05", "count": 1 }), json!({ "ym": "2024-05", "count": 1 })]);
        let m = timeline_month(&c, ME, "2024-05").unwrap().unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0]["path"], "/2024.jpg");
        assert!(timeline_month(&c, ME, "n'importe quoi").unwrap().is_none());
        let mem = memories(&c, ME, "05-17", 2026).unwrap();
        assert_eq!(mem.len(), 1, "l'année en cours est exclue");
        assert_eq!((mem[0]["year"].as_i64(), mem[0]["ago"].as_i64(), mem[0]["items"][0]["path"].as_str()), (Some(2024), Some(2), Some("/2024.jpg")));
        assert!(memories(&c, ME, "05/17", 2026).unwrap().is_empty());
    }

    #[test]
    fn search_combines_filters_and_pages_by_cursor() {
        let c = db();
        put(&c, ME, "/Vacances/plage 100%.jpg", "image/jpeg", 1, Some(T2024), "", None, Some("Coucher de soleil"));
        put(&c, ME, "/Vacances/montagne.jpg", "image/jpeg", 1, Some(T2024 + 10), "", None, None);
        put(&c, ME, "/Docs/contrat.pdf", "application/pdf", 1, Some(T2026), "", None, None);
        put(&c, "bb", "/Vacances/autre.jpg", "image/jpeg", 1, Some(T2024), "", None, None);
        c.execute("UPDATE ucloud_entries SET lat = 45.7, lon = 4.8 WHERE path = '/Vacances/montagne.jpg'", []).unwrap();
        let q = |s: Search| search(&c, ME, &s).unwrap();
        let paths = |v: &Value| v["items"].as_array().unwrap().iter().map(|i| i["path"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(paths(&q(Search { q: Some("VACANCES".into()), limit: 50, ..Search::default() })), ["/Vacances/montagne.jpg", "/Vacances/plage 100%.jpg"], "sans casse, et pas le compte voisin");
        assert_eq!(paths(&q(Search { q: Some("soleil".into()), limit: 50, ..Search::default() })), ["/Vacances/plage 100%.jpg"], "description");
        assert_eq!(paths(&q(Search { q: Some("100%".into()), limit: 50, ..Search::default() })).len(), 1, "le % est littéral");
        assert_eq!(paths(&q(Search { kind: Some("other".into()), limit: 50, ..Search::default() })), ["/Docs/contrat.pdf"]);
        assert_eq!(paths(&q(Search { bbox: Some((45.0, 4.0, 46.0, 5.0)), limit: 50, ..Search::default() })), ["/Vacances/montagne.jpg"]);
        assert_eq!(paths(&q(Search { from: Some(T2026 - 1), to: Some(T2026 + 1), limit: 50, ..Search::default() })), ["/Docs/contrat.pdf"]);
        // Curseur : deux pages d'un résultat.
        let p1 = q(Search { limit: 2, ..Search::default() });
        assert_eq!(paths(&p1).len(), 2);
        let next = &p1["next"];
        let p2 = q(Search { limit: 2, after: Some((next["t"].as_i64().unwrap(), next["path"].as_str().unwrap().to_owned())), ..Search::default() });
        assert_eq!(paths(&p2), ["/Vacances/plage 100%.jpg"]);
        assert!(p2["next"].is_null());
    }

    #[test]
    fn duplicates_group_exact_copies_and_near_images_without_touching_anything() {
        let c = db();
        let h: i64 = 0x0F0F_0F0F_0F0F_0F0F;
        put(&c, ME, "/a.jpg", "image/jpeg", 500, Some(T2024), SHA_A, Some(h), None);
        put(&c, ME, "/copie de a.jpg", "image/jpeg", 500, None, SHA_A, Some(h), None);
        // Même image recompressée : empreinte à 3 bits, contenu différent, plus gros.
        put(&c, ME, "/a-grande.jpg", "image/jpeg", 900, Some(T2024), &"cc".repeat(32), Some(h ^ 0b10101), None);
        // Image sans rapport.
        put(&c, ME, "/autre.jpg", "image/jpeg", 100, None, &"dd".repeat(32), Some(!h), None);
        // Un autre compte avec le même contenu : jamais mélangé.
        put(&c, "bb", "/x.jpg", "image/jpeg", 500, None, SHA_A, Some(h), None);
        let d = duplicates(&c, ME).unwrap();
        assert_eq!(d["exact"].as_array().unwrap().len(), 1);
        assert_eq!(d["exact"][0]["items"].as_array().unwrap().len(), 2);
        let sim = d["similar"].as_array().unwrap();
        assert_eq!(sim.len(), 1);
        let items = sim[0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 3, "les deux copies et la version recompressée");
        assert_eq!(items[sim[0]["keep"].as_u64().unwrap() as usize]["path"], "/a-grande.jpg", "on garde le plus gros");
        let n: i64 = c.query_row("SELECT count(*) FROM ucloud_entries", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 5, "rien n'est supprimé");
    }
}
