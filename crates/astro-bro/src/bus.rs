//! Bus de commandes : la station écoute les messages privés (NIP-17 emballés en NIP-59) qui lui sont adressés et
//! exécute les commandes de ses titulaires — solde, statut matériel, téléversement d'un document.
//!
//! Garanties :
//! - **authentifié** : l'auteur d'un message est vérifié par la signature du scellé (`nip59::unwrap`) ; seuls les
//!   comptes hébergés ici (Capitaine, MULTIPASS) sont obéis ; un inconnu ne reçoit **aucune réponse** (pas
//!   d'amplification, pas d'oracle) ;
//! - **au plus une fois** : chaque enveloppe est réclamée (`claimed`) avant d'être traitée ;
//! - **borné** : 10 commandes par minute et par expéditeur ;
//! - **sans fuite** : le journal d'activité ne contient que le nom de la commande, jamais ses arguments.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_core::ActivityBus;
use astro_crypto::nip59::{self, Rumor};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_nostr_proto::{Event, kind, pubkey_of};
use rusqlite::types::Value;
use tokio::sync::watch;
use zeroize::Zeroizing;

use crate::BroError;

/// Propriétaire des marques `claimed` du bus.
const CLAIM_OWNER: &str = "bro:bus";
/// Les enveloppes sont brouillées jusqu'à 2 jours dans le passé : on regarde 3 jours en arrière.
const LOOKBACK_SECS: u64 = 3 * 86_400;
/// Rétention des marques (strictement supérieure à la fenêtre de lecture : jamais de retraitement).
const CLAIM_RETENTION_SECS: u64 = 7 * 86_400;
/// Commandes acceptées par expéditeur et par minute.
pub const COMMANDS_PER_MINUTE: u32 = 10;
/// Enveloppes lues à chaque passage.
const BATCH: usize = 100;

/// Une commande.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `#solde` : solde Ğ1 / Ẑen du titulaire.
    Balance,
    /// `#statut` : matériel et Power-Score de la station.
    Status,
    /// `#upload <source> [nom]` : verse un document dans la file de BRO.
    Upload {
        /// CID IPFS ou URL du document.
        source: String,
        /// Nom souhaité.
        name: Option<String>,
    },
    /// `#aide`.
    Help,
    /// Commande inconnue (nom sans le `#`).
    Unknown(String),
}

/// Analyse un message. `None` : ce n'est pas une commande (le message ne commence pas par `#`).
pub fn parse(text: &str) -> Option<Command> {
    let text = text.trim();
    let rest = text.strip_prefix('#')?;
    let mut words = rest.split_whitespace();
    let name = words.next()?.to_lowercase();
    Some(match name.as_str() {
        "solde" | "balance" => Command::Balance,
        "statut" | "status" | "materiel" | "matériel" => Command::Status,
        "aide" | "help" => Command::Help,
        "upload" | "televerser" | "téléverser" => match words.next() {
            Some(source) => {
                let name = words.collect::<Vec<_>>().join(" ");
                Command::Upload { source: source.to_owned(), name: (!name.is_empty()).then_some(name) }
            }
            None => Command::Unknown("upload sans source".into()),
        },
        other => Command::Unknown(other.to_owned()),
    })
}

/// Ce que la station sait faire pour un titulaire. Chaque réponse est un texte prêt à envoyer ; l'erreur est
/// renvoyée au titulaire telle quelle (jamais de repli silencieux).
pub trait Services: Send + Sync + 'static {
    /// Solde du titulaire.
    fn balance(&self, owner_hex: &str) -> impl Future<Output = Result<String, String>> + Send;
    /// État matériel de la station.
    fn hardware(&self) -> impl Future<Output = Result<String, String>> + Send;
    /// Prise en charge d'un document.
    fn upload(&self, owner_hex: &str, source: &str, name: Option<&str>) -> impl Future<Output = Result<String, String>> + Send;
}

/// Envoi d'une enveloppe vers le relai du destinataire.
pub trait Reply: Send + Sync + 'static {
    /// Publie `wrap` pour `recipient_hex`.
    fn send(&self, recipient_hex: &str, wrap: Event) -> impl Future<Output = Result<(), String>> + Send;
}

/// Bilan d'un passage.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PollStats {
    /// Enveloppes nouvelles examinées.
    pub seen: usize,
    /// Commandes exécutées (réponse envoyée).
    pub answered: usize,
    /// Ignorées : illisibles, non-commandes, expéditeur inconnu, limite de débit.
    pub ignored: usize,
    /// Réponses non envoyées (échec de l'envoi).
    pub send_failures: usize,
}

/// Le bus.
pub struct CommandBus<S, R> {
    db: Arc<Db>,
    secret: Zeroizing<[u8; 32]>,
    station_hex: String,
    services: Arc<S>,
    reply: Arc<R>,
    activity: Option<ActivityBus>,
    /// `(début de la fenêtre, commandes)` par expéditeur.
    limiter: Mutex<HashMap<String, (u64, u32)>>,
}

impl<S: Services, R: Reply> CommandBus<S, R> {
    /// Bus de la station dont la clé Nostr est `station_secret`.
    pub fn new(db: Arc<Db>, station_secret: [u8; 32], services: Arc<S>, reply: Arc<R>, activity: Option<ActivityBus>) -> Result<Self, BroError> {
        let station_hex = pubkey_of(&station_secret).map_err(|e| BroError::Model(format!("clé de station invalide : {e}")))?;
        Ok(Self { db, secret: Zeroizing::new(station_secret), station_hex, services, reply, activity, limiter: Mutex::new(HashMap::new()) })
    }

    /// Clé publique de la station (adresse des messages).
    pub fn station_hex(&self) -> &str {
        &self.station_hex
    }

    fn announce(&self, message: String) {
        if let Some(bus) = &self.activity {
            bus.publish_to(astro_core::Audience::Captain, "bro", "bro:bus", message, None);
        }
    }

    fn allowed(&self, sender: &str, now: u64) -> bool {
        let mut map = self.limiter.lock().unwrap_or_else(|e| e.into_inner());
        let slot = map.entry(sender.to_owned()).or_insert((now, 0));
        if now.saturating_sub(slot.0) >= 60 {
            *slot = (now, 0);
        }
        slot.1 += 1;
        slot.1 <= COMMANDS_PER_MINUTE
    }

    async fn is_hosted(&self, hex: &str) -> Result<bool, BroError> {
        let hex = hex.to_owned();
        Ok(self
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1 AND role IN ('captain', 'multipass'))", [hex], |r| r.get::<_, bool>(0))?))
            .await?)
    }

    /// Lit les enveloppes nouvelles adressées à la station et exécute les commandes. `now` : secondes UNIX.
    pub async fn poll_once(&self, now: u64) -> Result<PollStats, BroError> {
        let (station, since) = (self.station_hex.clone(), i64::try_from(now.saturating_sub(LOOKBACK_SECS)).unwrap_or(0));
        let wraps: Vec<Event> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare_cached(
                    "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e \
                     JOIN nostr_tags t ON t.event_id = e.id AND t.name = 'p' AND t.value = ?1 \
                     WHERE e.kind = 1059 AND e.created_at >= ?2 \
                       AND NOT EXISTS (SELECT 1 FROM claimed c WHERE c.owner = 'bro:bus' AND c.event_id = e.id) \
                     ORDER BY e.created_at LIMIT ?3",
                )?;
                let mut rows = stmt.query(rusqlite::params![station, since, BATCH as i64])?;
                let mut out = Vec::new();
                while let Some(r) = rows.next()? {
                    out.push(row_to_event(r)?);
                }
                Ok(out)
            })
            .await?;
        let mut stats = PollStats::default();
        for wrap in wraps {
            let claim = self
                .db
                .writer()
                .execute("INSERT OR IGNORE INTO claimed(owner, event_id, at) VALUES (?1, ?2, ?3)", vec![Value::Text(CLAIM_OWNER.into()), Value::Text(wrap.id.clone()), Value::Integer(i64::try_from(now).unwrap_or(0))])
                .await?;
            if claim.changes == 0 {
                continue; // un autre passage l'a déjà réclamée
            }
            stats.seen += 1;
            self.handle(&wrap, now, &mut stats).await;
        }
        Ok(stats)
    }

    async fn handle(&self, wrap: &Event, now: u64, stats: &mut PollStats) {
        let opened = match nip59::unwrap(&self.secret, wrap) {
            Ok(o) => o,
            Err(e) => {
                self.announce(format!("message privé illisible ignoré ({e})"));
                stats.ignored += 1;
                return;
            }
        };
        let (sender, rumor) = (opened.sender, opened.rumor);
        if rumor.kind != kind::CHAT || sender == self.station_hex {
            stats.ignored += 1;
            return;
        }
        let Some(command) = parse(&rumor.content) else {
            stats.ignored += 1;
            return;
        };
        match self.is_hosted(&sender).await {
            Ok(true) => {}
            Ok(false) => {
                self.announce(format!("commande refusée : {} n'est pas hébergé ici", short(&sender)));
                stats.ignored += 1;
                return;
            }
            Err(e) => {
                self.announce(format!("commande abandonnée : {e}"));
                stats.ignored += 1;
                return;
            }
        }
        if !self.allowed(&sender, now) {
            self.announce(format!("commande ignorée : trop de demandes de {}", short(&sender)));
            stats.ignored += 1;
            return;
        }
        let name = match &command {
            Command::Balance => "solde",
            Command::Status => "statut",
            Command::Upload { .. } => "upload",
            Command::Help => "aide",
            Command::Unknown(_) => "inconnue",
        };
        self.announce(format!("commande #{name} de {}", short(&sender)));
        let text = match execute(self.services.as_ref(), &command, &sender).await {
            Ok(t) => t,
            Err(e) => format!("⚠️ {e}"),
        };
        match self.answer(&sender, &text, now).await {
            Ok(()) => stats.answered += 1,
            Err(e) => {
                self.announce(format!("réponse non envoyée à {} : {e}", short(&sender)));
                stats.send_failures += 1;
            }
        }
    }

    async fn answer(&self, recipient: &str, text: &str, now: u64) -> Result<(), String> {
        let rumor = Rumor { pubkey: self.station_hex.clone(), created_at: now, kind: kind::CHAT, tags: vec![vec!["p".into(), recipient.to_owned()]], content: text.to_owned() };
        let wrap = nip59::gift_wrap(&self.secret, recipient, &rumor, now).map_err(|e| e.to_string())?;
        self.reply.send(recipient, wrap).await
    }

    /// Écrit à un titulaire de la part de la station (résultat d'une tâche, alerte) : même enveloppe chiffrée et
    /// authentifiée que les réponses aux commandes.
    pub async fn notify(&self, recipient_hex: &str, text: &str, now: u64) -> Result<(), String> {
        self.answer(recipient_hex, text, now).await
    }

    /// Oublie les marques de plus de 7 jours.
    pub async fn purge_claims(&self, now: u64) -> Result<usize, BroError> {
        let cutoff = i64::try_from(now.saturating_sub(CLAIM_RETENTION_SECS)).unwrap_or(0);
        Ok(self.db.writer().execute("DELETE FROM claimed WHERE owner = ?1 AND at < ?2", vec![Value::Text(CLAIM_OWNER.into()), Value::Integer(cutoff)]).await?.changes)
    }

    /// Boucle de service : un passage toutes les `every`, jusqu'à ce que `stop` passe à `true`.
    pub async fn run(self: Arc<Self>, every: Duration, mut stop: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut passes = 0u64;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.changed() => break,
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            if let Err(e) = self.poll_once(now).await {
                self.announce(format!("bus de commandes : lecture impossible ({e})"));
            }
            passes += 1;
            if passes % 720 == 0 {
                if let Err(e) = self.purge_claims(now).await {
                    self.announce(format!("bus de commandes : purge impossible ({e})"));
                }
            }
        }
    }
}

/// Exécute une commande pour `owner`. Partagé par le bus de messages privés et par la console web de BRO.
pub async fn execute<S: Services>(services: &S, command: &Command, owner: &str) -> Result<String, String> {
    match command {
        Command::Balance => services.balance(owner).await,
        Command::Status => services.hardware().await,
        Command::Upload { source, name } => services.upload(owner, source, name.as_deref()).await,
        Command::Help => Ok(HELP.to_owned()),
        Command::Unknown(n) => Err(format!("commande « #{n} » inconnue.\n{HELP}")),
    }
}

const HELP: &str = "Commandes : #solde · #statut · #upload <CID ou URL> [nom] · #aide";

fn short(hex: &str) -> &str {
    hex.get(..8).unwrap_or(hex)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_db::events;
    use astro_nostr_proto::UnsignedEvent;

    use super::*;
    use crate::testkit::temp_db;

    const STATION: [u8; 32] = [1; 32];
    const ALICE: [u8; 32] = [3; 32];
    const EVE: [u8; 32] = [5; 32];
    const NOW: u64 = 1_792_000_000;

    struct Fake;
    impl Services for Fake {
        async fn balance(&self, owner: &str) -> Result<String, String> {
            Ok(format!("solde de {} : 12.50 Ẑ", &owner[..4]))
        }
        async fn hardware(&self) -> Result<String, String> {
            Ok("Power-Score 54 (Brain)".into())
        }
        async fn upload(&self, _: &str, source: &str, name: Option<&str>) -> Result<String, String> {
            if source.starts_with("Qm") { Ok(format!("document {} en file", name.unwrap_or("sans nom"))) } else { Err("source illisible".into()) }
        }
    }

    #[derive(Default)]
    struct Outbox(Mutex<Vec<(String, Event)>>);
    impl Reply for Outbox {
        async fn send(&self, to: &str, wrap: Event) -> Result<(), String> {
            self.0.lock().unwrap().push((to.to_owned(), wrap));
            Ok(())
        }
    }

    fn dm(from: &[u8; 32], text: &str, at: u64) -> Event {
        let rumor = Rumor { pubkey: pubkey_of(from).unwrap(), created_at: at, kind: kind::CHAT, tags: vec![], content: text.into() };
        nip59::gift_wrap(from, &pubkey_of(&STATION).unwrap(), &rumor, at).unwrap()
    }

    async fn store(db: &Db, e: Event) {
        db.writer().transaction(move |tx| events::insert_event(tx, &e).map(|_| ())).await.unwrap();
    }

    async fn setup(tag: &str) -> (Arc<Db>, Arc<Outbox>, CommandBus<Fake, Outbox>, std::path::PathBuf) {
        let (db, dir) = temp_db(tag);
        db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'multipass', 1)", vec![Value::Text(pubkey_of(&ALICE).unwrap())]).await.unwrap();
        let out = Arc::new(Outbox::default());
        let bus = CommandBus::new(db.clone(), STATION, Arc::new(Fake), out.clone(), None).unwrap();
        (db, out, bus, dir)
    }

    fn read_reply(out: &Outbox, i: usize, secret: &[u8; 32]) -> String {
        let (to, wrap) = out.0.lock().unwrap()[i].clone();
        assert_eq!(to, pubkey_of(secret).unwrap());
        let opened = nip59::unwrap(secret, &wrap).unwrap();
        assert_eq!(opened.sender, pubkey_of(&STATION).unwrap(), "la réponse est signée par la station");
        opened.rumor.content
    }

    #[test]
    fn commands_are_parsed_leniently_and_non_commands_are_not_commands() {
        assert_eq!(parse("  #Solde "), Some(Command::Balance));
        assert_eq!(parse("#statut"), Some(Command::Status));
        assert_eq!(parse("#upload QmAbc ma photo de vacances"), Some(Command::Upload { source: "QmAbc".into(), name: Some("ma photo de vacances".into()) }));
        assert!(matches!(parse("#upload"), Some(Command::Unknown(_))));
        assert_eq!(parse("bonjour BRO"), None);
        assert_eq!(parse("# "), None);
        assert!(matches!(parse("#danser"), Some(Command::Unknown(n)) if n == "danser"));
    }

    #[tokio::test]
    async fn a_hosted_owner_is_answered_once_with_an_authenticated_encrypted_reply() {
        let (db, out, bus, dir) = setup("bus-ok").await;
        store(&db, dm(&ALICE, "#solde", NOW - 100)).await;
        store(&db, dm(&ALICE, "#statut", NOW - 90)).await;
        store(&db, dm(&ALICE, "#upload Qm123 contrat.pdf", NOW - 80)).await;
        store(&db, dm(&ALICE, "#upload http://x", NOW - 70)).await;
        let stats = bus.poll_once(NOW).await.unwrap();
        assert_eq!((stats.seen, stats.answered, stats.ignored), (4, 4, 0));
        // Les horodatages des enveloppes sont brouillés (NIP-59) : l'ordre de traitement n'est pas celui de l'envoi.
        let mut replies: Vec<String> = (0..4).map(|i| read_reply(&out, i, &ALICE)).collect();
        replies.sort();
        let balance = format!("solde de {} : 12.50 Ẑ", &pubkey_of(&ALICE).unwrap()[..4]);
        assert_eq!(replies, ["Power-Score 54 (Brain)", "document contrat.pdf en file", balance.as_str(), "⚠️ source illisible"], "l'erreur est dite, pas avalée");
        // Rejouer ne répond pas une seconde fois.
        assert_eq!(bus.poll_once(NOW + 5).await.unwrap().seen, 0);
        assert_eq!(out.0.lock().unwrap().len(), 4);
        drop((bus, db));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn strangers_non_commands_and_floods_get_no_answer() {
        let (db, out, bus, dir) = setup("bus-deny").await;
        store(&db, dm(&EVE, "#solde", NOW - 50)).await; // non hébergé
        store(&db, dm(&ALICE, "salut BRO, ça va ?", NOW - 49)).await; // pas une commande
        // Enveloppe adressée à la station mais illisible.
        let junk = UnsignedEvent { created_at: NOW - 48, kind: kind::GIFT_WRAP, tags: vec![vec!["p".into(), pubkey_of(&STATION).unwrap()]], content: "pas du nip44".into() }.sign(&[9; 32]).unwrap();
        store(&db, junk).await;
        for i in 0..12 {
            store(&db, dm(&ALICE, "#aide", NOW - 40 + i)).await;
        }
        let stats = bus.poll_once(NOW).await.unwrap();
        assert_eq!(stats.seen, 15);
        assert_eq!(stats.answered, COMMANDS_PER_MINUTE as usize, "limite de débit par expéditeur");
        assert_eq!(stats.ignored, 5);
        assert!(out.0.lock().unwrap().iter().all(|(to, _)| *to == pubkey_of(&ALICE).unwrap()), "l'inconnu n'obtient rien");
        // Une minute plus tard la fenêtre est neuve.
        store(&db, dm(&ALICE, "#aide", NOW + 10)).await;
        assert_eq!(bus.poll_once(NOW + 70).await.unwrap().answered, 1);
        drop((bus, db));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn old_claims_are_purged_after_a_week() {
        let (db, _out, bus, dir) = setup("bus-purge").await;
        store(&db, dm(&ALICE, "#aide", NOW - 10)).await;
        bus.poll_once(NOW).await.unwrap();
        assert_eq!(bus.purge_claims(NOW + 6 * 86_400).await.unwrap(), 0);
        assert_eq!(bus.purge_claims(NOW + 8 * 86_400).await.unwrap(), 1);
        drop((bus, db));
        let _ = std::fs::remove_dir_all(dir);
    }
}
