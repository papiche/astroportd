//! Conversations privées 1 à 1 (NIP-17) : messages kind 14 emballés en NIP-59, comme pour le partage de fichiers.
//!
//! - **Envoi** : un message est emballé deux fois, pour l'ami (remis à sa station) et pour soi (copie publiée sur le relai local :
//!   l'historique se retrouve sur n'importe quelle station qui héberge le compte). Le texte est rangé tout de suite dans
//!   `chat_messages`, avec l'état `failed` si la remise a échoué : rien n'est perdu, l'erreur est dite.
//! - **Réception** : pour chaque MULTIPASS hébergé, les enveloppes nouvelles (suivies par un repère `seq`, car NIP-59 brouille
//!   les dates) sont ouvertes ; seuls comptent les messages d'un proche (suivi par le titulaire) ou d'un compte hébergé ici.
//!   Tout le reste est lu une fois et ignoré sans bruit. La signature du scellé prouve l'expéditeur.
//! - Les messages déchiffrés sont un **état local** (`chat_messages`) : les enveloppes restent dans `nostr_events`, la base se
//!   reconstruit en remettant le repère à zéro.
//! - **Appels visio** : la sonnerie et la poignée de main WebRTC (offre, réponse, raccrochage, refus, occupé) voyagent par le
//!   même chemin, en rumeur privée `kind::CALL_SIGNAL` dans une enveloppe qui **expire** (elle contient des adresses réseau).
//!   Seul un proche peut faire sonner. Un signal reçu est poussé en direct au navigateur (flux d'activité `call:<type>`) ;
//!   une sonnerie trop vieille, ou un appel raccroché sans réponse, devient « Appel manqué » dans la conversation. Le son et
//!   l'image vont ensuite de navigateur à navigateur, sans passer par la station.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::{KeySource, nip59};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_nostr_proto::{Event, is_hex64, kind};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

/// Remise d'une enveloppe au relai de `peer` (station locale ou maillage).
pub type DeliverFn = Arc<dyn Fn(String, Event) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

/// Taille maximale d'un message (caractères).
pub const MAX_CHARS: usize = 4000;
/// Enveloppes lues par MULTIPASS et par passage.
const BATCH: usize = 200;
/// Messages rendus par page.
const PAGE: usize = 60;
/// Espace du repère `seq` dans `kv`.
const SEQ_SCOPE: &str = "chat_seq";
/// Types de signaux d'appel. `media` : « voix seule », le pair cesse d'envoyer sa vidéo vers l'expéditeur (`video: false`)
/// ou la reprend (`video: true`), sans renégocier l'appel.
pub const CALL_TYPES: [&str; 8] = ["offer", "answer", "hangup", "reject", "busy", "join", "roster", "media"];
/// Appel de groupe (maillage) : participants au plus, hôte compris.
pub const MAX_ROOM: usize = 4;
/// Une salle autorisée par un ami vaut ce temps (un appel de groupe ne dure pas plus).
const ROOM_SECS: u64 = 4 * 3600;
/// Description de session (SDP) d'une offre ou d'une réponse : 64 Kio au plus.
pub const MAX_SDP: usize = 65_536;
/// Au-delà, une sonnerie est un appel manqué et un autre signal est périmé (station éteinte, réseau lent).
pub const RING_SECS: i64 = 60;
/// Durée de vie d'une enveloppe d'appel sur les relais.
const CALL_TTL: u64 = 600;
/// Traces d'appel rangées dans la conversation (le texte fait foi : l'interface reconnaît le pictogramme).
pub const MISSED: &str = "📹 Appel manqué";
/// Côté appelant : l'ami n'a pas décroché.
pub const UNANSWERED: &str = "📹 Appel sans réponse";

/// Erreurs d'usage du chat.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// Requête invalide.
    #[error("{0}")]
    Invalid(String),
    /// Clé du compte indisponible sur cette station.
    #[error("clé du compte indisponible : envoi impossible")]
    NoKey,
    /// La station de l'ami n'a pas reçu le signal.
    #[error("{0}")]
    Unreachable(String),
    /// Base de données.
    #[error("base : {0}")]
    Db(String),
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn db_err(e: impl std::fmt::Display) -> ChatError {
    ChatError::Db(e.to_string())
}

/// Texte d'un message : espaces de fin retirés, caractères de contrôle écartés (sauf retour à la ligne).
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || *c == '\n').collect::<String>().trim().to_owned()
}

/// Salle d'appel de groupe autorisée par un ami : hôte, participants, échéance.
type Room = (String, Vec<String>, std::time::Instant);

/// Service de conversations.
pub struct Chat {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    deliver: DeliverFn,
    publish_local: DeliverFn,
    activity: ActivityBus,
    people: Option<Arc<crate::person_keys::PersonKeys>>,
    /// Relais d'appel de la station : secret des identifiants et port UDP.
    turn: Option<([u8; 32], u16)>,
    /// Salles d'appel de groupe où un ami a invité le membre : `(membre, salle) → (hôte, participants, échéance)`. Un
    /// participant qui n'est pas un proche du membre ne peut le joindre que dans une telle salle. État en mémoire.
    rooms: std::sync::Mutex<std::collections::HashMap<(String, String), Room>>,
}

impl Chat {
    /// `deliver` remet l'enveloppe à l'ami ; `publish_local` la publie sur le relai de cette station (copie de l'expéditeur).
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, deliver: DeliverFn, publish_local: DeliverFn, activity: ActivityBus) -> Self {
        Self { db, keys, deliver, publish_local, activity, people: None, turn: None, rooms: std::sync::Mutex::default() }
    }

    /// Relais d'appel : une offre vers un membre de la station ou un ami réciproque emporte des identifiants pour lui seul.
    pub fn with_turn(mut self, secret: [u8; 32], port: u16) -> Self {
        self.turn = Some((secret, port));
        self
    }

    /// Comme [`Self::with_turn`], relais facultatif.
    pub fn with_turn_opt(mut self, turn: Option<([u8; 32], u16)>) -> Self {
        self.turn = turn;
        self
    }

    /// Ami N¹ : suivi par le titulaire **et** qui le suit (sa liste kind 3), ou compte hébergé par cette station.
    async fn mutual_or_member(&self, owner: &str, peer: &str) -> bool {
        let (o, p) = (owner.to_owned(), peer.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let member: Option<i64> = c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [&p], |r| r.get(0)).optional()?;
                let follows: Option<i64> = c.query_row("SELECT 1 FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", [&o, &p], |r| r.get(0)).optional()?;
                let back: Option<i64> = c.query_row("SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq WHERE e.kind = 3 AND e.pubkey = ?1 AND t.name = 'p' AND t.value = ?2 LIMIT 1", [&p, &o], |r| r.get(0)).optional()?;
                Ok(member.is_some() || (follows.is_some() && back.is_some()))
            })
            .await
            .unwrap_or(false)
    }

    /// Active les conversations par **personne** : ses clés dans plusieurs UPlanet (voir [`crate::person_keys`]).
    pub fn with_people(mut self, people: Arc<crate::person_keys::PersonKeys>) -> Self {
        self.people = Some(people);
        self
    }

    /// Les clés de la personne derrière `peer` (`peer` d'abord). Sans annuaire : `peer` seul.
    async fn class(&self, peer: &str) -> Vec<crate::person_keys::PlaneKey> {
        match &self.people {
            Some(p) => p.class_of(peer).await,
            None => vec![crate::person_keys::PlaneKey { plane: String::new(), hex: peer.to_owned(), via: "fiche".into() }],
        }
    }

    /// Clé à qui écrire : la conversation se fait entre deux clés **de la même UPlanet**. Si `peer` vit dans un plan voisin, on prend la
    /// clé de la même personne dans ce plan ; à défaut, l'erreur dit où ouvrir la conversation.
    async fn channel(&self, peer: &str) -> Result<String, ChatError> {
        let Some(people) = &self.people else { return Ok(peer.to_owned()) };
        let class = self.class(peer).await;
        let here = people.plane();
        let known_elsewhere = |k: &crate::person_keys::PlaneKey| !k.plane.is_empty() && k.plane != here;
        if class.first().is_none_or(|k| !known_elsewhere(k)) {
            return Ok(peer.to_owned());
        }
        if let Some(k) = class.iter().find(|k| k.plane == here) {
            return Ok(k.hex.clone());
        }
        let planes: Vec<&str> = class.iter().map(|k| k.plane.as_str()).filter(|p| !p.is_empty()).collect();
        Err(ChatError::Invalid(format!("cette personne n'a pas de clé connue dans l'UPlanet « {here} » : écrivez-lui depuis {}", planes.join(" ou "))))
    }

    /// Envoie `text` à `peer`. Rend le message tel qu'il est rangé (`state` : `ok` ou `failed`, avec `error`).
    pub async fn send(&self, owner: &str, peer: &str, text: &str) -> Result<Value, ChatError> {
        if !is_hex64(peer) || peer == owner {
            return Err(ChatError::Invalid("destinataire invalide".into()));
        }
        let text = clean(text);
        if text.is_empty() {
            return Err(ChatError::Invalid("message vide".into()));
        }
        if text.chars().count() > MAX_CHARS {
            return Err(ChatError::Invalid(format!("message trop long (plus de {MAX_CHARS} caractères)")));
        }
        let peer = self.channel(peer).await?;
        let peer = peer.as_str();
        let secret = self.keys.secret(owner).ok_or(ChatError::NoKey)?;
        let at = now();
        let rumor = nip59::Rumor { pubkey: owner.to_owned(), created_at: u64::try_from(at).unwrap_or(0), kind: kind::CHAT, tags: vec![vec!["p".into(), peer.to_owned()]], content: text.clone() };
        let id = rumor.id().map_err(|e| ChatError::Invalid(e.to_string()))?;
        let to_peer = nip59::gift_wrap(&secret, peer, &rumor, rumor.created_at).map_err(|e| ChatError::Invalid(e.to_string()))?;
        let to_self = nip59::gift_wrap(&secret, owner, &rumor, rumor.created_at).map_err(|e| ChatError::Invalid(e.to_string()))?;
        // La copie personnelle ne bloque pas la remise : on dit l'échec des deux.
        let delivered = (self.deliver)(peer.to_owned(), to_peer).await;
        let copied = (self.publish_local)(owner.to_owned(), to_self).await;
        let error = delivered.err().or_else(|| copied.err().map(|e| format!("copie personnelle : {e}")));
        let state = if error.is_some() { "failed" } else { "ok" };
        self.store(owner, peer, &id, true, at, &text, state, true).await?;
        if let Some(e) = &error {
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "alert", "chat:out", format!("message non remis : {e}"), None);
        }
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:msg", peer.to_owned(), None);
        Ok(json!({ "id": id, "peer": peer, "mine": true, "at": at, "text": text, "state": state, "error": error }))
    }

    #[allow(clippy::too_many_arguments)]
    async fn store(&self, owner: &str, peer: &str, id: &str, mine: bool, at: i64, text: &str, state: &str, read: bool) -> Result<bool, ChatError> {
        let r = self
            .db
            .writer()
            .execute(
                "INSERT INTO chat_messages(owner_hex, id, peer, mine, at, text, state, read, n) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, (SELECT COALESCE(MAX(n), 0) + 1 FROM chat_messages WHERE owner_hex = ?1)) \
                 ON CONFLICT(owner_hex, id) DO UPDATE SET state = excluded.state WHERE excluded.state = 'ok' AND chat_messages.state = 'failed'",
                vec![owner.to_owned().into(), id.to_owned().into(), peer.to_owned().into(), i64::from(mine).into(), at.into(), text.to_owned().into(), state.to_owned().into(), i64::from(read).into()],
            )
            .await
            .map_err(db_err)?;
        Ok(r.changes > 0)
    }

    /// Conversations du titulaire, la plus récente d'abord : `{ peer, label, last, at, mine, unread }`.
    pub async fn conversations(&self, owner: &str) -> Result<Value, ChatError> {
        let o = owner.to_owned();
        let rows = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT m.peer, m.text, m.at, m.mine, \
                            (SELECT COUNT(*) FROM chat_messages u WHERE u.owner_hex = m.owner_hex AND u.peer = m.peer AND u.mine = 0 AND u.read = 0), \
                            COALESCE((SELECT json_extract(f.fields, '$.name') FROM contact_cards f WHERE f.owner_hex = m.owner_hex AND json_extract(f.fields, '$.nostr') = m.peer LIMIT 1), \
                                     (SELECT json_extract(e.content, '$.name') FROM nostr_events e WHERE e.pubkey = m.peer AND e.kind = 0 ORDER BY e.created_at DESC LIMIT 1)) \
                     FROM chat_messages m WHERE m.owner_hex = ?1 \
                       AND m.at = (SELECT MAX(x.at) FROM chat_messages x WHERE x.owner_hex = m.owner_hex AND x.peer = m.peer) \
                     GROUP BY m.peer ORDER BY m.at DESC LIMIT 200",
                )?;
                let out = stmt
                    .query_map([&o], |r| {
                        let peer: String = r.get(0)?;
                        let label: Option<String> = r.get(5)?;
                        Ok(json!({ "peer": peer, "last": r.get::<_, String>(1)?.chars().take(120).collect::<String>(), "at": r.get::<_, i64>(2)?, "mine": r.get::<_, i64>(3)? == 1, "unread": r.get::<_, i64>(4)?,
                                   "label": label.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| peer.chars().take(8).collect()) }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .await
            .map_err(db_err)?;
        let rows = self.by_person(owner, rows).await;
        let unread: i64 = rows.iter().filter_map(|r| r["unread"].as_i64()).sum();
        Ok(json!({ "conversations": rows, "unread": unread, "plane": self.people.as_ref().map(|p| p.plane()) }))
    }

    /// Une conversation par **personne** : les clés liées d'une même personne se rejoignent, et les conversations des plans voisins
    /// (même email, lecture seule) s'y ajoutent avec leur UPlanet. Chaque entrée garde la liste de ses `channels`.
    async fn by_person(&self, owner: &str, own: Vec<Value>) -> Vec<Value> {
        let Some(people) = &self.people else { return own };
        let here = people.plane().to_owned();
        let mut all: Vec<Value> = own.into_iter().map(|mut r| {
            r["plane"] = json!(here);
            r
        }).collect();
        if let Some(email) = people.email_of(owner).await {
            all.extend(people.sibling_conversations(&email).await);
        }
        let mut groups: Vec<Value> = Vec::new();
        let mut sets: Vec<std::collections::BTreeSet<String>> = Vec::new();
        for r in all {
            let peer = r["peer"].as_str().unwrap_or_default().to_owned();
            let channel = json!({ "plane": r["plane"], "peer": peer, "unread": r["unread"] });
            let class: std::collections::BTreeSet<String> = people.class_of(&peer).await.into_iter().map(|k| k.hex).collect();
            let target = sets.iter().position(|g| !g.is_disjoint(&class));
            match target {
                Some(i) => {
                    sets[i].extend(class);
                    let g = &mut groups[i];
                    // Les non lus d'un plan voisin se lisent dans ce plan : ils ne gonflent ni la pastille ni ce plan.
                    if r["plane"] == json!(here) {
                        g["unread"] = json!(g["unread"].as_i64().unwrap_or(0) + r["unread"].as_i64().unwrap_or(0));
                    }
                    if r["at"].as_i64() > g["at"].as_i64() {
                        for k in ["last", "at", "mine"] {
                            g[k] = r[k].clone();
                        }
                    }
                    if let Some(a) = g["channels"].as_array_mut() {
                        a.push(channel);
                    }
                }
                None => {
                    let mut g = r;
                    if g["plane"] != json!(here) {
                        g["unread"] = json!(0);
                    }
                    g["channels"] = json!([channel]);
                    groups.push(g);
                    sets.push(class);
                }
            }
        }
        groups.sort_by_key(|g| std::cmp::Reverse(g["at"].as_i64().unwrap_or(0)));
        groups
    }

    /// Messages avec `peer`, du plus ancien au plus récent ; `before` (horodatage) pour remonter dans l'historique.
    pub async fn messages(&self, owner: &str, peer: &str, before: Option<i64>) -> Result<Value, ChatError> {
        if !is_hex64(peer) {
            return Err(ChatError::Invalid("correspondant invalide".into()));
        }
        let o = owner.to_owned();
        let before = before.unwrap_or(i64::MAX);
        // Toutes les clés de la personne : l'historique d'une ancienne clé et celui de la nouvelle ne font qu'un seul fil.
        let peers: String = serde_json::to_string(&self.class(peer).await.iter().map(|k| k.hex.clone()).collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into());
        let mut rows = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT id, mine, at, text, state FROM chat_messages WHERE owner_hex = ?1 AND peer IN (SELECT value FROM json_each(?2)) AND at < ?3 ORDER BY at DESC, n DESC LIMIT ?4")?;
                let out = stmt
                    .query_map(rusqlite::params![o, peers, before, PAGE as i64 + 1], |r| Ok(json!({ "id": r.get::<_, String>(0)?, "mine": r.get::<_, i64>(1)? == 1, "at": r.get::<_, i64>(2)?, "text": r.get::<_, String>(3)?, "state": r.get::<_, String>(4)? })))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .await
            .map_err(db_err)?;
        let more = rows.len() > PAGE;
        rows.truncate(PAGE);
        rows.reverse();
        Ok(json!({ "peer": peer, "messages": rows, "more": more }))
    }

    /// Marque la conversation comme lue.
    pub async fn mark_read(&self, owner: &str, peer: &str) -> Result<Value, ChatError> {
        if !is_hex64(peer) {
            return Err(ChatError::Invalid("correspondant invalide".into()));
        }
        let peers = serde_json::to_string(&self.class(peer).await.iter().map(|k| k.hex.clone()).collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into());
        let r = self.db.writer().execute("UPDATE chat_messages SET read = 1 WHERE owner_hex = ?1 AND peer IN (SELECT value FROM json_each(?2)) AND mine = 0 AND read = 0", vec![owner.to_owned().into(), peers.into()]).await.map_err(db_err)?;
        Ok(json!({ "read": r.changes }))
    }

    async fn follows(&self, owner: &str, sender: &str) -> bool {
        let (o, s) = (owner.to_owned(), sender.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let follows = c.query_row("SELECT 1 FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level IN (1, 2)", [&o, &s], |r| r.get::<_, i64>(0)).optional()?.is_some();
                let local = c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [&s], |r| r.get::<_, i64>(0)).optional()?.is_some();
                Ok(follows || local)
            })
            .await
            .unwrap_or(false)
    }

    async fn owners(&self) -> Vec<String> {
        self.db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT hex FROM identities WHERE hosted = 1 AND role IN ('multipass', 'captain')")?;
                Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Un passage : ouvre les enveloppes nouvelles de chaque MULTIPASS hébergé. Rend le nombre de messages rangés.
    pub async fn poll_once(&self) -> usize {
        let mut stored = 0;
        for owner in self.owners().await {
            let Some(secret) = self.keys.secret(&owner) else { continue };
            let o = owner.clone();
            let after: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SEQ_SCOPE, &o], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0);
            let o = owner.clone();
            let wraps: Vec<(i64, Event)> = self
                .db
                .readers()
                .read(move |c| {
                    let mut stmt = c.prepare_cached(
                        "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags, e.seq FROM nostr_events e \
                         JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'p' AND t.value = ?1 \
                         WHERE e.kind = 1059 AND e.seq > ?2 ORDER BY e.seq LIMIT ?3",
                    )?;
                    let mut rows = stmt.query(rusqlite::params![o, after, BATCH as i64])?;
                    let mut out = Vec::new();
                    while let Some(r) = rows.next()? {
                        out.push((r.get::<_, i64>(7)?, row_to_event(r)?));
                    }
                    Ok(out)
                })
                .await
                .unwrap_or_default();
            let Some(last) = wraps.last().map(|(s, _)| *s) else { continue };
            for (_, wrap) in wraps {
                stored += self.handle(&owner, &secret, &wrap).await;
            }
            let _ = self.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![SEQ_SCOPE.to_owned().into(), owner.clone().into(), last.to_string().into(), now().into()]).await;
        }
        stored
    }

    /// Identifiant d'appel : 8 à 64 caractères hexadécimaux, tiré au hasard par l'appelant.
    fn call_id(v: &Value) -> Option<String> {
        v["call"].as_str().filter(|c| (8..=64).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_owned)
    }

    /// Participants d'une salle (`roster`) : 1 à [`MAX_ROOM`] clés hexadécimales, sans doublon.
    fn roster(v: &Value) -> Option<Vec<String>> {
        let list: Vec<String> = v["roster"].as_array()?.iter().filter_map(Value::as_str).map(str::to_owned).collect();
        let mut seen = std::collections::HashSet::new();
        (!list.is_empty() && list.len() <= MAX_ROOM && list.len() == v["roster"].as_array()?.len() && list.iter().all(|h| is_hex64(h) && seen.insert(h.clone()))).then_some(list)
    }

    /// Envoie un signal d'appel à `peer` : `{ type, call, sdp?, video?, missed?, roster? }` (voir [`CALL_TYPES`]). `roster`
    /// (offre ou mise à jour d'un appel de groupe) : les participants de la salle, que la station de `peer` admettra. Rien n'est rangé,
    /// sauf la trace « Appel sans réponse » quand l'appelant raccroche avant qu'on décroche (`missed`).
    pub async fn signal(&self, owner: &str, peer: &str, sig: &Value) -> Result<Value, ChatError> {
        if !is_hex64(peer) || peer == owner {
            return Err(ChatError::Invalid("destinataire invalide".into()));
        }
        let ty = sig["type"].as_str().filter(|t| CALL_TYPES.contains(t)).ok_or_else(|| ChatError::Invalid("type de signal inconnu".into()))?;
        let call = Self::call_id(sig).ok_or_else(|| ChatError::Invalid("identifiant d'appel invalide".into()))?;
        let sdp = sig["sdp"].as_str();
        if matches!(ty, "offer" | "answer") != sdp.is_some_and(|d| !d.is_empty() && d.len() <= MAX_SDP) {
            return Err(ChatError::Invalid("description de session attendue pour une offre ou une réponse (64 Kio au plus), et seulement pour elles".into()));
        }
        let missed = ty == "hangup" && sig["missed"].as_bool() == Some(true);
        let mut content = json!({ "type": ty, "call": call, "video": sig["video"].as_bool().unwrap_or(true) });
        if let Some(d) = sdp {
            content["sdp"] = json!(d);
        }
        if missed {
            content["missed"] = json!(true);
        }
        if !sig["roster"].is_null() || ty == "roster" {
            content["roster"] = json!(Self::roster(sig).ok_or_else(|| ChatError::Invalid(format!("participants invalides (1 à {MAX_ROOM} clés)")))?);
        }
        let peer = self.channel(peer).await?;
        // Relais d'appel : des identifiants pour ce seul destinataire, s'il est membre de la station ou ami réciproque (N¹).
        // `turn_host` : le nom sous lequel l'appelant joint sa station (le navigateur le connaît, la station pas toujours).
        let host = sig["turn_host"].as_str().filter(|h| !h.is_empty() && h.len() <= 253 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.:[]".contains(&b)));
        if let (true, Some((turn_secret, port)), Some(host)) = (ty == "offer", self.turn, host) {
            if self.mutual_or_member(owner, &peer).await {
                let (username, credential) = crate::turn::credentials(&turn_secret, &format!("n1-{}", &peer[..16]), u64::try_from(now()).unwrap_or(0), crate::turn::PEER_CREDENTIAL_TTL);
                content["ice"] = json!({ "urls": [format!("turn:{host}:{port}?transport=udp")], "username": username, "credential": credential });
            }
        }
        let secret = self.keys.secret(owner).ok_or(ChatError::NoKey)?;
        let at = now();
        let created = u64::try_from(at).unwrap_or(0);
        let rumor = nip59::Rumor { pubkey: owner.to_owned(), created_at: created, kind: kind::CALL_SIGNAL, tags: vec![vec!["p".into(), peer.clone()]], content: content.to_string() };
        let id = rumor.id().map_err(|e| ChatError::Invalid(e.to_string()))?;
        let wrap = nip59::gift_wrap_expiring(&secret, &peer, &rumor, created, created + CALL_TTL).map_err(|e| ChatError::Invalid(e.to_string()))?;
        (self.deliver)(peer.clone(), wrap).await.map_err(|e| ChatError::Unreachable(format!("appel non transmis : {e}")))?;
        if missed {
            self.store(owner, &peer, &id, true, at, UNANSWERED, "ok", true).await?;
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:msg", peer.clone(), None);
        }
        // Décroché, refusé ou occupé ici : les autres onglets du membre (téléphone, ordinateur) cessent de sonner.
        if matches!(ty, "answer" | "reject" | "busy") {
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "call", "call:handled", String::new(), Some(json!({ "type": "handled", "call": call, "from": peer, "at": at })));
        }
        Ok(json!({ "sent": ty, "call": call, "peer": peer }))
    }

    /// Signal d'appel reçu. Seul un proche fait sonner ; un signal périmé ne sonne pas ; une sonnerie trop vieille ou un
    /// raccrochage sans réponse laisse « Appel manqué » dans la conversation. Rend le nombre de messages rangés.
    async fn handle_call(&self, owner: &str, sender: &str, rumor: &nip59::Rumor) -> usize {
        if sender == owner {
            return 0;
        }
        let Ok(v) = serde_json::from_str::<Value>(&rumor.content) else { return 0 };
        let (Some(ty), Some(call), Ok(id)) = (v["type"].as_str().filter(|t| CALL_TYPES.contains(t)), Self::call_id(&v), rumor.id()) else { return 0 };
        // Seul un proche fait sonner. Un participant d'appel de groupe qui n'est pas un proche n'est admis que dans une salle où
        // un proche a invité le membre, et seulement s'il figure sur la liste de cet hôte (jamais pour ouvrir une salle).
        let friend = self.follows(owner, sender).await;
        let key = (owner.to_owned(), call.clone());
        let in_room = {
            let mut rooms = self.rooms.lock().unwrap_or_else(|e| e.into_inner());
            rooms.retain(|_, r| r.2 > std::time::Instant::now());
            if friend && matches!(ty, "offer" | "roster") {
                if let Some(list) = Self::roster(&v) {
                    // L'hôte d'une salle est l'ami qui l'a ouverte : un autre ne peut pas en réécrire la liste.
                    let host = rooms.get(&key).map_or_else(|| sender.to_owned(), |r| r.0.clone());
                    if host == sender {
                        rooms.insert(key.clone(), (host, list, std::time::Instant::now() + std::time::Duration::from_secs(ROOM_SECS)));
                    }
                }
            }
            rooms.get(&key).is_some_and(|r| r.0 == sender || r.1.iter().any(|p| p == sender))
        };
        if !friend && !in_room {
            return 0;
        }
        let at = i64::try_from(rumor.created_at).unwrap_or(0).min(now());
        let stale = now() - at > RING_SECS;
        let mut stored = 0;
        if friend && ((ty == "offer" && stale) || (ty == "hangup" && v["missed"].as_bool() == Some(true))) {
            match self.store(owner, sender, &id, false, at, MISSED, "ok", false).await {
                Ok(true) => {
                    self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:msg", sender.to_owned(), None);
                    stored = 1;
                }
                Ok(false) => {}
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "chat:in", format!("appel manqué non rangé : {e}"), None);
                }
            }
        }
        if stale {
            return stored;
        }
        let label = self.label(owner, sender).await;
        let sdp = v["sdp"].as_str().filter(|d| d.len() <= MAX_SDP);
        // Accès au relais de l'appelant (offre seulement) : une adresse turn: et des identifiants, rien d'autre.
        let short = |k: &str| v["ice"][k].as_str().filter(|x| !x.is_empty() && x.len() <= 256).map(str::to_owned);
        let ice = (ty == "offer")
            .then(|| (v["ice"]["urls"][0].as_str().filter(|u| u.starts_with("turn:") && u.len() <= 300).map(str::to_owned), short("username"), short("credential")))
            .and_then(|(u, n, c)| Some(json!({ "urls": [u?], "username": n?, "credential": c? })));
        self.activity.publish_to(
            Audience::Owner(owner.to_owned()),
            "call",
            &format!("call:{ty}"),
            label.clone(),
            Some(json!({ "type": ty, "call": call, "from": sender, "label": label, "sdp": sdp, "ice": ice, "roster": Self::roster(&v), "friend": friend, "video": v["video"].as_bool().unwrap_or(true), "at": at })),
        );
        stored
    }

    /// Nom à montrer pour `peer` : sa fiche de contact, sinon son profil Nostr, sinon le début de sa clé.
    async fn label(&self, owner: &str, peer: &str) -> String {
        let (o, p) = (owner.to_owned(), peer.to_owned());
        let name: Option<String> = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE((SELECT json_extract(f.fields, '$.name') FROM contact_cards f WHERE f.owner_hex = ?1 AND json_extract(f.fields, '$.nostr') = ?2 LIMIT 1), \
                                     (SELECT json_extract(e.content, '$.name') FROM nostr_events e WHERE e.pubkey = ?2 AND e.kind = 0 ORDER BY e.created_at DESC LIMIT 1))",
                    [&o, &p],
                    |r| r.get::<_, Option<String>>(0),
                )?)
            })
            .await
            .ok()
            .flatten();
        name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| peer.chars().take(8).collect())
    }

    async fn handle(&self, owner: &str, secret: &[u8; 32], wrap: &Event) -> usize {
        let Ok(opened) = nip59::unwrap(secret, wrap) else { return 0 };
        let (sender, rumor) = (opened.sender, opened.rumor);
        if rumor.kind == kind::CALL_SIGNAL {
            return self.handle_call(owner, &sender, &rumor).await;
        }
        if rumor.kind != kind::CHAT {
            return 0;
        }
        let text = clean(&rumor.content);
        if text.is_empty() || text.chars().count() > MAX_CHARS {
            return 0;
        }
        let Ok(id) = rumor.id() else { return 0 };
        let at = i64::try_from(rumor.created_at).unwrap_or(0).min(now());
        // Copie de soi (envoyée depuis une autre station du même compte) : le correspondant est le destinataire de la rumeur.
        let (peer, mine) = if sender == owner {
            match rumor.tag_value("p") {
                Some(p) if is_hex64(p) && p != owner => (p.to_owned(), true),
                _ => return 0,
            }
        } else {
            if !self.follows(owner, &sender).await {
                return 0;
            }
            (sender, false)
        };
        match self.store(owner, &peer, &id, mine, at, &text, "ok", mine).await {
            Ok(true) => {
                if !mine {
                    self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:msg", peer, None);
                }
                1
            }
            Ok(false) => 0,
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "chat:in", format!("message d'un ami non rangé : {e}"), None);
                0
            }
        }
    }

    /// Boucle de réception jusqu'à l'arrêt. Une enveloppe NIP-59 acceptée par le relai réveille la lecture **tout de suite** :
    /// une sonnerie, puis la réponse de l'appelé, ne font plus chacune attendre le tour suivant (jusqu'à 2 × `every` de
    /// poignée de main perdus). Le passage régulier toutes les `every` reste le filet de sécurité : enveloppes rangées
    /// sans passer par le relai (synchronisation, rejeu), canal en retard ou fermé.
    pub async fn run(self: Arc<Self>, every: Duration, mut relay: broadcast::Receiver<Arc<Event>>, mut stop: watch::Receiver<bool>) {
        let mut live = true;
        loop {
            self.poll_once().await;
            let deadline = tokio::time::sleep(every);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    () = &mut deadline => break,
                    e = relay.recv(), if live => match e {
                        Ok(ev) if ev.kind == kind::GIFT_WRAP => {
                            // Une rafale (offre puis candidats réseau) se lit en un seul passage.
                            while relay.try_recv().is_ok() {}
                            break;
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => break,
                        Err(broadcast::error::RecvError::Closed) => live = false,
                    },
                    _ = stop.changed() => return,
                }
            }
        }
    }
}
