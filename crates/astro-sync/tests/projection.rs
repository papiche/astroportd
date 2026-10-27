//! Projection Nostr → tables `astro-db`, et convergence replay / temps réel.

#![allow(clippy::unwrap_used)]

mod common;

use astro_db::Db;
use astro_sync::{Ingested, ProjectError, Projector, replay_local};
use common::*;

async fn dump(db: &Db) -> Vec<String> {
    db.readers()
        .read(|c| {
            let mut out = Vec::new();
            for (label, sql) in [
                ("sg", "SELECT owner_hex||'|'||pubkey||'|'||level||'|'||coalesce(via,'') FROM social_graph"),
                ("id", "SELECT hex||'|'||role||'|'||coalesce(email,'')||'|'||coalesce(g1pub_nostr,'')||'|'||coalesce(ipns,'')||'|'||coalesce(contract,'')||'|'||hosted FROM identities"),
                ("ln", "SELECT event_id||'|'||author||'|'||dest||'|'||amount||'|'||prev||'|'||d||'|'||is_mint FROM ledger_n2"),
                ("dn", "SELECT key||'|'||coalesce(value,'')||'|'||at FROM done"),
                ("kv", "SELECT scope||'|'||key||'|'||value FROM kv WHERE scope = 'economic_health'"),
            ] {
                let mut s = c.prepare(sql)?;
                for r in s.query_map([], |r| r.get::<_, String>(0))? {
                    out.push(format!("{label}:{}", r?));
                }
            }
            out.sort();
            Ok(out)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn contact_lists_build_n1_and_n2_and_follow_updates() {
    let dir = TempDir::new("graph");
    let db = open_db(&dir);
    host(&db, &ALICE).await;

    ingest(&db, &follows(&ALICE, 100, &[&BOB])).await;
    assert_eq!(graph(&db).await, [row("alice", "bob", 1, None)]);

    // Bob (N1) suit Carol, Dave et Alice : Carol et Dave deviennent N2 ; Alice n'est pas son propre contact.
    ingest(&db, &follows(&BOB, 200, &[&CAROL, &DAVE, &ALICE])).await;
    assert_eq!(
        graph(&db).await,
        [row("alice", "bob", 1, None), row("alice", "carol", 2, Some("bob")), row("alice", "dave", 2, Some("bob"))]
    );

    // Carol (N2) suit Erin : sans effet, un N2 n'étend pas le cercle.
    ingest(&db, &follows(&CAROL, 300, &[&ERIN])).await;
    assert_eq!(graph(&db).await.len(), 3);

    // Bob ne suit plus que Carol : Dave disparaît (liste remplaçable).
    ingest(&db, &follows(&BOB, 400, &[&CAROL])).await;
    assert_eq!(graph(&db).await, [row("alice", "bob", 1, None), row("alice", "carol", 2, Some("bob"))]);

    // Alice suit désormais Carol directement et plus Bob : Carol passe N1, Erin devient N2 via Carol.
    ingest(&db, &follows(&ALICE, 500, &[&CAROL])).await;
    assert_eq!(graph(&db).await, [row("alice", "carol", 1, None), row("alice", "erin", 2, Some("carol"))]);

    // Un inconnu qui n'est ni hébergé ni N1 n'ajoute rien.
    ingest(&db, &follows(&MALLORY, 600, &[&ALICE, &BOB])).await;
    assert_eq!(graph(&db).await.len(), 2);
}

#[tokio::test]
async fn graph_does_not_depend_on_arrival_order() {
    let events = [
        follows(&ALICE, 100, &[&BOB, &DAVE]),
        follows(&BOB, 200, &[&CAROL, &ERIN]),
        follows(&CAROL, 300, &[&ALICE]),
        follows(&DAVE, 150, &[&ERIN]),
    ];
    let mut results = Vec::new();
    for order in [[0, 1, 2, 3], [3, 2, 1, 0], [1, 0, 3, 2], [2, 3, 0, 1]] {
        let dir = TempDir::new("order");
        let db = open_db(&dir);
        host(&db, &ALICE).await;
        for i in order {
            ingest(&db, &events[i]).await;
        }
        results.push(graph(&db).await);
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]), "ordres d'arrivée divergents : {results:#?}");
    assert_eq!(
        results[0],
        [
            row("alice", "bob", 1, None),
            row("alice", "carol", 2, Some("bob")),
            row("alice", "dave", 1, None),
            row("alice", "erin", 2, Some("bob")),
        ]
    );
}

fn did_content(secret: &[u8; 32], email: &str, status: &str) -> String {
    let h = hex(secret);
    serde_json::json!({
        "@context": ["https://w3id.org/did/v1"],
        "id": format!("did:nostr:{h}"),
        "type": "DIDNostr",
        "alsoKnownAs": [format!("mailto:{email}"), "did:g1:6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN", "/ipns/k51qzi5uqu5dlvj2baxnqndepeb86cbk3ng7n3i46uzyxzyqj2xjonzllnv0v8"],
        "verificationMethod": [
            {"id": format!("did:nostr:{h}#key1"), "type": "Multikey", "publicKeyMultibase": format!("fe70102{h}")},
            {"id": format!("did:nostr:{h}#g1-key"), "type": "Ed25519VerificationKey2020", "publicKeyBase58": "6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN"}
        ],
        "metadata": {"email": email, "contractStatus": status, "language": "fr", "astroportStation": {"ipns": "12D3KooWFSoxH5cZqhpcywX4P4ZKgaU1yTzcopYEvVhSQfHdhwwJ"}}
    })
    .to_string()
}

#[tokio::test]
async fn did_projects_identity_without_touching_local_state() {
    let dir = TempDir::new("did");
    let db = open_db(&dir);
    host(&db, &ALICE).await;
    db.writer().execute("UPDATE identities SET role = 'captain', key_formula = 'legacy' WHERE hex = ?1", vec![hex(&ALICE).into()]).await.unwrap();

    let did = ev(&ALICE, 30_800, 100, vec![t(&["d", "did"]), t(&["t", "uplanet"]), t(&["t", "did-document"])], &did_content(&ALICE, "alice@example.org", "cooperative_member_satellite"));
    assert_eq!(ingest(&db, &did).await, Ingested::Inserted);
    let carol = ev(&CAROL, 30_800, 100, vec![t(&["d", "did"])], &did_content(&CAROL, "carol@example.org", "active_rental"));
    ingest(&db, &carol).await;

    let (a, c): (String, String) = db
        .readers()
        .read(|conn| {
            let q = |hex: String| -> Result<String, astro_db::DbError> {
                Ok(conn.query_row(
                    "SELECT role||'|'||email||'|'||g1pub_nostr||'|'||ipns||'|'||coalesce(contract,'-')||'|'||status||'|'||lang||'|'||hosted||'|'||key_formula||'|'||npub FROM identities WHERE hex = ?1",
                    [hex],
                    |r| r.get(0),
                )?)
            };
            Ok((q(hex(&ALICE))?, q(hex(&CAROL))?))
        })
        .await
        .unwrap();
    let ipns = "/ipns/k51qzi5uqu5dlvj2baxnqndepeb86cbk3ng7n3i46uzyxzyqj2xjonzllnv0v8";
    let g1 = "6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN";
    // Alice : rôle, hébergement et formule restent LOCAUX ; le DID n'apporte que email, clés liées, contrat.
    assert!(a.starts_with(&format!("captain|alice@example.org|{g1}|{ipns}|cooperative_member|cooperative_member_satellite|fr|1|legacy|npub1")), "{a}");
    assert!(c.starts_with(&format!("multipass|carol@example.org|{g1}|{ipns}|active_rental|active_rental|fr|0|isolated|npub1")), "{c}");
}

#[tokio::test]
async fn did_variants_node_cells_and_forged_ids() {
    let node = ev(&BOB, 30_800, 1, vec![t(&["d", "12D3KooWFSoxH5cZqhpcywX4P4ZKgaU1yTzcopYEvVhSQfHdhwwJ"])], &did_content(&BOB, "n@x.org", "infrastructure_contributor"));
    let plan = Projector::plan(&node).unwrap();
    assert_eq!(plan.len(), 2, "identité + nettoyage du registre du capital");
    let cell = ev(&BOB, 30_800, 1, vec![t(&["d", "did"]), t(&["t", "UMAP"])], "{}");
    assert!(Projector::plan(&cell).unwrap().is_empty(), "les DID de cellule ne sont pas des identités");
    let coop = ev(&BOB, 30_800, 1, vec![t(&["d", "cooperative-config"])], "{}");
    assert!(Projector::plan(&coop).unwrap().is_empty());
    let forged = ev(&BOB, 30_800, 1, vec![t(&["d", "did"])], &did_content(&CAROL, "x@y.z", "active_rental"));
    assert_eq!(Projector::plan(&forged), Err(ProjectError::BadDid("l'identifiant ne correspond pas à l'auteur")));
    let bare = ev(&BOB, 30_800, 1, vec![t(&["d", "did"]), t(&["email", "bob@example.org"])], "pas du json");
    assert_eq!(Projector::plan(&bare).unwrap().len(), 1, "DID minimal : tags seulement");
}

fn ledger_tags(d: &str, to: &str, amount: &str, prev: &str, mint: bool) -> Vec<Vec<String>> {
    let mut v = vec![t(&["d", d]), t(&["p", to]), t(&["amount", amount]), t(&["prev", prev]), t(&["t", "g1-n2"])];
    if mint {
        v.push(t(&["t", "mint"]));
    }
    v
}

#[tokio::test]
async fn ledger_events_are_validated_and_projected() {
    let dir = TempDir::new("ledger");
    let db = open_db(&dir);
    let to = hex(&DAVE);
    let good = ev(&CAROL, 30_852, 100, ledger_tags("n2-1-aaaaaaaa", &to, "12.5", "genesis", false), "");
    assert_eq!(ingest(&db, &good).await, Ingested::Inserted);
    let mint = ev(&BOB, 30_852, 101, ledger_tags("n2-2-bbbbbbbb", &to, "100", &"ab".repeat(32), true), "");
    ingest(&db, &mint).await;
    let rows: Vec<String> = db
        .readers()
        .read(|c| {
            let mut s = c.prepare("SELECT amount||'|'||prev||'|'||is_mint FROM ledger_n2 ORDER BY created_at")?;
            Ok(s.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(rows, ["12.5|genesis|0".to_string(), format!("100|{}|1", "ab".repeat(32))]);

    let bad = |tags| Projector::plan(&ev(&CAROL, 30_852, 1, tags, ""));
    assert!(matches!(bad(ledger_tags("d", &to, "0", "genesis", false)), Err(ProjectError::BadLedger("montant invalide"))));
    assert!(matches!(bad(ledger_tags("d", &to, "-5", "genesis", false)), Err(ProjectError::BadLedger("montant invalide"))));
    assert!(matches!(bad(ledger_tags("d", &to, "1.234", "genesis", false)), Err(ProjectError::BadLedger("montant invalide"))));
    assert!(matches!(bad(ledger_tags("d", &to, "1e3", "genesis", false)), Err(ProjectError::BadLedger("montant invalide"))));
    assert!(matches!(bad(ledger_tags("d", &hex(&CAROL), "1", "genesis", false)), Err(ProjectError::BadLedger("destinataire invalide"))));
    assert!(matches!(bad(ledger_tags("d", "pasunehex", "1", "genesis", false)), Err(ProjectError::BadLedger("destinataire invalide"))));
    assert!(matches!(bad(ledger_tags("d", &to, "1", "zzz", false)), Err(ProjectError::BadLedger("prev invalide"))));
    let mut no_t = ledger_tags("d", &to, "1", "genesis", false);
    no_t.pop();
    assert!(matches!(bad(no_t), Err(ProjectError::BadLedger("tag t=g1-n2 absent"))));
    let mut dup = ledger_tags("d", &to, "1", "genesis", false);
    dup.push(t(&["amount", "2"]));
    assert!(matches!(bad(dup), Err(ProjectError::BadLedger("tag en double"))));
    let mut no_d = ledger_tags("d", &to, "1", "genesis", false);
    no_d.remove(0);
    assert_eq!(bad(no_d), Err(ProjectError::MissingTag("d")));
}

#[tokio::test]
async fn economic_events_feed_kv_and_done_markers() {
    let dir = TempDir::new("eco");
    let db = open_db(&dir);
    let health = ev(&BOB, 30_850, 100, vec![t(&["d", "economic-health"]), t(&["week", "2026-W43"]), t(&["health:status", "healthy"]), t(&["station", "12D3KooWx"])], "");
    assert_eq!(ingest(&db, &health).await, Ingested::Inserted);
    let issuance = ev(&BOB, 30_851, 101, vec![t(&["d", "inv-42"]), t(&["email", "x@y.z"]), t(&["amount", "10"]), t(&["tier", "multipass"])], "");
    ingest(&db, &issuance).await;
    let (v, markers): (String, Vec<String>) = db
        .readers()
        .read(|c| {
            let v: String = c.query_row("SELECT value FROM kv WHERE scope='economic_health'", [], |r| r.get(0))?;
            let mut s = c.prepare("SELECT key FROM done ORDER BY key")?;
            Ok((v, s.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?))
        })
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&v).unwrap();
    assert_eq!(v["tags"]["health:status"], "healthy");
    assert_eq!(v["created_at"], 100);
    assert_eq!(markers, [format!("economic_health:{}:2026-W43", hex(&BOB)), format!("zen_issuance:{}:inv-42", hex(&BOB))]);

    // Un plan plus ancien n'écrase pas un état plus récent.
    let newer = ev(&BOB, 30_850, 300, vec![t(&["d", "economic-health"]), t(&["health:status", "degraded"])], "");
    let older = ev(&BOB, 30_850, 200, vec![t(&["d", "economic-health"]), t(&["health:status", "stale"])], "");
    db.writer().batch(Projector::plan(&newer).unwrap()).await.unwrap();
    db.writer().batch(Projector::plan(&older).unwrap()).await.unwrap();
    let status: String = db
        .readers()
        .read(|c| Ok(c.query_row("SELECT json_extract(value, '$.tags.\"health:status\"') FROM kv WHERE scope='economic_health'", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(status, "degraded");
    assert_eq!(Projector::plan(&ev(&BOB, 30_851, 1, vec![], "")), Err(ProjectError::MissingTag("d")));
    assert!(Projector::plan(&ev(&BOB, 1, 1, vec![], "")).unwrap().is_empty());
}

#[tokio::test]
async fn replay_rebuilds_exactly_what_live_projection_built() {
    let dir = TempDir::new("replay");
    let db = open_db(&dir);
    host(&db, &ALICE).await;
    let to = hex(&DAVE);
    let events = [
        follows(&ALICE, 100, &[&BOB, &DAVE]),
        follows(&BOB, 200, &[&CAROL, &ERIN]),
        ev(&ALICE, 30_800, 110, vec![t(&["d", "did"])], &did_content(&ALICE, "alice@example.org", "active_rental")),
        ev(&CAROL, 30_800, 120, vec![t(&["d", "did"])], &did_content(&CAROL, "carol@example.org", "cooperative_member_constellation")),
        ev(&CAROL, 30_852, 130, ledger_tags("n2-1-aaaaaaaa", &to, "5", "genesis", false), ""),
        ev(&BOB, 30_850, 140, vec![t(&["d", "economic-health"]), t(&["week", "2026-W43"]), t(&["health:status", "ok"])], ""),
        ev(&BOB, 30_851, 150, vec![t(&["d", "inv-1"]), t(&["amount", "3"])], ""),
        ev(&BOB, 1, 160, vec![], "note sans projection"),
    ];
    for e in &events {
        ingest(&db, e).await;
    }
    // Marqueur LOCAL (sans événement source) : doit survivre au replay.
    db.writer().execute("INSERT INTO done(key, value, at) VALUES ('local:weekly', 'x', 1)", vec![]).await.unwrap();
    let live = dump(&db).await;
    assert!(live.iter().any(|l| l.starts_with("sg:")) && live.iter().any(|l| l.starts_with("ln:")) && live.iter().any(|l| l.starts_with("kv:")));

    let stats = replay_local(&db, 3).await.unwrap();
    assert_eq!(stats.scanned, 7, "{stats:?}"); // 2 kind 3 + 2 DID + 1 ledger + 1 santé + 1 émission
    assert_eq!(stats.skipped, 0, "{stats:?}");
    assert_eq!(dump(&db).await, live, "le replay doit converger vers l'état temps réel");

    // Destruction des projections : le replay les reconstruit depuis nostr_events seul.
    db.writer()
        .batch(vec![
            astro_db::Statement::new("DELETE FROM social_graph", vec![]),
            astro_db::Statement::new("DELETE FROM ledger_n2", vec![]),
            astro_db::Statement::new("DELETE FROM identities WHERE hosted = 0", vec![]),
            astro_db::Statement::new("DELETE FROM kv", vec![]),
        ])
        .await
        .unwrap();
    replay_local(&db, 1000).await.unwrap();
    assert_eq!(dump(&db).await, live);
    assert!(live.iter().any(|l| l.contains("local:weekly")), "l'état local est préservé");
}

#[tokio::test]
async fn replay_survives_a_db_whose_projections_are_empty_and_reports_bad_events() {
    let dir = TempDir::new("replay-bad");
    let db = open_db(&dir);
    // Un DID falsifié et un ledger invalide sont stockés (ingestion) mais ne se projettent pas.
    let forged = ev(&BOB, 30_800, 1, vec![t(&["d", "did"])], &did_content(&CAROL, "x@y.z", "active_rental"));
    let bad_ledger = ev(&BOB, 30_852, 2, ledger_tags("d", &hex(&DAVE), "0", "genesis", false), "");
    ingest(&db, &forged).await;
    ingest(&db, &bad_ledger).await;
    let stats = replay_local(&db, 10).await.unwrap();
    assert_eq!((stats.scanned, stats.projected, stats.skipped), (2, 0, 2));
    assert_eq!(stats.errors.len(), 2);
    assert!(stats.errors[0].1.contains("DID invalide"));
}

#[tokio::test]
async fn ingest_is_atomic_and_deduplicated() {
    let dir = TempDir::new("ingest");
    let db = open_db(&dir);
    let e = ev(&BOB, 1, 100, vec![], "une fois");
    assert_eq!(ingest(&db, &e).await, Ingested::Inserted);
    assert_eq!(ingest(&db, &e).await, Ingested::Duplicate);
    let mut forged = e.clone();
    forged.content = "altéré".into();
    assert!(matches!(ingest(&db, &forged).await, Ingested::Rejected(_)));
    let claimed: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM claimed", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(claimed, 1, "un événement rejeté n'est pas réclamé");
    let old = ev(&BOB, 0, 100, vec![], "{\"name\":\"ancien\"}");
    let new = ev(&BOB, 0, 200, vec![], "{\"name\":\"nouveau\"}");
    ingest(&db, &new).await;
    assert_eq!(ingest(&db, &old).await, Ingested::Superseded);
    let ephemeral = ev(&BOB, 24_315, 1, vec![], "");
    assert!(matches!(ingest(&db, &ephemeral).await, Ingested::Rejected(_)));
}

const NODE_ID: &str = "12D3KooWFSoxH5cZqhpcywX4P4ZKgaU1yTzcopYEvVhSQfHdhwwJ";

fn node_did(ts: u64, assets: &serde_json::Value) -> astro_nostr_proto::Event {
    let mut doc: serde_json::Value = serde_json::from_str(&did_content(&BOB, "n@x.org", "infrastructure_contributor")).unwrap();
    doc["assets"] = assets.clone();
    ev(&BOB, 30_800, ts, vec![t(&["d", NODE_ID])], &doc.to_string())
}

async fn capex(db: &Db) -> Vec<String> {
    db.readers()
        .read(|c| {
            let mut s = c.prepare("SELECT id||'|'||coalesce(label,'')||'|'||coalesce(email,'')||'|'||coalesce(value_zen,'null')||'|'||coalesce(amortized_zen,'null')||'|'||coalesce(depreciable,'null')||'|'||coalesce(depreciation_weeks,'null')||'|'||coalesce(status,'null') FROM capital_assets ORDER BY id")?;
            let mut out = Vec::new();
            for r in s.query_map([], |r| r.get::<_, String>(0))? {
                out.push(r?);
            }
            Ok(out)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn node_did_assets_fill_capital_assets_and_converge() {
    let v1 = node_did(
        100,
        &serde_json::json!([
            {"id": "a1", "label": "Serveur", "asset_type": "machine", "email": "alice@example.org", "value_zen": 500.0, "amortized_zen": 50.0, "depreciable": true, "depreciation_weeks": 52, "date": "2026-10-01", "status": "active"},
            {"id": "", "label": "sans identifiant"},
            {"id": "neg", "value_zen": -3.0},
        ]),
    );
    let v2 = node_did(200, &serde_json::json!([{"id": "a2", "label": "Antenne", "email": "carol@example.org", "value_zen": 80.0, "amortized_zen": 0.0, "depreciable": false, "depreciation_weeks": 0, "status": "active"}]));
    let mut results = Vec::new();
    for order in [[0, 1], [1, 0]] {
        let dir = TempDir::new("capex");
        let db = open_db(&dir);
        for i in order {
            ingest(&db, [&v1, &v2][i]).await;
        }
        results.push(capex(&db).await);
        // Le replay reconstruit la même table depuis l'historique des DID.
        db.writer().execute("DELETE FROM capital_assets", vec![]).await.unwrap();
        replay_local(&db, 100).await.unwrap();
        assert_eq!(capex(&db).await, results[0], "replay");
    }
    assert_eq!(results[0], results[1], "ordre d'arrivée");
    assert_eq!(results[0], ["a2|Antenne|carol@example.org|80.0|0.0|0|0|active"], "la version la plus récente remplace l'instantané");

    let dir = TempDir::new("capex1");
    let db = open_db(&dir);
    ingest(&db, &v1).await;
    // L'actif sans identifiant est ignoré ; un montant négatif devient NULL (jamais inventé) et ne casse pas la ligne.
    let rows = capex(&db).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0], "a1|Serveur|alice@example.org|500.0|50.0|1|52|active");
    assert_eq!(rows[1], "neg|||null|null|null|null|null");
}
