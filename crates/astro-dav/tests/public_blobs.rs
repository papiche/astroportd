//! Blobs publics : images en clair servies à `/ipfs/<cid>` par toute station, rapatriées chez les pairs, jamais autre chose.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use astro_dav::public::{self, MAX_PUBLIC_BYTES};
use astro_dav::{BlobStore, BlobWriter, FsBlobStore};
use axum::body::Body;
use common::*;
use tokio::io::AsyncWriteExt;

const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 2, 3, 4, 5, 6, 7, 8];

async fn store(s: &impl BlobStore, data: &[u8]) -> String {
    let mut w = s.begin().await.unwrap();
    w.write_all(data).await.unwrap();
    w.commit().await.unwrap().cid
}

async fn get(b: &Bench, cid: &str) -> axum::http::Response<Body> {
    call(&b.app, "GET", &format!("/ipfs/{cid}"), None, &[], Body::empty()).await
}

#[tokio::test]
async fn a_public_image_is_served_in_clear_even_with_the_encrypted_gateway_off() {
    let b = bench("public-gw").await; // passerelle chiffrée coupée (défaut)
    let cid = store(b.blobs.as_ref(), JPEG).await;
    assert_eq!(get(&b, &cid).await.status(), 404, "un blob non inscrit n'est jamais servi");
    public::register(&b.db, &cid, "image/jpeg", JPEG.len() as u64, None).await.unwrap();
    let r = get(&b, &cid).await;
    assert_eq!(r.status(), 200);
    for (h, v) in [("content-type", "image/jpeg"), ("x-content-type-options", "nosniff"), ("access-control-allow-origin", "*")] {
        assert_eq!(r.headers()[h], v, "{h}");
    }
    assert!(r.headers()["cache-control"].to_str().unwrap().contains("immutable"));
    assert!(r.headers()["content-security-policy"].to_str().unwrap().contains("sandbox"));
    assert_eq!(bytes(r).await.2, JPEG);
    // Mauvais CID : jamais d'accès au disque.
    for bad in ["pas-un-cid", "..%2f..%2fetc%2fpasswd", &cid.to_uppercase()] {
        assert_eq!(get(&b, bad).await.status(), 404, "{bad}");
    }
}

#[tokio::test]
async fn the_hook_is_asked_only_for_unknown_cids_and_its_refusal_is_a_404() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let b = bench_with("public-hook", move |cfg| {
        cfg.public_fetch = Some(Arc::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { false })
        }));
    })
    .await;
    let ghost = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
    assert_eq!(get(&b, ghost).await.status(), 404);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let known = store(b.blobs.as_ref(), JPEG).await;
    public::register(&b.db, &known, "image/jpeg", JPEG.len() as u64, None).await.unwrap();
    assert_eq!(get(&b, &known).await.status(), 200);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "un blob déjà public ne sollicite pas les pairs");
}

#[test]
fn only_real_images_are_recognised_by_their_magic_bytes() {
    assert_eq!(public::sniff(JPEG), Some("image/jpeg"));
    assert_eq!(public::sniff(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
    assert_eq!(public::sniff(b"RIFF\x10\0\0\0WEBPVP8 "), Some("image/webp"));
    for hostile in [&b"UENC\x02\x01\x10....................."[..], b"<svg onload=alert(1)>", b"<!doctype html>", b"GIF89a", b""] {
        assert_eq!(public::sniff(hostile), None, "{hostile:?}");
    }
}

struct Rig {
    db: Arc<astro_db::Db>,
    local: FsBlobStore,
    dir: std::path::PathBuf,
}

fn rig(tag: &str) -> Rig {
    let dir = std::env::temp_dir().join(format!("astro-public-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Rig { db: Arc::new(astro_db::Db::open(&dir.join("u.db"), 2).unwrap()), local: FsBlobStore::open(dir.join("blobs")).unwrap(), dir }
}

/// Un « pair » qui répond avec `data` : le rapatriement écrit les octets dans le magasin local, comme `fetch_blob`.
async fn pulled(r: &Rig, data: Vec<u8>) -> (String, bool) {
    let cid = {
        let tmp = FsBlobStore::open(r.dir.join("pair")).unwrap();
        store(&tmp, &data).await
    };
    let ok = public::pull(&r.db, &r.local, &cid, || async {
        store(&r.local, &data).await;
        true
    })
    .await;
    (cid, ok)
}

#[tokio::test]
async fn a_pulled_image_becomes_public_but_anything_else_is_removed_from_the_disk() {
    let r = rig("pull");
    let (cid, ok) = pulled(&r, JPEG.to_vec()).await;
    assert!(ok && public::mime_of(&r.db, &cid).await.as_deref() == Some("image/jpeg"));

    // Un blob chiffré de l'uCloud d'un ami, demandé par un inconnu : refusé, et pas gardé.
    let (uenc, ok) = pulled(&r, b"UENC\x02\x01\x10secret-secret-secret".to_vec()).await;
    assert!(!ok && public::mime_of(&r.db, &uenc).await.is_none());
    assert!(r.local.open(&uenc).await.is_err(), "retiré du disque");

    // Trop gros, même s'il commence comme une image.
    let mut big = JPEG.to_vec();
    big.resize(usize::try_from(MAX_PUBLIC_BYTES).unwrap() + 1, 7);
    let (huge, ok) = pulled(&r, big).await;
    assert!(!ok && r.local.open(&huge).await.is_err());
    let _ = std::fs::remove_dir_all(&r.dir);
}

#[tokio::test]
async fn a_private_blob_already_on_disk_is_never_promoted_nor_removed_nor_fetched() {
    let r = rig("private");
    let cid = store(&r.local, JPEG).await; // présent, mais non inscrit : c'est le blob d'un uCloud
    let asked = AtomicUsize::new(0);
    let ok = public::pull(&r.db, &r.local, &cid, || async {
        asked.fetch_add(1, Ordering::SeqCst);
        true
    })
    .await;
    assert!(!ok && asked.load(Ordering::SeqCst) == 0 && r.local.open(&cid).await.is_ok());
    assert!(public::mime_of(&r.db, &cid).await.is_none());
    assert!(!public::pull(&r.db, &r.local, "pas-un-cid", || async { true }).await);
    let _ = std::fs::remove_dir_all(&r.dir);
}

#[tokio::test]
async fn unknown_cids_are_rate_limited_and_a_miss_is_not_asked_twice() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let b = bench_with("public-gate", move |cfg| {
        cfg.public_fetch = Some(Arc::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { false })
        }));
    })
    .await;
    let cid = |n: u32| {
        // CID syntaxiquement valides et tous différents (n écrit en base 26 sur 4 lettres).
        let (d, c, e, f) = (n / 17_576 % 26, n / 676 % 26, n / 26 % 26, n % 26);
        let l = |x: u32| char::from(b'a' + u8::try_from(x).unwrap());
        format!("bafkrei{}{}{}{}{}", "a".repeat(48), l(d), l(c), l(e), l(f))
    };
    assert_eq!(get(&b, &cid(1)).await.status(), 404);
    assert_eq!(get(&b, &cid(1)).await.status(), 404);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "un CID déjà cherché en vain ne sollicite plus les pairs");
    let mut limited = 0;
    for n in 2..200 {
        if get(&b, &cid(n)).await.status() == 429 {
            limited += 1;
        }
    }
    assert!(limited > 0 && calls.load(Ordering::SeqCst) <= public::MAX_PULLS_PER_MINUTE, "{limited} refus, {} questions", calls.load(Ordering::SeqCst));
}

#[test]
fn the_gate_bounds_concurrency_and_its_memory() {
    let gate = public::PullGate::default();
    let permits: Vec<_> = (0..public::MAX_CONCURRENT_PULLS).map(|i| gate.admit(&format!("c{i}"))).collect();
    assert!(permits.iter().all(|p| matches!(p, public::Admit::Go(_))));
    assert!(matches!(gate.admit("autre"), public::Admit::Busy), "trop de rapatriements simultanés");
    drop(permits);
    assert!(matches!(gate.admit("autre"), public::Admit::Go(_)), "les jetons sont rendus");
    for i in 0..public::MAX_MISSES + 50 {
        gate.miss(&format!("m{i}"));
    }
    let known = (0..public::MAX_MISSES + 50).filter(|i| matches!(gate.admit(&format!("m{i}")), public::Admit::KnownMiss)).count();
    assert!(known <= public::MAX_MISSES, "mémoire bornée : {known}");
}

#[tokio::test]
async fn public_images_count_in_the_owners_quota_and_the_quota_is_enforced() {
    let r = rig("quota");
    let (owner, other) = ("ab".repeat(32), "cd".repeat(32));
    let img = |seed: u32| {
        let mut v = JPEG.to_vec();
        v.resize(1000, 7);
        v[32..36].copy_from_slice(&seed.to_be_bytes()); // chaque image a son propre CID
        v
    };
    let me = |quota| Some(public::Owner { hex: &owner, quota });
    // Quota de 2 500 octets : deux images de 1 000 octets passent, la troisième est refusée.
    assert!(public::put(&r.db, &r.local, &img(1), "image/jpeg", me(2500)).await.is_ok());
    assert!(public::put(&r.db, &r.local, &img(2), "image/jpeg", me(2500)).await.is_ok());
    let refused = public::put(&r.db, &r.local, &img(3), "image/jpeg", me(2500)).await.unwrap_err();
    assert!(refused.contains("quota"), "{refused}");
    // Elles comptent dans l'occupation du compte, comme ses fichiers (c'est ce que lisent WebDAV et le tableau de bord).
    let used = r.db.readers().read({ let o = owner.clone(); move |c| astro_dav::vfs::used_bytes(c, &o).map_err(|_| astro_db::DbError::Aborted) }).await.unwrap();
    assert_eq!(used, 2000);
    // Un fichier d'uCloud déjà là entame le même quota.
    r.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, size, data) VALUES (?1, '/a.bin', '/', 'file', 400, '{}')", vec![owner.clone().into()]).await.unwrap();
    assert!(public::put(&r.db, &r.local, &img(4), "image/jpeg", me(2500)).await.is_err(), "2 000 + 400 + 1 000 > 2 500");
    // La même image redéposée ne compte pas deux fois ; un autre compte a son propre quota ; un blob rapatrié ne coûte rien à personne.
    assert!(public::put(&r.db, &r.local, &img(1), "image/jpeg", me(2500)).await.is_ok());
    assert!(public::put(&r.db, &r.local, &img(9), "image/jpeg", Some(public::Owner { hex: &other, quota: 2500 })).await.is_ok());
    assert!(public::put(&r.db, &r.local, &img(10), "image/jpeg", None).await.is_ok());
    // Un faux type est refusé avant tout stockage.
    assert!(public::put(&r.db, &r.local, b"<svg/>", "image/jpeg", None).await.is_err());
    assert!(public::put(&r.db, &r.local, JPEG, "image/png", None).await.is_err(), "le type annoncé doit être celui des octets");
    let _ = std::fs::remove_dir_all(&r.dir);
}
