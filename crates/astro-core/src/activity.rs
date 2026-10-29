//! Bus d'activité de la station : chaque sous-système y publie ce qui se passe, l'interface web (flux SSE
//! `/api/v1/activity`) et le journal le consomment.
//!
//! Un anneau borné garde les derniers événements : un client qui se connecte (ou se reconnecte avec
//! `Last-Event-ID`) reçoit d'abord ce qu'il a manqué, puis le direct, **sans trou ni doublon**.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::broadcast;

/// Nombre d'événements conservés pour les reconnexions.
pub const BACKLOG: usize = 200;

/// Qui peut voir un événement dans le flux de l'interface.
///
/// Le flux est servi à tout compte authentifié : sans audience, l'inscription d'un MULTIPASS (avec son e-mail), un
/// virement ou un jeton de secours seraient visibles de tous les membres.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Audience {
    /// Tout compte connecté (événements de station sans donnée personnelle : démarrage, santé, relais).
    #[default]
    Everyone,
    /// Le seul titulaire de cette clé Nostr (64 hex).
    Owner(String),
    /// Le seul Capitaine.
    Captain,
}

impl Audience {
    /// L'événement est-il destiné à ce compte ?
    pub fn admits(&self, hex: &str, captain: bool) -> bool {
        match self {
            Self::Everyone => true,
            Self::Owner(owner) => owner == hex,
            Self::Captain => captain,
        }
    }
}

/// Un événement de la station.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Activity {
    /// Numéro croissant (identifiant SSE).
    pub id: u64,
    /// Secondes UNIX.
    pub at: u64,
    /// Nature : `bro`, `relay`, `p2p`, `economy`, `ucloud`, `facecloud`, `system`…
    pub kind: String,
    /// Émetteur (`bro:memory`, `bus:dm`…).
    pub source: String,
    /// Texte lisible, jamais de secret.
    pub message: String,
    /// Détails structurés facultatifs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Destinataires (jamais sérialisé : le filtrage se fait avant l'envoi).
    #[serde(skip)]
    pub audience: Audience,
}

struct Inner {
    next_id: u64,
    ring: VecDeque<Arc<Activity>>,
}

/// Bus d'activité. Cloner est peu coûteux.
#[derive(Clone)]
pub struct ActivityBus {
    inner: Arc<Mutex<Inner>>,
    tx: broadcast::Sender<Arc<Activity>>,
}

impl Default for ActivityBus {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityBus {
    /// Bus vide.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { inner: Arc::new(Mutex::new(Inner { next_id: 1, ring: VecDeque::with_capacity(BACKLOG) })), tx }
    }

    /// Publie un événement visible de tout compte connecté. Pour une donnée personnelle, voir [`ActivityBus::publish_to`].
    /// Ne bloque jamais, même sans abonné.
    pub fn publish(&self, kind: &str, source: &str, message: impl Into<String>, data: Option<serde_json::Value>) -> Arc<Activity> {
        self.publish_to(Audience::Everyone, kind, source, message, data)
    }

    /// Publie un événement pour une audience précise.
    pub fn publish_to(&self, audience: Audience, kind: &str, source: &str, message: impl Into<String>, data: Option<serde_json::Value>) -> Arc<Activity> {
        let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let ev = Arc::new(Activity { id: g.next_id, at, kind: kind.to_owned(), source: source.to_owned(), message: message.into(), data, audience });
        g.next_id += 1;
        if g.ring.len() == BACKLOG {
            g.ring.pop_front();
        }
        g.ring.push_back(ev.clone());
        // L'envoi se fait sous le verrou : `subscribe` ne peut pas s'intercaler entre l'anneau et le canal.
        let _ = self.tx.send(ev.clone());
        ev
    }

    /// Les événements d'identifiant > `after` encore en mémoire, et un récepteur pour la suite.
    pub fn subscribe(&self, after: u64) -> (Vec<Arc<Activity>>, broadcast::Receiver<Arc<Activity>>) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let backlog = g.ring.iter().filter(|a| a.id > after).cloned().collect();
        (backlog, self.tx.subscribe())
    }

    /// Les `n` derniers événements, du plus ancien au plus récent.
    pub fn recent(&self, n: usize) -> Vec<Arc<Activity>> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let skip = g.ring.len().saturating_sub(n);
        g.ring.iter().skip(skip).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_grow_and_the_ring_is_bounded() {
        let bus = ActivityBus::new();
        for i in 0..BACKLOG + 5 {
            bus.publish("system", "test", format!("m{i}"), None);
        }
        let recent = bus.recent(BACKLOG + 50);
        assert_eq!(recent.len(), BACKLOG);
        assert_eq!(recent.first().map(|a| a.id), Some(6));
        assert_eq!(recent.last().map(|a| a.id), Some((BACKLOG + 5) as u64));
    }

    #[test]
    fn an_audience_admits_only_its_recipients() {
        let owner = Audience::Owner("aa".into());
        assert!(owner.admits("aa", false) && !owner.admits("bb", false) && !owner.admits("bb", true), "le Capitaine ne lit pas les événements d'un autre titulaire");
        assert!(Audience::Captain.admits("bb", true) && !Audience::Captain.admits("aa", false));
        assert!(Audience::Everyone.admits("zz", false));
    }

    #[tokio::test]
    async fn a_late_subscriber_gets_the_backlog_then_the_live_stream_without_gap() {
        let bus = ActivityBus::new();
        bus.publish("bro", "t", "un", None);
        bus.publish("bro", "t", "deux", None);
        let (backlog, mut rx) = bus.subscribe(1);
        assert_eq!(backlog.iter().map(|a| a.message.as_str()).collect::<Vec<_>>(), ["deux"]);
        bus.publish("bro", "t", "trois", Some(serde_json::json!({"k": 1})));
        let live = rx.recv().await.unwrap();
        assert_eq!((live.id, live.message.as_str()), (3, "trois"));
    }
}
