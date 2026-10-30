//! Politique des tunnels P2P du Capitaine : qui peut ouvrir quel service de la station, à chaque flux.
//!
//! Chaque service exposé (`ssh`, `comfyui`, `ollama`…) a un **mode** réglé par le Capitaine, changeable à chaud :
//! - `off` : fermé à tous ;
//! - `listed` : seulement les pairs de la liste (un PeerID, un nom, une expiration facultative) — « laisser un ami m'aider » ;
//! - `friends` : les Capitaines **amis réciproques** de ce Capitaine, plus la liste ;
//! - `swarm` : tout membre du réseau (privé en ẐEN, ouvert en ORIGIN).
//!
//! Par défaut : `ssh` fermé en ẐEN et ouvert aux Capitaines amis en ORIGIN ; `comfyui`, `ollama` et les autres services
//! aux Capitaines amis. Le relai Nostr n'est pas un tunnel réglable : il a son propre pare-feu social (NIP-42). La décision est reprise par le watchdog de session : retirer un accès coupe les sessions
//! ouvertes. L'accès SSH n'est jamais ouvert sans consentement visible : le Capitaine le règle ici, chaque ouverture de
//! session SSH est journalisée, et `~/.ssh/authorized_keys` n'est touché que dans un bloc balisé `astroportd`.
//!
//! Les pairs « amis » se déduisent du graphe Nostr (suivi réciproque) et du DID de chaque Capitaine (`home_station`) : aucun
//! `12345.json` n'est lu.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_lifecycle::ylevel::peer_id_to_ssh_pub;
use astro_p2p::PeerId;
use astro_p2p::tunnel::{GateFuture, StreamGate};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

const SCOPE: &str = "options";
const KEY: &str = "tunnels";
/// Repères du bloc géré dans `authorized_keys`.
const BEGIN: &str = "# BEGIN astroportd (accès des Capitaines autorisés : ne pas modifier à la main)";
const END: &str = "# END astroportd";
/// Pairs listés au plus par service.
const MAX_GRANTS: usize = 50;
/// Durée maximale d'un accès temporaire, en heures (30 jours).
const MAX_HOURS: i64 = 720;

/// Qui peut ouvrir un service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Personne.
    Off,
    /// Les pairs de la liste.
    Listed,
    /// Les Capitaines amis réciproques et la liste.
    Friends,
    /// Tout membre du réseau.
    Swarm,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Listed => "listed",
            Self::Friends => "friends",
            Self::Swarm => "swarm",
        }
    }

    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "off" => Ok(Self::Off),
            "listed" => Ok(Self::Listed),
            "friends" => Ok(Self::Friends),
            "swarm" => Ok(Self::Swarm),
            other => Err(format!("mode inconnu : « {other} » (off, listed, friends ou swarm)")),
        }
    }
}

/// Un pair autorisé nommément.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// PeerID de sa station.
    pub peer: String,
    /// Nom affiché.
    pub label: String,
    /// Fin de l'accès (secondes UNIX) ; `None` = jusqu'à retrait.
    pub until: Option<i64>,
}

impl Grant {
    fn valid(&self, now: i64) -> bool {
        self.until.is_none_or(|u| u > now)
    }
}

/// Réglage d'un service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Mode.
    pub mode: Mode,
    /// Pairs listés.
    pub grants: Vec<Grant>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn valid_service(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Nom de pair sûr pour un commentaire de clé SSH.
fn comment_of(label: &str) -> String {
    let clean: String = label.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).take(40).collect();
    format!("astroportd:{}", if clean.is_empty() { "capitaine" } else { &clean })
}

/// La politique des tunnels d'une station.
pub struct TunnelPolicy {
    db: Arc<Db>,
    origin: bool,
    ssh_dir: Option<PathBuf>,
    activity: ActivityBus,
    rules: Mutex<BTreeMap<String, Rule>>,
}

impl TunnelPolicy {
    /// Charge les réglages conservés. `origin` : la station est en UPlanet ORIGIN (SSH ouvert aux Capitaines amis par défaut).
    /// `ssh_dir` : dossier `~/.ssh` dont `authorized_keys` suit la politique de `ssh` ; `None` = ne jamais y toucher.
    pub async fn load(db: Arc<Db>, origin: bool, ssh_dir: Option<PathBuf>, activity: ActivityBus) -> Arc<Self> {
        let raw: Option<String> = db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, KEY], |r| r.get(0)).optional()?)).await.ok().flatten();
        let rules = raw.and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| parse_rules(&v)).unwrap_or_default();
        Arc::new(Self { db, origin, ssh_dir, activity, rules: Mutex::new(rules) })
    }

    /// Mode par défaut d'un service.
    pub fn default_mode(&self, service: &str) -> Mode {
        match service {
            "ssh" if self.origin => Mode::Friends,
            "ssh" => Mode::Off,
            _ => Mode::Friends,
        }
    }

    /// Réglage en vigueur d'un service (celui du Capitaine, sinon le défaut).
    pub fn rule(&self, service: &str) -> Rule {
        let rules = self.rules.lock().unwrap_or_else(PoisonError::into_inner);
        rules.get(service).cloned().unwrap_or_else(|| Rule { mode: self.default_mode(service), grants: Vec::new() })
    }

    /// PeerID (et nom) des Capitaines amis réciproques des Capitaines hébergés ici.
    pub async fn friends(&self) -> Vec<(String, String)> {
        self.friends_full().await.into_iter().map(|(_, peer, label)| (peer, label)).collect()
    }

    /// Clé Nostr, PeerID et nom des Capitaines amis réciproques des Capitaines hébergés ici.
    pub async fn friends_full(&self) -> Vec<(String, String, String)> {
        self.db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT DISTINCT i.hex, COALESCE(i.peer_id, substr(i.home_station, 1, instr(i.home_station, ':') - 1)), i.email, \
                            (SELECT json_extract(e.content, '$.name') FROM nostr_events e WHERE e.pubkey = i.hex AND e.kind = 0 ORDER BY e.created_at DESC LIMIT 1) \
                     FROM identities cap \
                     JOIN social_graph g ON g.owner_hex = cap.hex AND g.level = 1 \
                     JOIN identities i ON i.hex = g.pubkey AND i.role = 'captain' AND i.hosted = 0 \
                     WHERE cap.hosted = 1 AND cap.role = 'captain' \
                       AND COALESCE(i.peer_id, substr(i.home_station, 1, instr(i.home_station, ':') - 1), '') <> '' \
                       AND EXISTS (SELECT 1 FROM nostr_events k JOIN nostr_tags t ON t.event_id = k.id WHERE k.kind = 3 AND k.pubkey = i.hex AND t.name = 'p' AND t.value = cap.hex)",
                )?;
                let rows = stmt.query_map([], |r| {
                    let (hex, peer, email, name): (String, String, Option<String>, Option<String>) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
                    let label = name.filter(|n| !n.trim().is_empty()).or_else(|| email.map(|e| e.split('@').next().unwrap_or_default().to_owned())).filter(|l| !l.is_empty()).unwrap_or_else(|| hex.chars().take(8).collect());
                    Ok((hex, peer, label))
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Les pairs que la politique nomme pour `service` (liste valide, plus les amis en mode `friends`).
    /// Vide pour `off` et `swarm` : ces modes ne nomment personne.
    pub async fn named_peers(&self, service: &str, at: i64) -> Vec<(String, String)> {
        let rule = self.rule(service);
        let mut out: Vec<(String, String)> = match rule.mode {
            Mode::Off | Mode::Swarm => return Vec::new(),
            Mode::Listed | Mode::Friends => rule.grants.iter().filter(|g| g.valid(at)).map(|g| (g.peer.clone(), g.label.clone())).collect(),
        };
        if rule.mode == Mode::Friends {
            for f in self.friends().await {
                if out.iter().all(|(p, _)| *p != f.0) {
                    out.push(f);
                }
            }
        }
        out
    }

    /// `peer` peut-il ouvrir `service` maintenant ?
    pub async fn permits(&self, peer: &str, service: &str, at: i64) -> bool {
        match self.rule(service).mode {
            Mode::Off => false,
            Mode::Swarm => true,
            Mode::Listed | Mode::Friends => self.named_peers(service, at).await.iter().any(|(p, _)| p == peer),
        }
    }

    async fn store(&self) -> Result<(), String> {
        let value = {
            let rules = self.rules.lock().unwrap_or_else(PoisonError::into_inner);
            rules_json(&rules).to_string()
        };
        self.db
            .writer()
            .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![SCOPE.to_owned().into(), KEY.to_owned().into(), value.into(), now().into()])
            .await
            .map(|_| ())
            .map_err(|e| format!("réglage non enregistré : {e}"))
    }

    fn audit(&self, text: String) {
        self.activity.publish_to(Audience::Captain, "system", "tunnels", text, None);
    }

    /// Vue pour l'écran du Capitaine. `exposed` : services que la station offre.
    pub async fn view(&self, exposed: &[String], at: i64) -> Value {
        let mut services = Vec::new();
        for name in exposed {
            let rule = self.rule(name);
            let named = self.named_peers(name, at).await.len();
            services.push(json!({
                "name": name,
                "mode": rule.mode.name(),
                "default_mode": self.default_mode(name).name(),
                "customized": self.rules.lock().unwrap_or_else(PoisonError::into_inner).contains_key(name.as_str()),
                "grants": rule.grants.iter().map(|g| json!({ "peer": g.peer, "label": g.label, "until": g.until, "expired": !g.valid(at) })).collect::<Vec<_>>(),
                "named": named,
            }));
        }
        let friends = self.friends().await.into_iter().map(|(peer, label)| json!({ "peer": peer, "label": label })).collect::<Vec<_>>();
        json!({
            "origin": self.origin,
            "modes": ["off", "listed", "friends", "swarm"],
            "services": services,
            "friends": friends,
            "ssh_managed": self.ssh_dir.is_some(),
        })
    }

    /// Applique une modification : `{ service, mode }`, `{ service, grant: { peer, label?, hours? } }`,
    /// `{ service, revoke: <peer> }` ou `{ service, reset: true }`. Rend la nouvelle vue.
    pub async fn set(&self, body: &Value, exposed: &[String], at: i64) -> Result<Value, String> {
        let service = body["service"].as_str().filter(|s| valid_service(s)).ok_or("service inconnu")?.to_owned();
        if !exposed.contains(&service) {
            return Err(format!("« {service} » n'est pas un service exposé par cette station"));
        }
        let mut rule = self.rule(&service);
        let mut reset = false;
        let note;
        if body["reset"].as_bool() == Some(true) {
            reset = true;
            note = format!("{service} : réglage par défaut rétabli");
        } else if let Some(mode) = body["mode"].as_str() {
            rule.mode = Mode::parse(mode)?;
            note = format!("{service} : accès « {} »", rule.mode.name());
        } else if let Some(g) = body.get("grant").filter(|g| g.is_object()) {
            let peer = g["peer"].as_str().ok_or("PeerID manquant")?;
            peer.parse::<PeerId>().map_err(|e| format!("PeerID illisible : {e}"))?;
            let hours = g["hours"].as_i64().unwrap_or(0);
            if !(0..=MAX_HOURS).contains(&hours) {
                return Err(format!("durée : 0 (sans limite) à {MAX_HOURS} heures"));
            }
            let label = g["label"].as_str().map(str::trim).filter(|l| !l.is_empty()).map_or_else(|| peer.chars().take(12).collect(), |l| l.chars().take(60).collect::<String>());
            rule.grants.retain(|x| x.peer != peer);
            if rule.grants.len() >= MAX_GRANTS {
                return Err(format!("{MAX_GRANTS} pairs au plus par service"));
            }
            rule.grants.push(Grant { peer: peer.to_owned(), label: label.clone(), until: (hours > 0).then(|| at + hours * 3600) });
            if rule.mode == Mode::Off {
                rule.mode = Mode::Listed; // autoriser quelqu'un nommément ouvre le service pour lui seul
            }
            note = format!("{service} : accès accordé à {label}{}", if hours > 0 { format!(" pour {hours} h") } else { String::new() });
        } else if let Some(peer) = body["revoke"].as_str() {
            let before = rule.grants.len();
            rule.grants.retain(|g| g.peer != peer);
            if rule.grants.len() == before {
                return Err("ce pair n'est pas dans la liste".into());
            }
            note = format!("{service} : accès retiré à {}", peer.chars().take(12).collect::<String>());
        } else {
            return Err("réglage attendu : mode, grant, revoke ou reset".into());
        }
        {
            let mut rules = self.rules.lock().unwrap_or_else(PoisonError::into_inner);
            if reset {
                rules.remove(&service);
            } else {
                rules.insert(service.clone(), rule);
            }
        }
        self.store().await?;
        self.audit(note);
        if service == "ssh" {
            self.sync_ssh(at).await?;
        }
        Ok(self.view(exposed, at).await)
    }

    /// Aligne le bloc `astroportd` de `authorized_keys` sur la politique de `ssh` : les pairs nommés (liste et amis) dont le
    /// PeerID est ed25519, sous la forme de leur clé SSH jumelle. Rend le nombre de clés. Rien ne change hors du bloc balisé.
    pub async fn sync_ssh(&self, at: i64) -> Result<usize, String> {
        let Some(dir) = &self.ssh_dir else { return Ok(0) };
        let keys: Vec<String> = self.named_peers("ssh", at).await.iter().filter_map(|(peer, label)| peer_id_to_ssh_pub(peer, &comment_of(label)).ok()).collect();
        write_block(&dir.join("authorized_keys"), &keys)?;
        Ok(keys.len())
    }
}

fn parse_rules(v: &Value) -> BTreeMap<String, Rule> {
    let mut out = BTreeMap::new();
    for (name, r) in v.as_object().into_iter().flatten() {
        let Some(mode) = r["mode"].as_str().and_then(|m| Mode::parse(m).ok()) else { continue };
        let grants = r["grants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| Some(Grant { peer: g["peer"].as_str()?.to_owned(), label: g["label"].as_str().unwrap_or("").to_owned(), until: g["until"].as_i64() }))
            .collect();
        out.insert(name.clone(), Rule { mode, grants });
    }
    out
}

fn rules_json(rules: &BTreeMap<String, Rule>) -> Value {
    Value::Object(rules.iter().map(|(n, r)| (n.clone(), json!({ "mode": r.mode.name(), "grants": r.grants.iter().map(|g| json!({ "peer": g.peer, "label": g.label, "until": g.until })).collect::<Vec<_>>() }))).collect())
}

/// Réécrit le bloc balisé de `path` avec `keys` (le retire si `keys` est vide). Le reste du fichier est conservé tel quel.
fn write_block(path: &Path, keys: &[String]) -> Result<(), String> {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    let mut kept = Vec::new();
    let mut inside = false;
    for line in current.lines() {
        if line == BEGIN {
            inside = true;
        } else if line == END {
            inside = false;
        } else if !inside {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    let mut out = kept.join("\n");
    if !keys.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(BEGIN);
        out.push('\n');
        out.push_str(&keys.join("\n"));
        out.push('\n');
        out.push_str(END);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    if out == current || (current.is_empty() && out.is_empty()) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
        #[cfg(unix)]
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).map_err(|e| format!("{} : {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("astroportd{}", std::process::id()));
    {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options.open(&tmp).map_err(|e| format!("{} : {e}", tmp.display()))?;
        f.write_all(out.as_bytes()).map_err(|e| format!("{} : {e}", tmp.display()))?;
        f.sync_all().map_err(|e| format!("{} : {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{} : {e}", path.display()))
}

/// Garde des services exposés : applique la [`TunnelPolicy`] à chaque flux et à chaque passage du watchdog.
pub struct PolicyGate {
    policy: Arc<TunnelPolicy>,
}

impl PolicyGate {
    /// Garde sur `policy`.
    pub fn new(policy: Arc<TunnelPolicy>) -> Self {
        Self { policy }
    }
}

impl StreamGate for PolicyGate {
    fn allow(&self, peer: PeerId, service: &str) -> GateFuture {
        let (policy, service, peer) = (self.policy.clone(), service.to_owned(), peer.to_string());
        Box::pin(async move {
            let ok = policy.permits(&peer, &service, now()).await;
            // Une session SSH est un événement : le Capitaine la voit, acceptée comme refusée.
            if service == "ssh" {
                policy.activity.publish_to(Audience::Captain, if ok { "system" } else { "alert" }, "tunnels", format!("SSH : {} par {}", if ok { "session ouverte" } else { "accès refusé" }, peer.chars().take(16).collect::<String>()), None);
            }
            ok
        })
    }

    fn still_allowed(&self, peer: PeerId, service: &str) -> GateFuture {
        let (policy, service, peer) = (self.policy.clone(), service.to_owned(), peer.to_string());
        Box::pin(async move { policy.permits(&peer, &service, now()).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: &str = "12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG";
    const OTHER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTf";

    async fn policy(tag: &str, origin: bool) -> (std::path::PathBuf, Arc<Db>, Arc<TunnelPolicy>) {
        let dir = std::env::temp_dir().join(format!("astroportd-tunnels-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let p = TunnelPolicy::load(db.clone(), origin, Some(dir.join("ssh")), ActivityBus::new()).await;
        (dir, db, p)
    }

    fn exposed() -> Vec<String> {
        ["ssh", "comfyui", "ollama"].map(String::from).to_vec()
    }

    #[tokio::test]
    async fn ssh_is_closed_by_default_in_zen_and_open_to_captain_friends_in_origin() {
        let (d1, _, zen) = policy("zen", false).await;
        let (d2, _, origin) = policy("origin", true).await;
        assert_eq!((zen.rule("ssh").mode, origin.rule("ssh").mode), (Mode::Off, Mode::Friends));
        assert!(!zen.permits(PEER, "ssh", 0).await, "fermé : même un pair connu est refusé");
        assert!(!zen.permits(PEER, "ollama", 0).await, "ni ami ni liste : refusé");
        for d in [d1, d2] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[tokio::test]
    async fn a_temporary_grant_opens_ssh_for_one_friend_then_expires_and_leaves_authorized_keys_clean() {
        let (dir, _, p) = policy("grant", false).await;
        let ak = dir.join("ssh/authorized_keys");
        std::fs::create_dir_all(dir.join("ssh")).unwrap();
        std::fs::write(&ak, "ssh-ed25519 AAAAmine moi@laptop\n").unwrap();
        let t0 = 1_000_000;
        let v = p.set(&json!({ "service": "ssh", "grant": { "peer": PEER, "label": "Léa", "hours": 2 } }), &exposed(), t0).await.unwrap();
        assert_eq!(v["services"][0]["mode"], "listed", "accorder à quelqu'un ouvre le service pour lui seul");
        assert!(p.permits(PEER, "ssh", t0 + 3600).await && !p.permits(OTHER, "ssh", t0 + 3600).await);
        let text = std::fs::read_to_string(&ak).unwrap();
        assert!(text.contains("moi@laptop") && text.contains(BEGIN) && text.contains("astroportd:La"), "{text}");
        // 2 h plus tard : l'accès a expiré (la garde le refuse tout de suite, la synchro retire la clé).
        assert!(!p.permits(PEER, "ssh", t0 + 2 * 3600).await);
        assert_eq!(p.sync_ssh(t0 + 2 * 3600).await.unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&ak).unwrap(), "ssh-ed25519 AAAAmine moi@laptop\n", "seule sa propre clé reste");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn settings_are_validated_kept_and_reset() {
        let (dir, db, p) = policy("set", false).await;
        for bad in [json!({ "service": "../x", "mode": "off" }), json!({ "service": "gpu", "mode": "off" }), json!({ "service": "ssh", "mode": "tous" }), json!({ "service": "ssh", "grant": { "peer": "pas-un-peer" } }), json!({ "service": "ssh", "grant": { "peer": PEER, "hours": 9999 } }), json!({ "service": "ssh", "revoke": PEER }), json!({ "service": "ssh" })] {
            assert!(p.set(&bad, &exposed(), 0).await.is_err(), "{bad}");
        }
        p.set(&json!({ "service": "ollama", "mode": "swarm" }), &exposed(), 0).await.unwrap();
        // Rechargé depuis la base : le réglage du Capitaine est conservé.
        let again = TunnelPolicy::load(db, false, None, ActivityBus::new()).await;
        assert_eq!(again.rule("ollama").mode, Mode::Swarm);
        let v = p.set(&json!({ "service": "ollama", "reset": true }), &exposed(), 0).await.unwrap();
        assert_eq!(v["services"][2]["customized"], false);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn only_reciprocal_captain_friends_count_in_friends_mode() {
        let (dir, db, p) = policy("friends", true).await;
        let cap = "aa".repeat(32);
        let (friend, mere_member, one_way) = (astro_nostr_proto::pubkey_of(&[0x0B; 32]).unwrap(), astro_nostr_proto::pubkey_of(&[0x0C; 32]).unwrap(), "dd".repeat(32));
        for (hex, role, hosted, home) in [(&cap, "captain", 1, None), (&friend, "captain", 0, Some(format!("{PEER}:{}", "11".repeat(32)))), (&mere_member, "multipass", 0, Some(format!("{OTHER}:{}", "22".repeat(32)))), (&one_way, "captain", 0, Some(format!("{OTHER}:{}", "33".repeat(32))))] {
            db.writer().execute("INSERT INTO identities(hex, role, hosted, home_station) VALUES (?1, ?2, ?3, ?4)", vec![hex.clone().into(), role.to_owned().into(), i64::from(hosted).into(), home.map_or(rusqlite::types::Value::Null, Into::into)]).await.unwrap();
            if *hex != cap {
                db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![cap.clone().into(), hex.clone().into()]).await.unwrap();
            }
        }
        // Seuls `friend` et `mere_member` suivent le Capitaine en retour ; `mere_member` n'est pas un Capitaine.
        for secret in [[0x0B; 32], [0x0C; 32]] {
            let ev = astro_nostr_proto::UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), cap.clone()]], content: String::new() }.sign(&secret).unwrap();
            db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &ev).map(|_| ())).await.unwrap();
        }
        let friends = p.friends().await;
        assert_eq!(friends.iter().map(|(peer, _)| peer.as_str()).collect::<Vec<_>>(), [PEER], "{friends:?}");
        assert!(p.permits(PEER, "ssh", 0).await, "ORIGIN : les Capitaines amis passent");
        assert!(!p.permits(OTHER, "ssh", 0).await, "un simple membre ou un suivi à sens unique ne passe pas");
        let _ = std::fs::remove_dir_all(dir);
    }
}
