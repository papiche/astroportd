//! Miniatures WebP : générées après le dépôt, servies par `?thumb`, chiffrées, ancrées dans le manifeste,
//! nettoyées au remplacement et à la suppression.

#![allow(clippy::unwrap_used)]

mod common;

use std::io::Cursor;
use std::time::Duration;

use astro_dav::BlobStore;
use axum::body::Body;
use common::*;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};

fn jpeg(w: u32, h: u32, seed: u8) -> Vec<u8> {
    let img = RgbImage::from_fn(w, h, |x, y| Rgb([(x % 251) as u8 ^ seed, (y % 241) as u8, seed]));
    let mut out = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(img).write_to(&mut out, ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

async fn thumb_status(b: &Bench, path: &str) -> (u16, Vec<u8>) {
    let (s, _, body) = bytes(call(&b.app, "GET", &format!("/dav{path}?thumb"), Some(&alice()), &[], Body::empty()).await).await;
    (s, body)
}

/// La miniature est générée en arrière-plan : on attend qu'elle paraisse.
async fn wait_thumb(b: &Bench, path: &str) -> Vec<u8> {
    for _ in 0..100 {
        let (s, body) = thumb_status(b, path).await;
        if s == 200 {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    unreachable!("pas de miniature pour {path}");
}

async fn thumb_cid(b: &Bench, path: &str) -> Option<String> {
    let (owner, path) = (hex(&ALICE), path.to_owned());
    b.db.readers().read(move |c| Ok(c.query_row("SELECT json_extract(data, '$.thumb.cid') FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", [owner, path], |r| r.get::<_, Option<String>>(0)).unwrap_or(None))).await.unwrap()
}

#[tokio::test]
async fn an_uploaded_photo_gets_a_small_encrypted_webp_served_by_thumb() {
    let b = bench("thumb").await;
    let original = jpeg(1920, 1080, 7);
    assert_eq!(call(&b.app, "PUT", "/dav/vacances.jpg", Some(&alice()), &[("content-type", "image/jpeg")], Body::from(original.clone())).await.status(), 201);
    let webp = wait_thumb(&b, "/vacances.jpg").await;
    let img = image::load_from_memory_with_format(&webp, ImageFormat::WebP).unwrap();
    assert_eq!((img.width(), img.height()), (320, 180));
    assert!(webp.len() < original.len() / 5, "{} octets pour {}", webp.len(), original.len());

    // L'original est intact, la miniature est un blob chiffré (jamais « RIFF » en clair sur le disque).
    let (_, _, full) = bytes(call(&b.app, "GET", "/dav/vacances.jpg", Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!(full, original);
    let cid = thumb_cid(&b, "/vacances.jpg").await.unwrap();
    let (mut reader, _) = b.blobs.open(&cid).await.unwrap();
    let mut raw = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut raw).await.unwrap();
    assert!(!raw.windows(4).any(|w| w == b"RIFF") && !raw.windows(4).any(|w| w == b"WEBP"), "miniature stockée en clair");

    // Cache : ETag = CID, 304 sur revalidation ; dimensions de l'original dans l'entrée ; autre compte : rien.
    let r = call(&b.app, "GET", "/dav/vacances.jpg?thumb", Some(&alice()), &[], Body::empty()).await;
    assert_eq!(r.headers()["content-type"], "image/webp");
    assert!(r.headers()["cache-control"].to_str().unwrap().contains("immutable"));
    let etag = r.headers()["etag"].to_str().unwrap().to_owned();
    assert_eq!(call(&b.app, "GET", "/dav/vacances.jpg?thumb", Some(&alice()), &[("if-none-match", &etag)], Body::empty()).await.status(), 304);
    let (o, p) = (hex(&ALICE), "/vacances.jpg".to_owned());
    let (ow, oh): (i64, i64) = b.db.readers().read(move |c| Ok(c.query_row("SELECT json_extract(data,'$.thumb.ow'), json_extract(data,'$.thumb.oh') FROM ucloud_entries WHERE owner_hex=?1 AND path=?2", [o, p], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
    assert_eq!((ow, oh), (1920, 1080));
    assert_eq!(call(&b.app, "GET", "/dav/vacances.jpg?thumb", Some(&bob()), &[], Body::empty()).await.status(), 404);
}

#[tokio::test]
async fn non_images_and_broken_images_still_upload_without_a_thumbnail() {
    let b = bench("thumb-none").await;
    assert_eq!(call(&b.app, "PUT", "/dav/notes.txt", Some(&alice()), &[], Body::from("texte")).await.status(), 201);
    assert_eq!(call(&b.app, "PUT", "/dav/cassee.jpg", Some(&alice()), &[], Body::from("pas un jpeg")).await.status(), 201, "le dépôt n'échoue jamais à cause de la miniature");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(thumb_status(&b, "/notes.txt").await.0, 404);
    assert_eq!(thumb_status(&b, "/cassee.jpg").await.0, 404);
    let (s, _, body) = bytes(call(&b.app, "GET", "/dav/cassee.jpg", Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!((s, body.as_slice()), (200, &b"pas un jpeg"[..]));
}

#[tokio::test]
async fn copies_share_the_thumbnail_and_nothing_leaks_after_replace_and_delete() {
    let b = bench("thumb-life").await;
    let a = alice();
    assert_eq!(call(&b.app, "PUT", "/dav/a.jpg", Some(&a), &[], Body::from(jpeg(800, 600, 1))).await.status(), 201);
    wait_thumb(&b, "/a.jpg").await;
    let first = thumb_cid(&b, "/a.jpg").await.unwrap();

    // COPY : la copie partage le blob et la miniature.
    assert_eq!(call(&b.app, "COPY", "/dav/a.jpg", Some(&a), &[("destination", "http://localhost/dav/copie.jpg")], Body::empty()).await.status(), 201);
    assert_eq!(thumb_cid(&b, "/copie.jpg").await.as_deref(), Some(first.as_str()));
    assert_eq!(call(&b.app, "DELETE", "/dav/a.jpg", Some(&a), &[], Body::empty()).await.status(), 204);
    assert!(b.blobs.open(&first).await.is_ok(), "la copie utilise encore la miniature");
    assert_eq!(thumb_status(&b, "/copie.jpg").await.0, 200);

    // Remplacement par une autre photo : l'ancienne miniature (et sa clé) disparaissent, la nouvelle apparaît.
    assert_eq!(call(&b.app, "PUT", "/dav/copie.jpg", Some(&a), &[], Body::from(jpeg(640, 480, 9))).await.status(), 204);
    for _ in 0..100 {
        if thumb_cid(&b, "/copie.jpg").await.is_some_and(|c| c != first) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let second = thumb_cid(&b, "/copie.jpg").await.unwrap();
    assert_ne!(second, first);
    assert!(b.blobs.open(&first).await.is_err(), "ancienne miniature supprimée");
    let owner = hex(&ALICE);
    let keys_for = |cid: String| {
        let owner = owner.clone();
        let db = b.db.clone();
        async move { db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_keyring WHERE owner_hex = ?1 AND cid = ?2", [owner, cid], |r| r.get::<_, i64>(0))?)).await.unwrap() }
    };
    assert_eq!(keys_for(first.clone()).await, 0);
    assert_eq!(keys_for(second.clone()).await, 1);

    // Suppression : plus aucun blob ni clé.
    assert_eq!(call(&b.app, "DELETE", "/dav/copie.jpg", Some(&a), &[], Body::empty()).await.status(), 204);
    assert!(b.blobs.open(&second).await.is_err());
    assert_eq!(keys_for(second).await, 0);
}

#[tokio::test]
async fn thumbnails_survive_a_manifest_round_trip() {
    let b = bench("thumb-manifest").await;
    let a = alice();
    assert_eq!(call(&b.app, "PUT", "/dav/p.jpg", Some(&a), &[], Body::from(jpeg(1000, 700, 3))).await.status(), 201);
    let before = wait_thumb(&b, "/p.jpg").await;
    assert!(b.publisher.flush().await.is_empty());
    let owner = hex(&ALICE);
    let event = b
        .db
        .readers()
        .read({
            let owner = owner.clone();
            move |c| astro_db::events::query_events(c, &astro_nostr_proto::Filter { kinds: Some(vec![30_078]), authors: Some(vec![owner]), ..Default::default() }, u64::MAX / 2, 10)
        })
        .await
        .unwrap()
        .remove(0);
    // Le VFS est vidé puis restauré depuis le manifeste : la miniature (entrée + clé) revient.
    b.db.writer().batch(vec![astro_db::Statement::new("DELETE FROM ucloud_entries", vec![]), astro_db::Statement::new("DELETE FROM ucloud_keyring", vec![])]).await.unwrap();
    assert_eq!(thumb_status(&b, "/p.jpg").await.0, 404);
    astro_dav::manifest::restore_from_event(&b.db, b.blobs.as_ref(), keys().as_ref(), &event).await.unwrap();
    assert_eq!(thumb_status(&b, "/p.jpg").await, (200, before));
}
