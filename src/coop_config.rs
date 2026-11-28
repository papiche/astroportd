//! Configuration coopérative de la constellation : kind 30800 `d=cooperative-config`, signé par la clé coopérative
//! `<UPLANETNAME>.G1` (celle de `UPLANETNAME_G1`), lisible et modifiable par le Capitaine depuis l'interface.
//!
//! Même format que `Astroport.ONE/tools/cooperative_config.sh` : les stations legacy et astroportd **se lisent
//! mutuellement**. Le contenu est un objet JSON de chaînes ; les valeurs sensibles (nom contenant `TOKEN`, `SECRET`, `KEY`,
//! `PASSWORD`, `API` ou `PRIVATE`) sont chiffrées AES-256-CBC avec `UPLANETNAME` (`iv:base64`, voir
//! [`astro_crypto::coop`]) ; les autres sont en clair. Les secrets sont **en écriture seule** dans l'interface : on voit
//! qu'ils sont renseignés, jamais leur valeur.
//!
//! L'événement est publié sur le relai local puis sur les relais configurés ; la synchronisation N¹/N² le réplique aux
//! autres stations (la clé coopérative fait partie des clés suivies par le crawl).

use std::collections::BTreeMap;
use std::sync::Arc;

use astro_bro::publisher::Publisher;
use astro_core::formula::{FormulaInput, KeyFormula};
use astro_crypto::coop;
use astro_db::Db;
use astro_nostr_proto::{Event, UnsignedEvent};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sha2::Digest;

/// Kind du DID coopératif.
pub const KIND: u32 = 30_800;
/// Étiquette `d` de la configuration.
pub const D_TAG: &str = "cooperative-config";

/// Une section de l'écran : identifiant, titre, clés.
pub struct Section {
    /// Identifiant stable.
    pub id: &'static str,
    /// Titre affiché.
    pub title: &'static str,
    /// Clés, dans l'ordre d'affichage (celui de `cooperative_config.sh`).
    pub keys: &'static [&'static str],
    /// Niveau de l'écran, par tâche : `essential` (mettre la station en service), `money` (recevoir les dons),
    /// `rules` (règles communes : répartition, impôt, PAF suggérée), `services` (retours des utilisateurs, reconnaissance des plantes), `legacy` (clés que
    /// seules les stations Astroport.ONE lisent : astroportd les publie et les garde sans s'en servir).
    pub level: &'static str,
}

/// Niveaux, dans l'ordre de l'écran.
pub const LEVELS: &[&str] = &["essential", "money", "rules", "services", "legacy"];

/// Styles qu'une UPlanet privée peut choisir (`UI_STYLE`). **Kraft est réservé à ORIGIN** : on voit d'un coup d'œil qu'on est
/// dans le bac à sable public.
pub const STYLES: &[&str] = &["aurore", "sobre"];

/// Clés qu'une **station** peut surdéfinir pour elle seule (le Capitaine y met sa propre clé quand celle de la coopérative
/// atteint ses limites). La valeur locale est de l'état local (`kv`, portée [`LOCAL_SCOPE`]) : jamais publiée, elle passe
/// avant celle de la coopérative dans [`setting`].
pub const STATION_KEYS: &[&str] = &["PLANTNET_API_KEY"];
/// Portée `kv` des valeurs locales.
pub const LOCAL_SCOPE: &str = "coop_local";
/// Préfixe d'une clé de `patch` qui vise la station seule (`station:PLANTNET_API_KEY`), pas la coopérative.
pub const LOCAL_PREFIX: &str = "station:";

/// Sections et clés connues. Toute autre clé est refusée à l'écriture (liste blanche, comme côté UPassport). L'`id` est
/// aussi l'ancre de la section dans `docs/CONFIG_COOP.md` (lien « ? » de l'écran).
pub const SECTIONS: &[Section] = &[
    Section { id: "uplanet", title: "Nom et charte de l'UPlanet", keys: &["UPLANET_NAME", "CHARTER_VERSION", "CHARTER_TEXT"], level: "essential" },
    Section { id: "look", title: "Monnaie et apparence", keys: &["CURRENCY_NAME", "CURRENCY_SYMBOL", "UI_STYLE"], level: "essential" },
    Section { id: "ovh", title: "Domaine de la coopérative (OVH)", keys: &["OVH_ZONE", "OVH_APP_KEY", "OVH_APP_SECRET", "OVH_CONSUMER_KEY"], level: "essential" },
    Section { id: "mesh", title: "Stations relais du maillage", keys: &["P2P_RELAYS", "ORIGIN_RELAY"], level: "essential" },
    Section { id: "oc", title: "OpenCollective", keys: &["OCSLUG", "OCAPIKEY", "OC_AUTO_CREDIT", "OC_URL_MEMBRE", "OC_URL_CLOUD", "OC_URL_SATELLITE", "OC_URL_CONSTELLATION"], level: "money" },
    Section { id: "tiers", title: "Routage des paliers", keys: &["TIER_SLUG_SATELLITE", "TIER_SLUG_CONSTELLATION", "TIER_SLUG_LABO"], level: "money" },
    Section { id: "3x13", title: "Règle 3×1/3 + 1 %", keys: &["TREASURY_PERCENT", "RND_PERCENT", "ASSETS_PERCENT", "CAPTAIN_BONUS_PERCENT"], level: "rules" },
    Section { id: "fiscal", title: "Impôt sur les sociétés", keys: &["IS_RATE_REDUCED", "IS_RATE_NORMAL", "IS_THRESHOLD"], level: "rules" },
    Section { id: "shares", title: "PAF suggérée", keys: &["PAF"], level: "rules" },
    Section { id: "api", title: "Retours des utilisateurs (Git)", keys: &["GIT_HOST", "GIT_OWNER", "GIT_TOKEN"], level: "services" },
    Section { id: "plantnet", title: "Reconnaissance des plantes (Pl@ntNet)", keys: &["PLANTNET_API_KEY"], level: "services" },
    Section { id: "legacy", title: "Stations Astroport.ONE", keys: &["TVA_RATE", "ZENCARD_SATELLITE", "ZENCARD_CONSTELLATION", "NCARD", "ZCARD", "TIER_SLUG_CLOUD", "MJ_APIKEY_PUBLIC", "MJ_APIKEY_PRIVATE", "MJ_SENDER_EMAIL"], level: "legacy" },
];

/// Relai ORIGIN de la coopérative, où une station ẐEN ouverte (Village, Place publique) annonce aussi sa balise : la carte
/// des stations y trouve toutes les UPlanet. `ORIGIN_RELAY` le change ; `none` n'annonce rien hors de sa constellation.
pub const DEFAULT_ORIGIN_RELAY: &str = "wss://relay.copylaradio.com";

/// Relais ORIGIN, au plus : celui de la coopérative et ceux qu'ajoutent les Capitaines.
pub const MAX_ORIGIN_RELAYS: usize = 4;

/// Lit `ORIGIN_RELAY` : adresses `wss://` séparées par des espaces, des virgules ou des retours à la ligne, ou `none` seul
/// (liste vide : ne rien annoncer hors de la constellation). Les doublons sont retirés.
pub fn parse_origin_relays(value: &str) -> Result<Vec<String>, String> {
    let items: Vec<&str> = value.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).collect();
    if items == ["none"] {
        return Ok(Vec::new());
    }
    let mut out: Vec<String> = Vec::new();
    for raw in items {
        let url = raw.trim_end_matches('/');
        if !url.strip_prefix("wss://").is_some_and(valid_domain) {
            return Err(format!("ORIGIN_RELAY : {raw} : une adresse wss://relai.exemple.org est attendue (une par ligne), ou none seul pour ne rien annoncer hors de la constellation"));
        }
        if !out.iter().any(|o| o == url) {
            out.push(url.to_owned());
        }
    }
    if out.len() > MAX_ORIGIN_RELAYS {
        return Err(format!("ORIGIN_RELAY : {MAX_ORIGIN_RELAYS} relais au plus"));
    }
    Ok(out)
}

/// Relais ORIGIN où annoncer la balise d'une station de `uplanet_name` : vide si la coopérative a choisi `none` ;
/// [`DEFAULT_ORIGIN_RELAY`] si rien n'est publié (ou si la valeur est illisible). Une station **ORIGIN** annonce toujours sur
/// [`DEFAULT_ORIGIN_RELAY`] et lui seul : c'est là que la carte des UPlanet les trouve, et la configuration d'ORIGIN est signée
/// par une clé que tout le monde peut calculer, donc on ne la suit pas pour ce choix.
pub async fn origin_relays(db: &Db, uplanet_name: &str) -> Vec<String> {
    if uplanet_name == crate::init::ORIGIN_UPLANETNAME {
        return vec![DEFAULT_ORIGIN_RELAY.to_owned()];
    }
    match setting(db, uplanet_name, "ORIGIN_RELAY").await.map(|v| parse_origin_relays(&v)) {
        Some(Ok(list)) => list,
        _ => vec![DEFAULT_ORIGIN_RELAY.to_owned()],
    }
}

/// Ventilation 3×1/3 de la constellation : les taux publiés par la coopérative (`IS_RATE_REDUCED`, `IS_RATE_NORMAL`,
/// `IS_THRESHOLD`, `CAPTAIN_BONUS_PERCENT`, `TREASURY_PERCENT`, `RND_PERCENT`, `ASSETS_PERCENT`) remplacent ceux de `base` ;
/// une valeur absente ou illisible garde celle de `base`. Lu à chaque clôture : une modification vaut dès la semaine suivante.
pub async fn split_config(db: &Db, uplanet_name: &str, base: astro_economy::split::SplitConfig) -> astro_economy::split::SplitConfig {
    let Ok((_, hex)) = coop_keys(uplanet_name) else { return base };
    let Some(stored) = load(db, &hex).await else { return base };
    split_of(&stored.values, base)
}

/// Les taux de `values` sur `base` (sans base de données : testable). Un pourcentage décimal (« 15.5 ») devient des points
/// de base sans flottant ; un taux au-delà de 100 % est ignoré.
pub fn split_of(values: &BTreeMap<String, String>, base: astro_economy::split::SplitConfig) -> astro_economy::split::SplitConfig {
    let num = |k: &str| values.get(k).and_then(|v| crate::oc_emission::cents_of_decimal(v.trim()));
    let bp = |k: &str, d: u32| num(k).and_then(|n| u32::try_from(n).ok()).filter(|n| *n <= 10_000).unwrap_or(d);
    let mut c = base;
    c.is_low_bp = bp("IS_RATE_REDUCED", c.is_low_bp);
    c.is_high_bp = bp("IS_RATE_NORMAL", c.is_high_bp);
    c.captain_bonus_bp = bp("CAPTAIN_BONUS_PERCENT", c.captain_bonus_bp);
    let thirds = (bp("TREASURY_PERCENT", c.treasury_bp), bp("RND_PERCENT", c.rnd_bp), bp("ASSETS_PERCENT", c.assets_bp));
    if thirds.0 + thirds.1 + thirds.2 > 0 {
        (c.treasury_bp, c.rnd_bp, c.assets_bp) = thirds;
    }
    if let Some(cents) = num("IS_THRESHOLD") {
        c.is_threshold = astro_core::Amount(cents);
    }
    c
}

/// Stations relais de la constellation, au plus.
pub const MAX_RELAYS: usize = 8;

/// Lit `P2P_RELAYS` : adresses de maillage séparées par des espaces, des virgules ou des retours à la ligne. Chacune finit
/// par `/p2p/<PeerID>`, passe par TCP et désigne la station elle-même (pas un circuit relais).
pub fn parse_relays(value: &str) -> Result<Vec<(astro_p2p::Multiaddr, astro_p2p::PeerId)>, String> {
    let mut out: Vec<(astro_p2p::Multiaddr, astro_p2p::PeerId)> = Vec::new();
    for raw in value.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()) {
        let (addr, peer) = crate::mesh_admin::parse_station(raw).map_err(|e| format!("P2P_RELAYS : {raw} : {e}"))?;
        let text = addr.to_string();
        if text.contains("/p2p-circuit") || !text.contains("/tcp/") {
            return Err(format!("P2P_RELAYS : {raw} : adresse directe attendue (/ip4/, /ip6/ ou /dns4/…/tcp/4101/p2p/<PeerID>)"));
        }
        if !out.iter().any(|(_, p)| *p == peer) {
            out.push((addr, peer));
        }
    }
    if out.len() > MAX_RELAYS {
        return Err(format!("P2P_RELAYS : {MAX_RELAYS} stations relais au plus"));
    }
    Ok(out)
}

/// Stations relais publiées par la coopérative de `uplanet_name` (liste vide si rien n'est publié ou si la valeur est illisible).
pub async fn p2p_relays(db: &Db, uplanet_name: &str) -> Vec<(astro_p2p::Multiaddr, astro_p2p::PeerId)> {
    let Ok((_, hex)) = coop_keys(uplanet_name) else { return Vec::new() };
    let Some(stored) = load(db, &hex).await else { return Vec::new() };
    stored.values.get("P2P_RELAYS").and_then(|v| parse_relays(v).ok()).unwrap_or_default()
}

/// Charte proposée tant que le Capitaine n'en a pas publié une (même texte que `docs/CHARTE_UPLANET.md`).
pub const DEFAULT_CHARTER: &str = include_str!("../docs/CHARTE_UPLANET.md");

/// Nom, version et texte de la charte d'une constellation, tels que la coopérative les a publiés (texte type à défaut).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Charter {
    /// Nom de l'UPlanet, s'il en a un.
    pub name: Option<String>,
    /// Numéro de version (`1.0` par défaut).
    pub version: String,
    /// Texte de la charte.
    pub text: String,
    /// Publiée par la coopérative (et non le texte type).
    pub custom: bool,
    /// Empreinte SHA-256 du texte : ce que chaque Capitaine signe.
    pub sha256: String,
}

/// La charte qui ressort de la configuration publiée (`None` : rien publié, donc texte type).
pub fn charter_of(stored: Option<&Stored>) -> Charter {
    let get = |k: &str| stored.and_then(|s| s.values.get(k)).filter(|v| !v.is_empty()).cloned();
    let custom = get("CHARTER_TEXT");
    let text = custom.clone().unwrap_or_else(|| DEFAULT_CHARTER.to_owned());
    let sha256 = hex::encode(sha2::Sha256::digest(text.as_bytes()));
    Charter { name: get("UPLANET_NAME"), version: get("CHARTER_VERSION").unwrap_or_else(|| "1.0".into()), text, custom: custom.is_some(), sha256 }
}

/// La clé est-elle connue ?
pub fn is_known(key: &str) -> bool {
    SECTIONS.iter().any(|s| s.keys.contains(&key))
}

/// Règle de `cooperative_config.sh` : un nom qui contient l'un de ces mots est chiffré.
pub fn is_secret(key: &str) -> bool {
    ["TOKEN", "SECRET", "KEY", "PASSWORD", "API", "PRIVATE"].iter().any(|w| key.contains(w))
}

/// Contrôle d'une valeur en clair avant écriture.
pub fn validate(key: &str, value: &str) -> Result<(), String> {
    // La charte est un texte : 6000 caractères au plus et des retours à la ligne ; toute autre valeur tient sur une ligne.
    let (max, text) = match key {
        "CHARTER_TEXT" => (6000, true),
        "P2P_RELAYS" | "ORIGIN_RELAY" => (1024, true),
        _ => (1024, false),
    };
    if value.len() > max || value.chars().any(|c| c.is_control() && !(text && c == '\n')) {
        return Err(format!("{key} : {max} caractères au plus, sans caractère de contrôle"));
    }
    if key == "UPLANET_NAME" && (value.chars().count() > 48 || value.trim() != value) {
        return Err(format!("{key} : 48 caractères au plus, sans espace au début ni à la fin"));
    }
    if key == "CURRENCY_NAME" && (value.chars().count() > 16 || value.trim() != value || value.chars().any(|c| c.is_ascii_digit())) {
        return Err(format!("{key} : 16 caractères au plus, sans chiffre ni espace au début ou à la fin (ex. Ẑen, Sol, Graine)"));
    }
    if key == "CURRENCY_SYMBOL" && (value.chars().count() > 3 || value.chars().any(|c| c.is_whitespace() || c.is_ascii_digit())) {
        return Err(format!("{key} : 3 caractères au plus, sans espace ni chiffre (ex. Ẑ, ☉, G)"));
    }
    if key == "UI_STYLE" && !STYLES.contains(&value) {
        return Err(format!("{key} : {} (le style Kraft est réservé à ORIGIN)", STYLES.join(" ou ")));
    }
    if key == "CHARTER_VERSION" && !value.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Err(format!("{key} : un numéro de version est attendu (ex. 1.0)"));
    }
    let numeric = key.ends_with("_RATE") || key.ends_with("_PERCENT") || key.ends_with("_THRESHOLD") || matches!(key, "ZENCARD_SATELLITE" | "ZENCARD_CONSTELLATION" | "PAF" | "NCARD" | "ZCARD");
    if numeric && (value.len() > 12 || value.is_empty() || value.split('.').count() > 2 || !value.bytes().all(|b| b.is_ascii_digit() || b == b'.') || value.starts_with('.') || value.ends_with('.')) {
        return Err(format!("{key} : un nombre décimal positif est attendu (ex. 20.0)"));
    }
    if (key.starts_with("OC_URL_") || key == "GIT_HOST") && !value.starts_with("https://") {
        return Err(format!("{key} : une adresse https:// est attendue"));
    }
    if key == "OC_AUTO_CREDIT" && !matches!(value, "0" | "1") {
        return Err(format!("{key} : 1 (la station crédite seule les dons OpenCollective) ou 0 (le Capitaine confirme)"));
    }
    if key == "P2P_RELAYS" {
        parse_relays(value)?;
    }
    if key == "ORIGIN_RELAY" {
        parse_origin_relays(value)?;
    }
    if key == "OVH_ZONE" && !value.is_empty() && !valid_domain(value) {
        return Err(format!("{key} : un nom de domaine est attendu (ex. copylaradio.com)"));
    }
    if key == "MJ_SENDER_EMAIL" && !(value.contains('@') && value.contains('.') && !value.contains(' ')) {
        return Err(format!("{key} : une adresse e-mail est attendue"));
    }
    Ok(())
}

/// Nom de domaine pour les adresses générées : étiquettes de lettres, chiffres et `-`, au moins un point.
pub fn valid_domain(d: &str) -> bool {
    d.len() <= 100 && d.contains('.') && d.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

/// Domaine des adresses générées : la zone DNS de l'API OVH (`OVH_ZONE` de la configuration coopérative), si elle est renseignée et valide.
pub async fn domain(db: &Db, uplanet_name: &str) -> Option<String> {
    let (_, hex) = coop_keys(uplanet_name).ok()?;
    let stored = load(db, &hex).await?;
    stored.values.get("OVH_ZONE").map(|d| d.trim().to_ascii_lowercase()).filter(|d| valid_domain(d))
}

/// Valeur d'un réglage de la coopérative, secret déchiffré, `None` si absent ou vide. Pour une clé de [`STATION_KEYS`], la valeur
/// propre à la station passe avant.
pub async fn setting(db: &Db, uplanet_name: &str, key: &str) -> Option<String> {
    if STATION_KEYS.contains(&key)
        && let Some(v) = local(db, key).await
    {
        return Some(v);
    }
    let (_, hex) = coop_keys(uplanet_name).ok()?;
    let stored = load(db, &hex).await?;
    let v = stored.values.get(key)?;
    let v = if astro_crypto::coop::is_encrypted(v) { astro_crypto::coop::decrypt(uplanet_name, v).ok()? } else { v.clone() };
    Some(v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Valeur propre à la station d'une clé de [`STATION_KEYS`], `None` si absente.
pub async fn local(db: &Db, key: &str) -> Option<String> {
    let k = key.to_owned();
    db.readers()
        .read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [LOCAL_SCOPE, k.as_str()], |r| r.get::<_, String>(0)).optional()?))
        .await
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
}

/// Clés de [`STATION_KEYS`] que cette station a surdéfinies.
pub async fn local_keys(db: &Db) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for k in STATION_KEYS {
        if local(db, k).await.is_some() {
            out.insert((*k).to_owned());
        }
    }
    out
}

/// Comment l'interface se présente sur cette constellation : nom de l'UPlanet, nom et symbole de la monnaie, style.
///
/// **ORIGIN ne se personnalise pas** : sa clé coopérative se calcule par tous (64 zéros), n'importe qui pourrait donc renommer sa
/// monnaie. ORIGIN reste « Ẑen », « Ẑ », style Kraft. Une UPlanet privée prend ce que sa coopérative publie, sinon les défauts.
pub async fn look(db: &Db, uplanet_name: &str) -> Value {
    let origin = uplanet_name == crate::init::ORIGIN_UPLANETNAME;
    let stored = if origin { None } else { coop_keys(uplanet_name).ok().map(|(_, hex)| hex) };
    let stored = match stored {
        Some(hex) => load(db, &hex).await,
        None => None,
    };
    look_of(origin, stored.as_ref().map(|s| &s.values))
}

/// [`look`] sans base de données (testable). Une valeur invalide (publiée par une station legacy, par exemple) est ignorée.
pub fn look_of(origin: bool, values: Option<&BTreeMap<String, String>>) -> Value {
    let get = |k: &str| if origin { None } else { values.and_then(|v| v.get(k)).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty() && validate(k, v).is_ok()) };
    json!({
        "origin": origin,
        "name": if origin { Some("ORIGIN".to_owned()) } else { get("UPLANET_NAME") },
        "currency": get("CURRENCY_NAME").unwrap_or_else(|| "Ẑen".into()),
        "symbol": get("CURRENCY_SYMBOL").unwrap_or_else(|| "Ẑ".into()),
        "style": if origin { Some("kraft".to_owned()) } else { get("UI_STYLE") },
    })
}

/// Clé coopérative `<UPLANETNAME>.G1` : `(secret Nostr, clé publique hexadécimale)`.
pub fn coop_keys(uplanet_name: &str) -> Result<([u8; 32], String), String> {
    let input = FormulaInput { salt: "", pepper: "", uplanet_name, coop_suffix: Some("G1") };
    let credentials = KeyFormula::CoopPrefix.credentials(&input).map_err(|e| e.to_string())?;
    let keys = credentials.seed().map_err(|e| e.to_string())?.nostr_keys().map_err(|e| e.to_string())?;
    Ok((*keys.secret_bytes(), keys.pubkey_hex()))
}

/// Configuration lue dans la base.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stored {
    /// Identifiant de l'événement.
    pub event_id: String,
    /// Date de l'événement.
    pub created_at: u64,
    /// Valeurs, telles que publiées (secrets chiffrés).
    pub values: BTreeMap<String, String>,
}

/// Dernière configuration publiée par la clé coopérative `author_hex`.
pub async fn load(db: &Db, author_hex: &str) -> Option<Stored> {
    let a = author_hex.to_owned();
    let row: Option<(String, i64, String)> = db
        .readers()
        .read(move |c| {
            Ok(c.query_row(
                "SELECT e.id, e.created_at, e.content FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'd' AND t.value = ?3 WHERE e.kind = ?1 AND e.pubkey = ?2 ORDER BY e.created_at DESC LIMIT 1",
                rusqlite::params![KIND, a, D_TAG],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
        })
        .await
        .ok()
        .flatten();
    let (event_id, created_at, content) = row?;
    let Value::Object(map) = serde_json::from_str::<Value>(&content).ok()? else { return None };
    let values = map.into_iter().filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_owned()))).collect();
    Some(Stored { event_id, created_at: u64::try_from(created_at).unwrap_or(0), values })
}

/// Vue pour l'interface : sections, valeurs en clair, secrets masqués (renseigné ou non, déchiffrable ou non).
/// `local` : clés de [`STATION_KEYS`] que la station a surdéfinies (champ `station` de la vue).
pub fn view(stored: Option<&Stored>, uplanet_name: &str, coop_hex: &str, local: &std::collections::BTreeSet<String>) -> Value {
    let get = |k: &str| stored.and_then(|s| s.values.get(k));
    let sections: Vec<Value> = SECTIONS
        .iter()
        .map(|s| {
            let fields: Vec<Value> = s
                .keys
                .iter()
                .map(|k| {
                    let mut f = match (is_secret(k), get(k)) {
                        (true, Some(v)) if !v.is_empty() => json!({ "key": k, "secret": true, "set": true, "readable": coop::decrypt(uplanet_name, v).is_ok() }),
                        (true, _) => json!({ "key": k, "secret": true, "set": false, "readable": true }),
                        (false, v) => json!({ "key": k, "secret": false, "value": v }),
                    };
                    if STATION_KEYS.contains(k) {
                        f["station"] = json!(local.contains(*k));
                    }
                    f
                })
                .collect();
            json!({ "id": s.id, "title": s.title, "level": s.level, "fields": fields })
        })
        .collect();
    let extra: Vec<&String> = stored.map(|s| s.values.keys().filter(|k| !is_known(k) && !k.starts_with('_')).collect()).unwrap_or_default();
    json!({
        "did": format!("did:nostr:{coop_hex}"), "event_id": stored.map(|s| s.event_id.clone()), "updated_at": stored.map(|s| s.created_at),
        "sections": sections, "unmanaged": extra, "exists": stored.is_some(),
    })
}

/// Fusionne `patch` dans la configuration existante, chiffre les secrets et signe le nouvel événement.
/// `None` ou chaîne vide : la clé est retirée. Les clés inconnues sont refusées ; celles qui ne sont pas gérées par
/// l'interface (`_comment_*`, `COOPERATIVE_VERSION`…) sont conservées telles quelles.
pub fn build_event(stored: Option<&Stored>, patch: &BTreeMap<String, Option<String>>, uplanet_name: &str, now: u64) -> Result<Event, String> {
    if patch.is_empty() {
        return Err("aucune modification".into());
    }
    let mut values = stored.map(|s| s.values.clone()).unwrap_or_default();
    for (key, value) in patch {
        if !is_known(key) {
            return Err(format!("clé inconnue : {key}"));
        }
        match value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            None => {
                values.remove(key);
            }
            Some(v) => {
                validate(key, v)?;
                let stored_value = if is_secret(key) { coop::encrypt(uplanet_name, v).map_err(|e| format!("aléa indisponible : {e}"))? } else { v.to_owned() };
                values.insert(key.clone(), stored_value);
            }
        }
    }
    values.entry("COOPERATIVE_VERSION".into()).or_insert_with(|| "1.0".into());
    // Garde de sécurité (celle du script) : un nom sensible ne part JAMAIS en clair.
    if let Some(k) = values.iter().find(|(k, v)| is_secret(k) && !v.is_empty() && !coop::is_encrypted(v)).map(|(k, _)| k) {
        return Err(format!("{k} : valeur sensible non chiffrée, publication refusée"));
    }
    let (secret, _) = coop_keys(uplanet_name)?;
    // Un événement remplaçable : la date doit dépasser celle du précédent, même si l'horloge a reculé.
    let created_at = stored.map_or(now, |s| now.max(s.created_at + 1));
    let tags = vec![vec!["d".into(), D_TAG.into()], vec!["t".into(), D_TAG.into()], vec!["t".into(), "uplanet".into()]];
    UnsignedEvent { created_at, kind: KIND, tags, content: serde_json::to_string(&values).map_err(|e| e.to_string())? }.sign(&secret).map_err(|e| e.to_string())
}

/// Écrit la configuration : construit, signe, publie. Rend la vue mise à jour. Les clés préfixées par [`LOCAL_PREFIX`] ne
/// partent pas : elles règlent la station seule (état local), et ne valent que pour [`STATION_KEYS`].
pub async fn save<P: Publisher>(db: &Db, publisher: &Arc<P>, uplanet_name: &str, patch: &BTreeMap<String, Option<String>>) -> Result<Value, String> {
    if patch.is_empty() {
        return Err("aucune modification".into());
    }
    let (_, coop_hex) = coop_keys(uplanet_name)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (mine, shared): (BTreeMap<_, _>, BTreeMap<_, _>) = patch.iter().map(|(k, v)| (k.clone(), v.clone())).partition(|(k, _)| k.starts_with(LOCAL_PREFIX));
    // Tout est vérifié avant d'écrire quoi que ce soit : une saisie fausse n'enregistre pas la moitié du reste.
    let mut writes = Vec::new();
    for (k, v) in &mine {
        let key = &k[LOCAL_PREFIX.len()..];
        if !STATION_KEYS.contains(&key) {
            return Err(format!("{key} : cette clé vaut pour toute la coopérative, une station ne la surdéfinit pas"));
        }
        let v = v.as_deref().map(str::trim).filter(|v| !v.is_empty());
        if let Some(v) = v {
            validate(key, v)?;
        }
        writes.push(match v {
            Some(v) => astro_db::Statement::new(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![LOCAL_SCOPE.to_owned().into(), key.to_owned().into(), v.to_owned().into(), i64::try_from(now).unwrap_or(0).into()],
            ),
            None => astro_db::Statement::new("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![LOCAL_SCOPE.to_owned().into(), key.to_owned().into()]),
        });
    }
    let stored = load(db, &coop_hex).await;
    let event = if shared.is_empty() { None } else { Some(build_event(stored.as_ref(), &shared, uplanet_name, now)?) };
    if !writes.is_empty() {
        db.writer().batch(writes).await.map_err(|e| e.to_string())?;
    }
    let mut published = None;
    if let Some(event) = event {
        publisher.publish(event.clone()).await.map_err(|e| format!("publication refusée : {e}"))?;
        published = Some(Stored { event_id: event.id.clone(), created_at: event.created_at, values: serde_json::from_str::<BTreeMap<String, String>>(&event.content).unwrap_or_default() });
    }
    // Journal d'audit : QUELLES clés ont changé, jamais leurs valeurs.
    let record = json!({ "keys": patch.keys().collect::<Vec<_>>(), "event": published.as_ref().map(|p| p.event_id.clone()) });
    let _ = db
        .writer()
        .execute("INSERT INTO journal(stream, at, data) VALUES ('coop_config', ?1, ?2)", vec![i64::try_from(now).unwrap_or(0).into(), record.to_string().into()])
        .await;
    let fresh = load(db, &coop_hex).await;
    Ok(view(fresh.as_ref().or(published.as_ref()).or(stored.as_ref()), uplanet_name, &coop_hex, &local_keys(db).await))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_db::events::insert_event;

    use super::*;

    const UP: &str = "UPlanetTestName";

    /// Publieur qui range l'événement dans la base, comme le fait le relai local.
    struct Store(Arc<Db>, Mutex<Vec<Event>>);

    impl Publisher for Store {
        async fn publish(&self, event: Event) -> Result<(), String> {
            self.1.lock().unwrap().push(event.clone());
            self.0.writer().transaction(move |tx| insert_event(tx, &event).map(|_| ())).await.map_err(|e| e.to_string())
        }
    }

    async fn db(tag: &str) -> (std::path::PathBuf, Arc<Db>) {
        let dir = std::env::temp_dir().join(format!("astroportd-coop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), Arc::new(Db::open(&dir.join("u.db"), 2).unwrap()))
    }

    fn patch(items: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        items.iter().map(|(k, v)| ((*k).to_owned(), v.map(str::to_owned))).collect()
    }

    #[test]
    fn the_published_rates_drive_the_weekly_split() {
        let base = astro_economy::split::SplitConfig::default();
        let v: BTreeMap<String, String> = [("IS_RATE_REDUCED", "10"), ("IS_RATE_NORMAL", "20.5"), ("IS_THRESHOLD", "30000"), ("CAPTAIN_BONUS_PERCENT", "2"), ("TREASURY_PERCENT", "50"), ("RND_PERCENT", "24"), ("ASSETS_PERCENT", "24")]
            .into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        let c = split_of(&v, base.clone());
        assert_eq!((c.is_low_bp, c.is_high_bp, c.captain_bonus_bp, c.is_threshold), (1000, 2050, 200, astro_core::Amount(3_000_000)));
        assert_eq!((c.treasury_bp, c.rnd_bp, c.assets_bp), (5000, 2400, 2400));
        // Illisible, au-delà de 100 % ou absent : la valeur de base reste.
        let bad: BTreeMap<String, String> = [("IS_RATE_REDUCED", "beaucoup"), ("IS_RATE_NORMAL", "150")].into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        assert_eq!(split_of(&bad, base.clone()), base);
        // Les pourcentages par défaut de cooperative_config.sh (33/33/33 + 1) donnent exactement la ventilation codée.
        let legacy: BTreeMap<String, String> = [("TREASURY_PERCENT", "33"), ("RND_PERCENT", "33"), ("ASSETS_PERCENT", "33"), ("CAPTAIN_BONUS_PERCENT", "1"), ("IS_RATE_REDUCED", "15.0"), ("IS_RATE_NORMAL", "25.0"), ("IS_THRESHOLD", "42500")].into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        assert_eq!(split_of(&legacy, base.clone()), base);
    }

    #[test]
    fn the_origin_relay_setting_is_a_wss_address_or_none() {
        assert!(validate("ORIGIN_RELAY", "wss://relay.copylaradio.com").is_ok());
        assert!(validate("ORIGIN_RELAY", "none").is_ok(), "none : ne rien annoncer hors de la constellation");
        for bad in ["https://relay.exemple.org", "wss://", "wss://relay exemple.org", "ws://relay.exemple.org", "wss://evil.example/x?y"] {
            assert!(validate("ORIGIN_RELAY", bad).is_err(), "{bad}");
        }
        assert!(!is_secret("ORIGIN_RELAY"), "en clair dans la configuration publiée");
        // Plusieurs relais : celui de la coopérative et celui qu'un Capitaine ajoute, une adresse par ligne.
        assert_eq!(parse_origin_relays("wss://relay.copylaradio.com\nwss://relai.chez-moi.org/, wss://relay.copylaradio.com").unwrap(), ["wss://relay.copylaradio.com", "wss://relai.chez-moi.org"]);
        assert_eq!(parse_origin_relays("none").unwrap(), Vec::<String>::new());
        assert!(parse_origin_relays("none wss://relay.copylaradio.com").is_err(), "none se suffit à lui-même");
        assert!(parse_origin_relays("wss://a.org wss://b.org wss://c.org wss://d.org wss://e.org").is_err(), "quatre au plus");
    }

    #[test]
    fn every_section_has_a_level_and_the_essentials_unlock_the_addresses() {
        assert!(SECTIONS.iter().all(|s| LEVELS.contains(&s.level)), "niveau inconnu");
        let level = |id: &str| SECTIONS.iter().find(|s| s.id == id).map(|s| s.level);
        assert_eq!(level("ovh"), Some("essential"), "le cockpit envoie ici pour activer les adresses : en tête d'écran");
        assert_eq!((level("fiscal"), level("3x13")), (Some("rules"), Some("rules")), "la fiscalité reste repliée");
        // Chaque clé connue est dans une seule section (l'écran n'en perd ni n'en double aucune).
        let all: Vec<&str> = SECTIONS.iter().flat_map(|s| s.keys.iter().copied()).collect();
        assert!(all.iter().all(|k| all.iter().filter(|x| *x == k).count() == 1), "clé en double");
        assert_eq!(all.len(), 43, "les 40 clés de la version précédente de l'écran et les 3 de « Monnaie et apparence », sans perte");
        assert!(LEVELS.iter().all(|l| SECTIONS.iter().any(|s| s.level == *l)), "aucun niveau vide");
    }

    #[test]
    fn relay_stations_are_direct_addresses_of_named_stations_in_clear() {
        let a = "/ip4/203.0.113.7/tcp/4101/p2p/12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG";
        let b = "/dns4/relais.exemple.org/tcp/4101/p2p/12D3KooWKnDdG3iXw9eTFijk3EWSunZcFi54Zka4wmtqtt6rPxc8";
        assert!(!is_secret("P2P_RELAYS"), "les adresses des relais doivent rester lisibles par toutes les stations");
        assert_eq!(parse_relays(&format!("{a}\n{b}, {a}")).unwrap().len(), 2, "doublon de station écarté");
        assert!(validate("P2P_RELAYS", &format!("{a}\n{b}")).is_ok());
        for bad in ["/ip4/203.0.113.7/tcp/4101", "relais.exemple.org:4101", "/ip4/203.0.113.7/udp/4101/quic-v1/p2p/12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG", &format!("{a}/p2p-circuit/p2p/12D3KooWKnDdG3iXw9eTFijk3EWSunZcFi54Zka4wmtqtt6rPxc8")] {
            assert!(parse_relays(bad).is_err(), "{bad}");
        }
        assert!(parse_relays(a).is_ok() && parse_relays("").unwrap().is_empty());
    }

    #[test]
    fn the_cooperative_key_is_the_one_of_uplanetname_g1() {
        let (secret, hex) = coop_keys(UP).unwrap();
        assert_eq!(astro_nostr_proto::pubkey_of(&secret).unwrap(), hex);
        assert_ne!(coop_keys("Autre").unwrap().1, hex);
        assert!(is_known("OCAPIKEY") && !is_known("ROOT_PASSWORD"));
        assert!(is_secret("OCAPIKEY") && is_secret("MJ_APIKEY_PUBLIC") && is_secret("GIT_TOKEN") && !is_secret("OCSLUG") && !is_secret("GIT_HOST") && !is_secret("TVA_RATE"));
    }

    #[test]
    fn values_are_validated_by_kind_of_field() {
        assert!(validate("TVA_RATE", "20.0").is_ok() && validate("PAF", "14").is_ok() && validate("TREASURY_PERCENT", "33").is_ok());
        for (k, v) in [("TVA_RATE", "vingt"), ("TVA_RATE", "-1"), ("TVA_RATE", "1.2.3"), ("TVA_RATE", ""), ("OC_URL_CLOUD", "http://x"), ("GIT_HOST", "ftp://x"), ("MJ_SENDER_EMAIL", "pas-un-mail"), ("OCSLUG", "a\nb")] {
            assert!(validate(k, v).is_err(), "{k}={v:?}");
        }
        assert!(validate("OCSLUG", "monnaie-libre").is_ok() && validate("OC_URL_CLOUD", "https://opencollective.com/x").is_ok());
    }

    #[tokio::test]
    async fn secrets_are_encrypted_plain_values_are_not_and_removal_and_replacement_work() {
        let (dir, db) = db("save").await;
        let publisher = Arc::new(Store(db.clone(), Mutex::new(Vec::new())));
        let (_, coop_hex) = coop_keys(UP).unwrap();
        assert!(load(&db, &coop_hex).await.is_none());
        let v = save(&db, &publisher, UP, &patch(&[("OCSLUG", Some("monnaie-libre")), ("OCAPIKEY", Some("sk_live_123")), ("TVA_RATE", Some("20.0"))])).await.unwrap();
        // Vue : le secret est renseigné mais jamais montré.
        let oc = v["sections"].as_array().unwrap().iter().find(|s| s["id"] == "oc").unwrap()["fields"].as_array().unwrap().clone();
        let field = |k: &str| oc.iter().find(|f| f["key"] == k).unwrap().clone();
        assert_eq!(field("OCSLUG")["value"], "monnaie-libre");
        assert_eq!((field("OCAPIKEY")["set"].clone(), field("OCAPIKEY")["readable"].clone()), (json!(true), json!(true)));
        assert!(field("OCAPIKEY").get("value").is_none(), "jamais de valeur secrète dans la vue");
        assert!(!v.to_string().contains("sk_live_123"));
        // Événement publié : valeur chiffrée au format du script legacy, signé par la clé coopérative.
        let ev = publisher.1.lock().unwrap()[0].clone();
        assert_eq!((ev.kind, ev.pubkey.as_str()), (KIND, coop_hex.as_str()));
        let content: BTreeMap<String, String> = serde_json::from_str(&ev.content).unwrap();
        assert_eq!(content["OCSLUG"], "monnaie-libre");
        assert!(coop::is_encrypted(&content["OCAPIKEY"]) && !ev.content.contains("sk_live_123"));
        assert_eq!(coop::decrypt(UP, &content["OCAPIKEY"]).unwrap(), "sk_live_123", "le legacy peut le relire");
        assert_eq!(content["COOPERATIVE_VERSION"], "1.0");
        // Remplacer et retirer : le remplaçable suivant gagne, les autres clés sont conservées.
        let v2 = save(&db, &publisher, UP, &patch(&[("OCAPIKEY", Some("sk_live_456")), ("TVA_RATE", None)])).await.unwrap();
        let stored = load(&db, &coop_hex).await.unwrap();
        assert_eq!(coop::decrypt(UP, &stored.values["OCAPIKEY"]).unwrap(), "sk_live_456");
        assert!(!stored.values.contains_key("TVA_RATE") && stored.values["OCSLUG"] == "monnaie-libre");
        assert!(v2["updated_at"].as_u64().unwrap() > ev.created_at, "la date du remplaçant dépasse celle du précédent");
        let audit: String = db.readers().read(|c| Ok(c.query_row("SELECT group_concat(data) FROM journal WHERE stream = 'coop_config'", [], |r| r.get(0))?)).await.unwrap();
        assert!(audit.contains("OCAPIKEY") && !audit.contains("sk_live"), "l'audit nomme les clés, jamais les valeurs : {audit}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn unknown_keys_bad_values_and_an_unreadable_secret_are_reported() {
        let (dir, db) = db("bad").await;
        let publisher = Arc::new(Store(db.clone(), Mutex::new(Vec::new())));
        assert!(save(&db, &publisher, UP, &patch(&[])).await.is_err());
        assert!(save(&db, &publisher, UP, &patch(&[("ROOT_PASSWORD", Some("x"))])).await.unwrap_err().contains("clé inconnue"));
        assert!(save(&db, &publisher, UP, &patch(&[("TVA_RATE", Some("vingt"))])).await.unwrap_err().contains("nombre"));
        assert!(publisher.1.lock().unwrap().is_empty(), "rien n'est publié sur une erreur");
        // Un secret chiffré par une AUTRE constellation reste visible comme renseigné mais « illisible ».
        let (_, coop_hex) = coop_keys(UP).unwrap();
        let foreign = Stored { event_id: "x".into(), created_at: 1, values: BTreeMap::from([("OCAPIKEY".to_owned(), coop::encrypt("Autre", "s").unwrap()), ("MA_CLE_A_MOI".to_owned(), "1".to_owned())]) };
        let v = view(Some(&foreign), UP, &coop_hex, &std::collections::BTreeSet::new());
        let oc = v["sections"].as_array().unwrap().iter().find(|s| s["id"] == "oc").unwrap()["fields"].as_array().unwrap().clone();
        let f = oc.iter().find(|f| f["key"] == "OCAPIKEY").unwrap();
        assert_eq!((f["set"].clone(), f["readable"].clone()), (json!(true), json!(false)));
        assert_eq!(v["unmanaged"], json!(["MA_CLE_A_MOI"]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_station_overrides_its_plantnet_key_without_publishing_it() {
        let (dir, db) = db("local").await;
        let publisher = Arc::new(Store(db.clone(), Mutex::new(Vec::new())));
        save(&db, &publisher, UP, &patch(&[("PLANTNET_API_KEY", Some("2b10coop"))])).await.unwrap();
        assert_eq!(setting(&db, UP, "PLANTNET_API_KEY").await.as_deref(), Some("2b10coop"));
        // La clé de la station passe avant, sans rien publier.
        let v = save(&db, &publisher, UP, &patch(&[("station:PLANTNET_API_KEY", Some("2b10station"))])).await.unwrap();
        assert_eq!(publisher.1.lock().unwrap().len(), 1, "une clé de station n'est jamais publiée");
        assert_eq!(setting(&db, UP, "PLANTNET_API_KEY").await.as_deref(), Some("2b10station"));
        let pn = v["sections"].as_array().unwrap().iter().find(|s| s["id"] == "plantnet").unwrap()["fields"][0].clone();
        assert_eq!((pn["set"].clone(), pn["station"].clone()), (json!(true), json!(true)));
        assert!(!v.to_string().contains("2b10station"), "la valeur ne revient jamais dans la vue");
        // Retirée : la coopérative reprend la main.
        save(&db, &publisher, UP, &patch(&[("station:PLANTNET_API_KEY", None)])).await.unwrap();
        assert_eq!(setting(&db, UP, "PLANTNET_API_KEY").await.as_deref(), Some("2b10coop"));
        // Seules les clés prévues se surdéfinissent ; une erreur n'écrit rien.
        assert!(save(&db, &publisher, UP, &patch(&[("station:OCAPIKEY", Some("x"))])).await.unwrap_err().contains("toute la coopérative"));
        assert!(save(&db, &publisher, UP, &patch(&[("station:PLANTNET_API_KEY", Some("a")), ("TVA_RATE", Some("vingt"))])).await.is_err());
        assert_eq!(setting(&db, UP, "PLANTNET_API_KEY").await.as_deref(), Some("2b10coop"), "rien d'écrit sur une erreur");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn origin_keeps_kraft_and_zen_a_private_uplanet_names_its_money() {
        let mine: BTreeMap<String, String> = [("UPLANET_NAME", "Pays Basque"), ("CURRENCY_NAME", "Sol"), ("CURRENCY_SYMBOL", "☉"), ("UI_STYLE", "sobre")].into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        assert_eq!(look_of(false, Some(&mine)), json!({ "origin": false, "name": "Pays Basque", "currency": "Sol", "symbol": "☉", "style": "sobre" }));
        // ORIGIN : sa configuration est signable par tous, elle ne change ni la monnaie ni le style.
        assert_eq!(look_of(true, Some(&mine)), json!({ "origin": true, "name": "ORIGIN", "currency": "Ẑen", "symbol": "Ẑ", "style": "kraft" }));
        // Rien de publié, ou une valeur invalide (Kraft demandé par une UPlanet privée) : les défauts.
        let bad: BTreeMap<String, String> = [("UI_STYLE", "kraft"), ("CURRENCY_SYMBOL", "EURO")].into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        assert_eq!(look_of(false, Some(&bad)), json!({ "origin": false, "name": null, "currency": "Ẑen", "symbol": "Ẑ", "style": null }));
        assert!(validate("UI_STYLE", "kraft").unwrap_err().contains("réservé à ORIGIN"));
        assert!(validate("CURRENCY_NAME", "Ẑen").is_ok() && validate("CURRENCY_NAME", "Zen2").is_err());
    }
}
