//! Crawl N² clef par clef : contre un vrai relai `astro-relay` (authentification NIP-42 comprise) et contre un relai simulé.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use astro_db::Db;
use astro_nostr_proto::{Event, Filter};
use astro_relay::{DbCircle, Relay, RelayConfig, serve};
use astro_sync::{ClientError, CrawlConfig, Crawler, RelayClient, Signer, WsClient};
use common::*;

const STATION: [u8; 32] = [0x51; 32];
const STRANGER: [u8; 32] = [0x99; 32];

/// Relai source : une base remplie d'événements, servie par un vrai `astro-relay` dont la station est membre du cercle.
async fn source_relay(events: &[Event]) -> (String, TempDir, Arc<Db>) {
    let dir = TempDir::new("source");
    let db = open_db(&dir);
    host(&db, &STATION).await;
    for e in events {
        ingest(&db, e).await;
    }
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db).await.unwrap();
    let relay = Relay::new(db.clone(), circle, RelayConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, relay));
    (url, dir, db)
}

fn did(secret: &[u8; 32], ts: u64, email: &str) -> Event {
    let h = hex(secret);
    let content = serde_json::json!({"id": format!("did:nostr:{h}"), "metadata": {"email": email}}).to_string();
    ev(secret, 30_800, ts, vec![t(&["d", "did"])], &content)
}

fn constellation() -> Vec<Event> {
    vec![
        follows(&ALICE, 100, &[&BOB]),
        ev(&ALICE, 0, 101, vec![], "{\"name\":\"alice\"}"),
        did(&ALICE, 102, "alice@example.org"),
        follows(&BOB, 200, &[&CAROL]),
        ev(&BOB, 1, 201, vec![], "note de bob"),
        ev(&BOB, 30_850, 202, vec![t(&["d", "economic-health"]), t(&["week", "2026-W43"]), t(&["health:status", "ok"])], ""),
        did(&CAROL, 300, "carol@example.org"),
        ev(&CAROL, 30_852, 301, vec![t(&["d", "n2-1-aaaaaaaa"]), t(&["p", &hex(&DAVE)]), t(&["amount", "7.5"]), t(&["prev", "genesis"]), t(&["t", "g1-n2"])], ""),
        ev(&CAROL, 1, 302, vec![], "note de carol"),
        // Hors cercle : ne doit jamais être répliqué.
        ev(&MALLORY, 1, 400, vec![], "spam"),
        // Privé : hors des kinds répliqués.
        ev(&BOB, 30_078, 203, vec![t(&["d", "prive"])], "secret"),
    ]
}

fn config(url: &str) -> CrawlConfig {
    CrawlConfig { relays: vec![url.to_owned()], fetch_timeout: Duration::from_secs(10), ..CrawlConfig::default() }
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    db.readers().read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?)).await.unwrap()
}

#[tokio::test]
async fn crawl_discovers_n1_then_n2_and_projects_everything() {
    let (url, _src_dir, _src) = source_relay(&constellation()).await;
    let dir = TempDir::new("crawler");
    let db = open_db(&dir);
    host(&db, &ALICE).await;

    let client = WsClient::new(Duration::from_secs(10)).with_signer(Signer::new(STATION));
    let crawler = Crawler::new(db.clone(), client, config(&url));
    let stats = crawler.run_until_stable(&[], 6).await.unwrap();
    assert!(stats.errors.is_empty(), "{:?}", stats.errors);

    // Alice (hébergée) → Bob (N1) → Carol (N2) : 9 événements répliqués (3 + 3 + 3) ; ni Mallory, ni le privé.
    assert_eq!(count(&db, "SELECT count(*) FROM nostr_events").await, 9);
    assert_eq!(count(&db, "SELECT count(*) FROM nostr_events WHERE kind = 30078").await, 0);
    assert_eq!(count(&db, "SELECT count(*) FROM nostr_events WHERE content = 'spam'").await, 0);
    assert_eq!(stats.inserted, 9);

    assert_eq!(graph(&db).await, [row("alice", "bob", 1, None), row("alice", "carol", 2, Some("bob"))]);
    let did_rows: Vec<String> = db
        .readers()
        .read(|c| {
            let mut s = c.prepare("SELECT email||'|'||hosted FROM identities WHERE email IS NOT NULL ORDER BY email")?;
            Ok(s.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(did_rows, ["alice@example.org|1", "carol@example.org|0"]);
    assert_eq!(count(&db, "SELECT count(*) FROM ledger_n2 WHERE amount = '7.5'").await, 1, "ledger répliqué depuis un kind privé de la constellation (auth NIP-42)");
    assert_eq!(count(&db, "SELECT count(*) FROM kv WHERE scope = 'economic_health'").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM done WHERE key LIKE 'economic_health:%'").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM kv WHERE scope = 'crawl_cursor'").await, 3, "un curseur par clé et par relai");
}

#[tokio::test]
async fn a_second_crawl_is_idempotent_and_the_cursor_picks_up_new_events() {
    let (url, _src_dir, src) = source_relay(&constellation()).await;
    let dir = TempDir::new("crawler2");
    let db = open_db(&dir);
    host(&db, &ALICE).await;
    let client = WsClient::new(Duration::from_secs(10)).with_signer(Signer::new(STATION));
    let crawler = Crawler::new(db.clone(), client, config(&url));
    crawler.run_until_stable(&[], 6).await.unwrap();

    let again = crawler.run_until_stable(&[], 6).await.unwrap();
    assert_eq!(again.inserted, 0, "rien de nouveau : tout est dédupliqué");
    assert!(again.duplicates > 0, "la marge de relecture est absorbée par `claimed`");
    assert_eq!(count(&db, "SELECT count(*) FROM nostr_events").await, 9);

    // Un nouvel événement de Carol arrive sur le relai source.
    ingest(&src, &ev(&CAROL, 1, 999, vec![], "nouvelle note")).await;
    let next = crawler.run_until_stable(&[], 6).await.unwrap();
    assert_eq!(next.inserted, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM nostr_events").await, 10);
}

#[tokio::test]
async fn crawl_needs_a_station_key_inside_the_remote_circle() {
    let (url, _src_dir, _src) = source_relay(&constellation()).await;
    for (label, client, expected) in [
        ("sans clé", WsClient::new(Duration::from_secs(10)), "authentification requise"),
        ("clé étrangère", WsClient::new(Duration::from_secs(10)).with_signer(Signer::new(STRANGER)), "restricted"),
    ] {
        let dir = TempDir::new("crawler3");
        let db = open_db(&dir);
        host(&db, &ALICE).await;
        let crawler = Crawler::new(db.clone(), client, config(&url));
        let stats = crawler.run_once(&[]).await.unwrap();
        assert_eq!(stats.inserted, 0, "{label}");
        assert_eq!(stats.errors.len(), 1, "{label} : {:?}", stats.errors);
        assert!(stats.errors[0].2.contains(expected), "{label} : {}", stats.errors[0].2);
        assert_eq!(count(&db, "SELECT count(*) FROM nostr_events").await, 0, "{label}");
    }
}

#[tokio::test]
async fn bootstrap_from_seed_keys_on_an_empty_database() {
    let (url, _src_dir, _src) = source_relay(&constellation()).await;
    let dir = TempDir::new("crawler4");
    let db = open_db(&dir);
    let client = WsClient::new(Duration::from_secs(10)).with_signer(Signer::new(STATION));
    let crawler = Crawler::new(db.clone(), client, config(&url));
    // Aucune identité hébergée : seule la clé d'amorçage est interrogée, et rien ne s'étend.
    let stats = crawler.run_until_stable(&[hex(&ALICE)], 4).await.unwrap();
    assert_eq!(stats.inserted, 3);
    assert!(graph(&db).await.is_empty(), "sans identité hébergée, pas de cercle");
    // Une fois Alice déclarée hébergée, un replay local reconstruit le cercle, et le crawl suivant l'étend.
    host(&db, &ALICE).await;
    astro_sync::replay_local(&db, 100).await.unwrap();
    assert_eq!(graph(&db).await, [row("alice", "bob", 1, None)]);
    crawler.run_until_stable(&[], 4).await.unwrap();
    assert_eq!(graph(&db).await.len(), 2);
}

/// Relai simulé en mémoire : respecte auteurs, kinds, `since`, `until` et `limit` ; renvoie en plus les `extra` aberrants.
struct MemoryRelay {
    events: Vec<Event>,
    extra: Vec<Event>,
}

impl RelayClient for MemoryRelay {
    async fn fetch(&self, _relay: &str, filter: &Filter) -> Result<Vec<Event>, ClientError> {
        let mut found: Vec<Event> = self.events.iter().filter(|e| filter.matches(e)).cloned().collect();
        found.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        found.truncate(filter.limit.unwrap_or(usize::MAX));
        found.extend(self.extra.iter().cloned());
        Ok(found)
    }
}

#[tokio::test]
async fn paging_walks_back_in_time_until_everything_is_fetched() {
    let events: Vec<Event> = (0..25).map(|i| ev(&BOB, 1, 1000 + i, vec![], &format!("note {i}"))).collect();
    let dir = TempDir::new("paging");
    let db = open_db(&dir);
    host(&db, &BOB).await;
    let cfg = CrawlConfig { relays: vec!["mem".into()], page_size: 10, ..CrawlConfig::default() };
    let crawler = Crawler::new(db.clone(), MemoryRelay { events, extra: vec![] }, cfg);
    let stats = crawler.run_once(&[]).await.unwrap();
    assert_eq!(stats.inserted, 25, "{stats:?}");
    assert_eq!(count(&db, "SELECT count(*) FROM nostr_events").await, 25);
}

#[tokio::test]
async fn hostile_relays_cannot_inject_other_authors_kinds_or_forgeries() {
    let good = ev(&BOB, 1, 100, vec![], "légitime");
    let other_author = ev(&MALLORY, 1, 101, vec![], "autre auteur");
    let other_kind = ev(&BOB, 30_078, 102, vec![], "kind non demandé");
    let mut forged = ev(&BOB, 1, 103, vec![], "falsifié");
    forged.content = "modifié après signature".into();
    let dir = TempDir::new("hostile");
    let db = open_db(&dir);
    host(&db, &BOB).await;
    let relay = MemoryRelay { events: vec![good], extra: vec![other_author, other_kind, forged] };
    let cfg = CrawlConfig { relays: vec!["mem".into()], ..CrawlConfig::default() };
    let stats = Crawler::new(db.clone(), relay, cfg).run_once(&[]).await.unwrap();
    assert_eq!((stats.fetched, stats.inserted, stats.rejected), (4, 1, 3), "{stats:?}");
    let contents: HashSet<String> = db
        .readers()
        .read(|c| {
            let mut s = c.prepare("SELECT content FROM nostr_events")?;
            Ok(s.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(contents, HashSet::from(["légitime".to_string()]));
}

#[tokio::test]
async fn replaceable_updates_and_late_older_versions_are_handled() {
    let dir = TempDir::new("replaceable");
    let db = open_db(&dir);
    host(&db, &ALICE).await;
    let v1 = follows(&ALICE, 100, &[&BOB]);
    let v2 = follows(&ALICE, 200, &[&CAROL]);
    let cfg = CrawlConfig { relays: vec!["mem".into()], ..CrawlConfig::default() };
    let relay = MemoryRelay { events: vec![v2.clone()], extra: vec![] };
    let crawler = Crawler::new(db.clone(), relay, cfg.clone());
    crawler.run_once(&[]).await.unwrap();
    assert_eq!(graph(&db).await, [row("alice", "carol", 1, None)]);
    // Un second relai, en retard, ne connaît que l'ancienne version : elle est écartée.
    let stale = Crawler::new(db.clone(), MemoryRelay { events: vec![v1], extra: vec![] }, CrawlConfig { relays: vec!["autre".into()], ..cfg });
    let stats = stale.run_once(&[]).await.unwrap();
    assert_eq!(stats.superseded, 1, "{stats:?}");
    assert_eq!(graph(&db).await, [row("alice", "carol", 1, None)]);
}

#[tokio::test]
async fn run_forever_stops_on_shutdown() {
    let dir = TempDir::new("forever");
    let db = open_db(&dir);
    let cfg = CrawlConfig { relays: vec![], ..CrawlConfig::default() };
    let crawler = Arc::new(Crawler::new(db, MemoryRelay { events: vec![], extra: vec![] }, cfg));
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(crawler.run_forever(Duration::from_secs(3600), rx));
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("arrêt attendu").unwrap();
}
