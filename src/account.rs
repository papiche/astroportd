//! « Mon compte » : identité et clés publiques du MULTIPASS, profil Nostr, changement du jeton WebDAV, export des clés.
//!
//! La station détient les clés de ses comptes hébergés : modifier le profil, c'est signer un nouveau kind 0 pour le
//! compte. L'export des clés secrètes est une action délibérée (confirmation explicite, quelques essais par heure,
//! trace dans le journal d'audit et dans l'activité du titulaire, jamais mise en cache).

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_db::Db;
use astro_nostr_proto::{UnsignedEvent, kind, relay_list};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::register::PublishFn;

/// Échec d'une opération de compte, avec la nature qui fixe le code HTTP.
#[derive(Debug, PartialEq, Eq)]
pub enum AccountError {
    /// Saisie refusée (message pour l'utilisateur).
    Invalid(String),
    /// Compte inconnu ici.
    NotFound,
    /// Trop d'essais.
    Limited(String),
    /// Identifiants refusés (PASS faux, compte inconnu).
    Denied(String),
    /// Panne de la station (détail pour le journal).
    Internal(String),
}

impl std::fmt::Display for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Limited(m) | Self::Denied(m) | Self::Internal(m) => f.write_str(m),
            Self::NotFound => f.write_str("compte non hébergé ici"),
        }
    }
}

fn internal(e: impl std::fmt::Display) -> AccountError {
    AccountError::Internal(e.to_string())
}

/// Langues proposées par l'interface.
pub const LANGS: [&str; 3] = ["fr", "en", "es"];
/// Exports de clés autorisés par heure et par compte.
const EXPORTS_PER_HOUR: usize = 3;
/// Échecs consécutifs du PASS avant le premier verrouillage.
pub const PASS_LOCK_AFTER: u32 = 5;
/// Premier verrouillage (secondes) ; il double à chaque échec suivant, jusqu'à [`PASS_LOCK_MAX`].
const PASS_LOCK_BASE: u64 = 900;
/// Plafond du verrouillage : 24 h.
const PASS_LOCK_MAX: u64 = 86_400;
/// Une erreur isolée s'oublie après ce délai sans nouvel échec.
const PASS_FORGET_AFTER: u64 = 86_400;
/// Tentatives de PASS par adresse IP et par quart d'heure (tous comptes confondus).
const UNLOCK_PER_IP: usize = 30;
/// Signatures par compte et par minute.
const SIGNS_PER_MINUTE: usize = 120;

/// Service « Mon compte ».
pub struct AccountService {
    db: Arc<Db>,
    nostr_root: PathBuf,
    keys: Arc<dyn KeySource>,
    station_secret: Vec<u8>,
    publish: PublishFn,
    public_base: Option<String>,
    exports: Mutex<HashMap<String, Vec<u64>>>,
    activity: Option<ActivityBus>,
    unlocks: Mutex<HashMap<IpAddr, Vec<u64>>>,
    signs: Mutex<HashMap<String, Vec<u64>>>,
    pass_gate: tokio::sync::Mutex<()>,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `npub1…` d'une clé x-only de 64 hex.
fn npub_of(hex: &str) -> Option<String> {
    let bytes: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect::<Option<_>>()?;
    bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("npub").ok()?, &bytes).ok()
}

/// Valide les champs de profil modifiables : `None` = inchangé, chaîne vide = retiré.
fn clean(field: &str, value: &Value, max: usize) -> Result<Option<String>, AccountError> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => {
            let s = s.trim();
            if s.chars().count() > max || s.chars().any(char::is_control) {
                return Err(AccountError::Invalid(format!("{field} : {max} caractères au plus, sans caractère de contrôle")));
            }
            Ok(Some(s.to_owned()))
        }
        _ => Err(AccountError::Invalid(format!("{field} : texte attendu"))),
    }
}

impl AccountService {
    /// `nostr_root` : dossier des comptes (`<racine>/<email>/.secret.nostr`).
    pub fn new(db: Arc<Db>, nostr_root: PathBuf, keys: Arc<dyn KeySource>, station_secret: Vec<u8>, publish: PublishFn, public_base: Option<String>) -> Self {
        Self { db, nostr_root, keys, station_secret, publish, public_base, exports: Mutex::default(), activity: None, unlocks: Mutex::default(), signs: Mutex::default(), pass_gate: tokio::sync::Mutex::new(()) }
    }

    /// Flux d'activité : le Capitaine et le titulaire sont prévenus d'un verrouillage du PASS.
    pub fn with_activity(mut self, bus: ActivityBus) -> Self {
        self.activity = Some(bus);
        self
    }

    async fn row(&self, hex: &str) -> Result<(String, Option<String>, Option<String>, Option<String>, Option<String>, String), AccountError> {
        let hex = hex.to_owned();
        self.db
            .readers()
            .read(move |c| {
                Ok(c.query_row("SELECT role, email, npub, g1pub_nostr, home_station, COALESCE(lang, 'fr') FROM identities WHERE hex = ?1 AND hosted = 1", [hex], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).optional()?)
            })
            .await
            .map_err(internal)?
            .ok_or(AccountError::NotFound)
    }

    async fn last_event(&self, hex: &str, kind: u32) -> Option<(String, String, i64)> {
        let hex = hex.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT content, tags, created_at FROM nostr_events WHERE pubkey = ?1 AND kind = ?2 ORDER BY created_at DESC LIMIT 1", rusqlite::params![hex, kind], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?)).await.ok().flatten()
    }

    fn dir_of(&self, email: &str) -> PathBuf {
        self.nostr_root.join(email)
    }

    /// Vue du compte : identité, profil, accès WebDAV, relais annoncés (NIP-65).
    pub async fn view(&self, hex: &str) -> Result<Value, AccountError> {
        let (role, email, npub, g1, home, lang) = self.row(hex).await?;
        let profile: Value = self.last_event(hex, kind::METADATA).await.and_then(|(c, ..)| serde_json::from_str(&c).ok()).unwrap_or_default();
        let relays = match self.last_event(hex, 10_002).await.and_then(|(_, t, _)| serde_json::from_str::<Vec<Vec<String>>>(&t).ok()) {
            Some(tags) => relay_list::parse(&tags).into_iter().map(|e| json!({ "url": e.url, "read": e.read, "write": e.write })).collect(),
            None => Vec::new(),
        };
        let epoch = astro_dav::auth::token_epoch(&self.db, hex).await;
        let login = email.clone().unwrap_or_else(|| hex.to_owned());
        let has_dunikey = email.as_deref().is_some_and(|e| self.dir_of(e).join(".secret.dunikey").is_file());
        Ok(json!({
            "hex": hex, "npub": npub.or_else(|| npub_of(hex)), "email": email, "role": role, "g1pub": g1, "home_station": home, "lang": lang, "langs": LANGS,
            "profile": { "name": profile["name"], "about": profile["about"], "picture": profile["picture"] },
            "dav": { "url": format!("{}/dav", self.public_base.as_deref().unwrap_or("").trim_end_matches('/')), "login": login },
            "guest": self.is_guest(hex).await, "token_generation": epoch, "keys_exportable": has_dunikey || email.is_some(), "has_g1_key": has_dunikey, "relays": relays,
        }))
    }

    /// Compte invité (adresse générée par la station) ?
    async fn is_guest(&self, hex: &str) -> bool {
        let h = hex.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM kv WHERE scope = 'guest' AND key = ?1", [h], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false)
    }

    /// Limite par adresse IP : au plus [`UNLOCK_PER_IP`] tentatives par quart d'heure.
    fn throttle_unlock(&self, ip: Option<IpAddr>, at: u64) -> Result<(), AccountError> {
        let Some(ip) = ip else { return Ok(()) };
        let mut map = self.unlocks.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, v| {
            v.retain(|t| at.saturating_sub(*t) < 900);
            !v.is_empty()
        });
        let hits = map.entry(ip).or_default();
        if hits.len() >= UNLOCK_PER_IP {
            return Err(AccountError::Limited("trop de tentatives depuis votre réseau : réessayez dans quelques minutes".into()));
        }
        hits.push(at);
        Ok(())
    }

    async fn guard_state(&self, hex: &str) -> (u32, u64, u64) {
        let h = hex.to_owned();
        let raw: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'pass_guard' AND key = ?1", [h], |r| r.get(0)).optional()?)).await.ok().flatten();
        let v: Value = raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
        (v["fails"].as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0), v["last"].as_u64().unwrap_or(0), v["until"].as_u64().unwrap_or(0))
    }

    async fn save_guard(&self, hex: &str, state: Option<(u32, u64, u64)>) -> Result<(), AccountError> {
        let h = hex.to_owned();
        match state {
            None => self.db.writer().execute("DELETE FROM kv WHERE scope = 'pass_guard' AND key = ?1", vec![h.into()]).await,
            Some((fails, last, until)) => {
                self.db
                    .writer()
                    .execute(
                        "INSERT INTO kv(scope, key, value, updated_at) VALUES ('pass_guard', ?1, ?2, ?3) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                        vec![h.into(), json!({ "fails": fails, "last": last, "until": until }).to_string().into(), i64::try_from(last).unwrap_or(0).into()],
                    )
                    .await
            }
        }
        .map(|_| ())
        .map_err(internal)
    }

    /// Instant jusqu'auquel le PASS du compte est verrouillé (secondes UNIX), s'il l'est.
    pub async fn pass_locked_until(&self, hex: &str) -> Option<u64> {
        let (_, _, until) = self.guard_state(hex).await;
        (until > now()).then_some(until)
    }

    /// Le Capitaine remet à zéro le compteur d'échecs d'un compte.
    pub async fn clear_pass_lock(&self, hex: &str) -> Result<Value, AccountError> {
        let _ = self.row(hex).await?;
        let _gate = self.pass_gate.lock().await;
        self.save_guard(hex, None).await?;
        Ok(json!({ "unlocked": true }))
    }

    /// Connexion par **PASS** (4 à 6 chiffres) : rend le jeton de session du compte, comme le ferait la saisie du jeton.
    ///
    /// Le PASS n'a que 10⁴ à 10⁶ valeurs : sa seule défense est la limite d'essais **de la station**. Après [`PASS_LOCK_AFTER`]
    /// échecs consécutifs le compte est verrouillé 15 min, puis le délai double à chaque nouvel échec (24 h au plus) ; un succès
    /// remet tout à zéro. Le PASS n'est jamais détruit par des échecs : n'importe qui connaissant une adresse pourrait sinon
    /// l'invalider à distance. Le Capitaine et le titulaire sont prévenus au verrouillage ; le Capitaine peut le lever.
    pub async fn unlock(&self, ip: Option<IpAddr>, email: &str, pass: &str) -> Result<Value, AccountError> {
        let email = email.trim().to_ascii_lowercase();
        if !(4..=6).contains(&pass.len()) || !pass.bytes().all(|b| b.is_ascii_digit()) {
            return Err(AccountError::Invalid("code PASS : 4 à 6 chiffres".into()));
        }
        let at = now();
        self.throttle_unlock(ip, at)?;
        let e = email.clone();
        let hex: Option<String> = self
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT hex FROM identities WHERE hosted = 1 AND lower(email) = ?1 AND role IN ('multipass', 'captain') ORDER BY (role = 'multipass') DESC LIMIT 1", [e], |r| r.get(0)).optional()?))
            .await
            .map_err(internal)?;
        let denied = || AccountError::Denied("identifiant ou code PASS incorrect".into());
        let Some(hex) = hex else {
            // Même travail, même réponse : un compte inconnu ne se distingue pas d'un PASS faux.
            let _ = astro_dav::auth::ct_eq(pass.as_bytes(), b"000000");
            return Err(denied());
        };
        let _gate = self.pass_gate.lock().await;
        let (mut fails, last, until) = self.guard_state(&hex).await;
        if until > at {
            return Err(AccountError::Limited(format!("compte verrouillé après trop d'essais : réessayez dans {} min", (until - at).div_ceil(60))));
        }
        if fails > 0 && fails < PASS_LOCK_AFTER && at.saturating_sub(last) > PASS_FORGET_AFTER {
            fails = 0;
        }
        let stored = std::fs::read_to_string(self.dir_of(&email).join(".pass")).map_err(|_| AccountError::Invalid("ce compte n'a pas de code PASS : connectez-vous avec votre jeton".into()))?;
        if astro_dav::auth::ct_eq(pass.as_bytes(), stored.trim().as_bytes()) {
            self.save_guard(&hex, None).await?;
            let token = astro_dav::auth::account_token(&self.db, &self.station_secret, &hex).await;
            let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('audit.unlock', ?1, ?2)", vec![i64::try_from(at).unwrap_or(0).into(), json!({ "hex": hex, "ip": ip.map(|i| i.to_string()) }).to_string().into()]).await;
            return Ok(json!({ "email": email, "hex": hex, "token": token, "guest": self.is_guest(&hex).await }));
        }
        fails += 1;
        if fails < PASS_LOCK_AFTER {
            self.save_guard(&hex, Some((fails, at, 0))).await?;
            return Err(AccountError::Denied(format!("code PASS incorrect : {} essai(s) restant(s) avant verrouillage", PASS_LOCK_AFTER - fails)));
        }
        let secs = (PASS_LOCK_BASE << (fails - PASS_LOCK_AFTER).min(7)).min(PASS_LOCK_MAX);
        self.save_guard(&hex, Some((fails, at, at + secs))).await?;
        let minutes = secs.div_ceil(60);
        if let Some(bus) = &self.activity {
            let from = ip.map_or_else(|| "adresse inconnue".to_owned(), |i| i.to_string());
            bus.publish_to(Audience::Captain, "alert", "pass", format!("PASS de {email} verrouillé {minutes} min après {fails} codes faux consécutifs (dernier essai depuis {from})"), None);
            bus.publish_to(Audience::Owner(hex.clone()), "alert", "pass", format!("Des codes PASS incorrects ont été saisis pour votre compte ({fails} essais) : connexion par PASS suspendue {minutes} min"), None);
        }
        Err(AccountError::Limited(format!("trop d'essais : compte verrouillé {minutes} min")))
    }

    /// Signe, avec la clé du compte, l'événement non signé `{ kind, content, tags?, created_at? }` : la clé secrète ne quitte
    /// jamais la station (signataire distant). Refusés : les kinds qui engagent l'identité coopérative ou le grand livre, les
    /// enveloppes NIP-59 (le client ne sait pas les chiffrer) et les réactions qui paient (`+N`).
    pub async fn sign(&self, hex: &str, ev: &Value) -> Result<Value, AccountError> {
        let at = now();
        {
            let mut map = self.signs.lock().unwrap_or_else(|e| e.into_inner());
            let hits = map.entry(hex.to_owned()).or_default();
            hits.retain(|t| at.saturating_sub(*t) < 60);
            if hits.len() >= SIGNS_PER_MINUTE {
                return Err(AccountError::Limited("trop de signatures : réessayez dans une minute".into()));
            }
            hits.push(at);
        }
        let kind = ev["kind"].as_u64().and_then(|k| u32::try_from(k).ok()).filter(|k| *k <= 65_535).ok_or_else(|| AccountError::Invalid("kind : entier de 0 à 65535 attendu".into()))?;
        let content = match &ev["content"] {
            Value::String(c) => c.clone(),
            Value::Null => String::new(),
            _ => return Err(AccountError::Invalid("content : texte attendu".into())),
        };
        if content.len() > 65_536 {
            return Err(AccountError::Invalid("content : 64 Ko au plus".into()));
        }
        let tags: Vec<Vec<String>> = match &ev["tags"] {
            Value::Null => Vec::new(),
            t => serde_json::from_value(t.clone()).map_err(|_| AccountError::Invalid("tags : liste de listes de textes attendue".into()))?,
        };
        if tags.len() > 500 || tags.iter().any(|t| t.is_empty() || t.len() > 16 || t.iter().any(|x| x.len() > 2048)) {
            return Err(AccountError::Invalid("tags : 500 au plus, chacun de 1 à 16 éléments de 2 Ko au plus".into()));
        }
        let created_at = match &ev["created_at"] {
            Value::Null => at,
            v => v.as_u64().filter(|t| t.abs_diff(at) <= 86_400).ok_or_else(|| AccountError::Invalid("created_at : à moins de 24 h de maintenant".into()))?,
        };
        let payment = kind == 7 && content.strip_prefix('+').is_some_and(|rest| rest.bytes().next().is_some_and(|b| b.is_ascii_digit()));
        if matches!(kind, 30_800 | 30_852 | 1059 | 13) || payment {
            return Err(AccountError::Invalid(format!("kind {kind} : signature refusée par la station{}", if payment { " (une réaction « +N » déclenche un paiement)" } else { "" })));
        }
        let secret = self.keys.secret(hex).ok_or_else(|| internal("clé du compte introuvable sur cette station"))?;
        let event = UnsignedEvent { created_at, kind, tags, content }.sign(&secret).map_err(internal)?;
        let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('audit.sign', ?1, ?2)", vec![i64::try_from(at).unwrap_or(0).into(), json!({ "hex": hex, "kind": kind }).to_string().into()]).await;
        serde_json::to_value(&event).map_err(internal)
    }

    /// Met à jour le profil : `{ name?, about?, picture?, lang? }` ; signe et publie un nouveau kind 0 (les autres champs sont gardés).
    pub async fn update_profile(&self, hex: &str, patch: &Value) -> Result<Value, AccountError> {
        let (_, email, ..) = self.row(hex).await?;
        let name = clean("nom", &patch["name"], 64)?;
        let about = clean("présentation", &patch["about"], 500)?;
        let picture = clean("image", &patch["picture"], 256)?;
        if name.as_deref() == Some("") {
            return Err(AccountError::Invalid("nom : ne peut pas être vide".into()));
        }
        if picture.as_deref().is_some_and(|p| !p.is_empty() && !p.starts_with("https://")) {
            return Err(AccountError::Invalid("image : adresse https:// attendue".into()));
        }
        let lang = match &patch["lang"] {
            Value::Null => None,
            Value::String(l) if LANGS.contains(&l.as_str()) => Some(l.clone()),
            _ => return Err(AccountError::Invalid(format!("langue : {} attendue", LANGS.join(", ")))),
        };
        if name.is_none() && about.is_none() && picture.is_none() && lang.is_none() {
            return Err(AccountError::Invalid("rien à modifier".into()));
        }
        if name.is_some() || about.is_some() || picture.is_some() {
            let secret = self.keys.secret(hex).ok_or_else(|| internal("clé du compte introuvable sur cette station"))?;
            let (mut content, tags, previous) = match self.last_event(hex, kind::METADATA).await {
                Some((c, t, at)) => (serde_json::from_str::<Value>(&c).unwrap_or_else(|_| json!({})), serde_json::from_str::<Vec<Vec<String>>>(&t).unwrap_or_default(), at),
                None => (json!({}), Vec::new(), 0),
            };
            for (key, value) in [("name", name), ("about", about), ("picture", picture)] {
                match value {
                    Some(v) if v.is_empty() => {
                        content.as_object_mut().map(|o| o.remove(key));
                    }
                    Some(v) => content[key] = json!(v),
                    None => {}
                }
            }
            // Remplaçable : l'horodatage doit dépasser celui du profil précédent.
            let at = now().max(u64::try_from(previous).unwrap_or(0) + 1);
            let event = UnsignedEvent { created_at: at, kind: kind::METADATA, tags, content: content.to_string() }.sign(&secret).map_err(internal)?;
            (self.publish)(event).await.map_err(|e| AccountError::Internal(format!("profil non publié : {e}")))?;
        }
        if let Some(lang) = lang {
            let h = hex.to_owned();
            let l = lang.clone();
            self.db.writer().execute("UPDATE identities SET lang = ?2 WHERE hex = ?1", vec![h.into(), l.into()]).await.map_err(internal)?;
            if let Some(email) = &email {
                std::fs::write(self.dir_of(email).join("LANG"), format!("{lang}\n")).map_err(|e| internal(format!("LANG : {e}")))?;
            }
        }
        self.view(hex).await
    }

    /// Change le jeton WebDAV : l'ancien cesse de fonctionner tout de suite ; le nouveau n'est montré qu'ici.
    pub async fn rotate_token(&self, hex: &str) -> Result<Value, AccountError> {
        let (_, email, ..) = self.row(hex).await?;
        let token = astro_dav::auth::rotate_token(&self.db, &self.station_secret, hex).await.map_err(internal)?;
        self.audit("audit.token", json!({ "by": hex, "for": hex, "rotated": true })).await;
        Ok(json!({ "login": email.unwrap_or_else(|| hex.to_owned()), "token": token, "generation": astro_dav::auth::token_epoch(&self.db, hex).await }))
    }

    /// Exporte les clés secrètes (Nostr `nsec`, clé Ğ1 au format `.dunikey`) ; `confirm` doit valoir `true`.
    pub async fn export_keys(&self, hex: &str, confirm: bool) -> Result<Value, AccountError> {
        if !confirm {
            return Err(AccountError::Invalid("confirmation attendue : cet export donne le contrôle total du compte".into()));
        }
        let (_, email, ..) = self.row(hex).await?;
        let email = email.ok_or_else(|| AccountError::Invalid("compte sans dossier de clés sur cette station".into()))?;
        {
            let mut log = self.exports.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let hits = log.entry(hex.to_owned()).or_default();
            hits.retain(|t| now().saturating_sub(*t) < 3_600);
            if hits.len() >= EXPORTS_PER_HOUR {
                return Err(AccountError::Limited(format!("{EXPORTS_PER_HOUR} exports par heure au plus : réessayez plus tard")));
            }
            hits.push(now());
        }
        let dir = self.dir_of(&email);
        let nostr = std::fs::read_to_string(dir.join(".secret.nostr")).map_err(|e| internal(format!(".secret.nostr : {e}")))?;
        let nsec = nostr.split(';').find_map(|p| p.trim().strip_prefix("NSEC=")).map(|v| v.trim().to_owned()).ok_or_else(|| internal(".secret.nostr : NSEC absent"))?;
        // Sentinelle visible plutôt que champ absent : un compte sans clé Ğ1 le dit.
        let dunikey = std::fs::read_to_string(dir.join(".secret.dunikey")).map_err(|e| format!("ERROR:{e}"));
        self.audit("audit.keys", json!({ "for": hex })).await;
        Ok(json!({ "nsec": nsec, "npub": npub_of(hex), "dunikey": dunikey.as_ref().ok(), "dunikey_error": dunikey.err() }))
    }

    async fn audit(&self, stream: &'static str, data: Value) {
        let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES (?1, ?2, ?3)", vec![stream.to_owned().into(), (now() as i64).into(), data.to_string().into()]).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astro_crypto::StaticKeySource;

    const SECRET: [u8; 32] = [0x42; 32];

    async fn setup(tag: &str) -> (std::path::PathBuf, AccountService, String, Arc<Mutex<Vec<astro_nostr_proto::Event>>>) {
        let dir = std::env::temp_dir().join(format!("astroportd-account-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nostr/ana@example.org")).unwrap();
        let hex = astro_nostr_proto::pubkey_of(&SECRET).unwrap();
        let nsec = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), &SECRET).unwrap();
        std::fs::write(dir.join("nostr/ana@example.org/.secret.nostr"), format!("NSEC={nsec}; NPUB=x; HEX={hex}\n")).unwrap();
        std::fs::write(dir.join("nostr/ana@example.org/.secret.dunikey"), "pub: abc\nsec: def\n").unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'ana@example.org', 1)", vec![hex.clone().into()]).await.unwrap();
        let sent: Arc<Mutex<Vec<astro_nostr_proto::Event>>> = Arc::default();
        let (db2, sent2) = (db.clone(), sent.clone());
        let publish: PublishFn = Arc::new(move |e| {
            let (db, sent) = (db2.clone(), sent2.clone());
            Box::pin(async move {
                sent.lock().unwrap().push(e.clone());
                db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &e).map(|_| ())).await.map_err(|e| e.to_string())
            })
        });
        let svc = AccountService::new(db, dir.join("nostr"), Arc::new(StaticKeySource::new().with(SECRET)), b"secret-station".to_vec(), publish, Some("https://station.example".into()));
        (dir, svc, hex, sent)
    }

    #[tokio::test]
    async fn profile_edits_sign_a_new_kind_0_and_keep_other_fields() {
        let (dir, svc, hex, sent) = setup("profile").await;
        let v = svc.view(&hex).await.unwrap();
        assert_eq!((v["email"].as_str(), v["dav"]["url"].as_str(), v["lang"].as_str()), (Some("ana@example.org"), Some("https://station.example/dav"), Some("fr")));
        assert!(v["npub"].as_str().unwrap().starts_with("npub1"));
        let v = svc.update_profile(&hex, &json!({ "name": "Ana", "about": "Bonjour", "lang": "es" })).await.unwrap();
        assert_eq!((v["profile"]["name"].as_str(), v["profile"]["about"].as_str(), v["lang"].as_str()), (Some("Ana"), Some("Bonjour"), Some("es")));
        assert_eq!(std::fs::read_to_string(dir.join("nostr/ana@example.org/LANG")).unwrap(), "es\n");
        let first = sent.lock().unwrap()[0].clone();
        assert!(first.verify().is_ok() && first.pubkey == hex && first.kind == 0);
        // Un second changement garde le premier et dépasse l'horodatage (remplaçable).
        let v = svc.update_profile(&hex, &json!({ "about": "" })).await.unwrap();
        assert_eq!((v["profile"]["name"].as_str(), v["profile"]["about"].is_null()), (Some("Ana"), true));
        assert!(sent.lock().unwrap()[1].created_at > first.created_at);
        for bad in [json!({}), json!({ "name": "" }), json!({ "name": "x".repeat(65) }), json!({ "picture": "http://x" }), json!({ "lang": "xx" }), json!({ "about": 3 }), json!({ "name": "a\u{0}b" })] {
            assert!(matches!(svc.update_profile(&hex, &bad).await, Err(AccountError::Invalid(_))), "{bad}");
        }
        assert!(matches!(svc.update_profile(&"cd".repeat(32), &json!({ "name": "X" })).await, Err(AccountError::NotFound)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn rotating_the_token_changes_what_the_station_expects() {
        let (dir, svc, hex, _) = setup("token").await;
        let before = astro_dav::auth::account_token(&svc.db, b"secret-station", &hex).await;
        let r = svc.rotate_token(&hex).await.unwrap();
        let after = astro_dav::auth::account_token(&svc.db, b"secret-station", &hex).await;
        assert_ne!(before, after);
        assert_eq!((r["token"].as_str(), r["generation"].as_u64(), r["login"].as_str()), (Some(after.as_str()), Some(1), Some("ana@example.org")));
        assert_eq!(svc.view(&hex).await.unwrap()["token_generation"], 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn key_export_needs_confirmation_is_audited_and_rate_limited() {
        let (dir, svc, hex, _) = setup("keys").await;
        assert!(matches!(svc.export_keys(&hex, false).await, Err(AccountError::Invalid(_))));
        let k = svc.export_keys(&hex, true).await.unwrap();
        assert!(k["nsec"].as_str().unwrap().starts_with("nsec1") && k["dunikey"].as_str().unwrap().contains("sec: def"));
        assert_eq!(k["npub"], svc.view(&hex).await.unwrap()["npub"]);
        let audited: i64 = svc.db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM journal WHERE stream = 'audit.keys'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(audited, 1);
        for _ in 0..2 {
            svc.export_keys(&hex, true).await.unwrap();
        }
        assert!(matches!(svc.export_keys(&hex, true).await, Err(AccountError::Limited(_))), "quatrième export dans l'heure");
        // Clé Ğ1 absente : dit explicitement, sans cacher l'erreur.
        std::fs::remove_file(dir.join("nostr/ana@example.org/.secret.dunikey")).unwrap();
        let (_, svc2, hex2, _) = setup("keys2").await;
        std::fs::remove_file(svc2.nostr_root.join("ana@example.org/.secret.dunikey")).unwrap();
        let k = svc2.export_keys(&hex2, true).await.unwrap();
        assert!(k["dunikey"].is_null() && k["dunikey_error"].as_str().unwrap().starts_with("ERROR:"), "{k}");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[tokio::test]
    async fn the_pass_unlocks_with_a_progressive_lock_that_never_destroys_it() {
        let (dir, svc, hex, _) = setup("pass").await;
        std::fs::write(dir.join("nostr/ana@example.org/.pass"), "123456\n").unwrap();
        let svc = svc.with_activity(ActivityBus::new());
        // Format refusé avant tout essai ; compte inconnu = même réponse qu'un PASS faux.
        assert!(matches!(svc.unlock(None, "ana@example.org", "12ab").await, Err(AccountError::Invalid(_))));
        let unknown = svc.unlock(None, "nobody@example.org", "123456").await.unwrap_err();
        let faux = svc.unlock(None, "ana@example.org", "000000").await.unwrap_err();
        assert!(matches!(&unknown, AccountError::Denied(m) if m.contains("incorrect")) && matches!(&faux, AccountError::Denied(m) if m.contains("4 essai(s)")), "{unknown:?} {faux:?}");
        // 4 échecs : toujours des essais restants ; le 5ᵉ verrouille, le bon code lui-même est alors refusé.
        for left in [3, 2, 1] {
            assert!(matches!(svc.unlock(None, "Ana@Example.org", "000000").await, Err(AccountError::Denied(m)) if m.contains(&format!("{left} essai"))));
        }
        let locked = svc.unlock(None, "ana@example.org", "000000").await.unwrap_err();
        assert!(matches!(&locked, AccountError::Limited(m) if m.contains("15 min")), "{locked:?}");
        assert!(matches!(svc.unlock(None, "ana@example.org", "123456").await, Err(AccountError::Limited(_))), "verrouillé : même le bon code");
        assert!(svc.pass_locked_until(&hex).await.is_some());
        // Le PASS n'est pas détruit : le Capitaine lève le verrou et le bon code fonctionne.
        assert!(dir.join("nostr/ana@example.org/.pass").is_file());
        svc.clear_pass_lock(&hex).await.unwrap();
        let ok = svc.unlock(None, "ana@example.org", "123456").await.unwrap();
        assert_eq!(ok["token"], astro_dav::auth::account_token(&svc.db, b"secret-station", &hex).await);
        assert_eq!((ok["hex"].as_str(), ok["guest"].as_bool()), (Some(hex.as_str()), Some(false)));
        // Un succès remet le compteur à zéro.
        assert!(matches!(svc.unlock(None, "ana@example.org", "000000").await, Err(AccountError::Denied(m)) if m.contains("4 essai")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_lock_doubles_after_each_new_failure_and_is_capped_at_a_day() {
        let (dir, svc, hex, _) = setup("backoff").await;
        std::fs::write(dir.join("nostr/ana@example.org/.pass"), "123456\n").unwrap();
        for _ in 0..5 {
            let _ = svc.unlock(None, "ana@example.org", "000000").await;
        }
        let first = svc.pass_locked_until(&hex).await.unwrap() - now();
        assert!((890..=900).contains(&first), "{first}");
        // Le délai écoulé, un seul nouvel échec reverrouille, deux fois plus longtemps.
        svc.db.writer().execute("UPDATE kv SET value = json_set(value, '$.until', 1) WHERE scope = 'pass_guard'", vec![]).await.unwrap();
        let _ = svc.unlock(None, "ana@example.org", "000000").await;
        let second = svc.pass_locked_until(&hex).await.unwrap() - now();
        assert!((1790..=1800).contains(&second), "{second}");
        for _ in 0..12 {
            svc.db.writer().execute("UPDATE kv SET value = json_set(value, '$.until', 1) WHERE scope = 'pass_guard'", vec![]).await.unwrap();
            let _ = svc.unlock(None, "ana@example.org", "000000").await;
        }
        assert!(svc.pass_locked_until(&hex).await.unwrap() - now() <= 86_400, "plafonné à 24 h");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_station_signs_for_the_account_and_refuses_what_commits_the_cooperative() {
        let (dir, svc, hex, _) = setup("sign").await;
        let ev = svc.sign(&hex, &json!({ "kind": 27235, "tags": [["u", "https://x.example/api"], ["method", "GET"]], "content": "" })).await.unwrap();
        assert_eq!((ev["pubkey"].as_str(), ev["kind"].as_u64()), (Some(hex.as_str()), Some(27235)));
        let parsed: astro_nostr_proto::Event = serde_json::from_value(ev).unwrap();
        assert!(parsed.verify().is_ok(), "id et signature valides");
        for bad in [
            json!({ "kind": 30800, "content": "{}" }),
            json!({ "kind": 30852, "content": "" }),
            json!({ "kind": 1059, "content": "x" }),
            json!({ "kind": 7, "content": "+50" }),
            json!({ "kind": 70000 }),
            json!({ "kind": 1, "content": 5 }),
            json!({ "kind": 1, "tags": [[]] }),
            json!({ "kind": 1, "created_at": 1 }),
        ] {
            assert!(matches!(svc.sign(&hex, &bad).await, Err(AccountError::Invalid(_))), "{bad}");
        }
        // Une réaction ordinaire passe ; la limite par minute protège la clé.
        svc.sign(&hex, &json!({ "kind": 7, "content": "+" })).await.unwrap();
        let mut refused = false;
        for _ in 0..SIGNS_PER_MINUTE {
            refused |= matches!(svc.sign(&hex, &json!({ "kind": 1, "content": "x" })).await, Err(AccountError::Limited(_)));
        }
        assert!(refused);
        let _ = std::fs::remove_dir_all(dir);
    }
}
