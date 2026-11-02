//! Essai **réel** sur la Ğ1 en ORIGIN (`UPLANETNAME` = 64 zéros : les clés du maître sont publiques, c'est le bac à sable).
//! Ignoré par défaut ; il n'envoie rien tant que `ASTRO_LIVE_G1` n'est pas posé :
//!
//! ```text
//! ASTRO_LIVE_G1=read cargo test --test billet_live -- --ignored --nocapture   # soldes seulement, aucun virement
//! ASTRO_LIVE_G1=1    cargo test --test billet_live -- --ignored --nocapture   # cycle complet : émettre, vérifier, encaisser
//! ```
//!
//! Le cycle complet envoie ≈ 1,2 Ğ1 du maître à un billet jetable, l'encaisse à travers `Billets` (billet → maître → destinataire) et
//! laisse la valeur (0,20 Ğ1) au destinataire de test, dont la clé est fixe : l'argent n'est jamais perdu.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_db::Db;
use astro_duniter::{Account, Amount, Chain, Ed25519Signer, PoolConfig, RpcPool, SubxtConnector, TransferMode};
use astroportd::billet::{Billets, shamir};
use astroportd::coop_wallets::Wallets;
use astroportd::duniter_nodes::{BOOTSTRAP_RPC, BOOTSTRAP_SQUID};
use astroportd::funds::{PrimalSource, SquidPrimal};
use serde_json::json;

/// Destinataire de test : clé fixe, donc retrouvable. Son primal sera le maître (1 Ğ1 reçu du maître).
const DEST_SEED: [u8; 32] = [0xD5; 32];
const ORIGIN: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn g1(c: u64) -> String {
    format!("{}.{:02} Ğ1", c / 100, c % 100)
}

#[tokio::test]
#[ignore = "réseau Ğ1 réel : ASTRO_LIVE_G1=read ou =1"]
async fn a_billet_is_issued_checked_and_cashed_on_the_real_chain() {
    let mode = std::env::var("ASTRO_LIVE_G1").unwrap_or_default();
    if mode.is_empty() {
        eprintln!("ASTRO_LIVE_G1 absent : rien n'est fait");
        return;
    }
    let dir = std::env::temp_dir().join(format!("astroportd-billet-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    let chain = Arc::new(RpcPool::new(SubxtConnector, PoolConfig { urls: BOOTSTRAP_RPC.iter().map(|s| (*s).to_owned()).collect(), cooldown: Duration::from_secs(30), reconnect_delay: Duration::from_secs(2) }));
    let wallets = Arc::new(Wallets::new(ORIGIN.to_owned(), db.clone()));
    let master = wallets.master().await.unwrap();
    let dest = Ed25519Signer::from_seed(DEST_SEED);
    let balance = |a: Account| {
        let chain = chain.clone();
        async move { chain.balance(&a).await.unwrap().total.0 }
    };
    let master_before = balance(master.account()).await;
    eprintln!("maître ORIGIN {} : {}", master.account().ss58(), g1(master_before));
    eprintln!("destinataire de test {} : {}", dest.account().ss58(), g1(balance(dest.account()).await));
    if mode == "read" {
        return;
    }
    assert!(master_before >= 500, "le maître ORIGIN a {} : il en faut au moins 5 Ğ1 pour l'essai", g1(master_before));

    let primals = Arc::new(SquidPrimal::new(BOOTSTRAP_SQUID.iter().map(|s| (*s).to_owned()).collect()));
    let wait_primal = |a: Account| {
        let primals = primals.clone();
        async move {
            for _ in 0..40 {
                if let Ok(Some(p)) = primals.primal_of(&a.ss58()).await {
                    return p;
                }
                tokio::time::sleep(Duration::from_secs(6)).await;
            }
            panic!("l'indexeur n'a pas vu le premier virement de {}", a.ss58());
        }
    };

    // 1. Le destinataire de test reçoit sa primo transaction du maître (une seule fois dans sa vie).
    if balance(dest.account()).await < 100 {
        eprintln!("primo transaction du destinataire (1 Ğ1)");
        chain.transfer(&master, &dest.account(), Amount(100), TransferMode::KeepAlive, Some("UPLANET:BILLET:TEST:PRIMO")).await.unwrap();
    }
    assert_eq!(wait_primal(dest.account()).await, master.account().ss58(), "le primal du destinataire est le maître");

    // 2. Un billet jetable : 1 Ğ1 d'activation + 0,20 Ğ1 de valeur, du maître (le circuit de /qr/billet/fund).
    let mut seed = [0u8; 32];
    getrandom_fill(&mut seed);
    let billet = Ed25519Signer::from_seed(seed);
    let mut coef = [0u8; 32];
    getrandom_fill(&mut coef);
    let mut it = coef.into_iter();
    let parts = shamir::split(&seed, || it.next().unwrap());
    // ORIGIN est un bac à sable aux clés publiques : on écrit les parts en entier pour ne jamais perdre les fonds d'un essai interrompu.
    eprintln!("billet {} parts {} {} {}", billet.account().ss58(), hex::encode(parts[0]), hex::encode(parts[1]), hex::encode(parts[2]));
    // Deux virements d'un même compte à la suite : on laisse au réseau le temps de voir le premier (nonce), sinon « Invalid Transaction (1010) ».
    for (cents, memo) in [(100, "UPLANET:BILLET:PRIMO"), (20, "UPLANET:BILLET:TEST")] {
        let mut sent = false;
        for attempt in 1..=4 {
            match chain.transfer(&master, &billet.account(), Amount(cents), TransferMode::KeepAlive, Some(memo)).await {
                Ok(_) => {
                    sent = true;
                    break;
                }
                Err(e) => {
                    eprintln!("virement {memo} essai {attempt} : {e}");
                    tokio::time::sleep(Duration::from_secs(15)).await;
                }
            }
        }
        assert!(sent, "virement {memo} impossible");
        tokio::time::sleep(Duration::from_secs(8)).await;
    }
    assert_eq!(wait_primal(billet.account()).await, master.account().ss58(), "le primal du billet est le maître");
    // Les nœuds ne voient pas tous le dernier bloc : on attend que le solde soit celui attendu avant de vérifier.
    for _ in 0..30 {
        if balance(billet.account()).await >= 120 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
    }

    // 3. Le service : vérifier, puis encaisser vers le destinataire, P3 puis P1 (ordre libre).
    let svc = Billets::new(db.clone(), chain.clone(), wallets, Some(primals), ORIGIN.to_owned(), ActivityBus::new());
    let (p1, p3) = (hex::encode(parts[0]), hex::encode(parts[2]));
    let view = svc.handle(None, "identify", &json!({ "parts": [p3, p1] })).await.unwrap();
    eprintln!("vérifié : {view}");
    assert_eq!((view["status"].as_str(), view["value_g1_cents"].as_u64(), view["authentic"].as_bool()), (Some("ready"), Some(20), Some(true)));
    let dest_before = balance(dest.account()).await;
    let out = svc.handle(None, "redeem", &json!({ "parts": [p3, p1], "dest": dest.account().ss58() })).await.unwrap();
    eprintln!("encaissé : {out}");
    assert_eq!(out["status"], "done");
    // Les nœuds ne voient pas tous le dernier bloc : on attend que les soldes se stabilisent.
    for _ in 0..30 {
        if balance(billet.account()).await == 0 && balance(dest.account()).await == dest_before + 20 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
    }
    assert_eq!(balance(billet.account()).await, 0, "billet vidé");
    assert_eq!(balance(dest.account()).await, dest_before + 20, "le destinataire reçoit exactement la valeur du billet");
    let again = svc.handle(None, "check", &json!({ "g1pub": billet.account().ss58() })).await.unwrap();
    assert_ne!(again["status"], "ready", "un billet encaissé ne l'est pas deux fois");
    let master_after = balance(master.account()).await;
    eprintln!("maître : {} → {} (différence = la valeur laissée au destinataire de test)", g1(master_before), g1(master_after));
    let _ = std::fs::remove_dir_all(dir);
}

/// Octets aléatoires sans dépendance de plus : on lit `/dev/urandom`.
fn getrandom_fill(buf: &mut [u8]) {
    use std::io::Read;
    std::fs::File::open("/dev/urandom").unwrap().read_exact(buf).unwrap();
}

/// Création, financement et **retour automatique** sur la vraie chaîne : le destinataire de test fait l'émetteur (il a un primal valide et
/// de quoi financer 0,20 Ğ1) ; la date de fin est avancée à la main, puis la station renvoie la valeur à l'émetteur.
#[tokio::test]
#[ignore = "réseau Ğ1 réel : ASTRO_LIVE_G1=1"]
async fn an_issued_billet_returns_to_its_issuer_on_the_real_chain() {
    if std::env::var("ASTRO_LIVE_G1").as_deref() != Ok("1") {
        eprintln!("ASTRO_LIVE_G1=1 requis : rien n'est fait");
        return;
    }
    let dir = std::env::temp_dir().join(format!("astroportd-billet-live2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("nostr/emetteur@exemple.org")).unwrap();
    let issuer = Ed25519Signer::from_seed(DEST_SEED);
    std::fs::write(dir.join("nostr/emetteur@exemple.org/.secret.dunikey"), astro_lifecycle::dunikey::render(&DEST_SEED)).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    let chain = Arc::new(RpcPool::new(SubxtConnector, PoolConfig { urls: BOOTSTRAP_RPC.iter().map(|s| (*s).to_owned()).collect(), cooldown: Duration::from_secs(30), reconnect_delay: Duration::from_secs(2) }));
    let wallets = Arc::new(Wallets::new(ORIGIN.to_owned(), db.clone()));
    let primals = Arc::new(SquidPrimal::new(BOOTSTRAP_SQUID.iter().map(|s| (*s).to_owned()).collect()));
    let (db2, publish): (Arc<Db>, astroportd::register::PublishFn) = (db.clone(), {
        let db = db.clone();
        Arc::new(move |e| {
            let db = db.clone();
            Box::pin(async move { db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &e).map(|_| ())).await.map_err(|x| x.to_string()) })
        })
    });
    let svc = Billets::new(db2.clone(), chain.clone(), wallets, Some(primals.clone()), ORIGIN.to_owned(), ActivityBus::new()).with_issuing(astroportd::billet::Issuing { accounts_dir: dir.join("nostr"), node_secret: zeroize::Zeroizing::new([0x42; 32]), public_base: Some("https://station.exemple".into()), publish });
    let balance = |a: Account| {
        let chain = chain.clone();
        async move { chain.balance(&a).await.unwrap().total.0 }
    };
    let settle = |want: Box<dyn Fn(u64) -> bool + Send + Sync>, a: Account| {
        let chain = chain.clone();
        async move {
            for _ in 0..40 {
                if want(chain.balance(&a).await.unwrap().total.0) {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(4)).await;
            }
        }
    };
    let issuer_hex = "ab".repeat(32);
    let before = balance(issuer.account()).await;
    eprintln!("émetteur {} : {}", issuer.account().ss58(), g1(before));
    assert!(before >= 140, "l'émetteur de test doit avoir au moins 1,40 Ğ1 : lancez d'abord le premier essai");

    let out = svc.issue(&issuer_hex, "emetteur@exemple.org", true, &json!({ "count": 1, "amount_zen": 2, "valid_days": 1, "return_to_issuer": true })).await.unwrap();
    let b = &out["billets"][0];
    let account: Account = b["g1pub"].as_str().unwrap().parse().unwrap();
    eprintln!("billet {} parts {} {}", account.ss58(), b["p1"], b["p2"]);
    let f = svc.fund(&issuer_hex, "emetteur@exemple.org", true, &account.ss58()).await.unwrap();
    eprintln!("financé : {f}");
    settle(Box::new(|c| c >= 120), account).await;
    assert_eq!(balance(account).await, 120, "1 Ğ1 d'activation + 0,20 Ğ1");

    // La date de fin passe (avancée à la main) : le porteur est refusé, la station renvoie la valeur à l'émetteur.
    db2.writer().execute("UPDATE nostr_tags SET value = '1' WHERE name = 'u'", vec![]).await.unwrap();
    for _ in 0..40 {
        if primals.primal_of(&account.ss58()).await.ok().flatten().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(6)).await;
    }
    let (p1, p2) = (b["p1"].as_str().unwrap(), b["p2"].as_str().unwrap());
    let refused = svc.handle(None, "redeem", &json!({ "parts": [p1, p2], "dest": issuer.account().ss58() })).await.unwrap_err();
    eprintln!("porteur refusé : {refused}");
    let issuer_before = balance(issuer.account()).await;
    assert_eq!(svc.sweep_once().await.unwrap(), 1, "la station renvoie le billet expiré");
    settle(Box::new(move |c| c == issuer_before + 20), issuer.account()).await;
    assert_eq!(balance(issuer.account()).await, issuer_before + 20, "l'émetteur retrouve la valeur du billet");
    settle(Box::new(|c| c == 0), account).await;
    assert_eq!(balance(account).await, 0, "billet vidé");
    let _ = std::fs::remove_dir_all(dir);
}
