//! Cycle complet du Sprint 7 : identités détectées au démarrage → transfert Ğ1-N² validé → loyer prélevé →
//! ventilation 3×1/3 appliquée → déficit : bascule gracieuse dans le Love Ledger (jamais d'arrêt de la station).
//!
//! Aucune clé réelle : toute l'arborescence `game/` est générée.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::time::Duration;

use astro_core::Amount;
use astro_core::week::iso_week;
use astro_db::Db;
use astro_duniter::{Account, Balance, Chain, ChainError, Ed25519Signer, FundsNotifier, TransferEvent, TransferMode, TxOutcome};
use astro_economy::love::Attestation;
use astro_economy::{Ledger, LedgerConfig, LedgerError, Outcome, SplitConfig, WeekInput, WeekOutcome, close_week, store as economy_store};
use astro_lifecycle::discovery::{Layout, discover, load_multipass_secret};
use astro_lifecycle::state::{AccountState, Action, Contract, Cycle, decide};
use astro_lifecycle::store as life_store;
use astro_nostr_proto::{Event, UnsignedEvent, kind, pubkey_of};
use tokio::sync::mpsc;

const NOW: u64 = 1_792_000_000; // 2026-10-14

fn nsec(secret: &[u8; 32]) -> String {
    bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), secret).unwrap()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// NODE = clés 1 (Nostr) et 2 (ed25519) ; Capitaine alice = 3 ; locataire bob = 4.
fn build_station(game: &Path) {
    write(&game.join("secret.nostr"), &format!("NSEC={}; HEX={}\n", nsec(&[1; 32]), pubkey_of(&[1; 32]).unwrap()));
    write(&game.join("secret.NODE.dunikey"), &astro_lifecycle::dunikey::render(&[2; 32]));
    write(&game.join("players/.current/.player"), "alice@example.org\n");
    for (email, n) in [("alice@example.org", 3u8), ("bob@example.org", 4)] {
        write(&game.join("nostr").join(email).join(".secret.nostr"), &format!("NSEC={}; HEX={}\n", nsec(&[n; 32]), pubkey_of(&[n; 32]).unwrap()));
        write(&game.join("nostr").join(email).join(".keyformula"), "nostr=isolated g1=isolated ipns=isolated");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in [game.join("secret.nostr"), game.join("secret.NODE.dunikey")].into_iter().chain(["alice@example.org", "bob@example.org"].map(|e| game.join("nostr").join(e).join(".secret.nostr"))) {
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

fn n2_event(secret: &[u8; 32], to: &str, amount: &str, prev: &str, d: &str, mint: bool, at: u64) -> Event {
    let mut tags = vec![vec!["d".into(), d.into()], vec!["p".into(), to.into()], vec!["amount".into(), amount.into()], vec!["prev".into(), prev.into()], vec!["t".into(), "g1-n2".into()]];
    if mint {
        tags.push(vec!["t".into(), "mint".into()]);
    }
    UnsignedEvent { created_at: at, kind: kind::LEDGER_N2, tags, content: String::new() }.sign(secret).unwrap()
}

/// Chaîne simulée : on y diffuse un virement vers le compte Ğ1 du NODE.
struct FakeChain {
    to: Account,
}

impl Chain for FakeChain {
    async fn balance(&self, _: &Account) -> Result<Balance, ChainError> {
        Ok(Balance { total: Amount(0), transferable: Amount(0), unclaimed_uds: Amount(0) })
    }

    async fn transfer(&self, _: &Ed25519Signer, _: &Account, _: Amount, _: TransferMode, _: Option<&str>) -> Result<TxOutcome, ChainError> {
        Err(ChainError::Invalid("non utilisé".into()))
    }

    async fn watch_transfers(&self, out: mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
        let ev = TransferEvent { block_number: 42, block_hash: [1; 32], from: Account([9; 32]), to: self.to, amount: Amount(5_000) };
        out.send(ev).await.map_err(|e| ChainError::Invalid(e.to_string()))
    }

    fn existential_deposit(&self) -> Amount {
        Amount(100)
    }
}

async fn count(db: &Db, sql: &'static str, param: String) -> i64 {
    db.readers().read(move |c| Ok(c.query_row(sql, [param], |r| r.get::<_, i64>(0))?)).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn identities_ledger_split_and_love_ledger_work_end_to_end() {
    let dir = std::env::temp_dir().join(format!("astroportd-cycle-{}", std::process::id()));
    let game = dir.join("game");
    build_station(&game);
    let db = Db::open(&dir.join("uplanet.db"), 2).unwrap();

    // 1. Découverte au démarrage, sans variable d'environnement ; inscription en base.
    let found = discover(&Layout::new(&game)).unwrap();
    assert!(found.problems.is_empty(), "{:?}", found.problems);
    db.writer().batch(life_store::identity_statements(&found)).await.unwrap();
    let node_hex = found.node.nostr_hex.clone();
    let captain = found.captain().unwrap().clone();
    let bob = found.multipasses.iter().find(|m| m.email == "bob@example.org").unwrap().clone();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM identities WHERE hosted = 1 AND role = ?1", "captain".into()).await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM identities WHERE role = 'multipass' AND key_formula = ?1", "isolated".into()).await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM identities WHERE role = 'node' AND ipns = ?1", found.node.peer_id.clone()).await, 1);

    // 2. Registre Ğ1-N² : le Capitaine crée 100 ẐEN pour bob ; bob règle 50 ẐEN de loyer au NODE ; les fraudes sont refusées.
    let (captain_sk, bob_sk) = (load_multipass_secret(&captain).unwrap(), load_multipass_secret(&bob).unwrap());
    let mut ledger = Ledger::new(LedgerConfig::with_mint_authorities([captain.hex.clone()]));
    let mint = n2_event(&captain_sk, &bob.hex, "100.00", "genesis", "mint-1", true, NOW - 1000);
    let rent = n2_event(&bob_sk, &node_hex, "50.00", "genesis", "rent-w42", false, NOW - 900);
    let report = ledger.replay(vec![rent.clone(), mint.clone()], NOW);
    assert_eq!(report.applied, [mint.id.clone(), rent.id.clone()], "convergence malgré l'ordre d'arrivée");
    assert_eq!((ledger.balance(&bob.hex), ledger.balance(&node_hex)), (Amount(5_000), Amount(5_000)));
    let overdraft = n2_event(&bob_sk, &node_hex, "50.01", &rent.id, "rent-w43", false, NOW - 800);
    assert_eq!(ledger.apply(&overdraft, NOW), Err(LedgerError::Overdraft { have: Amount(5_000), need: Amount(5_001) }));
    let double = n2_event(&bob_sk, &captain.hex, "50.00", "genesis", "other", false, NOW - 700);
    assert!(matches!(ledger.apply(&double, NOW), Err(LedgerError::DoubleSpend(_))));
    assert!(ledger.is_frozen(&bob.hex));
    assert!(matches!(ledger.apply(&rent, NOW), Ok(Outcome::Duplicate)));
    // Le nœud le plus récent du registre reste le seul état : le solde de bob n'a pas bougé.
    assert_eq!(ledger.balance(&bob.hex), Amount(5_000));

    // 3. Réception de fonds sur le compte Ğ1 de la station : notification instantanée.
    let station_g1 = Account([0xAB; 32]);
    let notifier = FundsNotifier::new([station_g1]);
    let mut rx = notifier.subscribe();
    tokio::time::timeout(Duration::from_secs(5), notifier.run(&FakeChain { to: station_g1 })).await.unwrap().unwrap();
    assert_eq!(rx.try_recv().unwrap().amount, Amount(5_000));

    // 4. Cycle de vie : bob (locataire depuis 7 jours) est prélevé de 50 ẐEN avec son solde N².
    let state = AccountState::new(Contract::ActiveRental, NOW - 8 * 86_400);
    let decision = decide(&state, &Cycle { rent_due: Amount(5_000), balance: Amount(5_000), shares_valid: true }, NOW);
    assert_eq!(decision.action, Action::Charge(Amount(5_000)));
    db.writer().batch(vec![life_store::save_state(&bob.hex, &decision.state, NOW), life_store::claim_week(&bob.hex, &iso_week(NOW), NOW)]).await.unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM kv WHERE scope = 'lifecycle' AND key = ?1", bob.hex.clone()).await, 1);

    // 5. Semaine 42 : recettes 50, charges 20 → surplus 30 ventilé en 3×1/3 ; la clôture est idempotente.
    let week42 = "2026-W42".to_owned();
    let cfg = SplitConfig::default();
    let input = WeekInput { week: week42.clone(), revenue: Amount(5_000), costs: Amount(2_000), cash_balance: Amount(0), ytd_profit: Amount(0) };
    let outcome = close_week(&cfg, &input).unwrap();
    let WeekOutcome::Split(alloc) = &outcome else { unreachable!("surplus attendu : {outcome:?}") };
    assert_eq!(alloc.total(), 3_000);
    assert_eq!((alloc.is_provision, alloc.captain_bonus), (Amount(450), Amount(25)), "IS 15 % de 30, prime 1 % du net");
    assert_eq!((alloc.cash, alloc.rnd, alloc.assets), (Amount(843), Amount(841), Amount(841)));
    let stmts = economy_store::close_statements(&week42, &outcome, &captain.hex, NOW);
    assert!(db.writer().batch(stmts).await.unwrap().changes > 0);
    let key = economy_store::week_key(&week42);
    assert_eq!(count(&db, economy_store::week_is_closed(), key.clone()).await, 1, "garde posée : une relance ne ventile pas deux fois");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM journal WHERE stream = 'economy.3x13' AND at = ?1", NOW.to_string()).await, 1);

    // 6. Semaine 43 : recettes 0, charges 30, trésorerie 0 → Bénévolat Actif, la station continue.
    let week43 = "2026-W43".to_owned();
    let deficit = close_week(&cfg, &WeekInput { week: week43.clone(), revenue: Amount(0), costs: Amount(3_000), cash_balance: Amount(0), ytd_profit: Amount(3_000) }).unwrap();
    let WeekOutcome::Volunteer(v) = &deficit else { unreachable!("bénévolat attendu : {deficit:?}") };
    assert_eq!(v.deficit, Amount(3_000));
    db.writer().batch(economy_store::close_statements(&week43, &deficit, &captain.hex, NOW + 7 * 86_400)).await.unwrap();
    // Attestation TrocZen 30305 signée par la station (clé NODE découverte), puis rattachée à la ligne du Love Ledger.
    let att = Attestation::new(&captain.hex, v, NOW + 7 * 86_400);
    let signed = att.unsigned(NOW + 7 * 86_400).sign(&found.node.nostr_secret()).unwrap();
    signed.verify().unwrap();
    assert_eq!((signed.kind, signed.pubkey.as_str()), (30305, node_hex.as_str()));
    db.writer().batch(vec![economy_store::link_attestation(&att, &signed.id)]).await.unwrap();
    let (total, weeks, linked): (i64, i64, i64) = db
        .readers()
        .read(|c| {
            let (t, w) = c.query_row(economy_store::love_totals(), [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            let l = c.query_row("SELECT COUNT(*) FROM love_ledger WHERE src_event IS NOT NULL", [], |r| r.get::<_, i64>(0))?;
            Ok((t, w, l))
        })
        .await
        .unwrap();
    assert_eq!((total, weeks, linked), (3000, 1, 1));

    db.writer().shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(dir);
}
