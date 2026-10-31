//! Agents autorisés à demander l'accès aux comptes d'un membre, et serveur MCP avec demande d'accord.
//!
//! - **Registre** : un événement kind 31903 `d = agents`, signé de la clé du membre, dont le contenu est chiffré pour lui seul
//!   (NIP-44 vers lui-même). Il garde, par agent, un nom, l'empreinte SHA-256 du jeton (jamais le jeton), le groupe du coffre
//!   que l'agent peut atteindre, la durée maximale d'un accès et le droit de lire un code TOTP. Le relai en fait un cache
//!   rejouable (`kv`, portée `agents`) ; la station le relit et le déchiffre avec la clé du membre qu'elle héberge.
//! - **Jeton** : `ag1_<16 premiers hex du compte>_<64 hex aléatoires>`, montré une seule fois. Seule son empreinte est gardée.
//! - **MCP** (JSON-RPC 2.0 sur `POST /mcp`, jeton en `Bearer`) : `demander_acces` et `statut_acces`. Un agent ne reçoit **jamais**
//!   un secret : il reçoit « en attente », « accordé », « refusé » ou « expiré ». Le membre décide dans l'interface.
//! - **Ce que ce module ne fait pas encore** : ouvrir `Coffre/agents.kdbx` et remettre l'identifiant à un navigateur. Un accord
//!   accordé n'est qu'une autorisation datée, tracée dans le journal (`audit.agent`) et dans l'activité du membre.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use astro_core::{ActivityBus, Audience};
use astro_crypto::{KeySource, nip44};
use astro_db::Db;
use astro_nostr_proto::{UnsignedEvent, kind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::register::PublishFn;

/// `d`-tag du registre (kind 31903).
pub const D_TAG: &str = "agents";
/// Portée `kv` du cache rejouable du registre.
pub const KV_SCOPE: &str = "agents";
const PREFIX: &str = "ag1_";
/// Agents déclarés au plus par membre.
const MAX_AGENTS: usize = 8;
/// Demandes en attente au plus par membre (au-delà : refus, pour ne pas inonder le titulaire).
const MAX_PENDING: usize = 5;
/// Demandes par agent et par heure.
const ASKS_PER_HOUR: usize = 20;
/// Une demande non traitée expire au bout de 5 minutes.
const REQUEST_TTL_SECS: u64 = 300;
/// Durée maximale d'un accord, quoi que dise le registre.
pub const MAX_GRANT_SECS: u64 = 3600;
const SESSION_TOOLS: &str = "2024-11-05";

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Échec d'une opération sur les agents.
#[derive(Debug, PartialEq, Eq)]
pub enum AgentError {
    /// Saisie refusée.
    Invalid(String),
    /// Inconnu ici.
    NotFound,
    /// Trop de demandes.
    Limited(String),
    /// Jeton absent ou faux.
    Unauthorized,
    /// Panne de la station.
    Internal(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Limited(m) | Self::Internal(m) => f.write_str(m),
            Self::NotFound => f.write_str("introuvable"),
            Self::Unauthorized => f.write_str("jeton d'agent refusé"),
        }
    }
}

/// Un agent du registre (les empreintes ne sortent jamais d'ici).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRecord {
    /// Identifiant court (8 hex).
    pub id: String,
    /// Nom donné par le membre (« BRO », « Claude Code »…).
    pub name: String,
    /// SHA-256 du jeton, en hexadécimal.
    pub token_sha256: String,
    /// Groupe du coffre `agents.kdbx` que l'agent peut atteindre.
    pub group: String,
    /// Durée maximale d'un accord, en secondes.
    pub max_secs: u64,
    /// L'agent peut demander un code TOTP.
    pub totp: bool,
    /// Création (secondes Unix).
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Registry {
    agents: Vec<AgentRecord>,
}

/// État d'une demande d'accès.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Le membre n'a pas répondu.
    Pending,
    /// Accordée jusqu'à `until`.
    Granted,
    /// Refusée.
    Denied,
}

#[derive(Debug, Clone)]
struct Request {
    owner: String,
    agent_id: String,
    agent_name: String,
    site: String,
    reason: String,
    created: u64,
    state: State,
    until: u64,
}

impl Request {
    /// État vu de l'extérieur : une demande ignorée ou un accord échu devient « expiré ».
    fn label(&self, at: u64) -> &'static str {
        match self.state {
            State::Pending if at >= self.created + REQUEST_TTL_SECS => "expired",
            State::Pending => "pending",
            State::Granted if at >= self.until => "expired",
            State::Granted => "granted",
            State::Denied => "denied",
        }
    }

    fn view(&self, id: &str, at: u64) -> Value {
        json!({ "id": id, "agent": self.agent_name, "site": self.site, "reason": self.reason, "state": self.label(at), "asked_at": self.created, "until": (self.state == State::Granted).then_some(self.until) })
    }
}

/// Un nom d'hôte seul : pas de schéma, de port, de chemin ni d'identifiants. Rend l'hôte en minuscules.
///
/// # Errors
/// [`AgentError::Invalid`] si `site` n'est pas un nom d'hôte DNS.
pub fn valid_host(site: &str) -> Result<String, AgentError> {
    let host = site.trim().trim_end_matches('.').to_ascii_lowercase();
    let label_ok = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if host.len() > 253 || !host.contains('.') || !host.split('.').all(label_ok) || host.split('.').all(|l| l.bytes().all(|b| b.is_ascii_digit())) {
        return Err(AgentError::Invalid("nom de site attendu seul (exemple : example.org), sans http://, sans chemin ni port".into()));
    }
    Ok(host)
}

fn sha_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Registre des agents et file des demandes d'accord.
pub struct Agents {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    publish: PublishFn,
    activity: Option<ActivityBus>,
    requests: Mutex<HashMap<String, Request>>,
    asks: Mutex<HashMap<String, Vec<u64>>>,
}

impl Agents {
    /// Service de registre sur `db`, signant avec les clés de `keys` et publiant par `publish`.
    #[must_use]
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, publish: PublishFn) -> Self {
        Self { db, keys, publish, activity: None, requests: Mutex::new(HashMap::new()), asks: Mutex::new(HashMap::new()) }
    }

    /// Dit les demandes sur le bus d'activité du titulaire.
    #[must_use]
    pub fn with_activity(mut self, bus: ActivityBus) -> Self {
        self.activity = Some(bus);
        self
    }

    async fn audit(&self, data: Value) {
        let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('audit.agent', ?1, ?2)", vec![i64::try_from(now()).unwrap_or(0).into(), data.to_string().into()]).await;
    }

    /// Registre du membre et horodatage de sa dernière version (0 : jamais écrit).
    async fn load_stamped(&self, owner: &str) -> Result<(Registry, u64), AgentError> {
        let o = owner.to_owned();
        let stored: Option<(String, i64)> = self
            .db
            .readers()
            .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value, updated_at FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![KV_SCOPE, o], |r| Ok((r.get(0)?, r.get(1)?))))?))
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
        let Some((payload, stamp)) = stored else { return Ok((Registry::default(), 0)) };
        let secret = self.keys.secret(owner).ok_or_else(|| AgentError::Internal("clé du compte absente de cette station".into()))?;
        let plain = nip44::decrypt_from_self(&secret, owner, &payload).map_err(|e| AgentError::Internal(format!("registre illisible : {e}")))?;
        let reg = serde_json::from_str(&plain).map_err(|e| AgentError::Internal(format!("registre illisible : {e}")))?;
        Ok((reg, u64::try_from(stamp).unwrap_or(0)))
    }

    async fn load(&self, owner: &str) -> Result<Registry, AgentError> {
        self.load_stamped(owner).await.map(|(r, _)| r)
    }

    /// Signe, publie puis range le registre. L'horodatage dépasse toujours celui de la version précédente : un relai garde le
    /// plus récent d'un événement remplaçable, et deux enregistrements dans la même seconde ne doivent pas se départager au hasard.
    async fn save(&self, owner: &str, reg: &Registry, previous: u64) -> Result<(), AgentError> {
        let secret = self.keys.secret(owner).ok_or_else(|| AgentError::Internal("clé du compte absente de cette station".into()))?;
        let plain = serde_json::to_string(reg).map_err(|e| AgentError::Internal(e.to_string()))?;
        let content = nip44::encrypt_to_self(&secret, owner, &plain).map_err(|e| AgentError::Internal(e.to_string()))?;
        let at = now().max(previous + 1);
        let event = UnsignedEvent { created_at: at, kind: kind::APP_PARAMS, tags: vec![vec!["d".into(), D_TAG.into()]], content: content.clone() }.sign(&secret).map_err(|e| AgentError::Internal(e.to_string()))?;
        (self.publish)(event).await.map_err(|e| AgentError::Internal(format!("registre non publié : {e}")))?;
        // Le projecteur remplit le même cache à son rythme (et le reconstruit au `replay`) ; on l'écrit aussi ici pour qu'une
        // lecture juste après l'enregistrement voie déjà la nouvelle version.
        self.db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at WHERE excluded.updated_at >= kv.updated_at",
                vec![KV_SCOPE.to_owned().into(), owner.to_owned().into(), content.into(), i64::try_from(at).unwrap_or(0).into()],
            )
            .await
            .map(|_| ())
            .map_err(|e| AgentError::Internal(e.to_string()))
    }

    /// Agents du membre, sans empreinte.
    ///
    /// # Errors
    /// Base ou clé indisponibles.
    pub async fn list(&self, owner: &str) -> Result<Value, AgentError> {
        let reg = self.load(owner).await?;
        Ok(json!({ "agents": reg.agents.iter().map(|a| json!({ "id": a.id, "name": a.name, "group": a.group, "max_secs": a.max_secs, "totp": a.totp, "created_at": a.created_at })).collect::<Vec<_>>() }))
    }

    /// Déclare un agent et rend son jeton, **montré cette seule fois**.
    ///
    /// # Errors
    /// Saisie invalide, trop d'agents, ou registre non publié.
    pub async fn enroll(&self, owner: &str, name: &str, group: &str, max_secs: u64, totp: bool) -> Result<Value, AgentError> {
        let name = name.trim();
        let group = group.trim();
        if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
            return Err(AgentError::Invalid("nom de 1 à 40 caractères attendu".into()));
        }
        if group.is_empty() || group.chars().count() > 60 || group.chars().any(char::is_control) {
            return Err(AgentError::Invalid("groupe du coffre de 1 à 60 caractères attendu".into()));
        }
        let (mut reg, stamp) = self.load_stamped(owner).await?;
        if reg.agents.len() >= MAX_AGENTS {
            return Err(AgentError::Limited(format!("{MAX_AGENTS} agents au plus : en retirer un d'abord")));
        }
        let mut raw = [0u8; 36];
        getrandom::fill(&mut raw).map_err(|e| AgentError::Internal(format!("aléa du système indisponible : {e}")))?;
        let id = hex::encode(&raw[..4]);
        let token = format!("{PREFIX}{}_{}", owner.get(..16).unwrap_or(owner), hex::encode(&raw[4..]));
        reg.agents.push(AgentRecord { id: id.clone(), name: name.to_owned(), token_sha256: sha_hex(&token), group: group.to_owned(), max_secs: max_secs.clamp(30, MAX_GRANT_SECS), totp, created_at: now() });
        self.save(owner, &reg, stamp).await?;
        self.audit(json!({ "event": "enroll", "hex": owner, "agent": id, "name": name })).await;
        Ok(json!({ "id": id, "name": name, "token": token, "note": "Ce jeton ne sera plus jamais affiché : copiez-le maintenant dans la configuration de l'agent." }))
    }

    /// Retire un agent : son jeton cesse de fonctionner et ses demandes en cours tombent.
    ///
    /// # Errors
    /// Agent inconnu ou registre non publié.
    pub async fn revoke(&self, owner: &str, id: &str) -> Result<Value, AgentError> {
        let (mut reg, stamp) = self.load_stamped(owner).await?;
        let before = reg.agents.len();
        reg.agents.retain(|a| a.id != id);
        if reg.agents.len() == before {
            return Err(AgentError::NotFound);
        }
        self.save(owner, &reg, stamp).await?;
        self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|_, r| !(r.owner == owner && r.agent_id == id));
        self.audit(json!({ "event": "revoke", "hex": owner, "agent": id })).await;
        Ok(json!({ "revoked": id }))
    }

    /// Identifie un agent d'après son jeton.
    async fn authenticate(&self, token: &str) -> Result<(String, AgentRecord), AgentError> {
        let rest = token.strip_prefix(PREFIX).ok_or(AgentError::Unauthorized)?;
        let (prefix, secret) = rest.split_once('_').ok_or(AgentError::Unauthorized)?;
        if prefix.len() != 16 || secret.len() != 64 || !prefix.bytes().chain(secret.bytes()).all(|b| b.is_ascii_hexdigit()) {
            return Err(AgentError::Unauthorized);
        }
        let like = format!("{prefix}%");
        let owners: Vec<String> = self
            .db
            .readers()
            .read(move |c| Ok(c.prepare("SELECT key FROM kv WHERE scope = ?1 AND key LIKE ?2 LIMIT 8")?.query_map(rusqlite::params![KV_SCOPE, like], |r| r.get(0))?.collect::<Result<_, _>>()?))
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
        let digest = sha_hex(token);
        for owner in owners {
            if let Some(a) = self.load(&owner).await?.agents.into_iter().find(|a| a.token_sha256 == digest) {
                return Ok((owner, a));
            }
        }
        Err(AgentError::Unauthorized)
    }

    fn sweep(map: &mut HashMap<String, Request>, at: u64) {
        map.retain(|_, r| at < r.created + REQUEST_TTL_SECS + 3600 && !(r.state == State::Granted && at >= r.until + 3600));
    }

    /// Un agent demande l'accès à `site`. Rend l'identifiant de la demande ; le membre est prévenu.
    ///
    /// # Errors
    /// Site invalide, trop de demandes (par agent ou en attente chez le membre).
    pub async fn ask(&self, owner: &str, agent: &AgentRecord, site: &str, reason: &str) -> Result<Value, AgentError> {
        let site = valid_host(site)?;
        let reason: String = reason.trim().chars().filter(|c| !c.is_control()).take(200).collect();
        let at = now();
        {
            let mut asks = self.asks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let list = asks.entry(format!("{owner}:{}", agent.id)).or_default();
            list.retain(|t| at.saturating_sub(*t) < 3600);
            if list.len() >= ASKS_PER_HOUR {
                return Err(AgentError::Limited("trop de demandes : réessayez plus tard".into()));
            }
            list.push(at);
        }
        let mut raw = [0u8; 8];
        getrandom::fill(&mut raw).map_err(|e| AgentError::Internal(format!("aléa du système indisponible : {e}")))?;
        let id = hex::encode(raw);
        {
            let mut map = self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::sweep(&mut map, at);
            let waiting = map.values().filter(|r| r.owner == owner && r.label(at) == "pending").count();
            if waiting >= MAX_PENDING {
                return Err(AgentError::Limited("trop de demandes en attente : le membre doit d'abord répondre".into()));
            }
            map.insert(id.clone(), Request { owner: owner.to_owned(), agent_id: agent.id.clone(), agent_name: agent.name.clone(), site: site.clone(), reason: reason.clone(), created: at, state: State::Pending, until: 0 });
        }
        self.audit(json!({ "event": "ask", "hex": owner, "agent": agent.id, "site": site, "request": id })).await;
        if let Some(bus) = &self.activity {
            bus.publish_to(Audience::Owner(owner.to_owned()), "agent", "agents", format!("{} demande l'accès à {site}", agent.name), Some(json!({ "request": id, "site": site, "agent": agent.name })));
        }
        Ok(json!({ "id": id, "state": "pending", "expires_in": REQUEST_TTL_SECS }))
    }

    /// État d'une demande, vu par l'agent qui l'a faite (jamais un secret).
    ///
    /// # Errors
    /// Demande inconnue ou d'un autre agent.
    pub fn status(&self, owner: &str, agent: &AgentRecord, id: &str) -> Result<Value, AgentError> {
        let map = self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let r = map.get(id).filter(|r| r.owner == owner && r.agent_id == agent.id).ok_or(AgentError::NotFound)?;
        Ok(r.view(id, now()))
    }

    /// Demandes en cours du membre (en attente ou accordées), pour son écran.
    #[must_use]
    pub fn requests(&self, owner: &str) -> Value {
        let at = now();
        let mut map = self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::sweep(&mut map, at);
        let mut rows: Vec<(u64, Value)> = map.iter().filter(|(_, r)| r.owner == owner && r.label(at) != "expired").map(|(id, r)| (r.created, r.view(id, at))).collect();
        rows.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        json!({ "requests": rows.into_iter().map(|(_, v)| v).collect::<Vec<_>>() })
    }

    /// Le membre accorde (`secs`, borné par l'agent et par [`MAX_GRANT_SECS`]) ou refuse.
    ///
    /// # Errors
    /// Demande inconnue, expirée ou déjà tranchée.
    pub async fn decide(&self, owner: &str, id: &str, approve: bool, secs: Option<u64>) -> Result<Value, AgentError> {
        let cap = self.load(owner).await.map(|r| r.agents).unwrap_or_default();
        let at = now();
        let view = {
            let mut map = self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let r = map.get_mut(id).filter(|r| r.owner == owner).ok_or(AgentError::NotFound)?;
            if r.label(at) != "pending" {
                return Err(AgentError::Invalid("cette demande n'est plus en attente".into()));
            }
            let agent_cap = cap.iter().find(|a| a.id == r.agent_id).map_or(MAX_GRANT_SECS, |a| a.max_secs);
            if approve {
                r.state = State::Granted;
                r.until = at + secs.unwrap_or(agent_cap).clamp(30, agent_cap.min(MAX_GRANT_SECS));
            } else {
                r.state = State::Denied;
            }
            (r.view(id, at), r.agent_id.clone(), r.site.clone())
        };
        self.audit(json!({ "event": if approve { "grant" } else { "deny" }, "hex": owner, "agent": view.1, "site": view.2, "request": id })).await;
        Ok(view.0)
    }

    fn tools() -> Value {
        json!([
            { "name": "demander_acces", "description": "Demande au membre l'accès à un de ses comptes. Rend l'identifiant d'une demande ; le membre est prévenu et répond lui-même. Aucun mot de passe n'est jamais renvoyé à l'agent.",
              "inputSchema": { "type": "object", "properties": { "site": { "type": "string", "description": "Nom du site seul, par exemple example.org" }, "raison": { "type": "string", "description": "Pourquoi (200 caractères au plus), montrée au membre" } }, "required": ["site"] } },
            { "name": "statut_acces", "description": "État d'une demande : pending, granted, denied ou expired.",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } }
        ])
    }

    /// Un message JSON-RPC 2.0 du serveur MCP. `None` : notification, rien à répondre.
    ///
    /// # Errors
    /// [`AgentError::Unauthorized`] si le jeton est absent ou faux (aucun traitement avant cette vérification).
    pub async fn mcp(&self, bearer: Option<&str>, body: &Value) -> Result<Option<Value>, AgentError> {
        let (owner, agent) = self.authenticate(bearer.ok_or(AgentError::Unauthorized)?).await?;
        let id = body.get("id").cloned();
        let method = body.get("method").and_then(Value::as_str).unwrap_or_default();
        let reply = |result: Value| id.clone().map(|id| json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        let fail = |code: i64, message: &str| id.clone().map(|id| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }));
        Ok(match method {
            "initialize" => reply(json!({ "protocolVersion": SESSION_TOOLS, "capabilities": { "tools": {} }, "serverInfo": { "name": "astroportd", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Ce serveur ne remet jamais de mot de passe. demander_acces prévient le membre, qui accorde ou refuse ; interrogez ensuite statut_acces." })),
            "ping" => reply(json!({})),
            "tools/list" => reply(json!({ "tools": Self::tools() })),
            "tools/call" => {
                let params = body.get("params").cloned().unwrap_or(Value::Null);
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                let text = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
                let outcome = match params.get("name").and_then(Value::as_str) {
                    Some("demander_acces") => self.ask(&owner, &agent, &text("site"), &text("raison")).await,
                    Some("statut_acces") => self.status(&owner, &agent, &text("id")),
                    _ => return Ok(fail(-32602, "outil inconnu")),
                };
                reply(match outcome {
                    Ok(v) => json!({ "content": [{ "type": "text", "text": v.to_string() }], "isError": false }),
                    Err(AgentError::Internal(e)) => {
                        eprintln!("agents : {e}");
                        json!({ "content": [{ "type": "text", "text": "erreur interne de la station" }], "isError": true })
                    }
                    Err(e) => json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true }),
                })
            }
            m if m.starts_with("notifications/") => None,
            _ => fail(-32601, "méthode inconnue"),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use astro_crypto::StaticKeySource;

    const ALICE: [u8; 32] = [7; 32];

    fn hex_of(secret: &[u8; 32]) -> String {
        astro_nostr_proto::pubkey_of(secret).unwrap()
    }

    async fn rig(tag: &str) -> (std::path::PathBuf, Arc<Db>, Agents, Arc<Mutex<Vec<astro_nostr_proto::Event>>>) {
        let dir = std::env::temp_dir().join(format!("astroportd-agents-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let published: Arc<Mutex<Vec<astro_nostr_proto::Event>>> = Arc::default();
        let (sink, store) = (published.clone(), db.clone());
        // La publication de la station écrit l'événement puis le projette ; ici on rejoue la projection du registre à la main.
        let publish: PublishFn = Arc::new(move |e| {
            let (sink, store) = (sink.clone(), store.clone());
            Box::pin(async move {
                store
                    .writer()
                    .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![KV_SCOPE.to_owned().into(), e.pubkey.clone().into(), e.content.clone().into(), i64::try_from(e.created_at).unwrap_or(0).into()])
                    .await
                    .map_err(|e| e.to_string())?;
                sink.lock().unwrap().push(e);
                Ok(())
            })
        });
        let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
        (dir, db.clone(), Agents::new(db, keys, publish), published)
    }

    fn token_of(v: &Value) -> String {
        v["token"].as_str().unwrap().to_owned()
    }

    async fn call(a: &Agents, token: &str, method: &str, params: Value) -> Value {
        a.mcp(Some(token), &json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })).await.unwrap().unwrap()
    }

    #[test]
    fn only_a_bare_host_name_is_accepted() {
        assert_eq!(valid_host(" Example.ORG. ").unwrap(), "example.org");
        for bad in ["", "localhost", "https://example.org", "example.org/login", "example.org:8443", "user@example.org", "-a.example.org", "a b.example.org", "10.0.0.1", "exa_mple.org", "example..org"] {
            assert!(valid_host(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn the_registry_is_a_signed_encrypted_31903_and_keeps_only_token_hashes() {
        let (dir, _, agents, published) = rig("registry").await;
        let owner = hex_of(&ALICE);
        let made = agents.enroll(&owner, "BRO", "Agents", 600, false).await.unwrap();
        let token = token_of(&made);
        assert!(token.starts_with(&format!("ag1_{}_", &owner[..16])) && token.len() == 4 + 16 + 1 + 64);
        let events = published.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!((ev.kind, ev.pubkey.as_str(), ev.tag_value("d")), (31903, owner.as_str(), Some("agents")));
        ev.verify().unwrap();
        assert!(!ev.content.contains("BRO") && !ev.content.contains(&token), "contenu chiffré pour le seul membre");
        let plain = nip44::decrypt_from_self(&ALICE, &owner, &ev.content).unwrap();
        assert!(plain.contains(&sha_hex(&token)) && !plain.contains(&token), "l'empreinte, jamais le jeton");
        // La liste ne montre ni jeton ni empreinte.
        let listed = agents.list(&owner).await.unwrap().to_string();
        assert!(listed.contains("BRO") && !listed.contains(&sha_hex(&token)) && !listed.contains("token"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn mcp_asks_the_owner_and_never_returns_a_secret() {
        let (dir, db, agents, _) = rig("mcp").await;
        let bus = ActivityBus::new();
        let agents = agents.with_activity(bus.clone());
        let owner = hex_of(&ALICE);
        let token = token_of(&agents.enroll(&owner, "Claude Code", "Agents", 120, false).await.unwrap());

        let init = call(&agents, &token, "initialize", json!({})).await;
        assert_eq!(init["result"]["serverInfo"]["name"], "astroportd");
        let tools = call(&agents, &token, "tools/list", json!({})).await;
        let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["demander_acces", "statut_acces"]);
        assert!(agents.mcp(Some(&token), &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.unwrap().is_none());

        let asked = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "Example.org", "raison": "relever ma facture" } })).await;
        assert_eq!(asked["result"]["isError"], false);
        let id = serde_json::from_str::<Value>(asked["result"]["content"][0]["text"].as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_owned();
        let mine = agents.requests(&owner);
        assert_eq!((mine["requests"][0]["site"].as_str(), mine["requests"][0]["state"].as_str()), (Some("example.org"), Some("pending")));
        let seen: Vec<_> = bus.recent(10).iter().filter(|a| a.kind == "agent").cloned().collect();
        assert!(seen.len() == 1 && seen[0].message.contains("Claude Code") && seen[0].message.contains("example.org"), "{seen:?}");

        let status = |t: String| {
            let (agents, id) = (&agents, id.clone());
            async move { call(agents, &t, "tools/call", json!({ "name": "statut_acces", "arguments": { "id": id } })).await }
        };
        let before = status(token.clone()).await;
        assert!(before["result"]["content"][0]["text"].as_str().unwrap().contains("\"pending\""));
        // Le membre accorde ; la durée est plafonnée par l'agent (120 s), même si 3600 est demandé.
        let granted = agents.decide(&owner, &id, true, Some(3600)).await.unwrap();
        assert_eq!(granted["state"], "granted");
        let until = granted["until"].as_u64().unwrap();
        assert!(until <= now() + 120 && until > now() + 60, "plafonné à 120 s");
        let text = status(token.clone()).await["result"]["content"][0]["text"].as_str().unwrap().to_owned();
        assert!(text.contains("\"granted\"") && !text.to_lowercase().contains("password") && !text.contains("secret"));
        assert!(agents.decide(&owner, &id, false, None).await.is_err(), "déjà tranchée");

        // Trace d'audit : qui a demandé quoi, jamais de secret.
        let audit: Vec<String> = db.readers().read(|c| Ok(c.prepare("SELECT data FROM journal WHERE stream = 'audit.agent' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert!(audit.iter().any(|d| d.contains("\"ask\"") && d.contains("example.org")) && audit.iter().any(|d| d.contains("\"grant\"")), "{audit:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn bad_tokens_bad_sites_flooding_and_revocation_are_refused() {
        let (dir, _, agents, _) = rig("hostile").await;
        let owner = hex_of(&ALICE);
        let token = token_of(&agents.enroll(&owner, "BRO", "Agents", 600, false).await.unwrap());
        let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
        // Jeton absent, mal formé, bien formé mais inconnu, ou changé d'un caractère : rien n'est traité.
        let mut flipped = token.clone();
        flipped.pop();
        flipped.push(if token.ends_with('0') { '1' } else { '0' });
        for bad in [None, Some(""), Some("Bearer x"), Some("ag1_zz"), Some(flipped.as_str()), Some(&format!("ag1_{}_{}", &owner[..16], "00".repeat(32)))] {
            assert_eq!(agents.mcp(bad, &ping).await.unwrap_err(), AgentError::Unauthorized, "{bad:?}");
        }
        let a = agents.mcp(Some(&token), &ping).await.unwrap().unwrap();
        assert_eq!(a["result"], json!({}));

        // Un site invalide est une erreur d'outil, pas une demande.
        let r = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "https://example.org/x" } })).await;
        assert_eq!(r["result"]["isError"], true);
        assert!(agents.requests(&owner)["requests"].as_array().unwrap().is_empty());
        let unknown = call(&agents, &token, "tools/call", json!({ "name": "lire_mot_de_passe", "arguments": {} })).await;
        assert_eq!(unknown["error"]["code"], -32602);

        // Au plus 5 demandes en attente.
        for n in 0..MAX_PENDING {
            let r = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": format!("s{n}.example.org") } })).await;
            assert_eq!(r["result"]["isError"], false, "{n}");
        }
        let flood = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "de-trop.example.org" } })).await;
        assert_eq!(flood["result"]["isError"], true);

        // Retrait : le jeton ne marche plus, les demandes tombent.
        let id = agents.list(&owner).await.unwrap()["agents"][0]["id"].as_str().unwrap().to_owned();
        agents.revoke(&owner, &id).await.unwrap();
        assert_eq!(agents.mcp(Some(&token), &ping).await.unwrap_err(), AgentError::Unauthorized);
        assert!(agents.requests(&owner)["requests"].as_array().unwrap().is_empty());
        assert_eq!(agents.revoke(&owner, &id).await.unwrap_err(), AgentError::NotFound);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn another_members_agent_cannot_read_or_decide_my_requests() {
        let (dir, _, agents, _) = rig("isolation").await;
        let owner = hex_of(&ALICE);
        let t1 = token_of(&agents.enroll(&owner, "A", "Agents", 600, false).await.unwrap());
        let t2 = token_of(&agents.enroll(&owner, "B", "Agents", 600, false).await.unwrap());
        let r = call(&agents, &t1, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "example.org" } })).await;
        let id = serde_json::from_str::<Value>(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_owned();
        let spy = call(&agents, &t2, "tools/call", json!({ "name": "statut_acces", "arguments": { "id": id } })).await;
        assert_eq!(spy["result"]["isError"], true, "l'agent B ne voit pas la demande de l'agent A");
        assert_eq!(agents.decide(&"ff".repeat(32), &id, true, None).await.unwrap_err(), AgentError::NotFound, "un autre compte ne tranche pas");
        let _ = std::fs::remove_dir_all(dir);
    }
}
