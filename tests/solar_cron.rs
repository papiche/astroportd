//! Nuit de 20h12 solaire : étapes, verrou du jour, garde-fou RTC, clôture hebdomadaire et événement de santé.

#![allow(clippy::unwrap_used)]


use std::sync::{Arc, Mutex};

use astro_bro::publisher::Publisher;
use astro_core::Amount;
use astro_core::solar::{Coordinates, next_20h12};
use astro_core::week::days_from_civil;
use astro_db::Db;
use astro_economy::split::SplitConfig;
use astro_nostr_proto::{Event, kind, pubkey_of};
use astroportd::solar_cron::{Hook, Nightly};
use rusqlite::types::Value as V;

const NODE: [u8; 32] = [3; 32];

#[derive(Default)]
struct Sink(Mutex<Vec<Event>>);

impl Publisher for Sink {
    async fn publish(&self, event: Event) -> Result<(), String> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

fn at(y: i64, m: u32, d: u32, h: u64) -> u64 {
    days_from_civil(y, m, d) as u64 * 86_400 + h * 3_600
}

fn hook(counter: Arc<Mutex<u32>>, answer: &'static str) -> Hook {
    Arc::new(move |_| {
        *counter.lock().unwrap() += 1;
        Box::pin(async move { Ok(answer.to_owned()) })
    })
}

async fn nightly(dir: &std::path::Path, db: Arc<Db>, sink: Arc<Sink>, dream: Hook, shares: Hook) -> Nightly<Sink> {
    Nightly {
        db,
        snapshot_dir: dir.join("snapshots"),
        node_hex: pubkey_of(&NODE).unwrap(),
        node_secret: Some(NODE),
        peer_id: Some("12D3KooWstation".into()),
        weekly_costs: Amount(2_000),
        split: SplitConfig::default(),
        purge_shares: Some(shares),
        dream: Some(dream),
        backup: None,
        integrity: None,
        publisher: Some(sink),
        activity: None,
        pause: std::time::Duration::ZERO,
    }
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    db.readers().read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?)).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_solar_night_runs_every_step_once_per_solar_day() {
    let dir = std::env::temp_dir().join(format!("astroportd-solar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    let monday_night = at(2026, 11, 2, 20);
    let lon = 2.35;

    // État de départ : première nuit le lundi précédent, 50 Ẑen de loyer la semaine W44, une vieille réclamation.
    let w = db.writer();
    w.execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('nightly', 'first_run', ?1, ?2)", vec![V::Text(at(2026, 10, 26, 20).to_string()), V::Integer(1)]).await.unwrap();
    w.execute("INSERT INTO rent_payments(event_id, owner_hex, at, amount_cents, src_kind) VALUES ('rent-1', 'aa', ?1, 5000, 30852)", vec![V::Integer(at(2026, 10, 28, 10) as i64)]).await.unwrap();
    w.execute("INSERT INTO claimed(owner, event_id, at) VALUES ('t', 'old', ?1), ('t', 'fresh', ?2)", vec![V::Integer((monday_night - 8 * 86_400) as i64), V::Integer((monday_night - 3_600) as i64)]).await.unwrap();

    let sink = Arc::new(Sink::default());
    let (dreams, shares) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(0)));
    let n = nightly(&dir, db.clone(), sink.clone(), hook(dreams.clone(), "2 emplacements"), hook(shares.clone(), "1 lien expiré")).await;

    let report = n.run_night(monday_night, lon).await;
    assert!(report.all_ok(), "{:?}", report.steps);
    assert_eq!(report.steps.iter().map(|s| s.name).collect::<Vec<_>>(), ["rtc", "snapshot", "checkpoint", "purge", "rêve", "sauvegarde", "intégrité", "clôture", "santé"]);

    // 3. Snapshot : un fichier SQLite lisible qui contient la loyer et le verrou posé avant le travail.
    let snap = dir.join("snapshots").join(format!("uplanet-{}.db", report.date));
    let copy = rusqlite::Connection::open_with_flags(&snap, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let rents: i64 = copy.query_row("SELECT COUNT(*) FROM rent_payments", [], |r| r.get(0)).unwrap();
    assert_eq!(rents, 1);
    // 4. Purge : la réclamation de 8 jours part, la récente reste ; le crochet des liens d'invités a été appelé.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM claimed").await, 1);
    assert_eq!((*shares.lock().unwrap(), *dreams.lock().unwrap()), (1, 1));
    // 5. Clôture : W44 = 50 Ẑ − 20 Ẑ de charges → surplus 30 Ẑ ventilé 3×1/3, journalisé, garde posée.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM done WHERE key = 'economy:close:2026-W44' AND value = 'split'").await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM journal WHERE stream = 'economy.3x13' AND data LIKE '%\"surplus\":\"30.00\"%'").await, 1);
    // 6. Santé : un seul 30850 signé par le NODE, avec le statut de la semaine close.
    let events = sink.0.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!((e.kind, e.pubkey.as_str()), (kind::ECONOMIC_HEALTH, pubkey_of(&NODE).unwrap().as_str()));
    let tag = |name: &str| e.tag_value(name).map(str::to_owned);
    assert_eq!((tag("d").as_deref(), tag("week").as_deref(), tag("health:status").as_deref()), (Some("economic-health"), Some("2026-W44"), Some("healthy")));
    assert_eq!((tag("health:revenue").as_deref(), tag("station").as_deref()), (Some("50.00"), Some("12D3KooWstation")));
    assert!(e.verify().is_ok());
    assert_eq!(count(&db, "SELECT COUNT(*) FROM journal WHERE stream = 'nightly'").await, 1);

    // Verrou du jour : la même nuit rejouée ne refait rien (pas de second snapshot, de second événement, de journal).
    let again = n.run_night(monday_night + 30, lon).await;
    assert!(again.already_done, "{:?}", again.steps);
    assert_eq!((*shares.lock().unwrap(), sink.0.lock().unwrap().len()), (1, 1));
    assert_eq!(count(&db, "SELECT COUNT(*) FROM journal WHERE stream = 'economy.3x13'").await, 1);

    // Le lendemain : une nouvelle nuit, mais aucune semaine à clôturer.
    let next = n.run_night(monday_night + 86_400, lon).await;
    assert!(next.all_ok() && !next.already_done, "{:?}", next.steps);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM journal WHERE stream = 'economy.3x13'").await, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clock_before_2026_stops_the_whole_night_without_touching_anything() {
    let dir = std::env::temp_dir().join(format!("astroportd-solar-rtc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    let sink = Arc::new(Sink::default());
    let calls = Arc::new(Mutex::new(0));
    let n = nightly(&dir, db.clone(), sink.clone(), hook(calls.clone(), "x"), hook(calls.clone(), "x")).await;
    let report = n.run_night(1_600_000_000, 0.0).await; // 2020 : RTC non initialisée
    assert_eq!(report.steps.len(), 1);
    assert!(!report.steps[0].ok && report.steps[0].detail.contains("horloge"), "{:?}", report.steps);
    assert_eq!(*calls.lock().unwrap(), 0);
    assert!(sink.0.lock().unwrap().is_empty());
    assert!(!dir.join("snapshots").exists());
    assert_eq!(count(&db, "SELECT COUNT(*) FROM done WHERE key LIKE 'nightly:%'").await, 0, "aucun verrou posé : la nuit reste à faire");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_step_is_reported_and_does_not_stop_the_next_ones() {
    let dir = std::env::temp_dir().join(format!("astroportd-solar-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    let sink = Arc::new(Sink::default());
    let boom: Hook = Arc::new(|_| Box::pin(async { Err("Ollama injoignable".to_owned()) }));
    let n = nightly(&dir, db.clone(), sink.clone(), boom, hook(Arc::new(Mutex::new(0)), "ok")).await;
    let report = n.run_night(at(2026, 11, 3, 20), 0.0).await;
    let failed: Vec<_> = report.steps.iter().filter(|s| !s.ok).map(|s| (s.name, s.detail.as_str())).collect();
    assert_eq!(failed, [("rêve", "Ollama injoignable")]);
    assert_eq!(report.steps.last().unwrap().name, "santé", "les étapes suivantes ont eu lieu");
    assert_eq!(sink.0.lock().unwrap().len(), 1);
    let logged: String = db.readers().read(|c| Ok(c.query_row("SELECT data FROM journal WHERE stream = 'nightly'", [], |r| r.get(0))?)).await.unwrap();
    assert!(logged.contains("Ollama injoignable"), "la nuit ratée est dans le journal : {logged}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_trigger_is_the_true_solar_20h12_of_the_station_longitude() {
    // Paris (2,35° E) le 2 novembre 2026 : le soleil vrai avance de 16,4 min (équation du temps) : 20h12 − 16,4 min − 9,4 min de longitude ≈ 19h46 UTC.
    let paris = Coordinates::new(48.85, 2.35).unwrap();
    let t = next_20h12(at(2026, 11, 2, 12), &paris).unwrap();
    let (h, m) = ((t % 86_400) / 3_600, (t % 3_600) / 60);
    assert_eq!(t / 86_400, at(2026, 11, 2, 0) / 86_400);
    assert_eq!((h, m), (19, 46));
}

async fn boot_station(tag: &str) -> (std::path::PathBuf, Arc<Db>, Nightly<Sink>) {
    let dir = std::env::temp_dir().join(format!("astroportd-solar-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("uplanet.db"), 2).unwrap());
    let n = nightly(&dir, db.clone(), Arc::new(Sink::default()), hook(Arc::default(), "ok"), hook(Arc::default(), "ok")).await;
    (dir, db, n)
}

async fn nights(db: &Db) -> Vec<String> {
    db.readers()
        .read(|c| {
            let mut s = c.prepare("SELECT key FROM done WHERE key LIKE 'nightly:%' ORDER BY key")?;
            let mut out = Vec::new();
            for r in s.query_map([], |r| r.get::<_, String>(0))? {
                out.push(r?);
            }
            Ok(out)
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn boot_catch_up_plays_the_missed_nights_in_the_background_job_and_only_once() {
    let paris = Coordinates::new(48.85, 2.35).unwrap();
    let mark = |db: Arc<Db>, date: &'static str| async move {
        db.writer().execute("INSERT INTO done(key, value, at) VALUES (?1, 'nightly', 1)", vec![V::Text(format!("nightly:{date}"))]).await.unwrap();
    };

    // 1. Station neuve, démarrée le matin avant le 20h12 solaire (19h46 UTC le 3 nov. ≈ 19h45) : rien à rattraper.
    let (dir, db, n) = boot_station("fresh-morning").await;
    assert!(n.catch_up(at(2026, 11, 3, 8), &paris).await.is_empty());
    assert!(nights(&db).await.is_empty());

    // 2. Station éteinte à 20h12 hier : la nuit de la veille est jouée, pas celle d'aujourd'hui (pas encore échue).
    let (dir2, db2, n2) = boot_station("yesterday").await;
    mark(db2.clone(), "2026-10-31").await;
    let r = n2.catch_up(at(2026, 11, 3, 8), &paris).await;
    assert_eq!(r.iter().map(|x| x.date.as_str()).collect::<Vec<_>>(), ["2026-11-02"]);
    assert!(r[0].all_ok(), "{:?}", r[0].steps);
    assert_eq!(nights(&db2).await, ["nightly:2026-10-31", "nightly:2026-11-02"]);
    // Idempotent : un second démarrage dans la même matinée ne refait rien.
    assert!(n2.catch_up(at(2026, 11, 3, 9), &paris).await.is_empty());
    // Le vrai 20h12 de ce soir reste disponible (le rattrapage n'a pas pris son verrou).
    assert!(!n2.run_night(at(2026, 11, 3, 20), 2.35).await.already_done);

    // 3. Station éteinte plusieurs jours, redémarrée après le 20h12 du jour : veille + jour courant, pas les nuits anciennes.
    let (dir3, db3, n3) = boot_station("days").await;
    mark(db3.clone(), "2026-10-28").await;
    let r = n3.catch_up(at(2026, 11, 3, 21), &paris).await;
    assert_eq!(r.iter().map(|x| x.date.as_str()).collect::<Vec<_>>(), ["2026-11-02", "2026-11-03"]);
    assert_eq!(nights(&db3).await, ["nightly:2026-10-28", "nightly:2026-11-02", "nightly:2026-11-03"]);
    assert!(n3.catch_up(at(2026, 11, 3, 22), &paris).await.is_empty());

    // 4. Station neuve démarrée après 20h12 : la nuit du jour est jouée.
    let (dir4, db4, n4) = boot_station("fresh-evening").await;
    let r = n4.catch_up(at(2026, 11, 3, 21), &paris).await;
    assert_eq!(r.iter().map(|x| x.date.as_str()).collect::<Vec<_>>(), ["2026-11-03"]);
    assert_eq!(nights(&db4).await, ["nightly:2026-11-03"]);

    // 5. Horloge RTC invalide (avant 2026) : aucun rattrapage, aucun verrou.
    let (dir5, db5, n5) = boot_station("rtc").await;
    mark(db5.clone(), "2026-10-31").await;
    assert!(n5.catch_up(at(2025, 6, 1, 21), &paris).await.is_empty());
    assert_eq!(nights(&db5).await, ["nightly:2026-10-31"]);

    for d in [dir, dir2, dir3, dir4, dir5] {
        let _ = std::fs::remove_dir_all(d);
    }
}
