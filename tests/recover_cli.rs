//! `astroportd multipass create | card | recover` pilotés comme un utilisateur, sur deux stations jetables : la clé de secours en 3 parts (P1 + P3)
//! imprimée sur la première rétablit le même compte sur la seconde, sans rien publier qui puisse écraser l'original.

#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use astro_db::Db;

fn bin(home: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_astroportd"))
        .args(args)
        .env("HOME", home)
        .env_remove("ASTRO_GAME_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(text) = stdin {
        child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// La ligne JSON de la sortie (la commande écrit d'abord le résumé de l'identité de la station).
fn json_of(o: &Output) -> serde_json::Value {
    let out = String::from_utf8_lossy(&o.stdout);
    let line = out.lines().find(|l| l.starts_with('{'));
    assert!(line.is_some(), "aucune ligne JSON : {}", text(o));
    serde_json::from_str(line.unwrap()).unwrap()
}

fn station(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("astroportd-recover-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_ed25519"), format!("fake-openssh-private-key-{tag}\n")).unwrap();
    let o = bin(&home, &["init", "--mode", "origin", "--lat", "43.6", "--lon", "1.44", "--email", "cap@example.org", "--yes"], None);
    assert!(o.status.success(), "{}", text(&o));
    home
}

#[tokio::test(flavor = "multi_thread")]
async fn the_printed_card_restores_the_same_account_on_a_blank_station_without_publishing_anything() {
    let (old, fresh) = (station("old"), station("new"));
    let created = bin(&old, &["multipass", "create", "alice@example.org", "--json"], None);
    assert!(created.status.success(), "{}", text(&created));
    let made = json_of(&created);
    let (card, hex, nsec) = (made["card"].as_str().unwrap().to_owned(), made["hex"].as_str().unwrap().to_owned(), made["nsec"].as_str().unwrap().to_owned());
    let (salt, pepper) = (made["salt"].as_str().unwrap().to_owned(), made["pepper"].as_str().unwrap().to_owned());
    assert!(card.starts_with("astro-share:v1|alice@example.org|"), "{card}");
    assert!(!card.contains(&salt) && !card.contains(&pepper), "la part du membre ne contient pas le secret");

    // P1 se réimprime à volonté (texte ou QR) et reste identique ; P3 est la part que gardent les amis.
    let p3 = bin(&old, &["multipass", "card", "alice@example.org", "--part", "3"], None);
    assert!(p3.status.success(), "{}", text(&p3));
    let p3 = String::from_utf8_lossy(&p3.stdout).lines().find(|l| l.starts_with("astro-share:")).unwrap_or_default().to_owned();
    assert!(p3.starts_with("astro-share:v1|alice@example.org|") && p3 != card, "{p3}");
    let both = format!("{card}\n{p3}\n");
    let again = bin(&old, &["multipass", "card", "alice@example.org"], None);
    assert!(again.status.success() && String::from_utf8_lossy(&again.stdout).lines().any(|l| l == card), "{}", text(&again));
    let svg = bin(&old, &["multipass", "card", "alice@example.org", "--svg"], None);
    assert!(svg.status.success() && String::from_utf8_lossy(&svg.stdout).contains("<svg"), "{}", text(&svg));

    // Une part seule ne rétablit rien.
    let alone = bin(&station("alone"), &["multipass", "recover", "-"], Some(&card));
    assert!(!alone.status.success() && text(&alone).contains("deux parts"), "{}", text(&alone));

    // Station vierge : P1 + P3 (lues sur l'entrée standard, jamais en argument) rétablissent la même clé.
    let restored = bin(&fresh, &["multipass", "recover", "-", "--json"], Some(&both));
    assert!(restored.status.success(), "{}", text(&restored));
    let r = json_of(&restored);
    assert_eq!((r["hex"].as_str().unwrap(), r["restoring"].as_bool()), (hex.as_str(), Some(true)));
    let pass = r["pass"].as_str().unwrap();
    assert!(pass.len() == 6 && pass.bytes().all(|b| b.is_ascii_digit()), "un nouveau PASS à 6 chiffres : {pass}");
    assert_ne!(Some(pass), made["pass"].as_str(), "l'ancien PASS n'est pas récupérable");
    let secret = std::fs::read_to_string(fresh.join(".zen/game/nostr/alice@example.org/.secret.nostr")).unwrap();
    assert!(secret.contains(&format!("NSEC={nsec};")), "la même clé secrète");
    // La station rétablie retrouve les trois parts d'origine : P1 imprimé et P3 chez les amis restent valables.
    for (n, expected) in [(1, Some(card.as_str())), (3, Some(p3.as_str())), (2, None)] {
        let kept = std::fs::read_to_string(fresh.join(format!(".zen/game/nostr/alice@example.org/.ssss/{n}"))).unwrap();
        assert!(expected.is_none_or(|e| kept == e), "P{n} identique à celle d'origine");
    }

    // Rien n'a été publié ni stocké qui puisse écraser les vrais profil et contacts ; la restauration est marquée en cours.
    let db = Db::open(&fresh.join(".zen/astroportd/uplanet.db"), 1).unwrap();
    let h = hex.clone();
    let (events, hosted, pending): (i64, i64, i64) = db
        .readers()
        .read(move |c| {
            Ok((
                c.query_row("SELECT COUNT(*) FROM nostr_events WHERE pubkey = ?1", [&h], |r| r.get(0))?,
                c.query_row("SELECT COUNT(*) FROM identities WHERE hex = ?1 AND hosted = 1 AND email = 'alice@example.org' AND key_formula = 'isolated'", [&h], |r| r.get(0))?,
                c.query_row("SELECT COUNT(*) FROM kv WHERE scope = 'restore' AND key = ?1 AND value = 'pending'", [&h], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!((events, hosted, pending), (0, 1, 1));
    drop(db);

    // Déjà rétabli ici : refusé. Carte d'une autre UPlanet ou altérée : refusée avant de créer quoi que ce soit.
    let twice = bin(&fresh, &["multipass", "recover", "-"], Some(&both));
    assert!(!twice.status.success(), "{}", text(&twice));
    // L'ancienne carte complète reste acceptée (ici reconstruite à la main à partir du SALT et du PEPPER).
    let uplanet = card.split('|').nth(4).unwrap().to_owned();
    let full = format!("astro-secret:v1|alice@example.org|{salt}|{pepper}|isolated|{uplanet}|{hex}");
    let old_card = bin(&station("legacy"), &["multipass", "recover", "-", "--json"], Some(&full));
    assert!(old_card.status.success(), "{}", text(&old_card));
    assert_eq!(json_of(&old_card)["hex"].as_str(), Some(hex.as_str()));
    let fields: Vec<&str> = full.split('|').collect();
    let other = [&fields[..5], &["0123456789abcdef"], &fields[6..]].concat().join("|");
    let blank = station("blank");
    let wrong = bin(&blank, &["multipass", "recover", "-"], Some(&other));
    assert!(!wrong.status.success() && text(&wrong).contains("autre UPlanet"), "{}", text(&wrong));
    let typo = bin(&blank, &["multipass", "recover", "-"], Some(&full.replacen(fields[2], &format!("{}X", fields[2]), 1)));
    assert!(!typo.status.success() && text(&typo).contains("ne redonnent pas"), "{}", text(&typo));
    assert!(!blank.join(".zen/game/nostr/alice@example.org").exists(), "aucun compte à moitié créé");
    for home in [old, fresh, blank] {
        let _ = std::fs::remove_dir_all(home);
    }
}
