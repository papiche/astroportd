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
use tokio::sync::watch;

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

/// Erreurs d'usage du chat.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// Requête invalide.
    #[error("{0}")]
    Invalid(String),
    /// Clé du compte indisponible sur cette station.
    #[error("clé du compte indisponible : envoi impossible")]
    NoKey,
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

/// Service de conversations.
pub struct Chat {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    deliver: DeliverFn,
    publish_local: DeliverFn,
    activity: ActivityBus,
    people: Option<Arc<crate::person_keys::PersonKeys>>,
}

impl Chat {
    /// `deliver` remet l'enveloppe à l'ami ; `publish_local` la publie sur le relai de cette station (copie de l'expéditeur).
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, deliver: DeliverFn, publish_local: DeliverFn, activity: ActivityBus) -> Self {
        Self { db, keys, deliver, publish_local, activity, people: None }
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

    async fn handle(&self, owner: &str, secret: &[u8; 32], wrap: &Event) -> usize {
        let Ok(opened) = nip59::unwrap(secret, wrap) else { return 0 };
        let (sender, rumor) = (opened.sender, opened.rumor);
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

    /// Boucle de réception jusqu'à l'arrêt.
    pub async fn run(self: Arc<Self>, every: Duration, mut stop: watch::Receiver<bool>) {
        loop {
            self.poll_once().await;
            tokio::select! {
                () = tokio::time::sleep(every) => {}
                _ = stop.changed() => return,
            }
        }
    }
}
