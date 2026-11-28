//! Écran « Capitaines » : les Capitaines connus de cette station, qui suit qui, **suivre / ne plus suivre** (liste de
//! contacts du Capitaine hébergé, signée ici et publiée), et pour chaque **ami réciproque** les services qu'il nous partage —
//! demandés à sa station par le maillage ([`SERVICES_PROTOCOL`]) : elle ne répond qu'avec ce que sa politique nous ouvre.
//! « Se connecter » ouvre un accès local à chaud (`127.0.0.1:<port>`) vers ce service ; c'est la station de l'ami qui
//! accepte ou refuse chaque flux.

use std::sync::Arc;

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_db::Db;
use astro_nostr_proto::{UnsignedEvent, kind};
use astro_p2p::P2pHandle;
use astro_sync::Projector;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::forwards::Forwards;
use crate::register::PublishFn;

/// Capitaines listés au plus (les plus récemment actifs d'abord).
const MAX_LISTED: i64 = 200;
/// Amis interrogés au plus à chaque affichage.
const MAX_ASKED: usize = 40;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn valid_hex(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `<PeerID>:<NODE_HEX>` (même règle que le projecteur).
fn valid_home(h: &str) -> bool {
    h.split_once(':').is_some_and(|(peer, node)| (10..=64).contains(&peer.len()) && peer.bytes().all(|b| b.is_ascii_alphanumeric()) && astro_nostr_proto::is_hex64(node))
}

/// Un Capitaine connu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captain {
    /// Clé Nostr.
    pub hex: String,
    /// Nom affiché.
    pub label: String,
    /// PeerID de sa station, s'il est connu.
    pub peer: Option<String>,
    /// Le Capitaine hébergé ici le suit.
    pub i_follow: bool,
    /// Il suit le Capitaine hébergé ici.
    pub follows_me: bool,
    /// La carte de sa station, reçue par le maillage (connexion authentifiée par le PeerID), le nomme Capitaine : son DID
    /// (signé par lui) et sa station se répondent.
    pub verified: bool,
}

/// L'écran des Capitaines.
pub struct Captains {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    publish: Option<PublishFn>,
    handle: Option<P2pHandle>,
    forwards: Arc<Forwards>,
    activity: ActivityBus,
    swarm: Option<std::path::PathBuf>,
}

impl Captains {
    /// `publish` : publication des listes de contacts (`None` : seulement rangées ici, pour les tests).
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, publish: Option<PublishFn>, handle: Option<P2pHandle>, forwards: Arc<Forwards>, activity: ActivityBus) -> Self {
        Self { db, keys, publish, handle, forwards, activity, swarm: None }
    }

    /// Dossier des cartes des stations du maillage (`~/.zen/tmp/swarm`) : sert à confirmer les Capitaines annoncés.
    #[must_use]
    pub fn with_swarm(mut self, dir: Option<std::path::PathBuf>) -> Self {
        self.swarm = dir;
        self
    }

    /// Clé du Capitaine hébergé ici (le premier, s'il y en a plusieurs).
    async fn me(&self) -> Result<String, String> {
        self.db
            .readers()
            .read(|c| Ok(c.query_row("SELECT hex FROM identities WHERE hosted = 1 AND role = 'captain' ORDER BY hex LIMIT 1", [], |r| r.get::<_, String>(0)).optional()?))
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "aucun Capitaine hébergé sur cette station".to_owned())
    }

    /// Les Capitaines connus (hors ceux hébergés ici), avec le sens du suivi (décidé en SQL sur les listes de contacts). Un
    /// Capitaine n'est montré que si la carte de sa station le confirme, ou si l'un suit déjà l'autre : un DID qui se dit
    /// Capitaine ne suffit pas à apparaître.
    pub async fn list(&self) -> Result<(String, Vec<Captain>), String> {
        let me = self.me().await?;
        let m = me.clone();
        let rows = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "WITH mine AS (SELECT t.value AS hex FROM nostr_tags t WHERE t.name = 'p' AND t.event_id = \
                        (SELECT seq FROM nostr_events WHERE pubkey = ?1 AND kind = 3 ORDER BY created_at DESC, seq DESC LIMIT 1)) \
                     SELECT i.hex, COALESCE(i.peer_id, NULLIF(substr(i.home_station, 1, instr(i.home_station, ':') - 1), '')), i.email, \
                            (SELECT json_extract(e.content, '$.name') FROM nostr_events e WHERE e.pubkey = i.hex AND e.kind = 0 ORDER BY e.created_at DESC LIMIT 1), \
                            EXISTS (SELECT 1 FROM mine WHERE mine.hex = i.hex), \
                            EXISTS (SELECT 1 FROM nostr_tags t WHERE t.name = 'p' AND t.value = ?1 AND t.event_id = \
                                (SELECT seq FROM nostr_events k WHERE k.pubkey = i.hex AND k.kind = 3 ORDER BY k.created_at DESC, k.seq DESC LIMIT 1)) \
                     FROM identities i WHERE i.role = 'captain' AND i.hosted = 0 AND i.hex <> ?1 \
                     ORDER BY 5 DESC, 6 DESC, i.hex LIMIT ?2",
                )?;
                let rows = stmt.query_map(rusqlite::params![m, MAX_LISTED], |r| {
                    let (hex, peer, email, name, mine, theirs): (String, Option<String>, Option<String>, Option<String>, bool, bool) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?);
                    let label = name.filter(|n| !n.trim().is_empty()).or_else(|| email.map(|e| e.split('@').next().unwrap_or_default().to_owned())).filter(|l| !l.is_empty()).unwrap_or_else(|| hex.chars().take(8).collect());
                    Ok(Captain { hex, label, peer, i_follow: mine, follows_me: theirs, verified: false })
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(|e| e.to_string())?;
        let cards = self.swarm.as_deref().map(astro_sync::read_peers).unwrap_or_default();
        let rows = rows
            .into_iter()
            .map(|mut c| {
                c.verified = c.peer.as_ref().is_some_and(|p| cards.iter().any(|k| &k.node_id == p && k.captain_hex.as_deref() == Some(c.hex.as_str())));
                c
            })
            .filter(|c| c.verified || c.i_follow || c.follows_me)
            .collect();
        Ok((me, rows))
    }

    /// Le Capitaine hébergé suit (`on`) ou ne suit plus `target` : nouvelle liste de contacts signée ici, rangée et
    /// projetée tout de suite (la politique des tunnels en dépend), puis publiée. Les autres entrées sont gardées telles quelles.
    pub async fn follow(&self, target: &str, on: bool) -> Result<(), String> {
        if !valid_hex(target) {
            return Err("clé de Capitaine invalide".into());
        }
        let me = self.me().await?;
        if target == me {
            return Err("on ne se suit pas soi-même".into());
        }
        let secret = self.keys.secret(&me).ok_or("clé du Capitaine introuvable sur cette station")?;
        let w = me.clone();
        let latest: Option<(String, String, i64)> = self
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT content, tags, created_at FROM nostr_events WHERE pubkey = ?1 AND kind = 3 ORDER BY created_at DESC, seq DESC LIMIT 1", [w], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?))
            .await
            .map_err(|e| e.to_string())?;
        let (content, mut tags, previous) = match latest {
            Some((c, t, at)) => (c, serde_json::from_str::<Vec<Vec<String>>>(&t).map_err(|e| format!("liste de contacts illisible : {e}"))?, at),
            None => (String::new(), Vec::new(), 0),
        };
        let is_target = |t: &Vec<String>| t.first().map(String::as_str) == Some("p") && t.get(1).map(String::as_str) == Some(target);
        let present = tags.iter().any(is_target);
        if present == on {
            return Ok(());
        }
        if on {
            tags.push(vec!["p".into(), target.to_owned()]);
        } else {
            tags.retain(|t| !is_target(t));
        }
        let created_at = u64::try_from(now().max(previous + 1)).unwrap_or(0);
        let event = UnsignedEvent { created_at, kind: kind::CONTACTS, tags, content }.sign(&secret).map_err(|e| e.to_string())?;
        let e2 = event.clone();
        self.db
            .writer()
            .transaction(move |tx| {
                astro_db::events::insert_event(tx, &e2)?;
                astro_db::apply_statements(tx, &Projector::plan(&e2).map_err(|err| astro_db::DbError::Corrupt(err.to_string()))?)?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        self.activity.publish_to(Audience::Captain, "system", "capitaines", format!("{} {}", if on { "vous suivez" } else { "vous ne suivez plus" }, target.chars().take(8).collect::<String>()), None);
        if let Some(publish) = self.publish.clone() {
            let bus = self.activity.clone();
            tokio::spawn(async move {
                if let Err(why) = publish(event).await {
                    bus.publish_to(Audience::Captain, "alert", "capitaines", format!("liste de contacts non publiée ({why})"), None);
                }
            });
        }
        Ok(())
    }

    /// Annonce ce Capitaine aux autres stations : son DID (kind 30800, `d = did`), republié avec `t = captain` et sa station
    /// d'attache `home_station = <PeerID>:<NODE_HEX>`. Les stations qui reçoivent la carte de celle-ci le synchronisent, et
    /// l'écran Capitaines de chacune le montre quand la carte et le DID se répondent. Rien n'est fait si le DID dit déjà la même
    /// chose. `Ok(false)` : rien à faire, ou DID pas encore là (on réessaiera).
    pub async fn announce(&self, home: &str) -> Result<bool, String> {
        if !valid_home(home) {
            return Err(format!("station d'attache illisible : {home}"));
        }
        let me = self.me().await?;
        let m = me.clone();
        let latest: Option<(String, String, i64)> = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT e.content, e.tags, e.created_at FROM nostr_events e WHERE e.pubkey = ?1 AND e.kind = 30800 \
                       AND EXISTS (SELECT 1 FROM nostr_tags t WHERE t.event_id = e.seq AND t.name = 'd' AND t.value = 'did') \
                     ORDER BY e.created_at DESC, e.seq DESC LIMIT 1",
                    [m],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?)
            })
            .await
            .map_err(|e| e.to_string())?;
        let Some((content, tags, previous)) = latest else { return Ok(false) };
        let mut tags: Vec<Vec<String>> = serde_json::from_str(&tags).map_err(|e| format!("DID illisible : {e}"))?;
        let has = |tags: &[Vec<String>], name: &str, value: &str| tags.iter().any(|t| t.first().map(String::as_str) == Some(name) && t.get(1).map(String::as_str) == Some(value));
        if has(&tags, "t", "captain") && has(&tags, "home_station", home) && tags.iter().filter(|t| t.first().map(String::as_str) == Some("home_station")).count() == 1 {
            return Ok(false);
        }
        tags.retain(|t| t.first().map(String::as_str) != Some("home_station"));
        if !has(&tags, "t", "captain") {
            tags.push(vec!["t".into(), "captain".into()]);
        }
        tags.push(vec!["home_station".into(), home.to_owned()]);
        let mut doc: Value = serde_json::from_str(&content).unwrap_or_else(|_| json!({}));
        if let Some(obj) = doc.as_object_mut() {
            let meta = obj.entry("metadata").or_insert_with(|| json!({}));
            if let Some(meta) = meta.as_object_mut() {
                meta.insert("astroportStation".into(), json!(home));
            }
        }
        let secret = self.keys.secret(&me).ok_or("clé du Capitaine introuvable sur cette station")?;
        let created_at = u64::try_from(now().max(previous + 1)).unwrap_or(0);
        let event = UnsignedEvent { created_at, kind: kind::DID, tags, content: doc.to_string() }.sign(&secret).map_err(|e| e.to_string())?;
        let stored = event.clone();
        self.db
            .writer()
            .transaction(move |tx| {
                astro_db::events::insert_event(tx, &stored)?;
                astro_db::apply_statements(tx, &Projector::plan(&stored).map_err(|err| astro_db::DbError::Corrupt(err.to_string()))?)?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        if let Some(publish) = &self.publish {
            publish(event).await.map_err(|e| format!("DID du Capitaine non publié ({e})"))?;
        }
        self.activity.publish_to(Audience::Captain, "system", "capitaines", "votre station est annoncée aux autres Capitaines du maillage".to_owned(), None);
        Ok(true)
    }

    /// Annonce répétée (le Capitaine peut s'inscrire après le démarrage) : tout de suite, puis toutes les 10 minutes.
    pub async fn keep_announced(self: Arc<Self>, home: String, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut wait = std::time::Duration::from_secs(20);
        let mut said = None::<String>;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.changed() => return,
            }
            wait = std::time::Duration::from_secs(600);
            match self.announce(&home).await {
                Ok(_) => said = None,
                // Pas encore de Capitaine : normal sur une station neuve, rien à dire.
                Err(e) if e.starts_with("aucun Capitaine") => {}
                Err(e) if said.as_deref() == Some(e.as_str()) => {}
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "capitaines", format!("annonce aux autres stations impossible : {e}"), None);
                    said = Some(e);
                }
            }
        }
    }

    /// Services que la station de `peer` nous ouvre (question posée par le maillage).
    async fn services_of(&self, peer: &str) -> Result<Vec<String>, String> {
        let h = self.handle.as_ref().ok_or("maillage P2P coupé")?;
        crate::tunnel_policy::ask_services(h, peer).await
    }

    fn local_id(peer: &str, service: &str) -> String {
        format!("cap:{peer}/{service}")
    }

    /// Vue de l'écran : Capitaines, suivi, services des amis réciproques et accès locaux ouverts.
    pub async fn view(&self) -> Result<Value, String> {
        let (_, caps) = self.list().await?;
        let at = now();
        let open = self.forwards.list(at);
        let asked = caps.iter().filter(|c| c.i_follow && c.follows_me).filter_map(|c| c.peer.clone()).take(MAX_ASKED).collect::<Vec<_>>();
        let answers = futures_util::future::join_all(asked.iter().map(|p| self.services_of(p))).await;
        let rows: Vec<Value> = caps
            .iter()
            .map(|c| {
                let friend = c.i_follow && c.follows_me;
                let services = c.peer.as_ref().and_then(|p| asked.iter().position(|x| x == p)).map(|i| match &answers[i] {
                    Ok(list) => json!(list.iter().map(|s| {
                        let id = Self::local_id(c.peer.as_deref().unwrap_or_default(), s);
                        let local = open.iter().find(|o| o.0 == id).map(|o| json!({ "addr": o.1.to_string(), "until": o.2 }));
                        json!({ "name": s, "local": local })
                    }).collect::<Vec<_>>()),
                    Err(e) => json!({ "error": e }),
                });
                json!({ "hex": c.hex, "label": c.label, "peer": c.peer, "i_follow": c.i_follow, "follows_me": c.follows_me, "friend": friend, "verified": c.verified, "services": services })
            })
            .collect();
        Ok(json!({ "mesh": self.handle.is_some(), "captains": rows }))
    }

    /// Actions : `{ follow: <hex> }`, `{ unfollow: <hex> }`. Ouvrir un accès à un service d'un ami (SSH, VNC, Ollama…) passe par
    /// [`crate::swarm_route::SwarmRoute`] (section `route` de l'écran Réseau, ou `astroportd tunnel connect`). Rend la vue.
    pub async fn act(&self, b: &Value) -> Result<Value, String> {
        if let Some(h) = b["follow"].as_str() {
            self.follow(h, true).await?;
        } else if let Some(h) = b["unfollow"].as_str() {
            self.follow(h, false).await?;
        } else {
            return Err("action attendue : follow ou unfollow (les accès aux services : Réseau › Services ouverts par la constellation, ou astroportd tunnel connect)".into());
        }
        self.view().await
    }
}
