//! Console SSH dans l'interface : après une aide acceptée, l'ami aidant ouvre un terminal dans son navigateur au lieu
//! d'un client `ssh` à part.
//!
//! La station lance `ssh` **elle-même**, dans un pseudo-terminal, vers l'accès local `127.0.0.1:<port>` qu'ouvre
//! [`crate::forwards`] ; le navigateur n'a qu'un WebSocket vers ce terminal. Ce n'est donc jamais un shell de la
//! station locale : la commande est fixe, et ni le navigateur ni l'utilisateur distant n'en choisissent les arguments.
//!
//! Sûretés : ticket de 30 s à usage unique, délivré au seul Capitaine ; nom d'utilisateur validé (pas d'option
//! injectable) ; `ssh` sans configuration utilisateur, sans agent, sans redirection, sans échappement (`-e none`), sans
//! mot de passe (`BatchMode`) ; clé d'hôte retenue par station (`HostKeyAlias` = PeerID) ; la session est tuée à
//! l'échéance de l'aide et quand le navigateur se déconnecte.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;

use astro_core::ActivityBus;
use astro_ui::api::{ConsoleIn, ConsoleLink};
use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};
use tokio::sync::mpsc;

/// Durée de validité d'un ticket.
const TICKET_SECS: i64 = 30;
/// Tickets en attente au plus (le Capitaine est seul à en demander).
const MAX_PENDING: usize = 8;

/// Une session autorisée mais pas encore ouverte.
struct Pending {
    addr: SocketAddr,
    user: String,
    station: String,
    until: i64,
    expires: i64,
}

/// Délivre les tickets et lance les sessions.
pub struct Consoles {
    program: PathBuf,
    identity: Option<PathBuf>,
    known_hosts: PathBuf,
    activity: Option<ActivityBus>,
    pending: Mutex<HashMap<String, Pending>>,
}

/// Nom d'utilisateur Unix plausible : jamais d'option (`-o…`), d'espace ni de `@`.
pub fn valid_user(user: &str) -> bool {
    let mut chars = user.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_') && user.len() <= 32 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

impl Consoles {
    /// `identity` : clé privée SSH de la station (Y-Level) ; `known_hosts` : où retenir les clés d'hôte.
    pub fn new(identity: Option<PathBuf>, known_hosts: PathBuf, activity: Option<ActivityBus>) -> Self {
        Self { program: PathBuf::from("ssh"), identity, known_hosts, activity, pending: Mutex::new(HashMap::new()) }
    }

    /// Remplace le programme lancé (tests : un faux `ssh`).
    pub fn with_program(mut self, program: PathBuf) -> Self {
        self.program = program;
        self
    }

    /// Autorise une session vers `addr` (accès local de `station`) pour `user`, jusqu'à `until`. Rend le ticket.
    pub fn ticket(&self, addr: SocketAddr, user: &str, station: &str, until: i64) -> Result<String, String> {
        if !valid_user(user) {
            return Err("nom d'utilisateur invalide (minuscules, chiffres, _ et -)".into());
        }
        if !addr.ip().is_loopback() {
            return Err("l'accès doit être local".into());
        }
        let t = now();
        if until <= t {
            return Err("l'aide est échue".into());
        }
        let mut raw = [0u8; 24];
        getrandom::fill(&mut raw).map_err(|e| e.to_string())?;
        let ticket = hex::encode(raw);
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, p| p.expires > t);
        if pending.len() >= MAX_PENDING {
            return Err("trop de tickets en attente".into());
        }
        pending.insert(ticket.clone(), Pending { addr, user: user.to_owned(), station: station.to_owned(), until, expires: t + TICKET_SECS });
        Ok(ticket)
    }

    /// Consomme le ticket et lance la session. `None` : inconnu, périmé, déjà utilisé, ou `ssh` impossible à lancer.
    pub fn open(&self, ticket: &str) -> Option<ConsoleLink> {
        let p = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(ticket).filter(|p| p.expires > now() && p.until > now())?;
        match self.spawn(&p) {
            Ok(link) => {
                if let Some(a) = &self.activity {
                    a.publish_to(astro_core::Audience::Captain, "system", "ui:console", format!("console SSH ouverte vers {}@{} ({})", p.user, p.station, p.addr), None);
                }
                Some(link)
            }
            Err(e) => {
                eprintln!("Console : lancement impossible : {e}");
                None
            }
        }
    }

    fn command(&self, p: &Pending) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.env_clear().env("TERM", "xterm-256color").env("LANG", "C.UTF-8");
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        if let Some(home) = std::env::var_os("HOME") {
            cmd.env("HOME", home);
        }
        cmd.args(["-tt", "-F", "/dev/null", "-e", "none", "-o", "BatchMode=yes", "-o", "ClearAllForwardings=yes", "-o", "ForwardAgent=no", "-o", "ForwardX11=no", "-o", "PermitLocalCommand=no", "-o", "StrictHostKeyChecking=accept-new", "-o", "IdentitiesOnly=yes"]);
        cmd.arg("-o").arg(format!("UserKnownHostsFile={}", self.known_hosts.display()));
        cmd.arg("-o").arg(format!("HostKeyAlias={}", p.station));
        if let Some(id) = &self.identity {
            cmd.arg("-i").arg(id);
        }
        cmd.arg("-p").arg(p.addr.port().to_string()).arg("--").arg(format!("{}@{}", p.user, p.addr.ip()));
        cmd
    }

    fn spawn(&self, p: &Pending) -> Result<ConsoleLink, String> {
        // Pseudo-terminal : le maître reste ici, l'esclave devient l'entrée, la sortie et l'erreur d'`ssh`.
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).map_err(|e| e.to_string())?;
        grantpt(&master).map_err(|e| e.to_string())?;
        unlockpt(&master).map_err(|e| e.to_string())?;
        let name = ptsname(&master, Vec::new()).map_err(|e| e.to_string())?;
        let slave: OwnedFd = rustix::fs::open(name.as_c_str(), OFlags::RDWR | OFlags::NOCTTY, Mode::empty()).map_err(|e| e.to_string())?;
        let _ = tcsetwinsize(&slave, Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 });
        let mut cmd = self.command(p);
        cmd.stdin(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?)).stdout(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?)).stderr(Stdio::from(slave)).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("{} : {e}", self.program.display()))?;
        let pid = child.id().and_then(|i| i32::try_from(i).ok()).and_then(rustix::process::Pid::from_raw);

        let reader: File = File::from(master.try_clone().map_err(|e| e.to_string())?);
        let mut writer: File = File::from(master);
        let resizer = writer.try_clone().map_err(|e| e.to_string())?;
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(64);
        let (in_tx, mut in_rx) = mpsc::channel::<ConsoleIn>(64);

        // Sortie du terminal → navigateur (lecture bloquante sur son propre thread ; EIO = le processus est parti).
        std::thread::spawn(move || {
            let (mut reader, mut buf) = (reader, vec![0u8; 8192]);
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        // Navigateur → terminal, et surveillance de l'échéance.
        let until = std::time::Duration::from_secs(u64::try_from(p.until - now()).unwrap_or(0));
        tokio::spawn(async move {
            let deadline = tokio::time::sleep(until);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    msg = in_rx.recv() => match msg {
                        Some(ConsoleIn::Data(d)) => {
                            let r = tokio::task::block_in_place(|| writer.write_all(&d));
                            if r.is_err() { break }
                        }
                        Some(ConsoleIn::Resize(cols, rows)) => {
                            let _ = tcsetwinsize(&resizer, Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 });
                            if let Some(pid) = pid {
                                let _ = rustix::process::kill_process(pid, rustix::process::Signal::WINCH);
                            }
                        }
                        None => break,
                    },
                    status = child.wait() => { let _ = status; break }
                    () = &mut deadline => break,
                }
            }
            let _ = child.kill().await;
        });
        Ok(ConsoleLink { input: in_tx, output: out_rx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_names_cannot_smuggle_options() {
        for ok in ["fred", "_svc", "a-b_9"] {
            assert!(valid_user(ok), "{ok}");
        }
        for bad in ["", "-oProxyCommand=x", "a b", "a@b", "Fred", "9a", "a;b", &"a".repeat(33)] {
            assert!(!valid_user(bad), "{bad}");
        }
    }

    #[test]
    fn tickets_are_single_use_local_and_bounded() {
        let c = Consoles::new(None, PathBuf::from("/nonexistent/kh"), None).with_program(PathBuf::from("/nonexistent/ssh"));
        let addr: SocketAddr = "127.0.0.1:2222".parse().unwrap();
        let soon = now() + 60;
        assert!(c.ticket(addr, "-oX", "peer", soon).is_err());
        assert!(c.ticket("10.0.0.1:22".parse().unwrap(), "fred", "peer", soon).is_err(), "accès non local");
        assert!(c.ticket(addr, "fred", "peer", now() - 1).is_err(), "aide échue");
        let t = c.ticket(addr, "fred", "peer", soon).unwrap();
        assert!(c.open("inconnu").is_none());
        assert!(c.open(&t).is_none(), "ssh introuvable : pas de session, mais le ticket est consommé");
        assert!(c.open(&t).is_none());
        for _ in 0..MAX_PENDING {
            c.ticket(addr, "fred", "peer", soon).unwrap();
        }
        assert!(c.ticket(addr, "fred", "peer", soon).is_err(), "trop de tickets");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_relays_bytes_through_a_pty_and_ends_with_the_process() {
        // Faux « ssh » : un script qui répète ce qu'il reçoit puis sort sur « bye ».
        let dir = std::env::temp_dir().join(format!("astroportd-console-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fakessh");
        std::fs::write(&fake, "#!/bin/sh\necho \"ARGS: $*\"\nwhile read l; do echo \"got:$l\"; [ \"$l\" = bye ] && exit 0; done\n").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let c = Consoles::new(Some(PathBuf::from("/id")), dir.join("kh"), None).with_program(fake);
        let t = c.ticket("127.0.0.1:2299".parse().unwrap(), "fred", "12D3KooWx", now() + 60).unwrap();
        let ConsoleLink { input, mut output } = c.open(&t).unwrap();
        input.send(ConsoleIn::Data(b"salut\nbye\n".to_vec())).await.unwrap();
        let mut seen = String::new();
        while let Some(chunk) = tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await.unwrap() {
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert!(seen.contains("got:salut") && seen.contains("got:bye"), "{seen}");
        assert!(seen.contains("-p 2299 -- fred@127.0.0.1") && seen.contains("HostKeyAlias=12D3KooWx") && seen.contains("-i /id") && seen.contains("-e none") && seen.contains("BatchMode=yes"), "{seen}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
