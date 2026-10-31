//! Jetons WebDAV d'appareil : un téléphone dépose dans le dossier choisi par son titulaire, sans rien voir ni toucher d'autre.

#![allow(clippy::unwrap_used)]

mod common;

use astro_dav::devices::{self, Mode};
use axum::body::Body;
use base64::Engine;
use common::*;

fn device_auth(login: &str, token: &str) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{login}:{token}")))
}

async fn all_paths(b: &Bench) -> Vec<String> {
    b.db.readers().read(|c| Ok(c.prepare("SELECT path FROM ucloud_entries ORDER BY path")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap()
}

#[tokio::test]
async fn a_write_only_device_drops_files_in_its_folder_and_sees_nothing_else() {
    let b = bench("dev-write").await;
    let owner = hex(&ALICE);
    let (dev, token) = devices::create(&b.db, &owner, "Mon téléphone", "/Photos/Téléphone", Mode::Write).await.unwrap();
    assert!(token.starts_with("dv_") && token.len() == 35 && dev.id.len() == 8);
    let me = device_auth("alice@example.org", &token);
    // Le dossier a été créé pour lui.
    assert!(all_paths(&b).await.contains(&"/Photos/Téléphone".to_owned()));

    // Dépôt : arrive dans le dossier choisi, vu depuis la racine du client.
    assert_eq!(call(&b.app, "PUT", "/dav/IMG_1.jpg", Some(&me), &[], Body::from("photo")).await.status(), 201);
    assert_eq!(call(&b.app, "MKCOL", "/dav/2026", Some(&me), &[], Body::empty()).await.status(), 201);
    assert_eq!(call(&b.app, "PUT", "/dav/2026/b.jpg", Some(&me), &[], Body::from("autre")).await.status(), 201);
    let (s, _, body) = bytes(call(&b.app, "GET", "/dav/Photos/T%C3%A9l%C3%A9phone/IMG_1.jpg", Some(&alice()), &[], Body::empty()).await).await;
    assert_eq!((s, body.as_slice()), (200, b"photo".as_slice()), "le jeton principal retrouve l'envoi à l'adresse réelle");

    // Envoi seul : ni lecture ni suppression.
    assert_eq!(call(&b.app, "GET", "/dav/IMG_1.jpg", Some(&me), &[], Body::empty()).await.status(), 403);
    assert_eq!(call(&b.app, "HEAD", "/dav/IMG_1.jpg", Some(&me), &[], Body::empty()).await.status(), 403);
    assert_eq!(call(&b.app, "DELETE", "/dav/IMG_1.jpg", Some(&me), &[], Body::empty()).await.status(), 403);
    // La liste ne révèle pas le chemin au-dessus de sa racine.
    let (s, xml) = text(call(&b.app, "PROPFIND", "/dav/", Some(&me), &[("depth", "1")], Body::empty()).await).await;
    assert_eq!(s, 207);
    assert!(xml.contains("<D:href>/dav/IMG_1.jpg</D:href>") && xml.contains("<D:href>/dav/2026/</D:href>") && xml.contains("<D:href>/dav/</D:href>"), "{xml}");
    assert!(!xml.contains("Photos") && !xml.contains("T%C3%A9l"), "aucun préfixe réel dans les liens : {xml}");
    // Contacts et agenda lui sont fermés.
    assert_eq!(call(&b.app, "PROPFIND", "/dav/addressbooks/alice@example.org/", Some(&me), &[("depth", "0")], Body::empty()).await.status(), 403);
    assert_eq!(call(&b.app, "PROPFIND", "/dav/calendars/alice@example.org/default/", Some(&me), &[("depth", "0")], Body::empty()).await.status(), 403);
    // Le jeton principal, lui, voit tout.
    let (_, root) = text(call(&b.app, "PROPFIND", "/dav/", Some(&alice()), &[("depth", "1")], Body::empty()).await).await;
    assert!(root.contains("/dav/Photos/"));
}

#[tokio::test]
async fn a_device_cannot_escape_its_folder_by_any_path() {
    let b = bench("dev-escape").await;
    let owner = hex(&ALICE);
    assert_eq!(call(&b.app, "PUT", "/dav/secret.txt", Some(&alice()), &[], Body::from("privé")).await.status(), 201);
    let (_, token) = devices::create(&b.db, &owner, "Tablette", "/Imports/Tablette", Mode::ReadWrite).await.unwrap();
    let me = device_auth("alice@example.org", &token);
    for target in ["/dav/../secret.txt", "/dav/%2e%2e/secret.txt", "/dav/a/../../secret.txt", "/dav/%2e%2e%2fsecret.txt", "/dav/..%5csecret.txt"] {
        let r = call(&b.app, "GET", target, Some(&me), &[], Body::empty()).await;
        let (s, _, body) = bytes(r).await;
        assert!(s != 200 || body != "privé".as_bytes(), "{target} a livré le fichier hors dossier");
    }
    // Le fichier du titulaire n'est visible ni par son nom ni par PROPFIND.
    assert_eq!(call(&b.app, "GET", "/dav/secret.txt", Some(&me), &[], Body::empty()).await.status(), 404);
    let (_, xml) = text(call(&b.app, "PROPFIND", "/dav/", Some(&me), &[("depth", "infinity")], Body::empty()).await).await;
    assert!(!xml.contains("secret"), "{xml}");
    // Déplacements et copies : la destination est replacée sous la racine, jamais ailleurs.
    assert_eq!(call(&b.app, "PUT", "/dav/m.txt", Some(&me), &[], Body::from("x")).await.status(), 201);
    for dest in ["http://localhost/dav/../Hors.txt", "/dav/%2e%2e/Hors.txt", "/dav/sous/Dedans.txt"] {
        let _ = call(&b.app, "MKCOL", "/dav/sous", Some(&me), &[], Body::empty()).await;
        let _ = call(&b.app, "COPY", "/dav/m.txt", Some(&me), &[("destination", dest)], Body::empty()).await;
    }
    let paths = all_paths(&b).await;
    assert!(paths.iter().all(|p| !p.ends_with("Hors.txt")), "rien hors de la racine : {paths:?}");
    assert!(paths.contains(&"/Imports/Tablette/sous/Dedans.txt".to_owned()), "{paths:?}");
    assert!(paths.iter().filter(|p| p.ends_with("m.txt") || p.ends_with("Dedans.txt")).all(|p| p.starts_with("/Imports/Tablette/")), "{paths:?}");
}

#[tokio::test]
async fn modes_tokens_accounts_and_revocation_are_enforced() {
    let b = bench("dev-modes").await;
    let owner = hex(&ALICE);
    let (ro, ro_token) = devices::create(&b.db, &owner, "Écran du salon", "/Photos/Album", Mode::Read).await.unwrap();
    let (rw, rw_token) = devices::create(&b.db, &owner, "Ordinateur", "/Photos/Album", Mode::ReadWrite).await.unwrap();
    assert_ne!(ro_token, rw_token);
    let (ro_a, rw_a) = (device_auth("alice@example.org", &ro_token), device_auth("alice@example.org", &rw_token));
    assert_eq!(call(&b.app, "PUT", "/dav/a.jpg", Some(&rw_a), &[], Body::from("A")).await.status(), 201);
    // Lecture seule : lit et liste, n'écrit ni ne crée.
    let (s, _, body) = bytes(call(&b.app, "GET", "/dav/a.jpg", Some(&ro_a), &[], Body::empty()).await).await;
    assert_eq!((s, body.as_slice()), (200, b"A".as_slice()));
    assert_eq!(call(&b.app, "PUT", "/dav/b.jpg", Some(&ro_a), &[], Body::from("B")).await.status(), 403);
    assert_eq!(call(&b.app, "MKCOL", "/dav/d", Some(&ro_a), &[], Body::empty()).await.status(), 403);
    assert_eq!(call(&b.app, "DELETE", "/dav/a.jpg", Some(&ro_a), &[], Body::empty()).await.status(), 403);
    // Lecture et écriture : déplace, supprime.
    assert_eq!(call(&b.app, "MOVE", "/dav/a.jpg", Some(&rw_a), &[("destination", "/dav/c.jpg")], Body::empty()).await.status(), 201);
    assert_eq!(call(&b.app, "DELETE", "/dav/c.jpg", Some(&rw_a), &[], Body::empty()).await.status(), 204);

    // Un jeton d'appareil n'est valable que pour SON compte, et un faux jeton ne vaut rien.
    let bob = hex(&BOB);
    assert_eq!(call(&b.app, "PROPFIND", "/dav/", Some(&device_auth(&bob, &rw_token)), &[("depth", "0")], Body::empty()).await.status(), 401, "le jeton d'Alice ne marche pas pour Bob");
    for bad in [device_auth("alice@example.org", "dv_00000000000000000000000000000000"), device_auth("alice@example.org", ""), device_auth("inconnu@example.org", &rw_token), "Basic !!!".to_owned()] {
        assert_eq!(call(&b.app, "PROPFIND", "/dav/", Some(&bad), &[("depth", "0")], Body::empty()).await.status(), 401, "{bad}");
    }
    // Le jeton d'appareil n'a jamais ouvert l'API de l'interface : il ne vaut que pour le WebDAV.
    assert!(devices::lookup(&b.db, &bob, &rw_token).await.is_none());

    // Dernier usage noté ; liste sans empreinte ; révocation immédiate, le reste intact.
    let list = devices::list(&b.db, &owner).await.unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|d| d.last_used.is_some()), "{list:?}");
    devices::revoke(&b.db, &owner, &rw.id).await.unwrap();
    assert_eq!(call(&b.app, "PROPFIND", "/dav/", Some(&rw_a), &[("depth", "0")], Body::empty()).await.status(), 401);
    assert_eq!(call(&b.app, "PROPFIND", "/dav/", Some(&ro_a), &[("depth", "0")], Body::empty()).await.status(), 207, "l'autre appareil continue");
    assert!(matches!(devices::revoke(&b.db, &owner, &rw.id).await, Err(devices::DeviceError::NotFound)));
    assert_eq!(devices::list(&b.db, &owner).await.unwrap().len(), 1);
    let _ = ro;
}

#[tokio::test]
async fn folders_names_and_the_number_of_devices_are_validated() {
    let b = bench("dev-valid").await;
    let owner = hex(&ALICE);
    for bad in ["/", "", "/Coffre", "/Contacts/x", "/Calendrier", "/Partages", "/Sauvegardes", "/.cache", "/a/../.."] {
        assert!(devices::create(&b.db, &owner, "x", bad, Mode::Write).await.is_err(), "{bad}");
    }
    assert!(devices::create(&b.db, &owner, "  ", "/Ok", Mode::Write).await.is_err(), "nom vide");
    // Un fichier à l'emplacement du dossier : refusé.
    assert_eq!(call(&b.app, "PUT", "/dav/fichier", Some(&alice()), &[], Body::from("x")).await.status(), 201);
    assert!(devices::create(&b.db, &owner, "x", "/fichier", Mode::Write).await.is_err());
    for n in 0..devices::MAX_DEVICES {
        devices::create(&b.db, &owner, &format!("Appareil {n}"), &format!("/Appareils/{n}"), Mode::Write).await.unwrap();
    }
    assert!(devices::create(&b.db, &owner, "de trop", "/Appareils/trop", Mode::Write).await.is_err(), "{} appareils au plus", devices::MAX_DEVICES);
    // Le jeton n'est jamais gardé en clair.
    let stored: Vec<String> = b.db.readers().read(|c| Ok(c.prepare("SELECT token_sha256 FROM dav_tokens")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
    assert!(stored.iter().all(|h| h.len() == 64 && !h.starts_with("dv_")));
}
