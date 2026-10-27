//! FaceCloud de bout en bout : dépôt WebDAV → file → appariement 512D → partage local et enveloppe NIP-59.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use astro_crypto::nip59;
use astro_crypto::{UencKey, uenc};
use astro_dav::BlobStore;
use astro_facecloud::{Decision, ShareVia, embedding_hash, unknown_name};
use common::*;

async fn faces_of(rig: &Rig, owner: &[u8; 32]) -> Vec<(String, Option<String>, Option<String>, Option<String>)> {
    let o = hex(owner);
    rig.db
        .readers()
        .read(move |c| {
            let mut s = c.prepare("SELECT name, pubkey, ref, suggest_name FROM faces WHERE owner_hex = ?1 ORDER BY face_id")?;
            Ok(s.query_map([o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

async fn count(rig: &Rig, sql: &'static str) -> i64 {
    rig.db.readers().read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?)).await.unwrap()
}

#[tokio::test]
async fn drop_a_photo_recognize_friends_share_locally_and_wrap_for_the_remote_friend() {
    let r = rig("e2e", |_| {}).await;
    enroll_friends(&r).await;
    let photo = "FACES:bob+p1,carol+p1,dave+p1,stranger";
    put_photo(&r, "vacances.jpg", photo).await;
    let stats = drain(&r).await;
    assert_eq!((stats.done, stats.retried, stats.failed), (1, 0, 0));

    // Catalogue : 3 références enrôlées + 4 visages de la photo (Bob, Carol, Dave reconnus ; un inconnu nommé par son empreinte).
    let faces = faces_of(&r, &ALICE).await;
    assert_eq!(faces.len(), 7);
    let on_photo = &faces[3..];
    let names: Vec<&str> = on_photo.iter().map(|f| f.0.as_str()).collect();
    assert_eq!(&names[..3], ["Bob", "Carol", "Dave"]);
    assert!(names[3].starts_with("Inconnu_") && names[3].len() == 8 + 8, "{names:?}");
    assert_eq!(on_photo[0].1.as_deref(), Some(hex(&BOB).as_str()));
    assert!(on_photo[3].1.is_none());
    let (status, info): (String, i64) = r
        .db
        .readers()
        .read(|c| Ok(c.query_row("SELECT faceid_status, json_extract(data, '$.faceid.count') FROM ucloud_entries WHERE path = '/vacances.jpg'", [], |r| Ok((r.get(0)?, r.get(1)?)))?))
        .await
        .unwrap();
    assert_eq!((status.as_str(), info), ("faces", 4));

    // Bob (ami réciproque, hébergé ici) : copie silencieuse, même blob, même clé.
    let (alice_cid,): (String,) = r.db.readers().read(|c| Ok((c.query_row("SELECT cid FROM ucloud_entries WHERE path = '/vacances.jpg'", [], |r| r.get(0))?,))).await.unwrap();
    let (cid, origin, status): (String, String, Option<String>) = r
        .db
        .readers()
        .read(|c| Ok(c.query_row("SELECT cid, origin, faceid_status FROM ucloud_entries WHERE owner_hex = ?1 AND path = '/Partages/De_alice/vacances.jpg'", [hex(&BOB)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?))
        .await
        .unwrap();
    assert_eq!((cid.as_str(), origin.as_str(), status), (alice_cid.as_str(), "facecloud-share", None));
    let (code, body) = dav(&r.app, "GET", "/dav/Partages/De_alice/vacances.jpg", "bob@example.org", &BOB, vec![]).await;
    assert_eq!((code, body.as_slice()), (200, photo.as_bytes()), "Bob relit la photo avec sa propre clé de trousseau");
    assert_eq!(count(&r, "SELECT count(*) FROM ucloud_keyring WHERE owner_hex = (SELECT hex FROM identities WHERE email = 'bob@example.org')").await, 1);

    // Carol (amie réciproque, hébergée ailleurs) : une seule enveloppe NIP-59, adressée à elle.
    let sent = r.outbox.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "ni Dave (pas réciproque) ni l'inconnu");
    let (recipient, wrap) = &sent[0];
    assert_eq!(recipient, &hex(&CAROL));
    assert_eq!(wrap.kind, 1059);
    wrap.verify().unwrap();
    assert_eq!(wrap.tags, vec![vec!["p".to_owned(), hex(&CAROL)]]);
    assert_ne!(wrap.pubkey, hex(&ALICE), "l'expéditeur n'apparaît pas sur l'enveloppe");
    assert!(!wrap.content.contains(&cid));
    let opened = nip59::unwrap(&CAROL, wrap).unwrap();
    assert_eq!(opened.sender, hex(&ALICE), "expéditeur authentifié par le scellé");
    assert_eq!(opened.rumor.kind, 15);
    assert_eq!(opened.rumor.tag_value("cid"), Some(cid.as_str()));
    assert_eq!(opened.rumor.tag_value("file-type"), Some("image/jpeg"));
    assert_eq!(opened.rumor.tag_value("encryption-algorithm"), Some("uenc-v2"));
    assert_eq!(opened.rumor.tag_value("name"), Some("vacances.jpg"));
    assert_eq!(opened.rumor.content, format!("ipfs://{cid}"));
    // Avec le CID et la clé du message, Carol déchiffre le blob IPFS.
    let key = UencKey::from_hex(opened.rumor.tag_value("decryption-key").unwrap()).unwrap();
    let (mut blob, _) = r.blobs.open(opened.rumor.tag_value("cid").unwrap()).await.unwrap();
    let mut plain = Vec::new();
    uenc::decrypt_stream(&key, &mut blob, &mut plain).await.unwrap();
    assert_eq!(plain, photo.as_bytes());
    // Seule Carol peut ouvrir l'enveloppe.
    assert!(nip59::unwrap(&BOB, wrap).is_err() && nip59::unwrap(&DAVE, wrap).is_err());

    let shares: Vec<(String, String)> = r
        .db
        .readers()
        .read(|c| Ok(c.prepare("SELECT recipient_hex, via FROM face_shares ORDER BY via")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?))
        .await
        .unwrap();
    assert_eq!(shares, [(hex(&BOB), "local".to_owned()), (hex(&CAROL), "nip17".to_owned())]);

    // Le partage local est ancré dans le manifeste de Bob (rejouable).
    assert!(r.publisher.flush().await.is_empty());
    assert_eq!(count(&r, "SELECT count(*) FROM nostr_events WHERE kind = 30078 AND d_tag = 'ucloud:manifest'").await, 2, "Alice et Bob");
}

#[tokio::test]
async fn thresholds_recognized_suggestion_unknown_and_deterministic_names() {
    let r = rig("thresholds", |_| {}).await;
    enroll_friends(&r).await;
    put_photo(&r, "p.jpg", "FACES:bob+x,bob~carol,mystere").await;
    let rep = r.fc.process_photo(&hex(&ALICE), "/p.jpg").await.unwrap();
    assert_eq!(rep.status, "faces");
    // 1. Même personne, autre photo : reconnu (score ≥ 0,82).
    assert!(matches!(rep.faces[0].decision, Decision::Recognized { score } if score > 0.95), "{:?}", rep.faces[0]);
    assert_eq!(rep.faces[0].name, "Bob");
    // 2. Mélange de Bob et Carol : zone grise → suggestion, jamais d'identité attribuée ni de partage.
    match &rep.faces[1].decision {
        Decision::Suggestion { score, candidate } => {
            assert!((0.55..0.82).contains(score), "{score}");
            assert!(candidate.as_deref().is_some_and(|c| c == "Bob" || c == "Carol"));
        }
        other => panic!("suggestion attendue : {other:?}"),
    }
    assert!(rep.faces[1].pubkey.is_none() && rep.faces[1].name.starts_with("Inconnu_"));
    // 3. Inconnu : nom déterministe issu de l'empreinte de l'embedding.
    assert_eq!(rep.faces[2].decision, Decision::Unknown);
    let mut v = r.engine_embedding("mystere");
    unit_check(&mut v);
    assert_eq!(rep.faces[2].name, unknown_name(&embedding_hash(&v)));
    // Partages : seul Bob (visage 0) a été reconnu et partagé.
    assert_eq!(rep.shares.len(), 1);
    assert_eq!((rep.shares[0].recipient.as_str(), rep.shares[0].via), (hex(&BOB).as_str(), ShareVia::Local));

    // Validation humaine de la suggestion : le visage prend l'identité suggérée.
    let id = rep.faces[1].face_id;
    assert!(r.fc.accept_suggestion(&hex(&ALICE), id).await.unwrap());
    assert!(!r.fc.accept_suggestion(&hex(&ALICE), id).await.unwrap(), "plus de suggestion en attente");
    let row = faces_of(&r, &ALICE).await.into_iter().find(|f| f.0 == "Bob" || f.0 == "Carol").unwrap();
    assert!(row.1.is_some());
    // Un inconnu nommé à la main devient un contact.
    let unknown_id = rep.faces[2].face_id;
    r.fc.name_face(&hex(&ALICE), unknown_id, Some(&hex(&DAVE)), "Dave bis").await.unwrap();
    assert!(r.fc.name_face(&hex(&ALICE), 9999, None, "x").await.is_err());
    // Une nouvelle photo du même inconnu (autre cliché) est désormais reconnue sous ce nom.
    put_photo(&r, "q.jpg", "FACES:mystere+z").await;
    let rep2 = r.fc.process_photo(&hex(&ALICE), "/q.jpg").await.unwrap();
    assert!(matches!(rep2.faces[0].decision, Decision::Recognized { .. }));
    assert_eq!(rep2.faces[0].name, "Dave bis");
}

#[tokio::test]
async fn photos_without_faces_and_weak_detections() {
    let r = rig("noface", |_| {}).await;
    put_photo(&r, "paysage.jpg", "NOFACE").await;
    put_photo(&r, "flou.jpg", "FACES:bob@0.5").await;
    for p in ["/paysage.jpg", "/flou.jpg"] {
        let rep = r.fc.process_photo(&hex(&ALICE), p).await.unwrap();
        assert_eq!((rep.status.as_str(), rep.faces.len()), ("no_face", 0), "{p}");
    }
    assert_eq!(count(&r, "SELECT count(*) FROM ucloud_entries WHERE faceid_status = 'no_face'").await, 2);
    assert_eq!(count(&r, "SELECT count(*) FROM faces").await, 0, "détection sous 0,60 écartée");
    // Pas une image : refusée sans analyse.
    assert_eq!(dav(&r.app, "PUT", "/dav/notes.txt", "alice@example.org", &ALICE, b"FACES:bob".to_vec()).await.0, 201);
    assert!(r.fc.process_photo(&hex(&ALICE), "/notes.txt").await.is_err());
    assert!(r.fc.process_photo(&hex(&ALICE), "/absent.jpg").await.is_err());
}

#[tokio::test]
async fn reprocessing_is_idempotent_and_received_copies_are_never_analyzed() {
    let r = rig("idem", |_| {}).await;
    enroll_friends(&r).await;
    put_photo(&r, "a.jpg", "FACES:bob+1,carol+1").await;
    let first = r.fc.process_photo(&hex(&ALICE), "/a.jpg").await.unwrap();
    assert_eq!(first.shares.len(), 2);
    let faces_before = count(&r, "SELECT count(*) FROM faces").await;
    let again = r.fc.process_photo(&hex(&ALICE), "/a.jpg").await.unwrap();
    assert!(again.faces.iter().all(|f| f.decision == Decision::AlreadyKnown), "{again:?}");
    assert_eq!(count(&r, "SELECT count(*) FROM faces").await, faces_before);
    assert!(again.shares.is_empty() && again.skipped.iter().all(|s| s.reason == "déjà partagé"), "{again:?}");
    assert_eq!(r.outbox.sent.lock().unwrap().len(), 1, "une seule enveloppe, jamais de doublon");
    // La copie reçue par Bob n'est jamais analysée (pas de cascade de partages).
    let calls = r.engine.calls.load(std::sync::atomic::Ordering::SeqCst);
    let rep = r.fc.process_photo(&hex(&BOB), "/Partages/De_alice/a.jpg").await.unwrap();
    assert_eq!(rep.status, "skipped");
    assert_eq!(r.engine.calls.load(std::sync::atomic::Ordering::SeqCst), calls);
    // Une autre photo avec le même ami local ne réécrase pas la première : noms uniques.
    put_photo(&r, "b.jpg", "FACES:bob+2").await;
    r.fc.process_photo(&hex(&ALICE), "/b.jpg").await.unwrap();
    assert_eq!(count(&r, "SELECT count(*) FROM ucloud_entries WHERE origin = 'facecloud-share'").await, 2);
}

#[tokio::test]
async fn owners_are_isolated_in_the_vector_index() {
    let r = rig("owners", |_| {}).await;
    enroll_friends(&r).await; // Alice connaît Carol
    // Bob dépose une photo de Carol : pour Bob, c'est un inconnu.
    let (code, _) = dav(&r.app, "PUT", "/dav/c.jpg", "bob@example.org", &BOB, b"FACES:carol+9".to_vec()).await;
    assert_eq!(code, 201);
    let rep = r.fc.process_photo(&hex(&BOB), "/c.jpg").await.unwrap();
    assert_eq!(rep.faces[0].decision, Decision::Unknown);
    assert!(rep.faces[0].pubkey.is_none() && rep.shares.is_empty());
}

#[tokio::test]
async fn sharing_failures_and_non_reciprocal_friends_are_reported_not_fatal() {
    let r = rig("failures", |_| {}).await;
    enroll_friends(&r).await;
    *r.outbox.fail.lock().unwrap() = true;
    put_photo(&r, "a.jpg", "FACES:carol+1,dave+1").await;
    let rep = r.fc.process_photo(&hex(&ALICE), "/a.jpg").await.unwrap();
    assert!(rep.shares.is_empty());
    let reasons: Vec<&str> = rep.skipped.iter().map(|s| s.reason.as_str()).collect();
    assert!(reasons.iter().any(|r| r.starts_with("envoi impossible")) && reasons.contains(&"pas un ami réciproque (N1)"), "{reasons:?}");
    assert_eq!(count(&r, "SELECT count(*) FROM face_shares").await, 0, "échec d'envoi : rien n'est marqué partagé, un retraitement réessaiera");
    // Le relai revient : le retraitement partage enfin.
    *r.outbox.fail.lock().unwrap() = false;
    let rep = r.fc.process_photo(&hex(&ALICE), "/a.jpg").await.unwrap();
    assert_eq!(rep.shares.len(), 1);
    // Partage désactivé : catalogage seul.
    let off = rig("off", |c| c.share_enabled = false).await;
    enroll_friends(&off).await;
    put_photo(&off, "a.jpg", "FACES:bob+1").await;
    let rep = off.fc.process_photo(&hex(&ALICE), "/a.jpg").await.unwrap();
    assert!(rep.shares.is_empty() && rep.skipped.is_empty());
}

#[tokio::test]
async fn only_one_inference_runs_at_a_time() {
    let r = rig("gpu", |_| {}).await;
    r.engine.delay_ms.store(30, std::sync::atomic::Ordering::SeqCst);
    for i in 0..4 {
        put_photo(&r, &format!("p{i}.jpg"), "NOFACE").await;
    }
    let owner = hex(&ALICE);
    let tasks: Vec<_> = (0..4)
        .map(|i| {
            let (fc, owner) = (r.fc.clone(), owner.clone());
            tokio::spawn(async move { fc.process_photo(&owner, &format!("/p{i}.jpg")).await.unwrap() })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(r.engine.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert_eq!(r.engine.max_running.load(std::sync::atomic::Ordering::SeqCst), 1, "jamais deux inférences simultanées");
}

#[tokio::test]
async fn queue_dedupes_retries_with_backoff_and_gives_up() {
    let r = rig("queue", |_| {}).await;
    put_photo(&r, "a.jpg", "NOFACE").await;
    for _ in 0..3 {
        r.fc.enqueue(&hex(&ALICE), "/a.jpg").await.unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(count(&r, "SELECT count(*) FROM tasks WHERE status = 'pending'").await, 1, "un seul exemplaire en attente (hook + enqueue manuels)");
    // Moteur en panne : replanifié avec délai, donc pas rejouable immédiatement.
    *r.engine.fail.lock().unwrap() = true;
    let s = r.fc.run_pending(10).await.unwrap();
    assert_eq!((s.done, s.retried, s.failed), (0, 1, 0));
    assert_eq!(r.fc.run_pending(10).await.unwrap(), Default::default(), "le délai de reprise n'est pas écoulé");
    // Échéance forcée : après 5 échecs, abandon.
    for expected_failed in [0, 0, 0, 1] {
        r.db.writer().execute("UPDATE tasks SET not_before = 0 WHERE status = 'pending'", vec![]).await.unwrap();
        let s = r.fc.run_pending(10).await.unwrap();
        assert_eq!(s.failed, expected_failed);
    }
    assert_eq!(count(&r, "SELECT count(*) FROM tasks WHERE status = 'failed'").await, 1);
    // Reprise sur incident : une tâche restée « running » après un arrêt brutal repart.
    r.fc.enqueue(&hex(&ALICE), "/a.jpg").await.unwrap();
    r.db.writer().execute("UPDATE tasks SET status = 'running' WHERE status = 'pending'", vec![]).await.unwrap();
    assert_eq!(r.fc.recover().await.unwrap(), 1);
    *r.engine.fail.lock().unwrap() = false;
    assert_eq!(r.fc.run_pending(10).await.unwrap().done, 1);
    // Photo disparue : échec définitif sans réessai.
    r.fc.enqueue(&hex(&ALICE), "/supprimee.jpg").await.unwrap();
    let s = r.fc.run_pending(10).await.unwrap();
    assert_eq!((s.failed, s.retried), (1, 0));
}

#[tokio::test]
async fn the_background_worker_processes_uploads_and_stops_on_shutdown() {
    let r = rig("worker", |_| {}).await;
    enroll_friends(&r).await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(r.fc.clone().run_worker(std::time::Duration::from_millis(20), rx));
    put_photo(&r, "auto.jpg", "FACES:carol+7").await;
    for _ in 0..100 {
        if r.outbox.sent.lock().unwrap().len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(r.outbox.sent.lock().unwrap().len(), 1, "le dépôt WebDAV a déclenché analyse et envoi sans intervention");
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), worker).await.expect("arrêt attendu").unwrap();
}

impl Rig {
    /// Embedding normalisé que le moteur simulé produit pour `label`.
    fn engine_embedding(&self, label: &str) -> Vec<f32> {
        random_vec(label)
    }
}
