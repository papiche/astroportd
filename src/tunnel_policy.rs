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
//! Le Capitaine peut aussi **partager à chaud** d'autres services de sa machine (`nom` → `hôte:port`, ex. un bureau VNC sur
//! `127.0.0.1:5900`) : ils sont gardés en base, exposés dans le maillage sans redémarrage et fermés à tous par défaut
//! (`listed` sans personne). La cible est la machine elle-même ou le réseau local privé, jamais une adresse publique : la
//! station ne doit pas devenir un relais vers Internet. Retirer un service coupe aussi ses sessions ouvertes.
//!
//! Les pairs « amis » se déduisent du graphe Nostr (suivi réciproque) et du DID de chaque Capitaine (`home_station`) : aucun
//! `12345.json` n'est lu.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use astro_core::{ActivityBus, Audience};
use astro_db::Db;
use astro_lifecycle::ylevel::peer_id_to_ssh_pub;
use astro_p2p::tunnel::{ExposeOpts, Exposed, GateFuture, StreamGate, expose};
use astro_p2p::{P2pHandle, PeerId};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

const SCOPE: &str = "options";
const KEY: &str = "tunnels";
/// Clé des services partagés par le Capitaine (`{ nom: "hôte:port" }`), état local de la machine.
const KEY_SHARED: &str = "tunnel_services";
/// Services partagés à chaud au plus.
const MAX_SHARED: usize = 16;
/// Noms pris par la station elle-même (le relai est offert au maillage sous son nom DRAGON).
const RESERVED: [&str; 1] = [astro_sync::peers::RELAY_SERVICE];
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
    /// Services partagés à chaud : nom → cible `hôte:port`.
    shared: Mutex<BTreeMap<String, String>>,
    /// Services retirés pendant cette exécution : leurs sessions encore ouvertes sont coupées par le watchdog.
    retired: Mutex<BTreeSet<String>>,
    /// Maillage P2P (posé par [`TunnelPolicy::attach`]) et services partagés qui y sont exposés.
    mesh: Mutex<Option<P2pHandle>>,
    live: Mutex<BTreeMap<String, Exposed>>,
    /// Services offerts au démarrage (`--p2p-ssh`, `--p2p-expose`…), retenus par [`TunnelPolicy::attach`].
    fixed: Mutex<Vec<String>>,
    /// Réponses à « quels services puis-je ouvrir chez toi ? » ([`SERVICES_PROTOCOL`]).
    answers: Mutex<Option<astro_p2p::ask::Answers>>,
}

/// Question d'un pair : « quels services puis-je ouvrir chez toi ? ». La réponse (`{"services":[…]}`) ne nomme que ceux
/// que la politique lui ouvre maintenant : un pair ne découvre jamais ce qui ne lui est pas destiné.
pub const SERVICES_PROTOCOL: &str = "/astroport/services/1";

/// Demande à la station `peer` les services qu'elle nous ouvre maintenant ([`SERVICES_PROTOCOL`]). Elle ne répond qu'avec ce
/// que sa politique nous permet : la liste dit donc ce qui est **utilisable**, jamais ce qui existe.
pub async fn ask_services(handle: &P2pHandle, peer: &str) -> Result<Vec<String>, String> {
    let id: PeerId = peer.parse().map_err(|_| "PeerID illisible".to_owned())?;
    let body = astro_p2p::ask::ask(handle, id, SERVICES_PROTOCOL, std::time::Duration::from_secs(4)).await?;
    let v: Value = serde_json::from_slice(&body).map_err(|_| "réponse illisible (version trop ancienne ?)".to_owned())?;
    Ok(v["services"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|s| s.len() <= 32).take(64).map(str::to_owned).collect())
}

/// Vérifie la cible d'un service partagé : `IP:port` (ou `localhost:port`) de cette machine ou du réseau local privé.
/// Pas de nom d'hôte (résolution détournable) ni d'adresse publique.
pub fn check_target(target: &str) -> Result<String, String> {
    let t = target.trim();
    let t = t.strip_prefix("localhost:").map_or_else(|| t.to_owned(), |port| format!("127.0.0.1:{port}"));
    let addr: SocketAddr = t.parse().map_err(|_| format!("cible « {target} » : attendu IP:port, par exemple 127.0.0.1:5900"))?;
    let local = match addr.ip() {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
    };
    if !local {
        return Err(format!("cible « {target} » : seulement cette machine (127.0.0.1) ou le réseau local privé"));
    }
    if addr.port() == 0 {
        return Err("port 0 invalide".into());
    }
    Ok(addr.to_string())
}

impl TunnelPolicy {
    /// Charge les réglages conservés. `origin` : la station est en UPlanet ORIGIN (SSH ouvert aux Capitaines amis par défaut).
    /// `ssh_dir` : dossier `~/.ssh` dont `authorized_keys` suit la politique de `ssh` ; `None` = ne jamais y toucher.
    pub async fn load(db: Arc<Db>, origin: bool, ssh_dir: Option<PathBuf>, activity: ActivityBus) -> Arc<Self> {
        let raw: Option<String> = db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, KEY], |r| r.get(0)).optional()?)).await.ok().flatten();
        let rules = raw.and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| parse_rules(&v)).unwrap_or_default();
        let raw: Option<String> = db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, KEY_SHARED], |r| r.get(0)).optional()?)).await.ok().flatten();
        let shared = raw
            .and_then(|t| serde_json::from_str::<BTreeMap<String, String>>(&t).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|(n, t)| valid_service(n) && check_target(t).is_ok())
            .take(MAX_SHARED)
            .collect();
        Arc::new(Self { db, origin, ssh_dir, activity, rules: Mutex::new(rules), shared: Mutex::new(shared), retired: Mutex::default(), mesh: Mutex::new(None), live: Mutex::default(), fixed: Mutex::default(), answers: Mutex::new(None) })
    }

    /// Services partagés à chaud, avec leur cible.
    pub fn shared_targets(&self) -> Vec<(String, String)> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner).iter().map(|(n, t)| (n.clone(), t.clone())).collect()
    }

    /// Tous les services offerts : ceux du démarrage (`fixed`), puis ceux partagés à chaud.
    pub fn services(&self, fixed: &[String]) -> Vec<String> {
        let mut all = fixed.to_vec();
        for (name, _) in self.shared_targets() {
            if !all.contains(&name) {
                all.push(name);
            }
        }
        all
    }

    /// Branche la politique sur le maillage et y expose les services partagés gardés (sauf ceux que `fixed` offre déjà).
    pub fn attach(self: &Arc<Self>, handle: &P2pHandle, fixed: &[String]) {
        *self.mesh.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle.clone());
        fixed.clone_into(&mut self.fixed.lock().unwrap_or_else(PoisonError::into_inner));
        let me = self.clone();
        let f: astro_p2p::ask::AnswerFn = Arc::new(move |peer| {
            let me = me.clone();
            Box::pin(async move { json!({ "services": me.offered_to(&peer.to_string(), now()).await }).to_string().into_bytes() })
        });
        match astro_p2p::ask::answer(handle, SERVICES_PROTOCOL, f) {
            Ok(a) => *self.answers.lock().unwrap_or_else(PoisonError::into_inner) = Some(a),
            Err(e) => self.audit(format!("services : les amis ne pourront pas voir ce qui leur est partagé ({e})")),
        }
        for (name, target) in self.shared_targets() {
            if fixed.contains(&name) {
                self.audit(format!("{name} : déjà offert au démarrage, le partage à chaud est ignoré"));
                continue;
            }
            match self.go_live(handle, &name, &target) {
                Ok(()) => println!("P2P : {name} partagé ({target}), accès « {:?} »", self.rule(&name).mode),
                Err(e) => self.audit(format!("{name} : non partagé ({e})")),
            }
        }
    }

    /// Services que `peer` peut ouvrir ici maintenant (offerts au démarrage ou partagés à chaud).
    pub async fn offered_to(&self, peer: &str, at: i64) -> Vec<String> {
        let fixed = self.fixed.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let mut out = Vec::new();
        for name in self.services(&fixed) {
            if self.permits(peer, &name, at).await {
                out.push(name);
            }
        }
        out
    }

    /// Expose `name` → `target` derrière la politique (remplace une exposition précédente du même nom).
    fn go_live(self: &Arc<Self>, handle: &P2pHandle, name: &str, target: &str) -> Result<(), String> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner).remove(name);
        let opts = ExposeOpts { gate: Some(Arc::new(PolicyGate::new(self.clone()))), ..ExposeOpts::default() };
        let exposed = expose(handle, name, target.to_owned(), opts).map_err(|e| e.to_string())?;
        self.live.lock().unwrap_or_else(PoisonError::into_inner).insert(name.to_owned(), exposed);
        Ok(())
    }

    async fn store_shared(&self) -> Result<(), String> {
        let value = serde_json::to_string(&*self.shared.lock().unwrap_or_else(PoisonError::into_inner)).map_err(|e| e.to_string())?;
        self.db
            .writer()
            .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![SCOPE.to_owned().into(), KEY_SHARED.to_owned().into(), value.into(), now().into()])
            .await
            .map(|_| ())
            .map_err(|e| format!("partage non enregistré : {e}"))
    }

    /// Partage (ou re-cible) un service de la machine : `{ name, target, mode? }`. Le mode par défaut est `listed`.
    async fn share(self: &Arc<Self>, s: &Value, fixed: &[String]) -> Result<String, String> {
        let name = s["name"].as_str().map(str::trim).filter(|n| valid_service(n)).ok_or("nom : minuscules, chiffres, - ou _ (32 au plus)")?.to_owned();
        if RESERVED.contains(&name.as_str()) || fixed.contains(&name) {
            return Err(format!("« {name} » est déjà un service de la station"));
        }
        let target = check_target(s["target"].as_str().unwrap_or_default())?;
        let mode = s["mode"].as_str().map(Mode::parse).transpose()?;
        {
            let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
            if !shared.contains_key(&name) && shared.len() >= MAX_SHARED {
                return Err(format!("{MAX_SHARED} services partagés au plus"));
            }
            shared.insert(name.clone(), target.clone());
        }
        self.retired.lock().unwrap_or_else(PoisonError::into_inner).remove(&name);
        if let Some(mode) = mode {
            let mut rule = self.rule(&name);
            rule.mode = mode;
            self.rules.lock().unwrap_or_else(PoisonError::into_inner).insert(name.clone(), rule);
            self.store().await?;
        }
        self.store_shared().await?;
        let handle = self.mesh.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let live = match handle {
            Some(h) => {
                // L'ancienne écoute du même nom se libère quand sa tâche s'arrête : quelques essais suffisent.
                let mut last = Ok(());
                for _ in 0..20 {
                    last = self.go_live(&h, &name, &target);
                    if last.is_ok() {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                last.map(|()| "")?
            }
            None => " (maillage P2P inactif : il sera offert à son démarrage)",
        };
        Ok(format!("{name} partagé ({target}), accès « {} »{live}", self.rule(&name).mode.name()))
    }

    /// Retire un service partagé : il n'est plus offert et ses sessions ouvertes sont coupées.
    async fn unshare(&self, name: &str) -> Result<String, String> {
        if self.shared.lock().unwrap_or_else(PoisonError::into_inner).remove(name).is_none() {
            return Err(format!("« {name} » n'est pas un service partagé"));
        }
        self.live.lock().unwrap_or_else(PoisonError::into_inner).remove(name);
        self.retired.lock().unwrap_or_else(PoisonError::into_inner).insert(name.to_owned());
        self.rules.lock().unwrap_or_else(PoisonError::into_inner).remove(name);
        self.store_shared().await?;
        self.store().await?;
        Ok(format!("{name} : partage retiré"))
    }

    /// Mode par défaut d'un service.
    pub fn default_mode(&self, service: &str) -> Mode {
        match service {
            "ssh" if self.origin => Mode::Friends,
            "ssh" => Mode::Off,
            // Un service ajouté par le Capitaine n'est ouvert à personne tant qu'il ne choisit pas qui.
            _ if self.shared.lock().unwrap_or_else(PoisonError::into_inner).contains_key(service) => Mode::Listed,
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
                       AND EXISTS (SELECT 1 FROM nostr_events k JOIN nostr_tags t ON t.event_id = k.seq WHERE k.kind = 3 AND k.pubkey = i.hex AND t.name = 'p' AND t.value = cap.hex)",
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
        if self.retired.lock().unwrap_or_else(PoisonError::into_inner).contains(service) {
            return false;
        }
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

    /// Vue pour l'écran du Capitaine. `exposed` : services offerts au démarrage (les services partagés à chaud s'y ajoutent).
    pub async fn view(&self, exposed: &[String], at: i64) -> Value {
        let mut services = Vec::new();
        let shared = self.shared_targets();
        for name in &self.services(exposed) {
            let rule = self.rule(name);
            let named = self.named_peers(name, at).await.len();
            services.push(json!({
                "name": name,
                "mode": rule.mode.name(),
                "default_mode": self.default_mode(name).name(),
                "customized": self.rules.lock().unwrap_or_else(PoisonError::into_inner).contains_key(name.as_str()),
                "grants": rule.grants.iter().map(|g| json!({ "peer": g.peer, "label": g.label, "until": g.until, "expired": !g.valid(at) })).collect::<Vec<_>>(),
                "named": named,
                "shared": shared.iter().any(|(n, _)| n == name),
            }));
        }
        let friends = self.friends().await.into_iter().map(|(peer, label)| json!({ "peer": peer, "label": label })).collect::<Vec<_>>();
        json!({
            "origin": self.origin,
            "modes": ["off", "listed", "friends", "swarm"],
            "services": services,
            "friends": friends,
            "ssh_managed": self.ssh_dir.is_some(),
            "max_shared": MAX_SHARED,
        })
    }

    /// Applique une modification : `{ service, mode }`, `{ service, grant: { peer, label?, hours? } }`,
    /// `{ service, revoke: <peer> }`, `{ service, reset: true }`, `{ share: { name, target, mode? } }` ou `{ unshare: <nom> }`.
    /// Rend la nouvelle vue.
    pub async fn set(self: &Arc<Self>, body: &Value, exposed: &[String], at: i64) -> Result<Value, String> {
        if let Some(s) = body.get("share").filter(|s| s.is_object()) {
            let note = self.share(s, exposed).await?;
            self.audit(note);
            return Ok(self.view(exposed, at).await);
        }
        if let Some(name) = body["unshare"].as_str() {
            let note = self.unshare(name).await?;
            self.audit(note);
            return Ok(self.view(exposed, at).await);
        }
        let service = body["service"].as_str().filter(|s| valid_service(s)).ok_or("service inconnu")?.to_owned();
        if !self.services(exposed).contains(&service) {
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

    #[test]
    fn a_shared_target_is_this_machine_or_the_private_lan_never_the_internet() {
        assert_eq!(check_target("localhost:5900").unwrap(), "127.0.0.1:5900");
        assert_eq!(check_target(" 192.168.1.20:8096 ").unwrap(), "192.168.1.20:8096");
        assert_eq!(check_target("[::1]:3389").unwrap(), "[::1]:3389");
        assert!(check_target("10.0.0.2:22").is_ok() && check_target("[fd00::1]:80").is_ok());
        for bad in ["1.1.1.1:53", "8.8.8.8:443", "example.org:80", "nas.local:5000", "169.254.169.254:80", "[2001:db8::1]:80", "127.0.0.1:0", "127.0.0.1", "", "127.0.0.1:99999", "0.0.0.0:22"] {
            assert!(check_target(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn sharing_rejects_hostile_names_reserved_names_and_too_many_services() {
        let (dir, _, p) = policy("share", false).await;
        for bad in [json!({ "share": { "name": "../etc", "target": "127.0.0.1:1" } }), json!({ "share": { "name": "Bureau", "target": "127.0.0.1:1" } }), json!({ "share": { "name": "ssh", "target": "127.0.0.1:1" } }), json!({ "share": { "name": "strfry", "target": "127.0.0.1:1" } }), json!({ "share": { "name": "x", "target": "8.8.8.8:53" } }), json!({ "share": { "name": "x", "target": "127.0.0.1:1", "mode": "tous" } }), json!({ "unshare": "jamais-vu" })] {
            assert!(p.set(&bad, &exposed(), 0).await.is_err(), "{bad}");
        }
        assert!(p.shared_targets().is_empty(), "rien de gardé après un refus");
        for i in 0..MAX_SHARED {
            p.set(&json!({ "share": { "name": format!("s{i}"), "target": format!("127.0.0.1:{}", 2000 + i) } }), &exposed(), 0).await.unwrap();
        }
        assert!(p.set(&json!({ "share": { "name": "un-de-trop", "target": "127.0.0.1:9" } }), &exposed(), 0).await.is_err());
        // Re-cibler un service existant reste permis à la limite ; régler son accès passe par le même chemin que les autres.
        p.set(&json!({ "share": { "name": "s0", "target": "127.0.0.1:9" } }), &exposed(), 0).await.unwrap();
        let v = p.set(&json!({ "service": "s0", "mode": "friends" }), &exposed(), 0).await.unwrap();
        assert!(v["services"].as_array().unwrap().iter().any(|s| s["name"] == "s0" && s["mode"] == "friends" && s["shared"] == true));
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
