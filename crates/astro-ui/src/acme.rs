//! Certificats TLS automatiques (ACME, défi HTTP-01) : Let's Encrypt, ZeroSSL ou toute autorité ACME.
//!
//! Le compte et les certificats sont persistés dans `kv` (scope `acme`) ; le renouvellement est automatique
//! (30 jours avant l'expiration par défaut) et le nouveau certificat est installé à chaud dans le
//! [`CertResolver`] : les connexions en cours ne sont pas coupées.
//!
//! `rustls-acme` n'implémente que TLS-ALPN-01 ; le défi HTTP-01 demandé (port 80, avec redirection vers HTTPS)
//! passe donc par `instant-acme`, qui laisse en outre le choix de l'autorité et du stockage.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwapOption;
use astro_db::Db;
use instant_acme::{Account, AccountCredentials, AuthorizationStatus, ChallengeType, ExternalAccountKey, HttpClient, Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// Annuaire de production de Let's Encrypt.
pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// Annuaire de test de Let's Encrypt (certificats non reconnus, quotas larges).
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
/// Annuaire de ZeroSSL (exige un compte à liaison externe, voir [`AcmeConfig::eab`]).
pub const ZEROSSL: &str = "https://acme.zerossl.com/v2/DV90";

const SCOPE: &str = "acme";

/// Erreurs ACME.
#[derive(Debug, thiserror::Error)]
pub enum AcmeError {
    /// Protocole ACME.
    #[error("ACME : {0}")]
    Protocol(String),
    /// Le défi n'a pas été validé.
    #[error("validation refusée pour {0}")]
    Validation(String),
    /// Certificat ou clé illisible.
    #[error("certificat invalide : {0}")]
    Certificate(String),
    /// Stockage.
    #[error("stockage : {0}")]
    Store(String),
}

impl From<instant_acme::Error> for AcmeError {
    fn from(e: instant_acme::Error) -> Self {
        Self::Protocol(e.to_string())
    }
}

/// Réglages ACME.
#[derive(Clone)]
pub struct AcmeConfig {
    /// Noms de domaine du certificat (le premier est le principal).
    pub domains: Vec<String>,
    /// Email de contact du compte.
    pub email: Option<String>,
    /// Annuaire ACME.
    pub directory_url: String,
    /// Renouveler quand il reste moins que cette durée.
    pub renew_before: Duration,
    /// Période de vérification en tâche de fond.
    pub check_every: Duration,
    /// Liaison de compte externe `(kid, clé HMAC en octets)` : ZeroSSL et autres autorités à compte préalable.
    pub eab: Option<(String, Vec<u8>)>,
}

impl AcmeConfig {
    /// Let's Encrypt de production, renouvellement 30 jours avant expiration, vérification toutes les 12 h.
    pub fn new(domains: Vec<String>, email: Option<String>) -> Self {
        Self { domains, email, directory_url: LETS_ENCRYPT.to_owned(), renew_before: Duration::from_secs(30 * 24 * 3600), check_every: Duration::from_secs(12 * 3600), eab: None }
    }
}

/// Réponses aux défis HTTP-01 en cours : `jeton → autorisation de clé`.
#[derive(Clone, Default)]
pub struct ChallengeStore {
    inner: Arc<Mutex<HashMap<String, String>>>,
}

impl ChallengeStore {
    /// Magasin vide.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enregistre la réponse d'un défi.
    pub fn insert(&self, token: String, key_authorization: String) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).insert(token, key_authorization);
    }

    /// Réponse attendue pour `token`.
    pub fn get(&self, token: &str) -> Option<String> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).get(token).cloned()
    }

    /// Retire un défi terminé.
    pub fn remove(&self, token: &str) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).remove(token);
    }
}

/// Certificat courant, remplaçable à chaud.
#[derive(Debug, Default)]
pub struct CertResolver {
    current: ArcSwapOption<CertifiedKey>,
}

impl CertResolver {
    /// Résolveur sans certificat (les poignées de main échouent jusqu'à la première émission).
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Installe un nouveau certificat sans interrompre les connexions existantes.
    pub fn install(&self, key: CertifiedKey) {
        self.current.store(Some(Arc::new(key)));
    }

    /// Un certificat est-il installé ?
    pub fn has_certificate(&self) -> bool {
        self.current.load().is_some()
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current.load_full()
    }
}

/// Certificat émis, tel que persisté.
#[derive(Clone, Serialize, Deserialize)]
pub struct CertBundle {
    /// Chaîne PEM (feuille d'abord).
    pub chain_pem: String,
    /// Clé privée PEM (PKCS#8).
    pub key_pem: String,
    /// Fin de validité, secondes UNIX.
    pub not_after: i64,
    /// Noms couverts.
    pub domains: Vec<String>,
}

impl CertBundle {
    /// Construit la clé rustls correspondante.
    pub fn to_certified_key(&self) -> Result<CertifiedKey, AcmeError> {
        let certs = CertificateDer::pem_slice_iter(self.chain_pem.as_bytes()).collect::<Result<Vec<_>, _>>().map_err(|e| AcmeError::Certificate(e.to_string()))?;
        let key = PrivateKeyDer::from_pem_slice(self.key_pem.as_bytes()).map_err(|e| AcmeError::Certificate(e.to_string()))?;
        let provider = CryptoProvider::get_default().cloned().unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
        CertifiedKey::from_der(certs, key, &provider).map_err(|e| AcmeError::Certificate(e.to_string()))
    }

    fn from_pems(chain_pem: String, key_pem: String, domains: Vec<String>) -> Result<Self, AcmeError> {
        let leaf = CertificateDer::pem_slice_iter(chain_pem.as_bytes()).next().ok_or_else(|| AcmeError::Certificate("chaîne vide".into()))?.map_err(|e| AcmeError::Certificate(e.to_string()))?;
        let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref()).map_err(|e| AcmeError::Certificate(e.to_string()))?;
        Ok(Self { chain_pem, key_pem, not_after: parsed.validity().not_after.timestamp(), domains })
    }
}

/// Stockage persistant du compte et des certificats (`kv`, scope `acme`).
#[derive(Clone)]
pub struct KvCertStore {
    db: Arc<Db>,
}

impl KvCertStore {
    /// Stockage sur la base de la station.
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    async fn get(&self, key: &str) -> Result<Option<String>, AcmeError> {
        let key = key.to_owned();
        self.db
            .readers()
            .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SCOPE, key], |r| r.get(0)))?))
            .await
            .map_err(|e| AcmeError::Store(e.to_string()))
    }

    async fn put(&self, key: &str, value: String) -> Result<(), AcmeError> {
        self.db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![SCOPE.to_owned().into(), key.to_owned().into(), value.into(), now().into()],
            )
            .await
            .map(|_| ())
            .map_err(|e| AcmeError::Store(e.to_string()))
    }

    /// Dernier certificat connu pour ce jeu de domaines.
    pub async fn load_cert(&self, domains: &[String]) -> Result<Option<CertBundle>, AcmeError> {
        Ok(self.get(&cert_key(domains)).await?.and_then(|v| serde_json::from_str(&v).ok()))
    }

    async fn save_cert(&self, bundle: &CertBundle) -> Result<(), AcmeError> {
        self.put(&cert_key(&bundle.domains), serde_json::to_string(bundle).map_err(|e| AcmeError::Store(e.to_string()))?).await
    }

    async fn load_account(&self, directory: &str) -> Result<Option<AccountCredentials>, AcmeError> {
        Ok(self.get(&format!("account:{directory}")).await?.and_then(|v| serde_json::from_str(&v).ok()))
    }

    async fn save_account(&self, directory: &str, creds: &AccountCredentials) -> Result<(), AcmeError> {
        self.put(&format!("account:{directory}"), serde_json::to_string(creds).map_err(|e| AcmeError::Store(e.to_string()))?).await
    }
}

/// Certificats stockés dans la base `conn` : `(noms couverts, fin de validité en secondes UNIX)`.
///
/// Lecture seule et synchrone, pour `astroportd doctor` (qui ne doit rien modifier ni démarrer le démon).
pub fn stored_certs(conn: &rusqlite::Connection) -> Result<Vec<(Vec<String>, i64)>, rusqlite::Error> {
    let mut stmt = conn.prepare("SELECT value FROM kv WHERE scope = ?1 AND key LIKE 'cert:%' ORDER BY key")?;
    let rows = stmt.query_map([SCOPE], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(Result::ok).filter_map(|v| serde_json::from_str::<CertBundle>(&v).ok()).map(|b| (b.domains, b.not_after)).collect())
}

fn cert_key(domains: &[String]) -> String {
    let mut d: Vec<String> = domains.iter().map(|s| s.to_ascii_lowercase()).collect();
    d.sort();
    format!("cert:{}", d.join(","))
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// État observable du gestionnaire de certificats.
#[derive(Debug, Default, Clone)]
pub struct AcmeStatus {
    /// Certificats émis depuis le démarrage.
    pub issued: u32,
    /// Fin de validité du certificat installé.
    pub not_after: Option<i64>,
    /// Dernière erreur.
    pub last_error: Option<String>,
}

/// Gestionnaire : obtient, persiste, installe et renouvelle le certificat.
pub struct AcmeManager {
    /// Réglages.
    pub config: AcmeConfig,
    store: KvCertStore,
    /// Défis HTTP-01 à servir sur le port 80.
    pub challenges: ChallengeStore,
    /// Certificat servi par le serveur TLS.
    pub resolver: Arc<CertResolver>,
    /// État.
    pub status: Arc<Mutex<AcmeStatus>>,
    http: Mutex<Option<Box<dyn HttpClient>>>,
}

impl AcmeManager {
    /// Crée le gestionnaire.
    pub fn new(config: AcmeConfig, store: KvCertStore) -> Arc<Self> {
        Self::with_challenges(config, store, ChallengeStore::new())
    }

    /// Crée un gestionnaire qui publie ses défis dans un magasin existant (celui que sert déjà le port 80).
    pub fn with_challenges(config: AcmeConfig, store: KvCertStore, challenges: ChallengeStore) -> Arc<Self> {
        Arc::new(Self { config, store, challenges, resolver: CertResolver::new(), status: Arc::default(), http: Mutex::new(None) })
    }

    /// Client HTTP personnalisé pour joindre l'annuaire (autorité de test en clair, racine privée).
    pub fn with_http_client(self: Arc<Self>, client: Box<dyn HttpClient>) -> Arc<Self> {
        *self.http.lock().unwrap_or_else(|e| e.into_inner()) = Some(client);
        self
    }

    fn set_error(&self, e: Option<String>) {
        self.status.lock().unwrap_or_else(|p| p.into_inner()).last_error = e;
    }

    async fn account(&self) -> Result<Account, AcmeError> {
        let builder = |m: &Self| -> Result<instant_acme::AccountBuilder, AcmeError> {
            match m.http.lock().unwrap_or_else(|e| e.into_inner()).take() {
                Some(client) => Ok(Account::builder_with_http(client)),
                None => Ok(Account::builder()?),
            }
        };
        if let Some(creds) = self.store.load_account(&self.config.directory_url).await? {
            return Ok(builder(self)?.from_credentials(creds).await?);
        }
        let contact = self.config.email.as_ref().map(|e| format!("mailto:{e}"));
        let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
        let eab = self.config.eab.as_ref().map(|(kid, key)| ExternalAccountKey::new(kid.clone(), key));
        let (account, creds) = builder(self)?
            .create(&NewAccount { contact: &contacts, terms_of_service_agreed: true, only_return_existing: false }, self.config.directory_url.clone(), eab.as_ref())
            .await?;
        self.store.save_account(&self.config.directory_url, &creds).await?;
        Ok(account)
    }

    /// Obtient un nouveau certificat (défi HTTP-01 servi par le port 80) et le persiste.
    pub async fn obtain(&self) -> Result<CertBundle, AcmeError> {
        let account = self.account().await?;
        let identifiers: Vec<Identifier> = self.config.domains.iter().map(|d| Identifier::Dns(d.clone())).collect();
        let mut order = account.new_order(&NewOrder::new(&identifiers)).await?;
        let mut tokens = Vec::new();
        {
            let mut authorizations = order.authorizations();
            while let Some(result) = authorizations.next().await {
                let mut authz = result?;
                match authz.status {
                    AuthorizationStatus::Pending => {}
                    AuthorizationStatus::Valid => continue,
                    other => return Err(AcmeError::Validation(format!("autorisation {other:?}"))),
                }
                let mut challenge = authz.challenge(ChallengeType::Http01).ok_or_else(|| AcmeError::Protocol("l'autorité ne propose pas HTTP-01".into()))?;
                let (token, key_auth) = (challenge.token.clone(), challenge.key_authorization().as_str().to_owned());
                self.challenges.insert(token.clone(), key_auth);
                tokens.push(token);
                challenge.set_ready().await?;
            }
        }
        let outcome = async {
            let status = order.poll_ready(&RetryPolicy::default()).await?;
            if status != OrderStatus::Ready {
                return Err(AcmeError::Validation(format!("commande {status:?}")));
            }
            let key_pem = order.finalize().await?;
            let chain_pem = order.poll_certificate(&RetryPolicy::default()).await?;
            Ok::<_, AcmeError>((chain_pem, key_pem))
        }
        .await;
        for t in &tokens {
            self.challenges.remove(t);
        }
        let (chain_pem, key_pem) = outcome?;
        let bundle = CertBundle::from_pems(chain_pem, key_pem, self.config.domains.clone())?;
        self.store.save_cert(&bundle).await?;
        Ok(bundle)
    }

    fn install(&self, bundle: &CertBundle) -> Result<(), AcmeError> {
        self.resolver.install(bundle.to_certified_key()?);
        self.status.lock().unwrap_or_else(|p| p.into_inner()).not_after = Some(bundle.not_after);
        Ok(())
    }

    /// Force une nouvelle émission et l'installe à chaud (renouvellement anticipé, changement de domaines…).
    pub async fn renew_now(&self) -> Result<(), AcmeError> {
        let bundle = self.obtain().await?;
        self.install(&bundle)?;
        self.status.lock().unwrap_or_else(|p| p.into_inner()).issued += 1;
        Ok(())
    }

    /// Un passage : installe le certificat en cache s'il est encore valable assez longtemps, sinon en obtient un.
    /// Retourne `true` si un nouveau certificat a été émis.
    pub async fn ensure(&self) -> Result<bool, AcmeError> {
        let renew_at = now().saturating_add(i64::try_from(self.config.renew_before.as_secs()).unwrap_or(i64::MAX));
        if let Some(cached) = self.store.load_cert(&self.config.domains).await? {
            if cached.not_after > renew_at {
                if !self.resolver.has_certificate() {
                    self.install(&cached)?;
                }
                return Ok(false);
            }
        }
        let bundle = self.obtain().await?;
        self.install(&bundle)?;
        self.status.lock().unwrap_or_else(|p| p.into_inner()).issued += 1;
        Ok(true)
    }

    /// Tâche de fond : première émission, puis vérification périodique, avec reprise espacée en cas d'échec.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut failures = 0u32;
        loop {
            let wait = match self.ensure().await {
                Ok(_) => {
                    failures = 0;
                    self.set_error(None);
                    self.config.check_every
                }
                Err(e) => {
                    failures += 1;
                    self.set_error(Some(e.to_string()));
                    // Reprise espacée (5 min, 10, 20… plafonnée à 6 h) : ne jamais marteler une autorité.
                    Duration::from_secs((300u64 << failures.min(6)).min(6 * 3600))
                }
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    }
}
