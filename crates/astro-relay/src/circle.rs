//! Cercle social de la station : `{MULTIPASS hébergés} ∪ {N1} ∪ {N2}`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use astro_db::{Db, DbError};
use tokio::task::JoinHandle;

/// Vue synchrone du cercle social, interrogée à chaque message.
pub trait CircleProvider: Send + Sync {
    /// Le MULTIPASS (ou la clé NODE/LOVE) est hébergé par cette station.
    fn is_hosted(&self, pubkey: &str) -> bool;
    /// La clé appartient à `{hébergés} ∪ {N1} ∪ {N2}`.
    fn in_circle(&self, pubkey: &str) -> bool;
    /// `reader` fait partie du cercle de confiance **N¹** de `author` (ceux que l'auteur suit). Faux par défaut :
    /// le N¹ n'est connu que pour les MULTIPASS hébergés ici.
    fn is_friend_of(&self, _author: &str, _reader: &str) -> bool {
        false
    }
}

/// Cercle fixe (tests, intégration).
#[derive(Debug, Default, Clone)]
pub struct StaticCircle {
    hosted: HashSet<String>,
    circle: HashSet<String>,
    friends: HashSet<(String, String)>,
}

impl StaticCircle {
    /// Construit un cercle ; les clés hébergées en font toujours partie.
    pub fn new<H, C>(hosted: H, others: C) -> Self
    where
        H: IntoIterator<Item = String>,
        C: IntoIterator<Item = String>,
    {
        let hosted: HashSet<String> = hosted.into_iter().collect();
        let mut circle: HashSet<String> = others.into_iter().collect();
        circle.extend(hosted.iter().cloned());
        Self { hosted, circle, friends: HashSet::new() }
    }

    /// Déclare `friend` dans le N¹ de `author`.
    pub fn with_friend(mut self, author: String, friend: String) -> Self {
        self.circle.insert(friend.clone());
        self.friends.insert((author, friend));
        self
    }
}

impl CircleProvider for StaticCircle {
    fn is_hosted(&self, pubkey: &str) -> bool {
        self.hosted.contains(pubkey)
    }

    fn in_circle(&self, pubkey: &str) -> bool {
        self.circle.contains(pubkey)
    }

    fn is_friend_of(&self, author: &str, reader: &str) -> bool {
        self.friends.contains(&(author.to_owned(), reader.to_owned()))
    }
}

#[derive(Default)]
struct Snapshot {
    hosted: HashSet<String>,
    circle: HashSet<String>,
    /// N¹ de chaque MULTIPASS hébergé : auteur → ceux qu'il suit.
    n1: HashMap<String, HashSet<String>>,
}

/// Nombre maximal de contacts lus dans un kind 3.
const MAX_CONTACTS: usize = 5_000;

/// Clés (64 hexadécimaux) des tags `p` d'une liste de contacts (JSON des tags d'un événement).
fn followed_keys(tags_json: &str) -> impl Iterator<Item = String> {
    let tags: Vec<Vec<String>> = serde_json::from_str(tags_json).unwrap_or_default();
    tags.into_iter().filter(|t| t.first().map(String::as_str) == Some("p")).filter_map(|t| t.into_iter().nth(1)).filter(|k| astro_nostr_proto::is_hex64(k)).take(MAX_CONTACTS)
}

/// Cercle projeté depuis `astro-db` (`identities.hosted` et `social_graph`), rafraîchi à la demande.
#[derive(Default)]
pub struct DbCircle {
    inner: RwLock<Snapshot>,
}

impl DbCircle {
    /// Cercle vide : appeler [`DbCircle::refresh`] avant usage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Recharge le cercle depuis la base.
    pub async fn refresh(&self, db: &Db) -> Result<(), DbError> {
        let (hosted, others, n1) = db
            .readers()
            .read(|c| {
                let hosted: HashSet<String> =
                    c.prepare("SELECT hex FROM identities WHERE hosted = 1")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
                let others: HashSet<String> = c
                    .prepare(
                        "SELECT DISTINCT pubkey FROM social_graph WHERE level IN (1, 2) \
                         AND owner_hex IN (SELECT hex FROM identities WHERE hosted = 1)",
                    )?
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let mut n1: HashMap<String, HashSet<String>> = HashMap::new();
                let mut stmt = c.prepare("SELECT owner_hex, pubkey FROM social_graph WHERE level = 1 AND owner_hex IN (SELECT hex FROM identities WHERE hosted = 1)")?;
                for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                    let (owner, friend) = row?;
                    n1.entry(owner).or_default().insert(friend);
                }
                // N¹ universel : le dernier kind 3 de chaque auteur de vidéo (kinds 21/22) stocké ici, qu'il soit hébergé
                // ou d'une station distante de la constellation. L'auteur est « ami » de ceux qu'il suit (tags `p`).
                let mut stmt = c.prepare(
                    "SELECT c.pubkey, c.tags FROM nostr_events c WHERE c.kind = 3 \
                       AND c.pubkey IN (SELECT DISTINCT pubkey FROM nostr_events WHERE kind IN (21, 22)) \
                       AND c.created_at = (SELECT MAX(created_at) FROM nostr_events WHERE pubkey = c.pubkey AND kind = 3)",
                )?;
                for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                    let (author, tags) = row?;
                    let follows = n1.entry(author.clone()).or_default();
                    follows.extend(followed_keys(&tags).filter(|k| *k != author));
                }
                Ok((hosted, others, n1))
            })
            .await?;
        let mut circle = others;
        circle.extend(hosted.iter().cloned());
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = Snapshot { hosted, circle, n1 };
        Ok(())
    }

    /// Rafraîchit le cercle toutes les `every` dans une tâche de fond.
    pub fn spawn_refresher(self: &Arc<Self>, db: Arc<Db>, every: Duration) -> JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                // Un échec transitoire garde le dernier cercle connu : jamais d'ouverture par défaut.
                let _ = me.refresh(&db).await;
            }
        })
    }
}

impl CircleProvider for DbCircle {
    fn is_hosted(&self, pubkey: &str) -> bool {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).hosted.contains(pubkey)
    }

    fn in_circle(&self, pubkey: &str) -> bool {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).circle.contains(pubkey)
    }

    fn is_friend_of(&self, author: &str, reader: &str) -> bool {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).n1.get(author).is_some_and(|f| f.contains(reader))
    }
}
