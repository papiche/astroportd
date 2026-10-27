//! Projection d'un événement Nostr vers les tables de `astro-db`.
//!
//! `plan` est une fonction pure : elle ne lit pas la base. Les dépendances à l'état (« cette clé est-elle
//! hébergée ? », « qui suit cette clé ? ») sont exprimées en SQL, ce qui rend la projection indépendante de
//! l'ordre d'arrivée des événements : le replay et le temps réel convergent vers le même état.

use std::collections::BTreeSet;

use astro_db::Statement;
use astro_economy::lease::{self, LeaseRequest, TARIFF_D_TAG, Tariff};
use astro_economy::Amount;
use astro_nostr_proto::{Event, is_hex64, kind};
use rusqlite::types::Value;
use serde_json::{Map, Value as Json};

/// Nombre maximal de contacts pris en compte dans un kind 3.
const MAX_CONTACTS: usize = 5_000;

/// Erreurs de projection : l'événement reste stocké, mais ne projette rien.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectError {
    /// DID incohérent.
    #[error("DID invalide : {0}")]
    BadDid(&'static str),
    /// Événement du registre Ğ1-N² invalide.
    #[error("ledger Ğ1-N² invalide : {0}")]
    BadLedger(&'static str),
    /// Tag obligatoire manquant.
    #[error("tag obligatoire manquant : {0}")]
    MissingTag(&'static str),
    /// Manifeste uCloud refusé à la revalidation.
    #[error("manifeste uCloud invalide : {0}")]
    BadManifest(String),
}

/// Traducteur d'événements en requêtes SQL.
pub struct Projector;

impl Projector {
    /// Kinds projetés.
    pub const KINDS: [u32; 6] = [kind::CONTACTS, kind::DID, kind::ECONOMIC_HEALTH, kind::ZEN_ISSUANCE, kind::LEDGER_N2, kind::APP_PARAMS];

    /// Requêtes à exécuter dans la transaction d'insertion de `e`. Vide si le kind n'est pas projeté.
    pub fn plan(e: &Event) -> Result<Vec<Statement>, ProjectError> {
        match e.kind {
            kind::CONTACTS => Ok(contacts(e)),
            kind::DID => did(e),
            kind::LEDGER_N2 => ledger(e),
            kind::ECONOMIC_HEALTH => Ok(economic_health(e)),
            kind::ZEN_ISSUANCE => zen_issuance(e),
            kind::APP_PARAMS => Ok(app_params(e)),
            _ => Ok(Vec::new()),
        }
    }

    /// Requêtes qui rétablissent le VFS (`ucloud_entries`) et le trousseau (`ucloud_keyring`) d'un compte depuis
    /// son manifeste déchiffré. Le déchiffrement NIP-44 et la lecture du blob IPFS sont asynchrones et exigent la
    /// clé du compte : ils sont faits en amont par [`crate::replay_with_ucloud`], qui appelle ensuite cette méthode
    /// pure. Les entrées sont revalidées (chemins, clés, types).
    pub fn plan_manifest(manifest: &astro_dav::manifest::Manifest, src_cid: &str) -> Result<Vec<Statement>, ProjectError> {
        astro_dav::manifest::restore_statements(manifest, src_cid).map_err(|e| ProjectError::BadManifest(e.to_string()))
    }

    /// Variante pour les crochets sans canal d'erreur : un événement non projetable ne projette rien.
    pub fn plan_lossy(e: &Event) -> Vec<Statement> {
        Self::plan(e).unwrap_or_default()
    }

    /// Vide les tables de projection avant un replay. L'état LOCAL est conservé :
    /// identités hébergées (`hosted = 1`) et marqueurs `done` sans événement source.
    pub fn reset() -> Vec<Statement> {
        [
            "DELETE FROM social_graph",
            "DELETE FROM ledger_n2",
            "DELETE FROM resource_leases",
            "DELETE FROM tariffs",
            "DELETE FROM rent_payments",
            "DELETE FROM private_copy_consent",
            "DELETE FROM capital_assets",
            "DELETE FROM kv WHERE scope = 'lifecycle'",
            "DELETE FROM identities WHERE hosted = 0",
            "DELETE FROM done WHERE src_event IS NOT NULL",
            "DELETE FROM kv WHERE scope = 'economic_health'",
        ]
        .into_iter()
        .map(|sql| Statement::new(sql, vec![]))
        .collect()
    }

    /// Déclare une identité hébergée par cette station (état LOCAL, jamais déduit d'un DID).
    pub fn mark_hosted(hex: &str, role: &str) -> Statement {
        Statement::new(
            "INSERT INTO identities(hex, role, hosted) VALUES (?1, ?2, 1) ON CONFLICT(hex) DO UPDATE SET hosted = 1",
            vec![text(hex), text(role)],
        )
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

fn int(n: u64) -> Value {
    Value::Integer(i64::try_from(n).unwrap_or(i64::MAX))
}

fn opt(s: Option<&str>) -> Value {
    s.map_or(Value::Null, text)
}

fn tags_named<'a>(e: &'a Event, name: &'a str) -> impl Iterator<Item = &'a str> {
    e.tags.iter().filter(move |t| t.first().map(String::as_str) == Some(name)).filter_map(|t| t.get(1)).map(String::as_str)
}

/// Kind 3 : réseau social N1/N2 de la station.
///
/// N1 : contacts d'un MULTIPASS hébergé, recalculés en entier à chaque nouvelle liste.
/// N2 : contacts des N1, **recalculés** pour les propriétaires concernés à partir des listes stockées ; quand
/// plusieurs N1 mènent au même N2, la plus petite clé est retenue (`via`). Le résultat ne dépend donc pas de
/// l'ordre d'arrivée des événements.
fn contacts(e: &Event) -> Vec<Statement> {
    let author = e.pubkey.as_str();
    let follows: BTreeSet<&str> = tags_named(e, "p").filter(|p| is_hex64(p) && *p != author).take(MAX_CONTACTS).collect();
    let ts = int(e.created_at);
    // Propriétaires dont les N2 dépendent de cette liste : ceux qui ont l'auteur en N1, et l'auteur s'il est hébergé.
    const AFFECTED: &str = "SELECT owner_hex FROM social_graph WHERE pubkey = ?1 AND level = 1 \
                            UNION SELECT hex FROM identities WHERE hex = ?1 AND hosted = 1";
    let mut out = vec![Statement::new(
        "DELETE FROM social_graph WHERE owner_hex = ?1 AND EXISTS (SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1)",
        vec![text(author)],
    )];
    for p in &follows {
        out.push(Statement::new(
            "INSERT OR REPLACE INTO social_graph(owner_hex, pubkey, level, via, certified_a4l, updated_at, src_event) \
             SELECT ?1, ?2, 1, NULL, 0, ?3, ?4 WHERE EXISTS (SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1)",
            vec![text(author), text(p), ts.clone(), text(&e.id)],
        ));
    }
    out.push(Statement::new(format!("DELETE FROM social_graph WHERE level = 2 AND owner_hex IN ({AFFECTED})"), vec![text(author)]));
    out.push(Statement::new(
        format!(
            "INSERT OR IGNORE INTO social_graph(owner_hex, pubkey, level, via, certified_a4l, updated_at, src_event) \
             SELECT sg.owner_hex, json_extract(j.value, '$[1]'), 2, MIN(sg.pubkey), 0, c.created_at, c.id \
             FROM social_graph sg \
             JOIN nostr_events c ON c.pubkey = sg.pubkey AND c.kind = 3, json_each(c.tags) j \
             WHERE sg.level = 1 AND sg.owner_hex IN ({AFFECTED}) \
               AND json_extract(j.value, '$[0]') = 'p' AND length(json_extract(j.value, '$[1]')) = 64 \
               AND json_extract(j.value, '$[1]') <> sg.owner_hex \
             GROUP BY sg.owner_hex, json_extract(j.value, '$[1]')"
        ),
        vec![text(author)],
    ));
    out
}

/// Kind 30800 : document DID W3C d'un MULTIPASS ou d'un NODE.
fn did(e: &Event) -> Result<Vec<Statement>, ProjectError> {
    let d = e.tag_value("d").unwrap_or("");
    let is_cell = tags_named(e, "t").any(|t| matches!(t, "UMAP" | "SECTOR" | "REGION"));
    let role = if d == "did" && !is_cell {
        "multipass"
    } else if d.starts_with("12D3Koo") && d.len() >= 46 {
        "node"
    } else {
        return Ok(Vec::new()); // cellules géographiques, cooperative-config…
    };

    let doc: Json = serde_json::from_str(&e.content).unwrap_or(Json::Null);
    let empty = Map::new();
    let obj = doc.as_object().unwrap_or(&empty);
    if let Some(id) = obj.get("id").and_then(Json::as_str) {
        if id != format!("did:nostr:{}", e.pubkey) {
            return Err(ProjectError::BadDid("l'identifiant ne correspond pas à l'auteur"));
        }
    }
    let meta = obj.get("metadata").and_then(Json::as_object).unwrap_or(&empty);
    let meta_str = |k: &str| meta.get(k).and_then(Json::as_str).filter(|s| !s.is_empty());

    let email = meta_str("email").or_else(|| e.tag_value("email").filter(|s| !s.is_empty()));
    let status = meta_str("contractStatus").or_else(|| e.tag_value("contractStatus").filter(|s| !s.is_empty()));
    let contract = match status {
        Some("active_rental") => Some("active_rental"),
        Some(s) if s.starts_with("cooperative_member") => Some("cooperative_member"),
        _ => None,
    };
    let g1 = obj
        .get("verificationMethod")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .find(|m| m.get("id").and_then(Json::as_str).is_some_and(|id| id.ends_with("#g1-key")))
        .and_then(|m| m.get("publicKeyBase58"))
        .and_then(Json::as_str);
    let ipns = obj
        .get("alsoKnownAs")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(Json::as_str)
        .find(|a| a.contains("/ipns/") || a.starts_with("k51"));
    let npub = hex_to_npub(&e.pubkey);

    let peer_id = (role == "node").then_some(d);
    let assets = if role == "node" { capital_assets(e, obj, meta) } else { Vec::new() };
    // Station d'attache (`<PeerID>:<NODE_HEX>`) : tag `home_station` ou champ `astroportStation` du DID.
    let home = e.tag_value("home_station").or_else(|| obj.get("astroportStation").and_then(Json::as_str)).or_else(|| meta_str("astroportStation")).filter(|h| valid_home_station(h));
    let mut out = vec![Statement::new(
        "INSERT INTO identities(hex, role, email, npub, g1pub_nostr, ipns, lang, contract, status, created_at, src_event, peer_id, home_station) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
         ON CONFLICT(hex) DO UPDATE SET peer_id = COALESCE(excluded.peer_id, peer_id), home_station = COALESCE(excluded.home_station, home_station), \
           email = COALESCE(excluded.email, email), npub = COALESCE(excluded.npub, npub), \
           g1pub_nostr = COALESCE(excluded.g1pub_nostr, g1pub_nostr), ipns = COALESCE(excluded.ipns, ipns), \
           lang = COALESCE(excluded.lang, lang), contract = excluded.contract, status = excluded.status, \
           src_event = excluded.src_event",
        vec![
            text(&e.pubkey),
            text(role),
            opt(email),
            npub.as_deref().map_or(Value::Null, text),
            opt(g1),
            opt(ipns),
            opt(meta_str("language")),
            opt(contract),
            opt(status),
            int(e.created_at),
            text(&e.id),
            opt(peer_id),
            opt(home),
        ],
    )];
    out.extend(assets);
    Ok(out)
}

/// Nombre maximal d'actifs retenus dans un DID Node.
const MAX_ASSETS: usize = 1_000;

/// `.assets[]` d'un DID Node : instantané complet du registre du capital (ẐEN Capex).
///
/// Le DID est remplaçable : seule sa version la plus récente (`created_at`, puis `id`) fait foi. Les deux
/// requêtes sont gardées par « aucun DID plus récent du même auteur n'est stocké », ce qui rend le résultat
/// indépendant de l'ordre d'arrivée, donc identique en temps réel et au replay. Un DID sans `assets` vide le registre.
fn capital_assets(e: &Event, obj: &Map<String, Json>, meta: &Map<String, Json>) -> Vec<Statement> {
    const NEWEST: &str = "NOT EXISTS (SELECT 1 FROM nostr_events n WHERE n.pubkey = ?1 AND n.kind = 30800 \
                          AND (n.created_at > ?2 OR (n.created_at = ?2 AND n.id > ?3)))";
    let list = obj.get("assets").or_else(|| meta.get("assets")).and_then(Json::as_array);
    let ts = int(e.created_at);
    let head = vec![text(&e.pubkey), ts.clone(), text(&e.id)];
    let mut out = vec![Statement::new(
        format!(
            "DELETE FROM capital_assets WHERE {NEWEST} AND node_hex = ?1"
        ),
        head.clone(),
    )];
    let amount = |a: &Json, k: &str| a.get(k).and_then(Json::as_f64).filter(|v| v.is_finite() && *v >= 0.0);
    for a in list.into_iter().flatten().take(MAX_ASSETS) {
        let Some(id) = a.get("id").and_then(Json::as_str).filter(|s| !s.is_empty() && s.len() <= 128) else { continue };
        let s = |k: &str| a.get(k).and_then(Json::as_str).filter(|v| !v.is_empty());
        let real = |k: &str| amount(a, k).map_or(Value::Null, Value::Real);
        let created = a.get("created_at").and_then(Json::as_u64).map_or_else(|| ts.clone(), int);
        let depreciable = a.get("depreciable").and_then(Json::as_bool).map_or(Value::Null, |b| Value::Integer(i64::from(b)));
        let weeks = a.get("depreciation_weeks").and_then(Json::as_u64).map_or(Value::Null, int);
        let mut params = head.clone();
        params.extend([
            text(id),
            opt(s("label")),
            opt(s("asset_type")),
            opt(s("email")),
            real("value_zen"),
            real("amortized_zen"),
            depreciable,
            weeks,
            opt(s("date")),
            created,
            opt(s("last_depreciated_week")),
            opt(s("status")),
        ]);
        out.push(Statement::new(
            format!(
                "INSERT OR REPLACE INTO capital_assets(node_hex, src_event, id, label, asset_type, email, value_zen, amortized_zen, depreciable, \
                 depreciation_weeks, date, created_at, last_depreciated_week, status) \
                 SELECT ?1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15 WHERE {NEWEST}"
            ),
            params,
        ));
    }
    out
}

/// `<PeerID>:<NODE_HEX>` : un PeerID alphanumérique et la clé Nostr du NODE en 64 hexadécimaux minuscules.
fn valid_home_station(h: &str) -> bool {
    h.split_once(':').is_some_and(|(peer, node)| (10..=64).contains(&peer.len()) && peer.bytes().all(|b| b.is_ascii_alphanumeric()) && is_hex64(node))
}

fn hex_to_npub(hex_pubkey: &str) -> Option<String> {
    let bytes: Vec<u8> = (0..hex_pubkey.len()).step_by(2).map(|i| u8::from_str_radix(hex_pubkey.get(i..i + 2)?, 16).ok()).collect::<Option<_>>()?;
    bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("npub").ok()?, &bytes).ok()
}

/// Kind 30852 : transaction du registre Ğ1-N². Contrôles structurels ; solde, gel et autorités de `mint`
/// relèvent de la politique du relai.
fn ledger(e: &Event) -> Result<Vec<Statement>, ProjectError> {
    let one = |name: &'static str| -> Result<&str, ProjectError> {
        let mut it = tags_named(e, name);
        match (it.next(), it.next()) {
            (Some(v), None) => Ok(v),
            (None, _) => Err(ProjectError::MissingTag(name)),
            _ => Err(ProjectError::BadLedger("tag en double")),
        }
    };
    let (d, dest, amount, prev) = (one("d")?, one("p")?, one("amount")?, one("prev")?);
    if !is_hex64(dest) || dest == e.pubkey {
        return Err(ProjectError::BadLedger("destinataire invalide"));
    }
    if !valid_amount(amount) {
        return Err(ProjectError::BadLedger("montant invalide"));
    }
    if prev != "genesis" && !is_hex64(prev) {
        return Err(ProjectError::BadLedger("prev invalide"));
    }
    if !tags_named(e, "t").any(|t| t == "g1-n2") {
        return Err(ProjectError::BadLedger("tag t=g1-n2 absent"));
    }
    let is_mint = tags_named(e, "t").any(|t| t == "mint");
    let mut out = vec![Statement::new(
        "INSERT OR IGNORE INTO ledger_n2(event_id, author, dest, amount, prev, d, is_mint, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        vec![text(&e.id), text(&e.pubkey), text(dest), text(amount), text(prev), text(d), Value::Integer(i64::from(is_mint)), int(e.created_at)],
    )];
    if !is_mint {
        // Opex : un virement est soit un loyer, soit l'achat d'un bail d'usage (jamais les deux, sinon le bail
        // compterait comme loyer payé et fausserait le cycle de vie et la clôture hebdomadaire).
        if LeaseRequest::is_lease(&e.tags) {
            out.push(lease_from_transfer(e, dest, amount));
        } else {
            out.push(rent_from_transfer(e, dest, amount));
        }
    }
    Ok(out)
}

/// Un virement du MULTIPASS (ou de sa clé LOVE, rattachée par l'email) vers un NODE ou le Capitaine hébergés ici
/// est un **loyer constaté**. Dépendance à l'état exprimée en SQL : le résultat ne dépend pas de l'ordre d'arrivée.
fn rent_from_transfer(e: &Event, dest: &str, amount: &str) -> Statement {
    Statement::new(
        "INSERT OR IGNORE INTO rent_payments(event_id, owner_hex, at, amount_cents, src_kind) \
         SELECT ?1, m.hex, ?3, ?4, 30852 FROM identities p \
         JOIN identities m ON m.hosted = 1 AND m.role = 'multipass' AND (m.hex = p.hex OR (p.email IS NOT NULL AND m.email = p.email)) \
         WHERE p.hex = ?2 AND p.role IN ('multipass', 'love') \
           AND EXISTS (SELECT 1 FROM identities d WHERE d.hex = ?5 AND d.hosted = 1 AND d.role IN ('node', 'captain'))",
        vec![text(&e.id), text(&e.pubkey), int(e.created_at), Value::Integer(cents(amount)), text(dest)],
    )
}

/// Un virement `t=lease` ouvre un bail chez le destinataire (voir `astro_economy::lease`). Illisible : le bail est
/// tout de même inscrit, `invalid`, pour qu'il reste visible.
fn lease_from_transfer(e: &Event, dest: &str, amount: &str) -> Statement {
    let paid = Amount(u64::try_from(cents(amount)).unwrap_or(0));
    match LeaseRequest::from_tags(&e.tags, e.created_at) {
        Ok(req) => lease::lease_statement(&e.id, &e.pubkey, dest, &req, paid, e.created_at),
        Err(_) => lease::invalid_lease_statement(&e.id, &e.pubkey, dest, e.created_at, paid),
    }
}

/// `12.34` → 1234 centimes ; toute valeur illisible vaut 0 (le loyer est constaté, le montant reste indicatif).
fn cents(s: &str) -> i64 {
    let (int_part, frac) = s.split_once('.').unwrap_or((s, ""));
    let whole: i64 = int_part.parse().unwrap_or(0);
    let mut f = frac.chars().take(2).collect::<String>();
    while f.len() < 2 {
        f.push('0');
    }
    whole.saturating_mul(100).saturating_add(f.parse().unwrap_or(0))
}

/// `^[0-9]+(\.[0-9]{1,2})?$` et strictement positif.
fn valid_amount(s: &str) -> bool {
    let (int_part, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits_ok = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    digits_ok(int_part)
        && frac.is_none_or(|f| digits_ok(f) && f.len() <= 2)
        && s.bytes().any(|b| (b'1'..=b'9').contains(&b))
}

fn tag_map(e: &Event) -> Json {
    let mut m = Map::new();
    for t in &e.tags {
        if let (Some(k), Some(v)) = (t.first(), t.get(1)) {
            m.entry(k.clone()).or_insert_with(|| Json::String(v.clone()));
        }
    }
    Json::Object(m)
}

/// Kind 31903 : paramètres applicatifs (un `d` par usage). Seule l'attestation de copie privée (`d = private_copy`)
/// est projetée ; elle n'est valable que signée par le titulaire et avec le texte légal exact (voir
/// `astro_dav::media::parse_consent`). La plus récente remplace l'ancienne, quel que soit l'ordre d'arrivée.
fn app_params(e: &Event) -> Vec<Statement> {
    let Some(version) = astro_dav::media::parse_consent(&e.tags, &e.content) else { return Vec::new() };
    vec![Statement::new(
        "INSERT INTO private_copy_consent(owner_hex, version, accepted_at, src_event) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(owner_hex) DO UPDATE SET version = excluded.version, accepted_at = excluded.accepted_at, src_event = excluded.src_event \
         WHERE excluded.accepted_at >= private_copy_consent.accepted_at",
        vec![text(&e.pubkey), Value::Integer(i64::from(version)), int(e.created_at), text(&e.id)],
    )]
}

/// Kind 30850 : santé économique d'une station (dernier état par auteur).
fn economic_health(e: &Event) -> Vec<Statement> {
    // Grille tarifaire du nœud (`d = tariffs`) : elle ne remplace pas son résumé de santé économique.
    if e.tag_value("d") == Some(TARIFF_D_TAG) {
        return match Tariff::from_tags(&e.tags) {
            Ok(t) => lease::tariff_statements(&e.pubkey, &t, e.created_at, &e.id),
            Err(_) => Vec::new(),
        };
    }
    let summary = serde_json::json!({ "event_id": e.id, "created_at": e.created_at, "tags": tag_map(e) }).to_string();
    let mut out = vec![Statement::new(
        "INSERT INTO kv(scope, key, value, updated_at) VALUES ('economic_health', ?1, ?2, ?3) \
         ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at \
         WHERE excluded.updated_at >= kv.updated_at",
        vec![text(&e.pubkey), Value::Text(summary), int(e.created_at)],
    )];
    if let Some(week) = e.tag_value("week").filter(|w| !w.is_empty()) {
        out.push(Statement::new(
            "INSERT OR REPLACE INTO done(key, value, at, src_event) VALUES (?1, ?2, ?3, ?4)",
            vec![Value::Text(format!("economic_health:{}:{week}", e.pubkey)), text(&e.id), int(e.created_at), text(&e.id)],
        ));
    }
    out
}

/// Kind 30851 : preuve d'émission ẐEN, marqueur d'idempotence `zen_issuance:<auteur>:<d>`.
fn zen_issuance(e: &Event) -> Result<Vec<Statement>, ProjectError> {
    let d = e.tag_value("d").filter(|d| !d.is_empty()).ok_or(ProjectError::MissingTag("d"))?;
    let proof = serde_json::json!({ "event_id": e.id, "tags": tag_map(e) }).to_string();
    let mut out = vec![Statement::new(
        "INSERT OR REPLACE INTO done(key, value, at, src_event) VALUES (?1, ?2, ?3, ?4)",
        vec![Value::Text(format!("zen_issuance:{}:{d}", e.pubkey)), Value::Text(proof), int(e.created_at), text(&e.id)],
    )];
    // Loyer attesté par la station : `type=rent`, `s=OK`, `email` du MULTIPASS. Extension additive du tag `type`
    // (comme `debit`) ; seul un NODE ou le Capitaine hébergés ici peuvent l'attester.
    if e.tag_value("type") == Some("rent") && e.tag_value("s") == Some("OK") {
        if let Some(email) = e.tag_value("email").filter(|m| !m.is_empty()) {
            out.push(Statement::new(
                "INSERT OR IGNORE INTO rent_payments(event_id, owner_hex, at, amount_cents, src_kind) \
                 SELECT ?1, m.hex, ?3, ?4, 30851 FROM identities m \
                 WHERE m.hosted = 1 AND m.role = 'multipass' AND m.email = ?5 \
                   AND EXISTS (SELECT 1 FROM identities a WHERE a.hex = ?2 AND a.hosted = 1 AND a.role IN ('node', 'captain'))",
                vec![text(&e.id), text(&e.pubkey), int(e.created_at), Value::Integer(cents(e.tag_value("amount").unwrap_or("0"))), text(email)],
            ));
        }
    }
    Ok(out)
}
