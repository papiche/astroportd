//! Relais d'appel (TURN) : identifiants exigés, relais seulement entre allocations de la station (jamais vers Internet ni le
//! réseau local), canaux, quotas, débit plafonné, paquets hostiles ignorés.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use astroportd::turn::{MAX_PER_USER, REALM, RELAY_IP, Relay, credentials};
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use sha1::Sha1;

const SECRET: [u8; 32] = [7; 32];
const MAGIC: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];

/// Petit client TURN de test : construit les requêtes comme un navigateur (attributs, MESSAGE-INTEGRITY, FINGERPRINT).
struct Client {
    addr: SocketAddr,
    user: String,
    pass: String,
    nonce: Vec<u8>,
    n: u8,
}

fn attr(buf: &mut Vec<u8>, t: u16, v: &[u8]) {
    buf.extend_from_slice(&t.to_be_bytes());
    buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
    buf.extend_from_slice(v);
    buf.resize(buf.len() + (4 - v.len() % 4) % 4, 0);
}

fn xor_peer(a: SocketAddr) -> Vec<u8> {
    let SocketAddr::V4(a) = a else { panic!("IPv4 attendu") };
    let mut v = vec![0, 1];
    v.extend_from_slice(&(a.port() ^ 0x2112).to_be_bytes());
    v.extend(a.ip().octets().iter().zip(MAGIC).map(|(b, k)| b ^ k));
    v
}

/// Attributs d'une réponse : `(type, valeur)`.
fn attrs(p: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut out = Vec::new();
    let mut at = 20;
    while at + 4 <= p.len() {
        let t = u16::from_be_bytes([p[at], p[at + 1]]);
        let l = u16::from_be_bytes([p[at + 2], p[at + 3]]) as usize;
        out.push((t, p[at + 4..at + 4 + l].to_vec()));
        at += 4 + l + (4 - l % 4) % 4;
    }
    out
}

fn get(p: &[u8], t: u16) -> Option<Vec<u8>> {
    attrs(p).into_iter().find(|a| a.0 == t).map(|a| a.1)
}

fn error_code(p: &[u8]) -> Option<u16> {
    get(p, 0x0009).map(|v| u16::from(v[2]) * 100 + u16::from(v[3]))
}

fn read_xor(v: &[u8]) -> SocketAddr {
    let port = u16::from_be_bytes([v[2], v[3]]) ^ 0x2112;
    let ip: Vec<u8> = v[4..8].iter().zip(MAGIC).map(|(b, k)| b ^ k).collect();
    SocketAddr::from(([ip[0], ip[1], ip[2], ip[3]], port))
}

impl Client {
    fn new(port: u16, tag: &str) -> Self {
        let (user, pass) = credentials(&SECRET, tag, unix(), 3600);
        Self { addr: SocketAddr::from(([198, 51, 100, 1], port)), user, pass, nonce: Vec::new(), n: 0 }
    }

    /// Requête `ty` avec `extra` attributs ; signée si un nonce est connu.
    fn request(&mut self, ty: u16, extra: &[(u16, Vec<u8>)]) -> Vec<u8> {
        self.n += 1;
        let mut b = ty.to_be_bytes().to_vec();
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&MAGIC);
        b.extend_from_slice(&[self.n; 12]);
        for (t, v) in extra {
            attr(&mut b, *t, v);
        }
        if !self.nonce.is_empty() {
            attr(&mut b, 0x0006, self.user.clone().as_bytes());
            attr(&mut b, 0x0014, REALM.as_bytes());
            attr(&mut b, 0x0015, &self.nonce.clone());
            let len = (b.len() - 20 + 24) as u16;
            b[2..4].copy_from_slice(&len.to_be_bytes());
            let key: [u8; 16] = Md5::digest(format!("{}:{REALM}:{}", self.user, self.pass).as_bytes()).into();
            let mut m = <Hmac<Sha1> as Mac>::new_from_slice(&key).unwrap();
            m.update(&b);
            attr(&mut b, 0x0008, &m.finalize().into_bytes());
        }
        let len = (b.len() - 20) as u16;
        b[2..4].copy_from_slice(&len.to_be_bytes());
        b
    }

    /// Envoie une requête au relais et rend la réponse (une seule attendue).
    fn ask(&mut self, r: &mut Relay, ty: u16, extra: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let req = self.request(ty, extra);
        let out = r.handle(&req, self.addr, Instant::now());
        assert_eq!(out.len(), 1, "une réponse attendue");
        assert_eq!(out[0].0, self.addr);
        out[0].1.clone()
    }

    /// Allocate complet (défi 401, puis requête signée) : rend l'adresse de relais.
    fn allocate(&mut self, r: &mut Relay) -> SocketAddr {
        let challenge = self.ask(r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])]);
        assert_eq!(error_code(&challenge), Some(401));
        self.nonce = get(&challenge, 0x0015).expect("nonce");
        let ok = self.ask(r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])]);
        assert_eq!(&ok[0..2], &[0x01, 0x03], "Allocate réussi : {:?}", error_code(&ok));
        assert!(get(&ok, 0x0008).is_some() && get(&ok, 0x8028).is_some(), "réponse signée et empreinte");
        read_xor(&get(&ok, 0x0016).expect("adresse de relais"))
    }

    fn permit(&mut self, r: &mut Relay, peer: SocketAddr) -> Vec<u8> {
        self.ask(r, 0x0008, &[(0x0012, xor_peer(peer))])
    }
}

fn unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn send_indication(peer: SocketAddr, data: &[u8]) -> Vec<u8> {
    let mut b = vec![0x00, 0x16, 0, 0];
    b.extend_from_slice(&MAGIC);
    b.extend_from_slice(&[9; 12]);
    attr(&mut b, 0x0012, &xor_peer(peer));
    attr(&mut b, 0x0013, data);
    let len = (b.len() - 20) as u16;
    b[2..4].copy_from_slice(&len.to_be_bytes());
    b
}

#[test]
fn two_browsers_of_the_station_talk_through_the_relay_by_indication_then_by_channel() {
    let mut r = Relay::new(SECRET);
    let (mut alice, mut bob) = (Client::new(40_001, "alice"), Client::new(40_002, "n1-bob"));
    let (ra, rb) = (alice.allocate(&mut r), bob.allocate(&mut r));
    assert_eq!((ra.ip(), rb.ip()), (std::net::IpAddr::V4(RELAY_IP), std::net::IpAddr::V4(RELAY_IP)), "adresses de relais virtuelles");
    assert_ne!(ra, rb);
    // Chacun ouvre sa porte à l'autre (permissions), puis Alice envoie : Bob reçoit une indication Data venant du relais d'Alice.
    assert_eq!(&alice.permit(&mut r, rb)[0..2], &[0x01, 0x08]);
    assert_eq!(&bob.permit(&mut r, ra)[0..2], &[0x01, 0x08]);
    let out = r.handle(&send_indication(rb, b"bonjour"), alice.addr, Instant::now());
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, bob.addr);
    assert_eq!((get(&out[0].1, 0x0013).unwrap(), read_xor(&get(&out[0].1, 0x0012).unwrap())), (b"bonjour".to_vec(), ra));
    // Bob lie un canal vers Alice : la suite arrive en ChannelData (4 octets d'en-tête au lieu d'une indication).
    let bind = bob.ask(&mut r, 0x0009, &[(0x000C, vec![0x40, 0x01, 0, 0]), (0x0012, xor_peer(ra))]);
    assert_eq!(&bind[0..2], &[0x01, 0x09]);
    let out = r.handle(&send_indication(rb, b"image"), alice.addr, Instant::now());
    assert_eq!(&out[0].1[..4], &[0x40, 0x01, 0x00, 0x05]);
    assert_eq!(&out[0].1[4..], b"image");
    // Et Bob répond par son canal : Alice le reçoit (en indication, elle n'a pas de canal).
    let out = r.handle(&[0x40, 0x01, 0x00, 0x03, b'o', b'u', b'i'], bob.addr, Instant::now());
    assert_eq!((out[0].0, get(&out[0].1, 0x0013).unwrap()), (alice.addr, b"oui".to_vec()));
    // Refresh à 0 : l'allocation disparaît, plus rien ne passe vers elle.
    let bye = bob.ask(&mut r, 0x0004, &[(0x000D, 0u32.to_be_bytes().to_vec())]);
    assert_eq!(&bye[0..2], &[0x01, 0x04]);
    assert!(r.handle(&send_indication(rb, b"?"), alice.addr, Instant::now()).is_empty());
    assert_eq!(r.allocations(), 1);
}

#[test]
fn the_relay_never_reaches_internet_or_the_station_network() {
    let mut r = Relay::new(SECRET);
    let mut alice = Client::new(40_010, "alice");
    alice.allocate(&mut r);
    for target in ["8.8.8.8:53", "127.0.0.1:8080", "192.168.1.1:80", "100.64.0.1:1"] {
        let peer: SocketAddr = target.parse().unwrap();
        assert_eq!(error_code(&alice.permit(&mut r, peer)), Some(403), "{target} : interdit");
        let bind = alice.ask(&mut r, 0x0009, &[(0x000C, vec![0x40, 0x02, 0, 0]), (0x0012, xor_peer(peer))]);
        assert_eq!(error_code(&bind), Some(403), "{target} : canal interdit");
        assert!(r.handle(&send_indication(peer, b"x"), alice.addr, Instant::now()).is_empty(), "{target} : rien n'est émis");
    }
}

#[test]
fn without_valid_credentials_nothing_is_allocated() {
    let mut r = Relay::new(SECRET);
    // Faux mot de passe.
    let mut eve = Client::new(40_020, "eve");
    eve.pass = "devine".into();
    let c = eve.ask(&mut r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])]);
    eve.nonce = get(&c, 0x0015).unwrap();
    assert_eq!(error_code(&eve.ask(&mut r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])])), Some(401));
    // Identifiants expirés.
    let mut old = Client::new(40_021, "vieux");
    let (u, p) = credentials(&SECRET, "vieux", unix() - 10_000, 3600);
    (old.user, old.pass) = (u, p);
    let c = old.ask(&mut r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])]);
    old.nonce = get(&c, 0x0015).unwrap();
    assert_eq!(error_code(&old.ask(&mut r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])])), Some(401));
    // Identifiants d'une autre station (autre secret).
    let mut other = Client::new(40_022, "ailleurs");
    let (u, p) = credentials(&[9; 32], "ailleurs", unix(), 3600);
    (other.user, other.pass) = (u, p);
    let c = other.ask(&mut r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])]);
    other.nonce = get(&c, 0x0015).unwrap();
    assert_eq!(error_code(&other.ask(&mut r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])])), Some(401));
    // Nonce forgé.
    let mut forged = Client::new(40_023, "f");
    forged.nonce = b"ffffffffff-0011223344556677".to_vec();
    assert_eq!(error_code(&forged.ask(&mut r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])])), Some(438));
    // TCP demandé : refusé.
    let mut tcp = Client::new(40_024, "t");
    let c = tcp.ask(&mut r, 0x0003, &[(0x0019, vec![6, 0, 0, 0])]);
    tcp.nonce = get(&c, 0x0015).unwrap();
    assert_eq!(error_code(&tcp.ask(&mut r, 0x0003, &[(0x0019, vec![6, 0, 0, 0])])), Some(442));
    assert_eq!(r.allocations(), 0);
}

#[test]
fn quotas_rate_and_hostile_packets_are_bounded() {
    let mut r = Relay::new(SECRET);
    // Une même personne (même étiquette) : pas plus de MAX_PER_USER allocations, même avec des identifiants redemandés.
    for i in 0..MAX_PER_USER as u16 {
        Client::new(41_000 + i, "gourmand").allocate(&mut r);
    }
    let mut extra = Client::new(41_100, "gourmand");
    let c = extra.ask(&mut r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])]);
    extra.nonce = get(&c, 0x0015).unwrap();
    assert_eq!(error_code(&extra.ask(&mut r, 0x0003, &[(0x0019, vec![17, 0, 0, 0])])), Some(486));

    // Débit plafonné : au-delà d'un mégaoctet par seconde, le relais se tait.
    let (mut a, mut b) = (Client::new(42_001, "a"), Client::new(42_002, "b"));
    let (ra, rb) = (a.allocate(&mut r), b.allocate(&mut r));
    a.permit(&mut r, rb);
    b.permit(&mut r, ra);
    a.ask(&mut r, 0x0009, &[(0x000C, vec![0x40, 0x10, 0, 0]), (0x0012, xor_peer(rb))]);
    let mut frame = vec![0x40, 0x10, 0x04, 0xB0];
    frame.resize(4 + 1200, 0xAB);
    let now = Instant::now();
    let passed = (0..1000).filter(|_| !r.handle(&frame, a.addr, now).is_empty()).count();
    assert!((800..=840).contains(&passed), "≈ 1 Mo par seconde : {passed} trames de 1200 octets");
    assert!(!r.handle(&frame, a.addr, now + Duration::from_millis(1100)).is_empty(), "la seconde suivante, ça repart");

    // Paquets hostiles : aucun ne fait répondre ni relayer, aucun ne fait paniquer.
    let stranger: SocketAddr = "203.0.113.66:5000".parse().unwrap();
    for p in [vec![], vec![0x40, 0x10, 0xFF, 0xFF, 1, 2], vec![0x40; 3], vec![0x7F, 0xFF, 0, 0], frame.clone(), vec![0x00, 0x16, 0, 4, 0x21, 0x12, 0xA4, 0x42], vec![0xFF; 2000]] {
        assert!(r.handle(&p, stranger, Instant::now()).is_empty(), "{:?}", &p[..p.len().min(8)]);
    }
}

#[test]
fn authenticated_traffic_is_never_throttled_but_challenges_are() {
    let mut r = Relay::new(SECRET);
    // Plusieurs navigateurs derrière une même adresse (NAT d'opérateur) : leurs échanges signés passent tous.
    let mut a = Client::new(43_001, "nat-a");
    a.allocate(&mut r);
    for _ in 0..60 {
        let ok = a.ask(&mut r, 0x0004, &[(0x000D, 600u32.to_be_bytes().to_vec())]);
        assert_eq!(&ok[0..2], &[0x01, 0x04], "rafraîchissement signé toujours servi");
    }
    // Les défis (non signés) restent limités par adresse : un inconnu ne fait pas répondre la station à volonté.
    let mut stranger = Client::new(43_002, "inconnu");
    let answered = (0..60).filter(|_| !r.handle(&stranger.request(0x0003, &[(0x0019, vec![17, 0, 0, 0])]), stranger.addr, Instant::now()).is_empty()).count();
    assert!(answered <= astroportd::stun::PER_IP_PER_SEC as usize, "{answered} défis servis");
}

/// Requête STUN « Binding » minimale.
fn binding(tx: u8) -> Vec<u8> {
    let mut p = vec![0x00, 0x01, 0x00, 0x00];
    p.extend_from_slice(&MAGIC);
    p.extend_from_slice(&[tx; 12]);
    p
}

#[tokio::test]
async fn a_new_address_of_the_machine_is_listened_to_without_restarting() {
    use std::net::{IpAddr, Ipv4Addr};
    let socks = astroportd::turn::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let port = socks[0].local_addr().unwrap().port();
    let (stop_tx, stop) = tokio::sync::watch::channel(false);
    // La machine « gagne » 127.0.0.2 (nouveau bail DHCP, Wi-Fi, VPN…).
    let ips = std::sync::Arc::new(std::sync::Mutex::new(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]));
    let seen = ips.clone();
    let rebind = astroportd::turn::Rebind { every: Duration::from_millis(50), ips: Box::new(move || seen.lock().unwrap().clone()) };
    let task = tokio::spawn(astroportd::turn::serve_rebinding(socks, Relay::new(SECRET).stun_only(), stop, Some(rebind)));
    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let ask = |to: SocketAddr, tx: u8| {
        let c = &client;
        async move {
            c.send_to(&binding(tx), to).await.unwrap();
            let mut buf = [0u8; 1500];
            tokio::time::timeout(Duration::from_millis(400), c.recv_from(&mut buf)).await.ok().map(|r| buf[..r.unwrap().0].to_vec())
        }
    };
    let (first, second): (SocketAddr, SocketAddr) = (format!("127.0.0.1:{port}").parse().unwrap(), format!("127.0.0.2:{port}").parse().unwrap());
    assert!(ask(first, 1).await.is_some_and(|r| r[..2] == [0x01, 0x01]), "l'adresse du démarrage répond");
    assert!(ask(second, 2).await.is_none(), "127.0.0.2 n'est pas encore une adresse de la machine");
    ips.lock().unwrap().push(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let reply = ask(second, 3).await.expect("la nouvelle adresse est écoutée, sur le même port");
    assert_eq!(&reply[..2], &[0x01, 0x01], "réponse Binding");
    stop_tx.send(true).unwrap();
    task.await.unwrap();
}
