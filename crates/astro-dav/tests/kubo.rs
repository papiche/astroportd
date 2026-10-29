//! Adaptateur Kubo contre un faux démon IPFS (API RPC) : écriture en flux, lecture avec Range, désépinglage, repli local.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_dav::blob::{AutoBlobStore, BlobError, BlobStore, BlobWriter, FsBlobStore, KuboBlobStore, cid_raw_sha256, is_cid};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use common::*;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[derive(Default)]
struct Mock {
    blobs: HashMap<String, Vec<u8>>,
    pinned: HashSet<String>,
    add_queries: Vec<String>,
    cat_queries: Vec<String>,
    calls: Vec<String>,
    fail_add: bool,
}

type Shared = Arc<Mutex<Mock>>;

fn query_param(q: &str, name: &str) -> Option<String> {
    q.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(str::to_owned))
}

fn json_error(msg: &str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{{\"Message\":\"{msg}\",\"Code\":0,\"Type\":\"error\"}}")).into_response()
}

async fn kubo(State(m): State<Shared>, req: Request) -> Response {
    let (path, query) = (req.uri().path().to_owned(), req.uri().query().unwrap_or("").to_owned());
    let boundary = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).and_then(|v| v.split("boundary=").nth(1)).map(str::to_owned);
    m.lock().unwrap().calls.push(path.clone());
    if req.method() != "POST" {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    match path.as_str() {
        "/api/v0/id" => "{\"ID\":\"12D3KooWMock\"}".into_response(),
        "/api/v0/add" => {
            let body = axum::body::to_bytes(req.into_body(), 64 << 20).await.unwrap();
            let mut g = m.lock().unwrap();
            g.add_queries.push(query);
            if g.fail_add {
                return json_error("disque plein");
            }
            let b = boundary.unwrap();
            let start = body.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let end = body.len() - format!("\r\n--{b}--\r\n").len();
            assert_eq!(&body[end..], format!("\r\n--{b}--\r\n").as_bytes(), "le multipart doit se terminer correctement");
            let content = body[start..end].to_vec();
            let cid = cid_raw_sha256(&Sha256::digest(&content).into());
            let size = content.len();
            g.blobs.insert(cid.clone(), content);
            g.pinned.insert(cid.clone());
            format!("{{\"Name\":\"blob\",\"Hash\":\"{cid}\",\"Size\":\"{size}\"}}\n").into_response()
        }
        "/api/v0/files/stat" => {
            let cid = query_param(&query, "arg").unwrap().trim_start_matches("/ipfs/").to_owned();
            match m.lock().unwrap().blobs.get(&cid) {
                Some(b) => format!("{{\"Size\":{},\"CumulativeSize\":{},\"Type\":\"file\"}}", b.len(), b.len()).into_response(),
                None => json_error("file does not exist"),
            }
        }
        "/api/v0/cat" => {
            let (cid, offset, length) = (query_param(&query, "arg").unwrap(), query_param(&query, "offset"), query_param(&query, "length"));
            let mut g = m.lock().unwrap();
            g.cat_queries.push(query.clone());
            let Some(b) = g.blobs.get(&cid) else { return json_error("merkledag: not found") };
            let off: usize = offset.map_or(0, |o| o.parse().unwrap());
            let len: usize = length.map_or(b.len() - off, |l| l.parse().unwrap());
            let slice = b[off..off + len].to_vec();
            let chunks: Vec<Result<Vec<u8>, std::io::Error>> = slice.chunks(10_000).map(|c| Ok(c.to_vec())).collect();
            Body::from_stream(futures_util::stream::iter(chunks)).into_response()
        }
        "/api/v0/pin/rm" => {
            let cid = query_param(&query, "arg").unwrap();
            let mut g = m.lock().unwrap();
            if g.pinned.remove(&cid) {
                g.blobs.remove(&cid); // le ramasse-miettes est supposé immédiat
                "{\"Pins\":[]}".into_response()
            } else {
                json_error("not pinned or pinned indirectly")
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn mock_kubo() -> (String, Shared) {
    let state: Shared = Arc::default();
    let app = axum::Router::new().fallback(axum::routing::any(kubo)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, state)
}

#[tokio::test]
async fn streaming_add_returns_the_cid_and_pins_with_cid_v1_raw_leaves() {
    let (url, mock) = mock_kubo().await;
    let store = KuboBlobStore::new(&url);
    assert!(store.ping().await);
    let data = sample(300_000, 4);
    let mut w = store.begin().await.unwrap();
    for piece in data.chunks(70_001) {
        w.write_all(piece).await.unwrap();
    }
    let meta = w.commit().await.unwrap();
    assert!(is_cid(&meta.cid));
    assert_eq!(meta.size, 300_000);
    assert_eq!(meta.cid, cid_raw_sha256(&Sha256::digest(&data).into()));
    let g = mock.lock().unwrap();
    assert_eq!(g.blobs[&meta.cid], data, "le contenu arrive intact, enveloppe multipart retirée");
    assert!(g.pinned.contains(&meta.cid));
    let q = &g.add_queries[0];
    for p in ["cid-version=1", "raw-leaves=true", "pin=true"] {
        assert!(q.contains(p), "{p} absent de {q}");
    }
}

#[tokio::test]
async fn reads_support_seek_and_partial_reads_through_cat_offset() {
    let (url, mock) = mock_kubo().await;
    let store = KuboBlobStore::new(&url);
    let data = sample(100_000, 9);
    let mut w = store.begin().await.unwrap();
    w.write_all(&data).await.unwrap();
    let cid = w.commit().await.unwrap().cid;

    let (mut r, len) = store.open(&cid).await.unwrap();
    assert_eq!(len, 100_000);
    let mut all = Vec::new();
    r.read_to_end(&mut all).await.unwrap();
    assert_eq!(all, data);
    // Seek puis lecture partielle : n'appelle `cat` que pour la plage utile.
    mock.lock().unwrap().cat_queries.clear();
    r.seek(std::io::SeekFrom::Start(65_536)).await.unwrap();
    let mut part = vec![0u8; 100];
    r.read_exact(&mut part).await.unwrap();
    assert_eq!(part, data[65_536..65_636]);
    assert!(mock.lock().unwrap().cat_queries[0].contains("offset=65536"));
    r.seek(std::io::SeekFrom::End(-10)).await.unwrap();
    let mut tail = Vec::new();
    r.read_to_end(&mut tail).await.unwrap();
    assert_eq!(tail, data[99_990..]);
    assert_eq!(r.seek(std::io::SeekFrom::Current(-5)).await.unwrap(), 99_995);
    r.seek(std::io::SeekFrom::Start(200_000)).await.unwrap();
    assert_eq!(r.read(&mut part).await.unwrap(), 0, "au-delà de la fin : EOF");
    assert!(r.seek(std::io::SeekFrom::End(-200_000)).await.is_err());
    // Inconnu / CID invalide.
    assert!(matches!(store.open("bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku").await, Err(BlobError::NotFound)));
    assert!(matches!(store.open("../etc").await, Err(BlobError::BadCid)));
}

#[tokio::test]
async fn remove_unpins_and_is_idempotent() {
    let (url, mock) = mock_kubo().await;
    let store = KuboBlobStore::new(&url);
    let mut w = store.begin().await.unwrap();
    w.write_all(b"a supprimer").await.unwrap();
    let cid = w.commit().await.unwrap().cid;
    store.remove(&cid).await.unwrap();
    assert!(!mock.lock().unwrap().pinned.contains(&cid));
    store.remove(&cid).await.unwrap();
    assert!(mock.lock().unwrap().calls.iter().filter(|c| c.ends_with("pin/rm")).count() == 2);
    assert!(matches!(store.remove("zzz").await, Err(BlobError::BadCid)));
}

#[tokio::test]
async fn kubo_errors_surface_and_abandoned_writes_pin_nothing() {
    let (url, mock) = mock_kubo().await;
    let store = KuboBlobStore::new(&url);
    mock.lock().unwrap().fail_add = true;
    let mut w = store.begin().await.unwrap();
    w.write_all(b"x").await.unwrap();
    let err = w.commit().await.unwrap_err().to_string();
    assert!(err.contains("disque plein"), "{err}");
    mock.lock().unwrap().fail_add = false;
    {
        let mut w = store.begin().await.unwrap();
        w.write_all(b"abandonne").await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(mock.lock().unwrap().pinned.is_empty(), "une écriture non validée n'épingle rien");
    // Démon injoignable.
    let dead = KuboBlobStore::new("http://127.0.0.1:1");
    assert!(!dead.ping().await);
    let mut w = dead.begin().await.unwrap();
    let _ = w.write_all(b"x").await;
    assert!(w.commit().await.is_err());
}

#[tokio::test]
async fn auto_store_falls_back_to_disk_and_returns_to_kubo_when_it_comes_back() {
    let dir = TempDir::new("auto");
    let fs = FsBlobStore::open(dir.0.join("blobs")).unwrap();
    // Kubo absent : tout va sur disque.
    let auto = AutoBlobStore::new(Some(KuboBlobStore::new("http://127.0.0.1:1")), fs.clone()).with_health_ttl(Duration::from_millis(50));
    let mut w = auto.begin().await.unwrap();
    w.write_all(b"local").await.unwrap();
    let local = w.commit().await.unwrap();
    assert!(fs.open(&local.cid).await.is_ok());
    // Sans Kubo configuré : idem.
    let only_fs = AutoBlobStore::new(None, fs.clone());
    let (mut r, _) = only_fs.open(&local.cid).await.unwrap();
    let mut got = String::new();
    r.read_to_string(&mut got).await.unwrap();
    assert_eq!(got, "local");

    // Kubo démarre : les nouvelles écritures vont à Kubo, les anciens blobs locaux restent lisibles.
    let (url, mock) = mock_kubo().await;
    let auto = AutoBlobStore::new(Some(KuboBlobStore::new(&url)), fs.clone()).with_health_ttl(Duration::from_millis(1));
    let mut w = auto.begin().await.unwrap();
    w.write_all(b"distant").await.unwrap();
    let remote = w.commit().await.unwrap();
    assert!(mock.lock().unwrap().pinned.contains(&remote.cid));
    assert!(fs.open(&remote.cid).await.is_err(), "pas de copie locale");
    let (mut r, len) = auto.open(&remote.cid).await.unwrap();
    assert_eq!(len, 7);
    r.seek(std::io::SeekFrom::Start(2)).await.unwrap();
    let mut rest = String::new();
    r.read_to_string(&mut rest).await.unwrap();
    assert_eq!(rest, "stant");
    let (mut r, _) = auto.open(&local.cid).await.unwrap();
    let mut old = String::new();
    r.read_to_string(&mut old).await.unwrap();
    assert_eq!(old, "local");
    // Suppression des deux côtés.
    auto.remove(&local.cid).await.unwrap();
    auto.remove(&remote.cid).await.unwrap();
    assert!(matches!(auto.open(&remote.cid).await, Err(BlobError::NotFound)));
    assert!(matches!(auto.open(&local.cid).await, Err(BlobError::NotFound)));
}

#[tokio::test]
async fn webdav_runs_on_kubo_with_range_reads_and_a_replayable_manifest() {
    let (url, mock) = mock_kubo().await;
    let dir = TempDir::new("davkubo");
    let store = Arc::new(KuboBlobStore::new(&url));
    let (db, app, publisher) = serve_on(&dir, store.clone(), |_| {}).await;
    let a = alice();
    let plain = sample(200_000, 21);
    assert_eq!(call(&app, "PUT", "/dav/photo.jpg", Some(&a), &[], streamed(plain.clone(), 30_000)).await.status(), 201);
    // Le blob chiffré (pas le clair) est épinglé dans Kubo.
    {
        let g = mock.lock().unwrap();
        assert_eq!(g.pinned.len(), 1);
        let blob = g.blobs.values().next().unwrap();
        assert_eq!(&blob[..5], b"UENC\x02");
        assert_eq!(blob.len(), 14 + 200_000 + 4 * 16);
    }
    let (status, _, body) = bytes(call(&app, "GET", "/dav/photo.jpg", Some(&a), &[], Body::empty()).await).await;
    assert_eq!((status, body == plain), (200, true));
    mock.lock().unwrap().cat_queries.clear();
    let (status, headers, part) = bytes(call(&app, "GET", "/dav/photo.jpg", Some(&a), &[("range", "bytes=131072-131081")], Body::empty()).await).await;
    assert_eq!((status, part.as_slice(), headers["content-range"].to_str().unwrap()), (206, &plain[131_072..131_082], "bytes 131072-131081/200000"));
    let q = mock.lock().unwrap().cat_queries.clone();
    assert!(q.iter().any(|c| c.contains("offset=") && !c.contains("offset=0&")), "seul le chunk utile est relu : {q:?}");
    // Manifeste publié sur Kubo, puis rejouable après purge de la base.
    assert!(publisher.flush().await.is_empty());
    assert_eq!(mock.lock().unwrap().pinned.len(), 2, "fichier + manifeste");
    let owner = hex(&ALICE);
    let ev = db
        .readers()
        .read(move |c| {
            let f = astro_nostr_proto::Filter { kinds: Some(vec![30_078]), authors: Some(vec![owner]), ..Default::default() };
            Ok(astro_db::events::query_events(c, &f, u64::MAX / 2, 5)?.pop().unwrap())
        })
        .await
        .unwrap();
    db.writer().batch(vec![astro_db::Statement::new("DELETE FROM ucloud_entries", vec![]), astro_db::Statement::new("DELETE FROM ucloud_keyring", vec![])]).await.unwrap();
    astro_dav::manifest::restore_from_event(&db, store.as_ref(), keys().as_ref(), &ev).await.unwrap();
    let (_, _, again) = bytes(call(&app, "GET", "/dav/photo.jpg", Some(&a), &[], Body::empty()).await).await;
    assert_eq!(again, plain);
    // Suppression : le blob est désépinglé.
    assert_eq!(call(&app, "DELETE", "/dav/photo.jpg", Some(&a), &[], Body::empty()).await.status(), 204);
    assert_eq!(mock.lock().unwrap().pinned.len(), 1, "il ne reste que le manifeste");
}

#[test]
fn providers_are_read_from_the_findprovs_stream() {
    let ndjson = concat!(
        r#"{"ID":"","Extra":"","Type":6,"Responses":null}"#, "\n",
        r#"{"ID":"","Extra":"","Type":4,"Responses":[{"ID":"12D3KooWA","Addrs":[]},{"ID":"12D3KooWB","Addrs":[]}]}"#, "\n",
        r#"{"ID":"","Extra":"","Type":4,"Responses":[{"ID":"12D3KooWA","Addrs":[]}]}"#, "\n",
        r#"{"ID":"12D3KooWZ","Extra":"","Type":1,"Responses":null}"#, "\n",
        r#"{"ID":"","Extra":"","Type":4,"Resp"#, // flux coupé à l'échéance
    );
    assert_eq!(astro_dav::kubo::parse_providers(ndjson), ["12D3KooWA", "12D3KooWB"], "dédoublonné, trié, requêtes DHT et ligne tronquée ignorées");
}

#[tokio::test]
async fn providers_keeps_what_arrived_before_the_deadline() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let cid = cid_raw_sha256(&Sha256::digest(b"fournisseurs").into());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let expected = format!("POST /api/v0/routing/findprovs?arg={cid}&num-providers=3");
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let n = s.read(&mut buf).await.unwrap();
        let head = String::from_utf8_lossy(&buf[..n]).into_owned();
        assert!(head.starts_with(&expected), "{head}");
        let line = r#"{"ID":"","Extra":"","Type":4,"Responses":[{"ID":"12D3KooWA","Addrs":[]}]}"#;
        s.write_all(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{line}\n\r\n", line.len() + 1).as_bytes()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await; // la recherche dure : jamais de fin de flux
    });
    let store = astro_dav::kubo::KuboBlobStore::new(&format!("http://{addr}"));
    let got = store.providers(&cid, 3, std::time::Duration::from_millis(400)).await.unwrap();
    assert_eq!(got, ["12D3KooWA"]);
    assert!(store.providers("../x", 3, std::time::Duration::from_millis(100)).await.is_err(), "CID invalide refusé avant toute requête");
}
