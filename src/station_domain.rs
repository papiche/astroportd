//! Nom de domaine de la station, réglé depuis le cockpit, et redémarrage demandé par le Capitaine.
//!
//! Le Capitaine n'a plus à relancer `astroportd init --domain` dans un terminal : le cockpit vérifie le nom (forme, DNS,
//! ports 80/443 de cette machine), l'écrit dans `station.conf`, puis propose de **redémarrer** la station. Au démarrage,
//! `serve` sans `--domain` reprend le domaine de `station.conf` ([`domain_from_conf`]) et obtient seul son certificat ;
//! derrière un reverse proxy (`--http-listen` imposé par `init`), ce domaine devient son domaine public (`--public-domain`).
//!
//! Redémarrer = s'arrêter proprement avec un code d'échec : l'unité systemd écrite par `init` (`Restart=on-failure`)
//! relance alors la station. Hors systemd, rien ne la relancerait : le cockpit donne la commande au lieu du bouton.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Port de l'interface web quand la station ne sert pas elle-même HTTPS : en clair sur `127.0.0.1` (essai local), sur
/// toutes les interfaces derrière un reverse proxy (NPM dans Docker la joint par `172.17.0.1`, un NPM d'une autre
/// machine par le réseau local). La visio (STUN/TURN) écoute sur le même numéro en UDP.
pub const UI_PORT: u16 = 8090;

/// Clé de `station.conf` qui dit qu'un reverse proxy sert le domaine (`web = proxy`) ; vide : la station décide.
pub const WEB_KEY: &str = "web";

/// Écoute de l'interface derrière un reverse proxy.
pub fn proxy_listen() -> std::net::SocketAddr {
    std::net::SocketAddr::from(([0, 0, 0, 0], UI_PORT))
}

/// Écoute de l'interface sans domaine (essai local).
pub fn local_listen() -> std::net::SocketAddr {
    std::net::SocketAddr::from(([127, 0, 0, 1], UI_PORT))
}

/// Signal de redémarrage : notifié par l'API, attendu par `serve::run`.
pub static RESTART: Notify = Notify::const_new();
/// `serve::run` (une seule constellation par processus) sait redémarrer ; `planes` ne l'arme pas.
static ARMED: AtomicBool = AtomicBool::new(false);
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Domaine de `station.conf` écarté au démarrage (ports indisponibles) : la cause, pour le cockpit.
static REFUSED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Note pourquoi le domaine enregistré n'a pas été appliqué.
pub fn set_refused(why: String) {
    let _ = REFUSED.set(why);
}

/// Cause du refus du domaine enregistré, s'il y en a eu un.
pub fn refused() -> Option<&'static str> {
    REFUSED.get().map(String::as_str)
}

/// Appelé par `serve::run` : le redémarrage depuis le cockpit devient possible.
pub fn arm() {
    ARMED.store(true, Ordering::SeqCst);
}

/// Le bouton « Redémarrer » a-t-il un sens ici ? Il faut une boucle qui écoute (`serve`) et systemd pour relancer.
pub fn restart_available() -> bool {
    ARMED.load(Ordering::SeqCst) && std::env::var_os("INVOCATION_ID").is_some()
}

/// Demande l'arrêt (après la réponse HTTP : `delay`).
pub fn request_restart(delay: std::time::Duration) {
    REQUESTED.store(true, Ordering::SeqCst);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        RESTART.notify_one();
    });
}

/// L'arrêt en cours vient-il d'une demande de redémarrage ?
pub fn restart_requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

/// Nom de domaine acceptable : lettres, chiffres, tirets, au moins deux étiquettes, 253 caractères au plus.
pub fn normalize_domain(input: &str) -> Result<String, String> {
    let d = input.trim().trim_end_matches('.').to_ascii_lowercase();
    let d = d.strip_prefix("https://").or_else(|| d.strip_prefix("http://")).unwrap_or(&d).trim_end_matches('/').to_owned();
    let labels: Vec<&str> = d.split('.').collect();
    let label_ok = |l: &&str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if d.len() > 253 || labels.len() < 2 || !labels.iter().all(label_ok) || labels.last().is_some_and(|t| t.chars().all(|c| c.is_ascii_digit())) {
        return Err(format!("« {} » n'est pas un nom de domaine (exemple : station.mondomaine.org)", input.trim()));
    }
    Ok(d)
}

/// Remplace (ou ajoute) la ligne `clé = valeur` d'un `station.conf`, sans toucher au reste. Fichier en `0600`.
pub fn set_conf_value(path: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut found = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| match l.split_once('=') {
            Some((k, _)) if !l.trim_start().starts_with('#') && k.trim() == key => {
                found = true;
                format!("{key} = {value}")
            }
            _ => l.to_owned(),
        })
        .collect();
    if !found {
        lines.push(format!("{key} = {value}"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(path)?, (lines.join("\n") + "\n").as_bytes())?;
    // `mode` ne vaut qu'à la création : un fichier déjà là garde ses droits, on les resserre.
    #[cfg(unix)]
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}

/// Domaine et email de contact ACME repris de `station.conf` (`domain`, `captain_email`) pour un `serve` lancé sans
/// `--domain`. L'appelant décide : HTTPS de la station, ou domaine servi par un reverse proxy (`--http-listen` imposé).
pub fn domain_from_conf(conf: &Path) -> Option<(String, Option<String>)> {
    let conf = crate::init::read_conf(conf);
    let get = |k: &str| conf.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
    let domain = normalize_domain(&get("domain")?).ok()?;
    Some((domain, get("captain_email")))
}

/// Adresse du visiteur derrière un reverse proxy : quand la connexion vient du proxy (boucle locale ou réseau privé),
/// l'IP retenue est la **dernière** de `X-Forwarded-For`, celle que le proxy a lui-même ajoutée (les précédentes, posées
/// par le client, ne sont pas crues). Une connexion directe d'Internet garde son adresse : l'en-tête ne s'usurpe pas.
pub fn forwarded_client(peer: std::net::IpAddr, xff: Option<&str>) -> std::net::IpAddr {
    let from_proxy = match peer {
        std::net::IpAddr::V4(v) => v.is_loopback() || v.is_private() || v.is_link_local(),
        std::net::IpAddr::V6(v) => v.is_loopback() || v.to_ipv4_mapped().is_some_and(|m| m.is_loopback() || m.is_private()) || (v.segments()[0] & 0xfe00) == 0xfc00,
    };
    if !from_proxy {
        return peer;
    }
    xff.and_then(|h| h.rsplit(',').next()).and_then(|ip| ip.trim().parse().ok()).unwrap_or(peer)
}

/// Couche axum (mode `--public-domain`) : remplace l'adresse de connexion par celle du visiteur ([`forwarded_client`]),
/// pour que les limites par IP (inscription, relai…) visent les visiteurs et non le proxy.
pub async fn client_ip_from_proxy(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    if let Some(info) = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().copied() {
        let xff = req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok());
        let ip = forwarded_client(info.0.ip(), xff);
        if ip != info.0.ip() {
            req.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(ip, info.0.port())));
        }
    }
    next.run(req).await
}

/// Qui répond en HTTP (port 80) sous le nom de domaine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Cette machine.
    Me,
    /// Un autre serveur web : un reverse proxy (NPM…) sur une autre machine reçoit le domaine. Contient son en-tête `Server`.
    Other(String),
    /// Personne, ou impossible à savoir (port 80 de cette machine indisponible, box qui ne renvoie pas vers elle-même
    /// les connexions venues du réseau local…).
    Unknown,
}

/// Demande `http://<domaine>/.well-known/acme-challenge/<jeton>` pendant que cette machine sert ce jeton sur son port 80.
/// Avant de prendre les ports 80/443 et de demander un certificat, la station sait ainsi si le domaine arrive bien chez
/// elle, ou chez un reverse proxy installé ailleurs, qu'elle ne peut pas voir. **Bloquant** (5 s au plus par étape).
pub fn who_answers(domain: &str) -> Answer {
    who_answers_at(domain, 80, 80)
}

/// [`who_answers`] : jeton servi sur le port `listen` de cette machine, demandé à `domain:target`.
pub fn who_answers_at(domain: &str, listen: u16, target: u16) -> Answer {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream, ToSocketAddrs};
    let timeout = std::time::Duration::from_secs(5);
    let mut token = [0u8; 16];
    if getrandom::fill(&mut token).is_err() {
        return Answer::Unknown;
    }
    let token = hex::encode(token);
    let Ok(listener) = TcpListener::bind(("0.0.0.0", listen)) else { return Answer::Unknown };
    if listener.set_nonblocking(true).is_err() {
        return Answer::Unknown;
    }
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let server = {
        let (stop, token) = (stop.clone(), token.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.set_nonblocking(false);
                        let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                        let mut buf = [0u8; 2048];
                        let _ = s.read(&mut buf);
                        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{token}", token.len());
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
                }
            }
        })
    };
    let answer = (|| {
        let addr = (domain, target).to_socket_addrs().ok()?.next()?;
        let mut s = TcpStream::connect_timeout(&addr, timeout).ok()?;
        s.set_read_timeout(Some(timeout)).ok()?;
        s.set_write_timeout(Some(timeout)).ok()?;
        write!(s, "GET /.well-known/acme-challenge/astroportd-{token} HTTP/1.1\r\nHost: {domain}\r\nUser-Agent: astroportd\r\nConnection: close\r\n\r\n").ok()?;
        let mut body = Vec::new();
        let _ = s.take(64 * 1024).read_to_end(&mut body);
        Some(String::from_utf8_lossy(&body).into_owned())
    })();
    stop.store(true, Ordering::SeqCst);
    let _ = server.join();
    match answer {
        Some(text) if text.contains(&token) => Answer::Me,
        Some(text) if text.starts_with("HTTP/") => {
            let server = text.lines().take_while(|l| !l.is_empty()).find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("server")).map(|(_, v)| v.trim().to_owned()));
            Answer::Other(server.filter(|v| !v.is_empty()).unwrap_or_else(|| "un autre serveur web".to_owned()))
        }
        _ => Answer::Unknown,
    }
}

/// Mode web enregistré dans `station.conf` : `true` si un reverse proxy sert le domaine.
pub fn proxy_from_conf(conf: &Path) -> bool {
    crate::init::read_conf(conf).iter().any(|(k, v)| k == WEB_KEY && v == "proxy")
}

/// Ce qu'il faut régler hors de la station, une ligne par geste, pour le mode choisi. `proxy` : un reverse proxy sert le
/// domaine ; `domain` : le nom (vide = essai local) ; `p2p_port` : port du maillage ; `visio` : port UDP de la visio.
pub fn network_steps(proxy: bool, domain: Option<&str>, p2p_port: Option<u16>, visio: Option<u16>) -> Vec<String> {
    let mut v = Vec::new();
    match (proxy, domain) {
        (true, d) => {
            let d = d.unwrap_or("<votre domaine>");
            v.push(format!("Dans le reverse proxy (Nginx Proxy Manager…) : un seul hôte {d} → http://<adresse de cette machine>:{UI_PORT}, « Websockets Support » coché, certificat Let's Encrypt du proxy. NPM dans Docker sur cette machine : adresse 172.17.0.1 ; sur une autre machine : l'adresse de celle-ci sur le réseau local."));
            v.push("Dans le proxy, onglet « Advanced » (conseillé) : proxy_read_timeout 1h; proxy_send_timeout 1h;".to_owned());
            v.push(format!("Pare-feu de cette machine : laissez le port {UI_PORT}/tcp ouvert au proxy (réseau local, Docker), jamais à Internet."));
        }
        (false, Some(_)) => v.push("Dans la box : ports 80 et 443 (TCP) redirigés vers cette machine ; la station obtient et renouvelle seule son certificat.".to_owned()),
        (false, None) => {}
    }
    if let Some(p) = visio {
        v.push(format!("Visio : dans la box, port {p} en UDP vers cette machine (STUN et relais d'appel ; un proxy web ne transmet pas l'UDP)."));
    }
    if let Some(p) = p2p_port {
        v.push(format!("Maillage : dans la box, port {p} (TCP) vers cette machine. Indispensable pour une station relais de la constellation ; conseillé pour les autres (sinon elles passent par un relais)."));
    }
    v
}

/// État d'un port de cette machine.
pub fn port_state(port: u16) -> &'static str {
    match std::net::TcpListener::bind(("0.0.0.0", port)) {
        Ok(_) => "free",
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => "busy",
        Err(_) => "privileged",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behind_a_proxy_the_visitor_address_is_the_one_the_proxy_saw() {
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert_eq!(forwarded_client(ip("127.0.0.1"), Some("203.0.113.9")), ip("203.0.113.9"), "proxy local");
        assert_eq!(forwarded_client(ip("192.168.1.2"), Some("6.6.6.6, 198.51.100.4")), ip("198.51.100.4"), "seule la dernière adresse, ajoutée par le proxy, compte");
        assert_eq!(forwarded_client(ip("198.51.100.7"), Some("10.0.0.1")), ip("198.51.100.7"), "connexion directe d'Internet : l'en-tête n'est pas cru");
        assert_eq!(forwarded_client(ip("127.0.0.1"), Some("pas-une-ip")), ip("127.0.0.1"));
        assert_eq!(forwarded_client(ip("::1"), None), ip("::1"));
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
    }

    #[test]
    fn the_station_knows_whether_its_domain_reaches_itself_or_another_web_server() {
        use std::io::{Read, Write};
        let me = free_port();
        assert_eq!(who_answers_at("127.0.0.1", me, me), Answer::Me, "le domaine arrive ici");

        // Un reverse proxy d'une autre machine (ici : un autre port) répond à sa façon.
        let proxy = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = proxy.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = c.read(&mut buf);
            let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nServer: openresty\r\nContent-Length: 0\r\n\r\n");
        });
        assert_eq!(who_answers_at("127.0.0.1", free_port(), proxy_port), Answer::Other("openresty".into()));
        t.join().unwrap();

        let closed = free_port();
        assert_eq!(who_answers_at("127.0.0.1", free_port(), closed), Answer::Unknown, "personne ne répond : on ne sait pas");
    }

    #[test]
    fn the_network_steps_follow_the_web_mode() {
        let proxy = network_steps(true, Some("s.exemple.org"), Some(4101), Some(UI_PORT)).join("\n");
        assert!(proxy.contains("s.exemple.org → http://<adresse de cette machine>:8090") && proxy.contains("172.17.0.1") && proxy.contains("8090 en UDP") && proxy.contains("4101"), "{proxy}");
        assert!(!proxy.contains("80 et 443"), "derrière un proxy, la box renvoie 80/443 au proxy, pas à la station");
        let direct = network_steps(false, Some("s.exemple.org"), Some(4101), Some(443)).join("\n");
        assert!(direct.contains("80 et 443") && direct.contains("443 en UDP") && !direct.contains("proxy_read_timeout"), "{direct}");
        assert!(network_steps(false, None, None, None).is_empty(), "essai local : rien à régler");
    }

    #[test]
    fn a_domain_is_cleaned_and_checked() {
        assert_eq!(normalize_domain(" HTTPS://Station.Exemple.org/ ").unwrap(), "station.exemple.org");
        assert_eq!(normalize_domain("station.exemple.org.").unwrap(), "station.exemple.org");
        for bad in ["localhost", "exemple", "-a.org", "a..org", "a b.org", "192.168.1.10", "évé.org", "a.org/chemin"] {
            assert!(normalize_domain(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_conf_keeps_its_other_lines_and_serve_reads_the_domain_back() {
        let dir = std::env::temp_dir().join(format!("astroportd-domain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("station.conf");
        std::fs::write(&conf, "# commentaire\nmode = origin\ndomain =\ncaptain_email = cap@exemple.org\nkind.7 = read=all\n").unwrap();
        assert_eq!(domain_from_conf(&conf), None, "domaine vide : HTTP local");
        set_conf_value(&conf, "domain", "station.exemple.org").unwrap();
        let text = std::fs::read_to_string(&conf).unwrap();
        assert!(text.starts_with("# commentaire\nmode = origin\ndomain = station.exemple.org\n") && text.contains("kind.7 = read=all"), "{text}");
        assert_eq!(domain_from_conf(&conf), Some(("station.exemple.org".into(), Some("cap@exemple.org".into()))));
        set_conf_value(&conf, "lat", "43.6").unwrap();
        assert!(std::fs::read_to_string(&conf).unwrap().ends_with("lat = 43.6\n"), "clé absente : ajoutée");
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&conf).unwrap().permissions()) & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }
}
