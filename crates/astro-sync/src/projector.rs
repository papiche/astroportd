//! Projection d'un événement Nostr vers les tables de `astro-db`.
//!
//! `plan` est une fonction pure : elle ne lit pas la base. Les dépendances à l'état (« cette clé est-elle
//! hébergée ? », « qui suit cette clé ? ») sont exprimées en SQL, ce qui rend la projection indépendante de
//! l'ordre d'arrivée des événements : le replay et le temps réel convergent vers le même état.

use std::collections::BTreeSet;

use astro_db::Statement;
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
    pub const KINDS: [u32; 8] = [kind::CONTACTS, kind::DID, kind::ECONOMIC_HEALTH, kind::ZEN_ISSUANCE, kind::LEDGER_N2, kind::ISLAND_CREDIT, kind::MEETING, kind::APP_PARAMS];

    /// Requêtes à exécuter dans la transaction d'insertion de `e`. Vide si le kind n'est pas projeté.
    pub fn plan(e: &Event) -> Result<Vec<Statement>, ProjectError> {
        match e.kind {
            kind::CONTACTS => Ok(contacts(e, true)),
            kind::DID => did(e),
            kind::LEDGER_N2 if astro_economy::island::is_promise(e) => island_promise(e),
            kind::LEDGER_N2 => ledger(e),
            kind::ISLAND_CREDIT => island_credit(e),
            kind::MEETING => meeting(e),
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

    /// Variante du replay : un kind 3 n'y recalcule pas les N2 (ce serait quadratique sur des milliers de listes) ; la phase
    /// se termine par [`Projector::rebuild_social_graph`], un seul recalcul global.
    pub fn plan_replay(e: &Event) -> Result<Vec<Statement>, ProjectError> {
        match e.kind {
            kind::CONTACTS => Ok(contacts(e, false)),
            _ => Self::plan(e),
        }
    }

    /// Fin de la phase « réseau social » du replay, en **une** transaction (le pare-feu ne voit jamais un graphe vide) :
    /// retire les N1 dont la liste source n'existe plus (ou dont le propriétaire n'est plus hébergé), puis recalcule tous
    /// les N2 depuis les listes stockées.
    pub fn rebuild_social_graph() -> Vec<Statement> {
        vec![
            Statement::new(
                "DELETE FROM social_graph WHERE level <> 2 AND (src_event IS NULL                  OR NOT EXISTS (SELECT 1 FROM nostr_events n WHERE n.id = social_graph.src_event AND n.kind = 3)                  OR NOT EXISTS (SELECT 1 FROM identities i WHERE i.hex = social_graph.owner_hex AND i.hosted = 1))",
                vec![],
            ),
            Statement::new("DELETE FROM social_graph WHERE level = 2", vec![]),
            Statement::new(format!("{N2_INSERT} WHERE sg.level = 1 AND length(t.value) = 64 AND t.value <> sg.owner_hex GROUP BY sg.owner_hex, t.value"), vec![]),
        ]
    }

    /// Variante pour les crochets sans canal d'erreur : un événement non projetable ne projette rien.
    pub fn plan_lossy(e: &Event) -> Vec<Statement> {
        Self::plan(e).unwrap_or_default()
    }

    /// Vide les tables de projection avant un replay. L'état LOCAL est conservé :
    /// identités hébergées (`hosted = 1`) et marqueurs `done` sans événement source. `social_graph` n'est **pas** vidé :
    /// le pare-feu social resterait fermé pendant tout le replay ; il est remis d'aplomb en fin de phase
    /// ([`Projector::rebuild_social_graph`]).
    pub fn reset() -> Vec<Statement> {
        [
            "DELETE FROM ledger_n2",
            "DELETE FROM rent_payments",
            "DELETE FROM island_credits",
            "DELETE FROM island_promises",
            "DELETE FROM meetings",
            "DELETE FROM capital_assets",
            "DELETE FROM kv WHERE scope = 'lifecycle'",
            "DELETE FROM identities WHERE hosted = 0",
            "DELETE FROM done WHERE src_event IS NOT NULL",
            "DELETE FROM kv WHERE scope = 'economic_health'",
            "DELETE FROM kv WHERE scope = 'agents'",
            "DELETE FROM kv WHERE scope = 'member_ai'",
            "DELETE FROM key_links",
        ]
        .into_iter()
        .map(|sql| Statement::new(sql, vec![]))
        .collect()
    }

    /// Déclare une identité hébergée par cette station (état LOCAL, jamais déduit d'un DID).
    pub fn mark_hosted(hex: &str, role: &str) -> Statement {
        Statement::new(
            // Un rôle `captain` venu d'un DID distant ne survit pas à l'hébergement : le Capitaine d'ici est désigné localement.
            "INSERT INTO identities(hex, role, hosted) VALUES (?1, ?2, 1) \
             ON CONFLICT(hex) DO UPDATE SET role = CASE WHEN hosted = 0 AND role = 'captain' THEN excluded.role ELSE role END, hosted = 1",
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
/// `with_n2 = false` (replay) : N1 seulement, les N2 sont recalculés une fois en fin de phase.
fn contacts(e: &Event, with_n2: bool) -> Vec<Statement> {
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
    if !with_n2 {
        return out;
    }
    // Temps réel : seulement si l'auteur est N1 d'un hébergé (ou hébergé lui-même) — sinon `AFFECTED` est vide et ces deux
    // requêtes ne touchent rien. Les `p` viennent de `nostr_tags` (index `nt_lookup`), pas d'un `json_each` sur chaque liste.
    out.push(Statement::new(format!("DELETE FROM social_graph WHERE level = 2 AND owner_hex IN ({AFFECTED})"), vec![text(author)]));
    out.push(Statement::new(
        format!("{N2_INSERT} WHERE sg.level = 1 AND sg.owner_hex IN ({AFFECTED}) AND length(t.value) = 64 AND t.value <> sg.owner_hex GROUP BY sg.owner_hex, t.value"),
        vec![text(author)],
    ));
    out
}

/// N2 = contacts (`p` des kind 3 stockés) des N1 ; à plusieurs chemins, la plus petite clé N1 est retenue (`via`).
const N2_INSERT: &str = "INSERT OR IGNORE INTO social_graph(owner_hex, pubkey, level, via, certified_a4l, updated_at, src_event) \
     SELECT sg.owner_hex, t.value, 2, MIN(sg.pubkey), 0, c.created_at, c.id \
     FROM social_graph sg \
     JOIN nostr_events c ON c.pubkey = sg.pubkey AND c.kind = 3 \
     JOIN nostr_tags t ON t.event_id = c.seq AND t.name = 'p'";

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
    // Capitaine d'une autre station : son DID, signé de sa clé, se déclare Capitaine (`t = captain`) ET nomme sa station
    // d'attache. La carte du maillage, elle, dit l'inverse (la station nomme son Capitaine) : l'écran Capitaines croise les
    // deux. Le rôle ne change jamais pour un compte hébergé ici (voir le `ON CONFLICT`).
    let role = if role == "multipass" && home.is_some() && tags_named(e, "t").any(|t| t == "captain") { "captain" } else { role };
    let mut out = vec![Statement::new(
        "INSERT INTO identities(hex, role, email, npub, g1pub_nostr, ipns, lang, contract, status, created_at, src_event, peer_id, home_station) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
         ON CONFLICT(hex) DO UPDATE SET role = CASE WHEN hosted = 0 AND role IN ('multipass', 'captain') AND excluded.role IN ('multipass', 'captain') \
             THEN excluded.role ELSE role END, peer_id = COALESCE(excluded.peer_id, peer_id), home_station = COALESCE(excluded.home_station, home_station), \
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
    // Les loyers arrivés avant ce DID sont rattrapés par le trigger `identities_rent_catch_up` (schéma).
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

/// Kind 30852 : transaction du registre Ğ1-N². Contrôles structurels seulement : le solde, le gel sur fork et les autorités
/// de `mint` ne sont PAS encore vérifiés (ni ici ni dans le relai ; `astro_economy::ledger::Ledger` existe mais n'est pas
/// branché). Tant que ce n'est pas fait, la table `ledger_n2` est une trace, jamais une preuve de paiement.
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
    // Un virement N² n'est jamais un loyer constaté : signé par le payeur seul, sans contrôle de solde ni de chaîne,
    // il permettrait à un membre de se déclarer « à jour » sans rien payer. Le loyer vient du 30851 attesté par la station.
    Ok(vec![Statement::new(
        "INSERT OR IGNORE INTO ledger_n2(event_id, author, dest, amount, prev, d, is_mint, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        vec![text(&e.id), text(&e.pubkey), text(dest), text(amount), text(prev), text(d), Value::Integer(i64::from(is_mint)), int(e.created_at)],
    )])
}

/// Kind 30854 : ouverture de crédit d'une île (ISLAND-1), vérifiée (signature, île ouverte par son auteur). Le plus récent
/// instantané d'un membre l'emporte, quel que soit l'ordre d'arrivée.
fn island_credit(e: &Event) -> Result<Vec<Statement>, ProjectError> {
    let o = astro_economy::island::Opening::from_event(e).ok_or(ProjectError::BadLedger("ouverture d'île invalide"))?;
    let c = o.credit;
    let n = |v: u64| Value::Integer(i64::try_from(v).unwrap_or(i64::MAX));
    Ok(vec![Statement::new(
        "INSERT OR REPLACE INTO island_credits(island, member_hex, node_hex, g1, opening, ud, ud_period, ud_until, at, src_event) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10 \
         WHERE NOT EXISTS (SELECT 1 FROM island_credits WHERE island = ?1 AND member_hex = ?2 AND (at > ?9 OR (at = ?9 AND src_event > ?10)))",
        vec![text(&o.island), text(&o.member), text(&o.node), text(&o.g1), n(c.opening.0), n(c.ud.0), n(c.ud_period), n(c.ud_until), n(c.from), text(&e.id)],
    )])
}

/// Kind 30855 : rencontre attestée ; la preuve de présence est revérifiée ici (fonction pure), la plus récente l'emporte
/// (événement remplaçable : c'est aussi la seule qu'un replay retrouve).
fn meeting(e: &Event) -> Result<Vec<Statement>, ProjectError> {
    let m = astro_economy::meet::Meeting::from_event(e).ok_or(ProjectError::MissingTag("p"))?;
    Ok(vec![Statement::new(
        "INSERT OR REPLACE INTO meetings(owner_hex, target_hex, how, verified, at, src_event) SELECT ?1, ?2, ?3, ?4, ?5, ?6 \
         WHERE NOT EXISTS (SELECT 1 FROM meetings WHERE owner_hex = ?1 AND target_hex = ?2 AND (at > ?5 OR (at = ?5 AND src_event > ?6)))",
        vec![text(&m.owner), text(&m.target), text(m.how), Value::Integer(i64::from(m.verified)), int(m.at), text(&e.id)],
    )])
}

/// Kind 30852 `t=island` : promesse d'île, rangée « en attente » ; sa validité dépend de toute la chaîne du payeur et
/// est recalculée par `island::revalidate` (station), jamais ici.
fn island_promise(e: &Event) -> Result<Vec<Statement>, ProjectError> {
    let tx = astro_economy::Tx::from_event(e).map_err(|_| ProjectError::BadLedger("promesse d'île mal formée"))?;
    let island = astro_economy::island::island_of(e).ok_or(ProjectError::MissingTag("island"))?;
    let g1 = e.tag_value("g1").ok_or(ProjectError::MissingTag("g1"))?;
    let mut out = vec![Statement::new(
        "INSERT OR IGNORE INTO island_promises(event_id, island, author, dest, dest_g1, amount, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        vec![text(&e.id), text(island), text(&e.pubkey), text(&tx.dest), text(g1), Value::Integer(i64::try_from(tx.amount.0).unwrap_or(i64::MAX)), int(e.created_at)],
    )];
    // La promesse embarque l'ouverture de crédit de son payeur, signée par son NODE : une station coupée de celle-ci (la panne
    // en touche souvent plusieurs) la vérifie seule. Elle n'est prise que si elle vise bien ce payeur et cette île.
    if let Ok(opening) = serde_json::from_str::<Event>(&e.content) {
        if astro_economy::island::Opening::from_event(&opening).is_some_and(|o| o.member == e.pubkey && o.island == island) {
            out.extend(island_credit(&opening)?);
        }
    }
    Ok(out)
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

/// Kind 31903 : paramètres applicatifs (un `d` par usage). Sont projetés : le registre des agents (`d = agents`, voir
/// `src/agents.rs`), les paramètres IA du membre et le lien entre deux clés (`plane-link`). La plus récente remplace
/// l'ancienne, quel que soit l'ordre d'arrivée.
fn app_params(e: &Event) -> Vec<Statement> {
    // Registre des agents du membre : contenu chiffré pour lui seul, gardé tel quel (le plus récent l'emporte).
    // Même principe pour les paramètres IA du membre (`d = ai`) : chiffrés pour lui seul, gardés tels quels.
    if let Some(scope) = e.tag_value("d").and_then(|d| match d {
        "agents" => Some("agents"),
        "ai" => Some("member_ai"),
        _ => None,
    }) {
        return vec![Statement::new(
            "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at \
             WHERE excluded.updated_at >= kv.updated_at",
            vec![text(scope), text(&e.pubkey), text(&e.content), int(e.created_at)],
        )];
    }
    if e.tag_value("d") == Some(PLANE_LINK) {
        return plane_link(e);
    }
    Vec::new()
}

/// `d`-tag du lien entre deux clés d'une même personne (voir [`plane_link`]).
pub const PLANE_LINK: &str = "plane-link";
/// `d`-tag de la preuve signée par l'autre clé, portée dans le contenu du lien.
pub const PLANE_LINK_PROOF: &str = "plane-link-proof";

/// Lien entre deux clés d'une même personne (`d = plane-link`) : l'événement est signé par la clé `a` et nomme `b` (tag `p`) ;
/// son contenu est un événement signé par `b` qui nomme `a`. Sans ces deux signatures croisées, rien n'est retenu : une clé ne peut
/// pas se prétendre liée à celle d'un autre.
fn plane_link(e: &Event) -> Vec<Statement> {
    let Some(b) = e.tag_value("p").filter(|b| astro_nostr_proto::is_hex64(b) && *b != e.pubkey) else { return Vec::new() };
    let Ok(proof) = serde_json::from_str::<Event>(&e.content) else { return Vec::new() };
    let valid = proof.verify().is_ok() && proof.kind == kind::APP_PARAMS && proof.pubkey == b && proof.tag_value("d") == Some(PLANE_LINK_PROOF) && proof.tag_value("p") == Some(e.pubkey.as_str());
    if !valid {
        return Vec::new();
    }
    let plane: String = e.tag_value("plane").unwrap_or_default().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-').take(32).collect();
    vec![Statement::new(
        "INSERT INTO key_links(a, b, plane, at, src_event) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(a, b) DO UPDATE SET plane = excluded.plane, at = excluded.at, src_event = excluded.src_event WHERE excluded.at >= key_links.at",
        vec![text(&e.pubkey), text(b), text(&plane), int(e.created_at), text(&e.id)],
    )]
}

/// Kind 30850 : santé économique d'une station (dernier état par auteur).
fn economic_health(e: &Event) -> Vec<Statement> {
    // Les anciennes grilles tarifaires (`d = tariffs`) ne sont plus projetées et ne remplacent pas le résumé de santé.
    if e.tag_value("d") == Some("tariffs") {
        return Vec::new();
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
