//! `astroportd init` / `ylevel` pilotés comme un utilisateur : dans un HOME jetable, sans jamais toucher aux vraies clés.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_astroportd")).args(args).env("HOME", home).env_remove("ASTRO_GAME_DIR").output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn fresh(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("astroportd-initcli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join(".ssh")).unwrap();
    d
}

const LEGACY_KEY: &[u8] = b"fake-openssh-private-key-for-test\n";
/// PeerID que `keygen -t ipfs` du legacy donne pour sha512(LEGACY_KEY) (voir `ylevel.rs`).
const LEGACY_PEER: &str = "12D3KooWRxhgiLtvqhypB6uyQNbDD1D6wMV7Zt6vYYZF68czb9aG";

#[test]
fn origin_init_links_ssh_to_ipfs_keeps_the_old_key_and_is_replayable() {
    let home = fresh("origin");
    std::fs::write(home.join(".ssh/id_ed25519"), LEGACY_KEY).unwrap();
    let o = bin(&home, &["init", "--mode", "origin", "--lat", "43.6", "--lon", "1.44", "--email", "cap@example.org", "--yes"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains(LEGACY_PEER) && text(&o).contains("constellation ORIGIN"), "{}", text(&o));
    assert_eq!(std::fs::read(home.join(".ssh/origin.key")).unwrap(), LEGACY_KEY, "l'ancienne clé SSH est conservée");
    let conf = std::fs::read_to_string(home.join(".zen/astroportd/station.conf")).unwrap();
    assert!(conf.contains("mode = origin") && conf.contains(&"0".repeat(64)) && conf.contains("lat = 43.6"), "{conf}");
    assert!(home.join(".zen/game/nostr").is_dir() && home.join(".zen/astroportd/blobs").is_dir());

    let v = bin(&home, &["ylevel", "--verify"]);
    assert!(v.status.success() && text(&v).contains("Y-Level conforme"), "{}", text(&v));
    // Rejouer ne change ni les clés ni la position saisie.
    let before = std::fs::read(home.join(".zen/game/secret.june")).unwrap();
    assert!(bin(&home, &["init", "--mode", "origin", "--yes"]).status.success());
    assert_eq!(std::fs::read(home.join(".zen/game/secret.june")).unwrap(), before);
    assert!(std::fs::read_to_string(home.join(".zen/astroportd/station.conf")).unwrap().contains("captain_email = cap@example.org"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_swarm_key_is_required_for_zen_and_refused_for_origin_before_any_key_is_touched() {
    let home = fresh("zen");
    std::fs::write(home.join(".ssh/id_ed25519"), LEGACY_KEY).unwrap();
    let no_key = bin(&home, &["init", "--mode", "zen", "--yes"]);
    assert!(!no_key.status.success() && text(&no_key).contains("swarm.key obligatoire"), "{}", text(&no_key));
    let origin_with_key = bin(&home, &["init", "--mode", "origin", "--found-constellation", "--yes"]);
    assert!(!origin_with_key.status.success(), "{}", text(&origin_with_key));
    assert_eq!(std::fs::read(home.join(".ssh/id_ed25519")).unwrap(), LEGACY_KEY, "aucune clé touchée après une erreur de configuration");
    assert!(!home.join(".zen/game/secret.june").exists());

    // Fonder sa constellation : swarm.key neuve (0600), UPLANETNAME = secret partagé, réseau privé dans la commande de démarrage.
    let o = bin(&home, &["init", "--mode", "zen", "--found-constellation", "--yes"]);
    assert!(o.status.success(), "{}", text(&o));
    let key = std::fs::read_to_string(home.join(".zen/astroportd/swarm.key")).unwrap();
    assert!(key.starts_with("/key/swarm/psk/1.0.0/\n/base16/\n"));
    let secret = key.lines().last().unwrap();
    let conf = std::fs::read_to_string(home.join(".zen/astroportd/station.conf")).unwrap();
    assert!(conf.contains("mode = zen") && conf.contains(&format!("uplanetname = {secret}")), "{conf}");
    assert!(text(&o).contains("--swarm-key") && text(&o).contains("Empreinte de la swarm.key"));

    // Une constellation existante : la swarm.key reçue est validée puis copiée.
    let other = fresh("zen2");
    let o2 = bin(&other, &["init", "--mode", "zen", "--swarm-key", home.join(".zen/astroportd/swarm.key").to_str().unwrap(), "--yes"]);
    assert!(o2.status.success(), "{}", text(&o2));
    let fp = |o: &Output| text(o).lines().find(|l| l.starts_with("Empreinte")).map(str::to_owned);
    assert_eq!(fp(&o), fp(&o2), "même constellation, même empreinte");
    let bad = other.join("bad.key");
    std::fs::write(&bad, "pas une clé").unwrap();
    assert!(!bin(&other, &["init", "--mode", "zen", "--swarm-key", bad.to_str().unwrap(), "--yes"]).status.success());
    for d in [home, other] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn ylevel_verify_fails_when_the_ssh_key_does_not_match_the_node() {
    let home = fresh("mismatch");
    assert!(bin(&home, &["init", "--mode", "origin", "--yes"]).status.success());
    // Quelqu'un remplace la clé SSH par une autre : le lien SSH ⇔ IPFSNODEID est rompu.
    let intruder = home.join("intruder");
    assert!(Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(&intruder).status().map(|s| s.success()).unwrap_or(false), "ssh-keygen requis");
    std::fs::copy(home.join("intruder.pub"), home.join(".ssh/id_ed25519.pub")).unwrap();
    let v = bin(&home, &["ylevel", "--verify"]);
    assert!(!v.status.success() && text(&v).contains("✘ SSH"), "{}", text(&v));
    let soft = bin(&home, &["ylevel"]);
    assert!(soft.status.success(), "sans --verify, on informe sans échouer");
    let _ = std::fs::remove_dir_all(home);
}
