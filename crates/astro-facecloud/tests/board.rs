//! Table de tri : positions PCA, groupes de visages inconnus, vignettes recadrées, rangement et oubli.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Cursor;

use astro_db::VectorTable;
use astro_facecloud::board::BoardCache;
use astro_facecloud::review::{forget_faces, name_faces};
use common::*;

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(300, 200, |x, y| image::Rgb([(x % 251) as u8, (y % 241) as u8, 120]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

/// Visage `seed` : `+tag` = même personne, autre photo (cosinus ≈ 0,995).
fn emb(spec: &str) -> Vec<f32> {
    let mut v = match spec.split_once('+') {
        Some((a, tag)) => random_vec(a).iter().zip(random_vec(&format!("{a}/{tag}"))).map(|(x, y)| x + y * 0.1).collect(),
        None => random_vec(spec),
    };
    unit_check(&mut v);
    v
}

async fn add_face(rig: &Rig, id: i64, name: &str, pubkey: Option<&str>, reference: Option<&str>, spec: &str) {
    let owner = hex(&ALICE);
    rig.db
        .writer()
        .execute(
            "INSERT INTO faces(face_id, owner_hex, cid, source_path, name, pubkey, ref, det_score, bbox_x1, bbox_y1, bbox_x2, bbox_y2, embed_hash, ts, ucloud_path) \
             VALUES (?1, ?2, 'bafy', '/photo.png', ?3, ?4, ?5, 0.99, 40, 30, 120, 130, ?6, 0, '/photo.png')",
            vec![id.into(), owner.clone().into(), name.to_owned().into(), pubkey.map_or(rusqlite::types::Value::Null, |p| p.to_owned().into()), reference.map_or(rusqlite::types::Value::Null, |p| p.to_owned().into()), format!("h{id}").into()],
        )
        .await
        .unwrap();
    rig.db.writer().upsert_vector(VectorTable::Face, id, &owner, &emb(spec)).await.unwrap();
}

#[tokio::test]
async fn the_board_groups_unknown_faces_places_everyone_and_hides_references() {
    let r = rig("board", |_| {}).await;
    let alice = hex(&ALICE);
    // Marie (3 visages, clé de Bob), quatre inconnus qui sont la même personne, un inconnu seul, une référence de contact.
    for (i, spec) in ["marie", "marie+a", "marie+b"].iter().enumerate() {
        add_face(&r, 1 + i as i64, "Marie", Some(&hex(&BOB)), None, spec).await;
    }
    for (i, spec) in ["lea", "lea+a", "lea+b", "lea+c"].iter().enumerate() {
        add_face(&r, 10 + i as i64, &format!("Inconnu_{i:08}"), None, None, spec).await;
    }
    add_face(&r, 20, "Inconnu_solitaire", None, None, "passant").await;
    add_face(&r, 30, "Contact", None, Some("contact"), "ref").await;

    let cache = BoardCache::new();
    let b = cache.board(&r.db, &alice).await.unwrap();
    assert_eq!(b.faces.len(), 8, "la référence de contact n'est jamais affichée");
    assert_eq!((b.unknown, b.groups_skipped, b.truncated), (5, false, false));
    assert_eq!(b.people.len(), 1);
    assert_eq!((b.people[0].name.as_str(), b.people[0].faces, b.people[0].photos), ("Marie", 3, 1));
    let by = |id: i64| b.faces.iter().find(|f| f.face_id == id).unwrap();
    let g = by(10).group.clone().expect("les quatre inconnus proches forment un groupe");
    assert_eq!(g.1, 4);
    for id in 11..=13 {
        assert_eq!(by(id).group.as_ref().map(|x| &x.0), Some(&g.0));
    }
    assert!(by(20).group.is_none() && by(1).group.is_none(), "le passant est seul ; une personne nommée n'est pas groupée");
    assert!(b.faces.iter().all(|f| f.x.abs() <= 1.0 + 1e-5 && f.y.abs() <= 1.0 + 1e-5));
    assert!(by(10).x != by(20).x || by(10).y != by(20).y, "positions distinctes");
    // Mémoïsé : même résultat, même groupe.
    assert_eq!(cache.board(&r.db, &alice).await.unwrap().faces.iter().find(|f| f.face_id == 10).unwrap().group.as_ref().unwrap().0, g.0);

    // Ranger le groupe dans « Léa » (nouvelle personne) puis un visage dans « Marie » (reprend la clé de Marie).
    assert_eq!(name_faces(&r.db, &alice, &[10, 11, 12, 13], None, "  Léa ").await.unwrap(), 4);
    assert_eq!(name_faces(&r.db, &alice, &[20], None, "Marie").await.unwrap(), 1);
    let b2 = cache.board(&r.db, &alice).await.unwrap();
    assert_eq!(b2.unknown, 0);
    assert!(b2.faces.iter().all(|f| f.group.is_none()), "plus d'inconnus, plus de groupes : la révision a changé");
    let marie = b2.people.iter().find(|p| p.name == "Marie").unwrap();
    assert_eq!((marie.faces, marie.pubkey.as_deref()), (4, Some(hex(&BOB).as_str())), "ranger dans Marie reprend sa clé");
    assert_eq!(b2.people.iter().find(|p| p.name == "Léa").unwrap().faces, 4);
    // Validation.
    assert!(name_faces(&r.db, &alice, &[], None, "x").await.is_err());
    assert!(name_faces(&r.db, &alice, &[1], None, "   ").await.is_err());
    assert!(name_faces(&r.db, &alice, &[1], Some("pas-une-cle"), "x").await.is_err());
    assert_eq!(name_faces(&r.db, &alice, &[30], None, "Pirate").await.unwrap(), 0, "une référence n'est jamais rangée");

    // Oublier : le visage et son empreinte disparaissent, la photo reste ; la référence est intacte.
    assert_eq!(forget_faces(&r.db, &alice, &[20, 30, 999]).await.unwrap(), 1);
    let left: (i64, i64) = r.db.readers().read(|c| Ok((c.query_row("SELECT count(*) FROM faces", [], |r| r.get(0))?, c.query_row("SELECT count(*) FROM face_vectors", [], |r| r.get(0))?))).await.unwrap();
    assert_eq!(left, (8, 8), "9 visages dont la référence, moins le visage oublié");
}

#[tokio::test]
async fn face_thumbs_are_cropped_by_the_station_decoded_once_and_cached() {
    let r = rig("thumbs", |_| {}).await;
    let (status, _) = dav(&r.app, "PUT", "/dav/photo.png", "alice@example.org", &ALICE, png()).await;
    assert_eq!(status, 201);
    add_face(&r, 1, "Marie", None, None, "marie").await;
    add_face(&r, 2, "Inconnu_x", None, None, "autre").await;
    let alice = hex(&ALICE);

    let got = r.fc.face_thumbs(&alice, &[1, 2, 99], 64).await.unwrap();
    assert_eq!(got.thumbs.len(), 2, "{:?}", got.failed);
    assert_eq!(got.failed.iter().map(|f| f.0).collect::<Vec<_>>(), [99], "un visage inconnu est signalé, pas ignoré");
    for url in got.thumbs.values() {
        let jpeg = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, url.strip_prefix("data:image/jpeg;base64,").unwrap()).unwrap();
        assert_eq!(image::GenericImageView::dimensions(&image::load_from_memory(&jpeg).unwrap()), (64, 64));
    }
    // Deuxième appel : servi par la mémoire, même contenu, même quand la photo n'est plus lisible.
    r.db.writer().execute("DELETE FROM ucloud_keyring", vec![]).await.unwrap();
    let again = r.fc.face_thumbs(&alice, &[1, 2], 64).await.unwrap();
    assert_eq!((again.thumbs.len(), again.failed.len()), (2, 0));
    assert_eq!(again.thumbs[&1], got.thumbs[&1]);
    // Une autre taille n'est pas en mémoire : la photo illisible est rapportée avec sa raison.
    let other = r.fc.face_thumbs(&alice, &[1], 96).await.unwrap();
    assert!(other.thumbs.is_empty() && other.failed[0].1.contains("introuvable"), "{:?}", other.failed);
    // Un autre titulaire ne reçoit jamais les visages d'Alice.
    let bob = r.fc.face_thumbs(&hex(&BOB), &[1], 64).await.unwrap();
    assert!(bob.thumbs.is_empty() && bob.failed[0].1.contains("inconnu"));
    assert!(r.fc.face_thumbs(&alice, &[1], 8).await.is_err() && r.fc.face_thumbs(&alice, &[], 64).await.is_err());
}

#[tokio::test]
async fn blurry_faces_do_not_form_groups_but_stay_on_the_board() {
    let r = rig("board-blur", |_| {}).await;
    let alice = hex(&ALICE);
    for (i, spec) in ["lea", "lea+a", "lea+b"].iter().enumerate() {
        add_face(&r, 10 + i as i64, &format!("Inconnu_{i:08}"), None, None, spec).await;
    }
    // Deux visages de la même personne, flous (score de détection sous le seuil de noyau) : visibles, jamais agrégés.
    for (i, spec) in ["tom", "tom+a"].iter().enumerate() {
        add_face(&r, 40 + i as i64, &format!("Inconnu_t{i:07}"), None, None, spec).await;
    }
    r.db.writer().execute("UPDATE faces SET det_score = 0.65 WHERE face_id >= 40", vec![]).await.unwrap();
    let b = BoardCache::new().board(&r.db, &alice).await.unwrap();
    assert_eq!(b.faces.len(), 5);
    let by = |id: i64| b.faces.iter().find(|f| f.face_id == id).unwrap();
    assert!(by(10).group.is_some(), "les visages nets forment un groupe");
    assert!(by(40).group.is_none() && by(41).group.is_none(), "les visages flous ne s'agrègent pas");
}
