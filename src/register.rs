//! Inscription d'un MULTIPASS sur la station : depuis l'interface web (`/register`) ou la ligne de commande
//! (`astroportd multipass create`).
//!
//! [`Registrar::register`] enchaîne : limites (quota de comptes, débit par adresse IP) → création des clés et du
//! dossier du compte ([`astro_lifecycle::multipass`]) → base (compte hébergé + événements de naissance, en une
//! transaction) → publication sur les relais. Si la base refuse, le dossier du compte est retiré : jamais de compte à
//! moitié créé.
//!
//! L'inscription est **ouverte** par défaut (choix de la station) ; ses garde-fous sont réglables : `--max-accounts`,
//! `--register-per-hour`, et `--no-register` pour la fermer.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use astro_core::ActivityBus;
use astro_db::Db;
use astro_lifecycle::multipass::{self, MultipassError};
use astro_nostr_proto::Event;
use astro_sync::Projector;

/// Publication des événements de naissance (relai local et relais de la constellation) ; l'échec n'annule pas
/// l'inscription, il est dit sur le bus d'activité.
pub type PublishFn = Arc<dyn Fn(Event) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> + Send + Sync>;

/// Jeton de connexion web d'un compte (dérivé de la clé de la station).
pub type TokenFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Réglages de l'inscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// L'inscription est ouverte.
    pub open: bool,
    /// Nombre maximal de MULTIPASS hébergés ici (0 = illimité).
    pub max_accounts: u64,
    /// Inscriptions acceptées par adresse IP et par heure (0 = illimité).
    pub per_ip_per_hour: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { open: true, max_accounts: 500, per_ip_per_hour: 5 }
    }
}

/// Refus d'inscription.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegisterError {
    /// L'inscription est fermée sur cette station.
    #[error("l'inscription est fermée sur cette station")]
    Closed,
    /// Quota de comptes atteint.
    #[error("cette station a atteint son nombre maximal de MULTIPASS")]
    Full,
    /// Trop d'inscriptions depuis cette adresse.
    #[error("trop d'inscriptions depuis cette adresse : réessayez dans {0} min")]
    RateLimited(u64),
    /// Donnée refusée (email, secrets).
    #[error("{0}")]
    Invalid(String),
    /// Email ou clé déjà inscrits.
    #[error("{0}")]
    Conflict(String),
    /// Défaillance interne (détail dans les journaux).
    #[error("inscription impossible : {0}")]
    Internal(String),
}

/// Ce qui est remis au nouveau titulaire, **une seule fois**.
pub struct Registered {
    /// Compte créé.
    pub account: multipass::Created,
    /// Jeton de connexion web (avec l'email : identifiant Basic de l'interface et du WebDAV).
    pub token: String,
    /// `ORIGIN` ou `ẐEN`.
    pub constellation: &'static str,
    /// Compte invité : adresse générée par la station, pas un email réel.
    pub guest: bool,
}

/// Le service d'inscription.
pub struct Registrar {
    db: Arc<Db>,
    nostr_root: PathBuf,
    uplanet_name: String,
    captain_hex: Option<String>,
    relays: Vec<String>,
    station_domain: Option<String>,
    token_of: TokenFn,
    publish: Option<PublishFn>,
    activity: Option<ActivityBus>,
    limits: Limits,
    /// Heures d'inscription récentes par adresse (mémoire : une relance du démon remet les compteurs à zéro).
    recent: Mutex<HashMap<IpAddr, Vec<u64>>>,
    /// Une inscription à la fois : le quota et l'unicité se jugent sur un état stable.
    gate: tokio::sync::Mutex<()>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn create_error(e: MultipassError) -> RegisterError {
    match e {
        MultipassError::Exists(_) | MultipassError::IdentityConflict(_) => RegisterError::Conflict(e.to_string()),
        MultipassError::BadEmail(_) | MultipassError::BadSecrets => RegisterError::Invalid(e.to_string()),
        other => RegisterError::Internal(other.to_string()),
    }
}

/// Alias d'une adresse générée : minuscules ASCII sans accent, lettres et chiffres, 20 caractères au plus ; `membre` à défaut.
fn guest_alias(raw: Option<&str>) -> String {
    let mut out = String::new();
    for c in raw.unwrap_or("").chars().flat_map(char::to_lowercase) {
        let c = match c {
            'à' | 'â' | 'ä' | 'á' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' => 'i',
            'ô' | 'ö' | 'ó' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() && out.len() < 20 {
            out.push(c);
        }
    }
    if out.is_empty() { "membre".to_owned() } else { out }
}

/// 6 caractères aléatoires (minuscules et chiffres), tirés sans biais.
fn random_suffix() -> Result<String, getrandom::Error> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(6);
    let mut buf = [0u8; 32];
    while out.len() < 6 {
        getrandom::fill(&mut buf)?;
        // 36 caractères : on rejette au-delà de 252 (= 36 × 7) pour que chacun soit équiprobable.
        out.extend(buf.iter().filter(|b| **b < 252).map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char).take(6 - out.len()));
    }
    Ok(out)
}

impl Registrar {
    /// Service d'inscription de la constellation `uplanet_name`, dossiers de comptes `nostr_root`.
    pub fn new(db: Arc<Db>, nostr_root: PathBuf, uplanet_name: String, token_of: TokenFn, limits: Limits) -> Self {
        Self { db, nostr_root, uplanet_name, captain_hex: None, relays: Vec::new(), station_domain: None, token_of, publish: None, activity: None, limits, recent: Mutex::default(), gate: tokio::sync::Mutex::new(()) }
    }

    /// Clé du Capitaine, que chaque nouveau MULTIPASS suit.
    pub fn with_captain(mut self, hex: Option<String>) -> Self {
        self.captain_hex = hex;
        self
    }

    /// Relais annoncés (NIP-65, kind 10002) dans l'événement de naissance de chaque nouveau MULTIPASS.
    pub fn with_relays(mut self, relays: Vec<String>) -> Self {
        self.relays = relays;
        self
    }

    /// Domaine de la station (`--domain`) : repli pour les adresses générées si la configuration coopérative n'en donne pas.
    pub fn with_station_domain(mut self, domain: Option<String>) -> Self {
        self.station_domain = domain;
        self
    }

    /// Publication des événements de naissance.
    pub fn with_publisher(mut self, publish: PublishFn, activity: Option<ActivityBus>) -> Self {
        self.publish = Some(publish);
        self.activity = activity;
        self
    }

    /// `ORIGIN` ou `ẐEN`.
    pub fn constellation(&self) -> &'static str {
        multipass::constellation_label(&self.uplanet_name)
    }

    /// Réglages.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// MULTIPASS hébergés ici.
    pub async fn hosted_accounts(&self) -> Result<u64, RegisterError> {
        self.db
            .readers()
            .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM identities WHERE hosted = 1 AND role = 'multipass'", [], |r| r.get::<_, i64>(0))?))
            .await
            .map(|n| u64::try_from(n).unwrap_or(0))
            .map_err(|e| RegisterError::Internal(e.to_string()))
    }

    /// Limite de débit : au plus `per_ip_per_hour` inscriptions par adresse et par heure glissante.
    fn throttle(&self, ip: Option<IpAddr>, at: u64) -> Result<(), RegisterError> {
        let (Some(ip), true) = (ip, self.limits.per_ip_per_hour > 0) else { return Ok(()) };
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.retain(|_, v| {
            v.retain(|t| at.saturating_sub(*t) < 3_600);
            !v.is_empty()
        });
        let hits = recent.entry(ip).or_default();
        if hits.len() >= self.limits.per_ip_per_hour {
            let oldest = hits.iter().min().copied().unwrap_or(at);
            return Err(RegisterError::RateLimited((3_600 - at.saturating_sub(oldest)).div_ceil(60).max(1)));
        }
        hits.push(at);
        Ok(())
    }

    /// Crée un MULTIPASS. `ip` est l'adresse du client (absente en ligne de commande : pas de limite de débit).
    pub async fn register(&self, ip: Option<IpAddr>, email: &str, lang: &str) -> Result<Registered, RegisterError> {
        if !self.limits.open {
            return Err(RegisterError::Closed);
        }
        let email = multipass::normalize_email(email).map_err(|e| RegisterError::Invalid(e.to_string()))?;
        self.create(ip, vec![email], lang, false).await
    }

    /// Crée un MULTIPASS **invité** pour quelqu'un qui préfère ne pas donner son email : l'adresse `<alias>-<6 caractères>@<domaine>`
    /// est un identifiant UPlanet (nostr et IPFS) ; la station ne crée aucune boîte aux lettres (le domaine étant celui de la zone OVH, une messagerie SMTP/IMAP pourra la servir plus tard). L'unicité est contrôlée (nouveau tirage en cas de collision).
    pub async fn register_guest(&self, ip: Option<IpAddr>, alias: Option<&str>, lang: &str) -> Result<Registered, RegisterError> {
        if !self.limits.open {
            return Err(RegisterError::Closed);
        }
        let (domain, base) = (self.mail_domain().await, guest_alias(alias));
        let mut candidates = Vec::new();
        for _ in 0..5 {
            let email = format!("{base}-{}@{domain}", random_suffix().map_err(|e| RegisterError::Internal(e.to_string()))?);
            candidates.push(multipass::normalize_email(&email).map_err(|e| RegisterError::Internal(e.to_string()))?);
        }
        self.create(ip, candidates, lang, true).await
    }

    /// Domaine des adresses générées : la zone OVH (`OVH_ZONE`) de la configuration coopérative, sinon le domaine de la station
    /// (`--domain`), sinon `uplanet.local`.
    pub async fn mail_domain(&self) -> String {
        if let Some(d) = crate::coop_config::domain(&self.db, &self.uplanet_name).await {
            return d;
        }
        self.station_domain.clone().filter(|d| crate::coop_config::valid_domain(d)).unwrap_or_else(|| "uplanet.local".to_owned())
    }

    /// Crée le compte avec la première des adresses `emails` qui est libre.
    async fn create(&self, ip: Option<IpAddr>, emails: Vec<String>, lang: &str, guest: bool) -> Result<Registered, RegisterError> {
        let _one_at_a_time = self.gate.lock().await;
        if self.limits.max_accounts > 0 && self.hosted_accounts().await? >= self.limits.max_accounts {
            return Err(RegisterError::Full);
        }
        // Le débit est compté avant le travail coûteux (scrypt) : un client qui insiste n'use pas le processeur.
        let at = now();
        self.throttle(ip, at)?;

        let mut created = None;
        let last = emails.len().saturating_sub(1);
        for (i, email) in emails.into_iter().enumerate() {
            let (root, name, lang) = (self.nostr_root.clone(), self.uplanet_name.clone(), lang.to_owned());
            let result = tokio::task::spawn_blocking(move || multipass::create(&root, &email, &name, &lang)).await.map_err(|e| RegisterError::Internal(e.to_string()))?;
            match result {
                Ok(account) => {
                    created = Some(account);
                    break;
                }
                Err(MultipassError::Exists(_) | MultipassError::IdentityConflict(_)) if i < last => {}
                Err(e) => return Err(create_error(e)),
            }
        }
        let account = created.ok_or_else(|| RegisterError::Internal("aucune adresse libre".into()))?;

        let events = multipass::birth_events(&account, &self.uplanet_name, self.captain_hex.as_deref(), &self.relays, at).map_err(|e| RegisterError::Internal(e.to_string()))?;
        if let Err(e) = self.store(&account.hex, &events, guest).await {
            // Base refusée : on retire le dossier, l'adresse pourra se réinscrire.
            let _ = std::fs::remove_dir_all(&account.dir);
            return Err(if e.contains("UNIQUE") { RegisterError::Conflict(format!("{} a déjà un MULTIPASS", account.email)) } else { RegisterError::Internal(e) });
        }
        if let Some(publish) = self.publish.clone() {
            let activity = self.activity.clone();
            let who = account.email.clone();
            tokio::spawn(async move {
                for e in events {
                    if let Err(why) = publish(e).await {
                        if let Some(bus) = &activity {
                            bus.publish_to(astro_core::Audience::Captain, "alert", "register", format!("MULTIPASS {who} : événement de naissance non publié ({why})"), None);
                        }
                    }
                }
            });
        }
        if let Some(bus) = &self.activity {
            bus.publish_to(astro_core::Audience::Captain, "system", "register", format!("nouveau MULTIPASS {}{} ({})", account.email, if guest { " [invité]" } else { "" }, self.constellation()), None);
        }
        let token = (self.token_of)(&account.hex);
        Ok(Registered { account, token, constellation: self.constellation(), guest })
    }

    /// Compte hébergé, événements de naissance et leur projection : une transaction.
    async fn store(&self, hex: &str, events: &[Event], guest: bool) -> Result<(), String> {
        let (hex, events) = (hex.to_owned(), events.to_vec());
        self.db
            .writer()
            .transaction(move |tx| {
                astro_db::apply_statements(tx, &[Projector::mark_hosted(&hex, "multipass")])?;
                // Un compte (re)créé repart à la génération 0 du jeton (celle que `token_of` calcule).
                tx.execute("DELETE FROM kv WHERE scope = 'token_epoch' AND key = ?1", [&hex])?;
                if guest {
                    tx.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES ('guest', ?1, '1', ?2)", rusqlite::params![hex, i64::try_from(now()).unwrap_or(0)])?;
                }
                for e in &events {
                    astro_db::events::insert_event(tx, e)?;
                    astro_db::apply_statements(tx, &Projector::plan(e).map_err(|err| astro_db::DbError::Corrupt(err.to_string()))?)?;
                }
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(tag: &str, limits: Limits) -> (PathBuf, Arc<Db>, Registrar) {
        let dir = std::env::temp_dir().join(format!("astroportd-register-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let r = Registrar::new(db.clone(), dir.join("nostr"), "0".repeat(64), Arc::new(|hex| format!("jeton-{}", &hex[..8])), limits).with_captain(Some("cd".repeat(32)));
        (dir, db, r)
    }

    #[tokio::test]
    async fn a_registration_creates_keys_hosts_the_account_and_projects_its_birth() {
        let (dir, db, r) = setup("ok", Limits::default());
        let reg = r.register(None, "Alice@Exemple.org", "fr").await.unwrap();
        assert_eq!((reg.constellation, reg.account.email.as_str()), ("ORIGIN", "alice@exemple.org"));
        assert_eq!(reg.token, format!("jeton-{}", &reg.account.hex[..8]));
        assert!(dir.join("nostr/alice@exemple.org/.secret.nostr").is_file());
        let hex = reg.account.hex.clone();
        let (row, follows, events): ((String, i64, Option<String>), i64, i64) = db
            .readers()
            .read(move |c| {
                let row = c.query_row("SELECT role, hosted, email FROM identities WHERE hex = ?1", [&hex], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                let follows = c.query_row("SELECT COUNT(*) FROM social_graph WHERE owner_hex = ?1 AND level = 1", [&hex], |r| r.get(0))?;
                let events = c.query_row("SELECT COUNT(*) FROM nostr_events WHERE pubkey = ?1", [&hex], |r| r.get(0))?;
                Ok((row, follows, events))
            })
            .await
            .unwrap();
        assert_eq!(row, ("multipass".to_owned(), 1, Some("alice@exemple.org".to_owned())));
        assert_eq!((follows, events), (1, 3), "suit le Capitaine ; profil + contacts + DID stockés");
        assert_eq!(r.hosted_accounts().await.unwrap(), 1);
        // Même email (autre casse) : refusé, rien de plus n'est créé.
        assert!(matches!(r.register(None, "ALICE@exemple.org", "fr").await, Err(RegisterError::Conflict(_))));
        assert_eq!(std::fs::read_dir(dir.join("nostr")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_station_can_close_cap_and_throttle_registrations() {
        let (dir, _db, closed) = setup("closed", Limits { open: false, ..Limits::default() });
        assert_eq!(closed.register(None, "a@b.co", "fr").await.err(), Some(RegisterError::Closed));
        assert!(!dir.join("nostr").exists());

        let (_d2, _db2, capped) = setup("cap", Limits { max_accounts: 1, ..Limits::default() });
        capped.register(None, "un@exemple.org", "fr").await.unwrap();
        assert_eq!(capped.register(None, "deux@exemple.org", "fr").await.err(), Some(RegisterError::Full));

        let (_d3, _db3, slow) = setup("rate", Limits { per_ip_per_hour: 2, ..Limits::default() });
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        slow.register(Some(ip), "a@exemple.org", "fr").await.unwrap();
        slow.register(Some(ip), "b@exemple.org", "fr").await.unwrap();
        assert!(matches!(slow.register(Some(ip), "c@exemple.org", "fr").await, Err(RegisterError::RateLimited(m)) if (1..=60).contains(&m)));
        // Une autre adresse n'est pas pénalisée ; la ligne de commande (sans IP) non plus.
        slow.register(Some("203.0.113.8".parse().unwrap()), "d@exemple.org", "fr").await.unwrap();
        slow.register(None, "e@exemple.org", "fr").await.unwrap();
        // Un email invalide est refusé avant tout travail et ne consomme pas le débit.
        assert!(matches!(slow.register(Some("203.0.113.9".parse().unwrap()), "../etc@x.y", "fr").await, Err(RegisterError::Invalid(_))));
        for d in [dir, _d2, _d3] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[tokio::test]
    async fn a_database_failure_leaves_no_half_created_account() {
        let (dir, db, r) = setup("rollback", Limits::default());
        // Le writer est arrêté : la transaction échoue après la création des fichiers.
        db.writer().shutdown().await.unwrap();
        let err = r.register(None, "z@exemple.org", "fr").await.err();
        assert!(matches!(err, Some(RegisterError::Internal(_))), "{err:?}");
        assert!(!dir.join("nostr/z@exemple.org").exists(), "le dossier du compte est retiré");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[tokio::test]
    async fn a_guest_gets_a_unique_generated_address_on_the_ovh_zone_then_the_station_domain() {
        let (dir, db, r) = setup("guest", Limits::default());
        let r = r.with_station_domain(Some("station.exemple.org".into()));
        // Sans zone OVH : le domaine de la station.
        let a = r.register_guest(None, Some("Léa Martin!"), "fr").await.unwrap();
        assert!(a.guest && a.account.email.starts_with("leamartin-") && a.account.email.ends_with("@station.exemple.org"), "{}", a.account.email);
        let suffix = a.account.email.trim_start_matches("leamartin-").trim_end_matches("@station.exemple.org");
        assert!(suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()), "{suffix}");
        assert_eq!(a.account.pass.len(), 6, "PASS à 6 chiffres pour un compte neuf");
        // Avec la zone OVH de la configuration coopérative : elle l'emporte.
        let (secret, hex) = crate::coop_config::coop_keys(&"0".repeat(64)).unwrap();
        let ev = astro_nostr_proto::UnsignedEvent { created_at: 10, kind: crate::coop_config::KIND, tags: vec![vec!["d".into(), crate::coop_config::D_TAG.into()]], content: r#"{"OVH_ZONE":"CopylaRadio.com"}"#.into() }.sign(&secret).unwrap();
        db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &ev).map(|_| ())).await.unwrap();
        let _ = hex;
        let b = r.register_guest(None, None, "fr").await.unwrap();
        assert!(b.account.email.starts_with("membre-") && b.account.email.ends_with("@copylaradio.com"), "{}", b.account.email);
        assert_ne!(a.account.email, b.account.email);
        // Marqué « invité » en base ; un compte à email réel ne l'est pas.
        let real = r.register(None, "vrai@exemple.org", "fr").await.unwrap();
        assert!(!real.guest);
        let (g, h) = (a.account.hex.clone(), real.account.hex.clone());
        let flags: (i64, i64) = db.readers().read(move |c| Ok((c.query_row("SELECT COUNT(*) FROM kv WHERE scope = 'guest' AND key = ?1", [&g], |r| r.get(0))?, c.query_row("SELECT COUNT(*) FROM kv WHERE scope = 'guest' AND key = ?1", [&h], |r| r.get(0))?))).await.unwrap();
        assert_eq!(flags, (1, 0));
        // Un domaine invalide dans la configuration est ignoré (repli sur la station) ; sans aucun domaine : uplanet.local.
        assert!(crate::coop_config::valid_domain("copylaradio.com") && !crate::coop_config::valid_domain("pas un domaine") && !crate::coop_config::valid_domain("localhost"));
        let (_d2, _db2, bare) = setup("guest-bare", Limits::default());
        assert!(bare.register_guest(None, None, "fr").await.unwrap().account.email.ends_with("@uplanet.local"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn one_email_is_one_multipass_even_if_the_folder_check_were_bypassed() {
        let (dir, db, r) = setup("unique", Limits::default());
        r.register(None, "Pierre@Exemple.org", "fr").await.unwrap();
        // L'index unique de la base refuse un second MULTIPASS hébergé de même adresse (casse comprise)…
        let dup = db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'PIERRE@exemple.org', 1)", vec!["ee".repeat(32).into()]).await;
        assert!(dup.is_err(), "doublon refusé");
        // …mais la ZenCard ou le Capitaine du même membre peuvent porter la même adresse.
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'zencard', 'pierre@exemple.org', 1)", vec!["ff".repeat(32).into()]).await.unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
