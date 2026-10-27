//! Outils partagés par les tests d'intégration.

#![allow(dead_code, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use astro_db::Db;
use astro_nostr_proto::{Event, UnsignedEvent, pubkey_of};
use astro_sync::{Ingested, Projector, ingest_event};

pub const ALICE: [u8; 32] = [0xA1; 32];
pub const BOB: [u8; 32] = [0xB2; 32];
pub const CAROL: [u8; 32] = [0xC3; 32];
pub const DAVE: [u8; 32] = [0xD5; 32];
pub const ERIN: [u8; 32] = [0xE6; 32];
pub const MALLORY: [u8; 32] = [0xF7; 32];

pub fn hex(secret: &[u8; 32]) -> String {
    pubkey_of(secret).unwrap()
}

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("astro-sync-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn open_db(dir: &TempDir) -> Arc<Db> {
    Arc::new(Db::open(&dir.0.join("uplanet.db"), 2).unwrap())
}

pub fn ev(secret: &[u8; 32], kind: u32, ts: u64, tags: Vec<Vec<String>>, content: &str) -> Event {
    UnsignedEvent { created_at: ts, kind, tags, content: content.into() }.sign(secret).unwrap()
}

pub fn t(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

pub fn follows(secret: &[u8; 32], ts: u64, who: &[&[u8; 32]]) -> Event {
    ev(secret, 3, ts, who.iter().map(|w| t(&["p", &hex(w)])).collect(), "")
}

pub async fn host(db: &Db, secret: &[u8; 32]) {
    db.writer().batch(vec![Projector::mark_hosted(&hex(secret), "multipass")]).await.unwrap();
}

pub async fn ingest(db: &Db, e: &Event) -> Ingested {
    ingest_event(db.writer(), e.clone(), "test").await.unwrap()
}

/// `(owner, pubkey, level, via)` de `social_graph`, triés, avec les clés remplacées par leur prénom.
pub async fn graph(db: &Db) -> Vec<(String, String, i64, Option<String>)> {
    let names: Vec<(String, &str)> =
        [(ALICE, "alice"), (BOB, "bob"), (CAROL, "carol"), (DAVE, "dave"), (ERIN, "erin"), (MALLORY, "mallory")].iter().map(|(k, n)| (hex(k), *n)).collect();
    let name = move |h: String| names.iter().find(|(x, _)| *x == h).map_or(h.clone(), |(_, n)| (*n).to_owned());
    let rows: Vec<(String, String, i64, Option<String>)> = db
        .readers()
        .read(|c| {
            let mut s = c.prepare("SELECT owner_hex, pubkey, level, via FROM social_graph")?;
            Ok(s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    let mut out: Vec<_> = rows.into_iter().map(|(o, p, l, v)| (name(o), name(p), l, v.map(&name))).collect();
    out.sort();
    out
}

pub fn row(owner: &str, pubkey: &str, level: i64, via: Option<&str>) -> (String, String, i64, Option<String>) {
    (owner.into(), pubkey.into(), level, via.map(Into::into))
}

/// Données pseudo-aléatoires reproductibles.
pub fn sample(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}
