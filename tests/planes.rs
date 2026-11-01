//! Plusieurs plans, un seul processus : deux UPlanet (ORIGIN et ZEN1) servies par `astroportd planes`, même NODE, bases séparées, routage par
//! nom d'hôte, arrêt propre ; et `--plane` qui remplit les chemins d'une commande.

#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_astroportd");

struct Rig {
    home: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn astro(home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(BIN).args(args).env("HOME", home).env_remove("ASTRO_GAME_DIR").output().unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

/// Requête HTTP/1.1 brute vers `addr` avec l'en-tête `Host` donné : rend (code, corps).
fn get(addr: SocketAddr, host: &str, path: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let code = raw.split_whitespace().nth(1)?.parse().ok()?;
    Some((code, raw.split_once("\r\n\r\n").map(|x| x.1).unwrap_or_default().to_owned()))
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < Duration::from_secs(60), "trop long : {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn rig(tag: &str) -> Rig {
    let home = std::env::temp_dir().join(format!("astroportd-planes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_str().unwrap();
    let (ok, out) = astro(&home, &["init", "--mode", "origin", "--lat", "43.6", "--lon", "1.44", "--home", h, "--game-dir", &format!("{h}/.zen/game"), "--no-ssh"]);
    assert!(ok, "init : {out}");
    Rig { home }
}

#[test]
fn two_planes_run_in_one_process_with_one_node_separate_databases_and_host_routing() {
    let r = rig("run");
    let (o_port, z_port, front_port, p2p) = (free_port(), free_port(), free_port(), free_port());
    // L'assistant fonde ZEN1 (clé neuve) ; ORIGIN garde ses ports d'essai.
    let (ok, out) = astro(&r.home, &["plane-setup", "add", "--name", "zen1", "--found", "--http-listen", &format!("127.0.0.1:{z_port}"), "--p2p-port", &p2p.to_string()]);
    assert!(ok && out.contains("plan « zen1 » ajouté"), "{out}");
    let cfg = r.home.join(".zen/astroportd/planes.json");
    let mut file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    file["planes"][0]["http_listen"] = format!("127.0.0.1:{o_port}").into();
    file["planes"][0].as_object_mut().unwrap().remove("kubo"); // pas de Kubo dans l'essai
    std::fs::write(&cfg, file.to_string()).unwrap();
    let (ok, out) = astro(&r.home, &["planes", "--check"]);
    assert!(ok && out.contains("configuration valide : 2 plan(s)"), "{out}");

    let mut proc = Proc(Command::new(BIN).args(["planes", "--front", &format!("127.0.0.1:{front_port}")]).env("HOME", &r.home).env_remove("ASTRO_GAME_DIR").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap());
    let (o, z, f): (SocketAddr, SocketAddr, SocketAddr) = (format!("127.0.0.1:{o_port}").parse().unwrap(), format!("127.0.0.1:{z_port}").parse().unwrap(), format!("127.0.0.1:{front_port}").parse().unwrap());
    wait_for("les deux plans répondent", || get(o, "x", "/api/v1/register").is_some_and(|r| r.0 == 200) && get(z, "x", "/api/v1/register").is_some_and(|r| r.0 == 200));

    // Chaque plan a sa constellation : ORIGIN ouverte, ZEN1 privée ; le nom d'hôte choisit le plan sur le port commun.
    let reg = |addr, host: &str| serde_json::from_str::<serde_json::Value>(&get(addr, host, "/api/v1/register").unwrap().1).unwrap()["constellation"].as_str().unwrap().to_owned();
    let (origin, zen) = (reg(o, "x"), reg(z, "x"));
    assert_ne!(origin, zen, "deux plans, deux constellations : {origin} / {zen}");
    wait_for("le routage par hôte répond", || get(f, "station.localhost", "/api/v1/register").is_some_and(|r| r.0 == 200));
    assert_eq!(reg(f, "station.localhost"), origin, "le nom nu va à ORIGIN");
    assert_eq!(reg(f, &format!("zen1.localhost:{front_port}")), zen, "zen1.<domaine> va au plan zen1");
    assert_eq!(reg(f, "inconnu.localhost"), origin, "un nom inconnu va à ORIGIN, jamais à un plan au hasard");

    // Deux bases, un seul bassin de blobs, un seul NODE (liens vers les fichiers d'ORIGIN).
    let root = r.home.join(".zen/astroportd");
    assert!(root.join("uplanet.db").is_file() && root.join("zen1/uplanet.db").is_file(), "une base par plan ; ORIGIN garde sa base et son dossier actuels");
    assert!(root.join("pool").is_dir());
    let link = std::fs::read_link(r.home.join(".zen/game/zen1/secret.nostr")).unwrap();
    assert_eq!(link, r.home.join(".zen/game/secret.nostr"), "même clé de NODE donc même IPFSNODEID");

    // Arrêt propre sur SIGINT : les deux plans s'arrêtent, code 0.
    let pid = proc.0.id().to_string();
    assert!(Command::new("kill").args(["-INT", &pid]).status().unwrap().success());
    wait_for("arrêt du processus", || proc.0.try_wait().unwrap().is_some());
    assert!(proc.0.wait().unwrap().success(), "arrêt propre");
}

/// Possède le processus d'essai : il est tué si le test échoue avant l'arrêt propre.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_plane_flag_fills_the_paths_of_a_command_and_refuses_an_unknown_plane() {
    let r = rig("flag");
    let (ok, out) = astro(&r.home, &["plane-setup", "add", "--name", "zen1", "--found"]);
    assert!(ok, "{out}");
    let (ok, out) = astro(&r.home, &["pool-status", "--plane", "zen1"]);
    assert!(ok, "{out}");
    assert!(out.contains("0 blob(s)") && out.contains(".zen/astroportd/pool"), "le bassin commun est celui du plan : {out}");
    assert!(out.contains("zen1/uplanet.db"), "la base est celle du plan : {out}");
    let (ok, out) = astro(&r.home, &["pool-status", "--plane", "zen9"]);
    assert!(!ok && out.contains("plan inconnu") && out.contains("origin, zen1"), "{out}");
    // Une option donnée n'est jamais remplacée.
    let (ok, out) = astro(&r.home, &["pool-status", "--plane=zen1", "--db", "/nulle/part.db"]);
    assert!(ok && out.contains("/nulle/part.db") && !out.contains("zen1/uplanet.db"), "{out}");
}
