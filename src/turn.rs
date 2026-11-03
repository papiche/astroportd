//! Relais d'appel (TURN, RFC 8656, sous-ensemble UDP) : quand deux navigateurs ne peuvent pas se joindre en direct (réseaux
//! mobiles, réseaux d'entreprise), le son et l'image passent par la station.
//!
//! **La station est le point de rendez-vous, pas un relais ouvert.** L'appelant joint à son offre les accès TURN de *sa* station ;
//! l'appelé s'y connecte aussi. Le trafic ne circule **qu'entre deux allocations de cette station** : chaque allocation reçoit une
//! adresse de relais *virtuelle* ([`RELAY_IP`] et un port propre), et toute permission vers une autre adresse est refusée (403).
//! Personne ne peut donc s'en servir pour joindre Internet ou le réseau de la station, et aucun port de plus n'est ouvert :
//! tout passe par le port UDP de STUN (le même numéro que l'interface web).
//!
//! - **Réservé aux membres de la station et à leurs amis N¹** : un membre reçoit ses identifiants par l'API authentifiée ; un ami
//!   réciproque d'un membre ne les reçoit que dans l'offre d'appel chiffrée de bout en bout, fabriqués pour lui seul par la station
//!   (voir `chat::Chat::signal`). Format « REST » de coturn : `expiration:étiquette` / `base64(HMAC-SHA1(secret, nom))` ; mot de
//!   passe à long terme (MESSAGE-INTEGRITY), nonce sans état, daté et signé. Le quota se compte par étiquette (par personne).
//! - Bornes : [`MAX_PER_USER`] allocations par identifiant, [`MAX_ALLOCATIONS`] en tout, durée de vie bornée, débit plafonné
//!   par allocation ([`BYTES_PER_SEC`]). Les réponses de contrôle restent limitées par adresse (voir [`crate::stun`]).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use sha1::Sha1;

use crate::stun;

/// Adresse virtuelle des relais : elle ne sert qu'à nommer les allocations entre elles (jamais routée).
pub const RELAY_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 1);
/// Domaine d'authentification annoncé aux navigateurs.
pub const REALM: &str = "astroportd";
/// Allocations par identifiant (un appel en ouvre une ou deux par navigateur).
pub const MAX_PER_USER: usize = 4;
/// Allocations en tout.
pub const MAX_ALLOCATIONS: usize = 256;
/// Débit maximal relayé par allocation (octets par seconde, ≈ 8 Mbit/s : une visio confortable).
pub const BYTES_PER_SEC: u64 = 1_000_000;
/// Durée de vie des identifiants remis par l'API.
pub const CREDENTIAL_TTL: u64 = 24 * 3600;
const MAGIC: u32 = 0x2112_A442;
const HEADER: usize = 20;
const MAX_PACKET: usize = 1500;
const DEFAULT_LIFETIME: u32 = 600;
const MAX_LIFETIME: u32 = 3600;
const PERMISSION_SECS: u64 = 300;
const CHANNEL_SECS: u64 = 600;
const NONCE_SECS: u64 = 3600;

// Méthodes et classes (RFC 5389 §6, RFC 8656 §18).
const ALLOCATE: u16 = 0x003;
const REFRESH: u16 = 0x004;
const SEND: u16 = 0x006;
const DATA: u16 = 0x007;
const CREATE_PERMISSION: u16 = 0x008;
const CHANNEL_BIND: u16 = 0x009;
const REQUEST: u16 = 0x0000;
const INDICATION: u16 = 0x0010;
const SUCCESS: u16 = 0x0100;
const ERROR: u16 = 0x0110;
// Attributs.
const A_USERNAME: u16 = 0x0006;
const A_INTEGRITY: u16 = 0x0008;
const A_ERROR_CODE: u16 = 0x0009;
const A_CHANNEL: u16 = 0x000C;
const A_LIFETIME: u16 = 0x000D;
const A_PEER: u16 = 0x0012;
const A_DATA: u16 = 0x0013;
const A_REALM: u16 = 0x0014;
const A_NONCE: u16 = 0x0015;
const A_RELAYED: u16 = 0x0016;
const A_TRANSPORT: u16 = 0x0019;
const A_DONT_FRAGMENT: u16 = 0x001A;
const A_MAPPED: u16 = 0x0020;
const A_FINGERPRINT: u16 = 0x8028;
/// Attributs « à comprendre obligatoirement » que ce relais connaît (les autres valent une erreur 420).
const UNDERSTOOD: [u16; 12] = [A_USERNAME, A_INTEGRITY, A_ERROR_CODE, A_CHANNEL, A_LIFETIME, A_PEER, A_DATA, A_REALM, A_NONCE, A_RELAYED, A_TRANSPORT, A_DONT_FRAGMENT];

type HmacSha1 = Hmac<Sha1>;

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
    // Une clé HMAC accepte toutes les longueurs : `new_from_slice` ne peut pas échouer ici.
    let Ok(mut m) = <HmacSha1 as Mac>::new_from_slice(key) else { return [0; 20] };
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

fn unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Durée de vie des identifiants glissés dans une offre d'appel pour un ami N¹.
pub const PEER_CREDENTIAL_TTL: u64 = 2 * 3600;

/// Identifiants éphémères pour `tag` (une personne), valables `ttl` secondes à partir de `now` : `(nom, mot de passe)`.
pub fn credentials(secret: &[u8; 32], tag: &str, now: u64, ttl: u64) -> (String, String) {
    let username = format!("{}:{tag}", now + ttl);
    let password = base64::engine::general_purpose::STANDARD.encode(hmac(secret, &[username.as_bytes()]));
    (username, password)
}

/// Message STUN lu : méthode, classe, transaction, attributs `(type, valeur, décalage de l'attribut)`.
struct Msg<'a> {
    method: u16,
    class: u16,
    txid: [u8; 12],
    attrs: Vec<(u16, &'a [u8], usize)>,
}

impl Msg<'_> {
    fn attr(&self, t: u16) -> Option<&[u8]> {
        self.attrs.iter().find(|a| a.0 == t).map(|a| a.1)
    }
}

fn parse(p: &[u8]) -> Option<Msg<'_>> {
    if p.len() < HEADER || p[0] & 0xC0 != 0 {
        return None;
    }
    let ty = u16::from_be_bytes([p[0], p[1]]);
    let len = usize::from(u16::from_be_bytes([p[2], p[3]]));
    if u32::from_be_bytes([p[4], p[5], p[6], p[7]]) != MAGIC || len % 4 != 0 || HEADER + len != p.len() {
        return None;
    }
    let mut txid = [0u8; 12];
    txid.copy_from_slice(&p[8..HEADER]);
    let (mut attrs, mut at) = (Vec::new(), HEADER);
    while at < p.len() {
        if at + 4 > p.len() {
            return None;
        }
        let t = u16::from_be_bytes([p[at], p[at + 1]]);
        let l = usize::from(u16::from_be_bytes([p[at + 2], p[at + 3]]));
        let end = at + 4 + l;
        if end > p.len() {
            return None;
        }
        attrs.push((t, &p[at + 4..end], at));
        at = end + (4 - l % 4) % 4;
    }
    Some(Msg { method: (ty & 0x000F) | ((ty & 0x00E0) >> 1) | ((ty & 0x3E00) >> 2), class: ty & 0x0110, txid, attrs })
}

fn msg_type(method: u16, class: u16) -> u16 {
    (method & 0x000F) | ((method & 0x0070) << 1) | ((method & 0x0F80) << 2) | class
}

/// Adresse XOR (XOR-MAPPED, XOR-PEER, XOR-RELAYED) codée.
fn xor_addr(a: SocketAddr, txid: &[u8; 12]) -> Vec<u8> {
    let port = a.port() ^ (MAGIC >> 16) as u16;
    let mut out = vec![0, if a.is_ipv4() { 1 } else { 2 }];
    out.extend_from_slice(&port.to_be_bytes());
    match a.ip() {
        IpAddr::V4(ip) => out.extend_from_slice(&(u32::from(ip) ^ MAGIC).to_be_bytes()),
        IpAddr::V6(ip) => {
            let key: Vec<u8> = MAGIC.to_be_bytes().iter().chain(txid).copied().collect();
            out.extend(ip.octets().iter().zip(key).map(|(b, k)| b ^ k));
        }
    }
    out
}

fn read_xor_addr(v: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if v.len() < 8 {
        return None;
    }
    let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC >> 16) as u16;
    match v[1] {
        1 => Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(u32::from_be_bytes([v[4], v[5], v[6], v[7]]) ^ MAGIC)), port)),
        2 if v.len() >= 20 => {
            let key: Vec<u8> = MAGIC.to_be_bytes().iter().chain(txid).copied().collect();
            let o: Vec<u8> = v[4..20].iter().zip(key).map(|(b, k)| b ^ k).collect();
            Some(SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::from(<[u8; 16]>::try_from(o.as_slice()).ok()?)), port))
        }
        _ => None,
    }
}

/// Construction d'une réponse : attributs, puis MESSAGE-INTEGRITY (si `key`) et FINGERPRINT.
struct Out {
    buf: Vec<u8>,
}

impl Out {
    fn new(method: u16, class: u16, txid: &[u8; 12]) -> Self {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&msg_type(method, class).to_be_bytes());
        buf.extend_from_slice(&[0, 0]);
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.extend_from_slice(txid);
        Self { buf }
    }
    fn set_len(&mut self, extra: usize) {
        let len = u16::try_from(self.buf.len() - HEADER + extra).unwrap_or(u16::MAX);
        self.buf[2..4].copy_from_slice(&len.to_be_bytes());
    }
    fn attr(mut self, t: u16, v: &[u8]) -> Self {
        self.buf.extend_from_slice(&t.to_be_bytes());
        self.buf.extend_from_slice(&u16::try_from(v.len()).unwrap_or(0).to_be_bytes());
        self.buf.extend_from_slice(v);
        self.buf.resize(self.buf.len() + (4 - v.len() % 4) % 4, 0);
        self
    }
    fn error(self, code: u16, reason: &str) -> Self {
        let mut v = vec![0, 0, u8::try_from(code / 100).unwrap_or(4), u8::try_from(code % 100).unwrap_or(0)];
        v.extend_from_slice(reason.as_bytes());
        self.attr(A_ERROR_CODE, &v)
    }
    fn finish(mut self, key: Option<&[u8; 16]>) -> Vec<u8> {
        if let Some(k) = key {
            self.set_len(24);
            let mac = hmac(k, &[&self.buf]);
            self = self.attr(A_INTEGRITY, &mac);
        }
        self.set_len(8);
        let crc = crc32fast::hash(&self.buf) ^ 0x5354_554E;
        self.attr(A_FINGERPRINT, &crc.to_be_bytes()).buf
    }
}

/// Une allocation : un navigateur et son adresse de relais virtuelle.
struct Allocation {
    username: String,
    relay: SocketAddr,
    expires: Instant,
    alloc_txid: [u8; 12],
    permissions: HashMap<IpAddr, Instant>,
    channels: HashMap<u16, (SocketAddr, Instant)>,
    budget: (Instant, u64),
}

impl Allocation {
    fn permitted(&self, ip: IpAddr, now: Instant) -> bool {
        self.permissions.get(&ip).is_some_and(|t| *t > now)
    }
    fn channel_of(&self, peer: SocketAddr, now: Instant) -> Option<u16> {
        self.channels.iter().find(|(_, (p, t))| *p == peer && *t > now).map(|(c, _)| *c)
    }
    /// Débit : `true` si `n` octets passent encore cette seconde.
    fn spend(&mut self, n: usize, now: Instant) -> bool {
        if now.duration_since(self.budget.0) >= Duration::from_secs(1) {
            self.budget = (now, 0);
        }
        self.budget.1 += n as u64;
        self.budget.1 <= BYTES_PER_SEC
    }
}

/// État du relais : allocations par adresse de navigateur, et par port de relais virtuel.
pub struct Relay {
    secret: [u8; 32],
    /// `false` : STUN seulement (`--no-turn`), toute demande de relais est ignorée.
    relaying: bool,
    by_client: HashMap<SocketAddr, Allocation>,
    by_relay: HashMap<u16, SocketAddr>,
    next_port: u16,
    limiter: stun::Limiter,
    swept: Instant,
}

/// Paquets à émettre : `(destinataire, octets)`.
pub type Sends = Vec<(SocketAddr, Vec<u8>)>;

impl Relay {
    /// Relais vide, identifiants signés par `secret` (celui qui fabrique les identifiants de l'API).
    pub fn new(secret: [u8; 32]) -> Self {
        Self { secret, relaying: true, by_client: HashMap::new(), by_relay: HashMap::new(), next_port: 49_152, limiter: stun::Limiter::default(), swept: Instant::now() }
    }

    /// STUN seulement : aucune allocation, aucun relais.
    pub fn stun_only(mut self) -> Self {
        self.relaying = false;
        self
    }

    /// Nombre d'allocations vivantes.
    pub fn allocations(&self) -> usize {
        self.by_client.len()
    }

    fn nonce(&self, ip: IpAddr, now: u64) -> String {
        let until = now + NONCE_SECS;
        let mac = hmac(&self.secret, &[b"nonce", ip.to_string().as_bytes(), &until.to_be_bytes()]);
        format!("{until:x}-{}", hex::encode(&mac[..8]))
    }

    fn nonce_ok(&self, nonce: &str, ip: IpAddr, now: u64) -> bool {
        let Some((until, _)) = nonce.split_once('-') else { return false };
        let Ok(until) = u64::from_str_radix(until, 16) else { return false };
        until > now && until <= now + NONCE_SECS && self.nonce(ip, until - NONCE_SECS) == nonce
    }

    /// Clé de l'identifiant d'une requête authentifiée, si tout est valide : nom connu, non expiré, intégrité vérifiée.
    /// Sinon la réponse d'erreur à renvoyer (401 ou 438).
    fn authenticate(&self, raw: &[u8], m: &Msg<'_>, from: SocketAddr, now: u64) -> Result<(String, [u8; 16]), Vec<u8>> {
        let challenge = |code: u16, reason: &str| Out::new(m.method, ERROR, &m.txid).error(code, reason).attr(A_REALM, REALM.as_bytes()).attr(A_NONCE, self.nonce(from.ip(), now).as_bytes()).finish(None);
        let (Some(user), Some(_), Some(nonce), Some(mi)) = (m.attr(A_USERNAME), m.attr(A_REALM), m.attr(A_NONCE), m.attrs.iter().find(|a| a.0 == A_INTEGRITY)) else {
            return Err(challenge(401, "Unauthorized"));
        };
        let (Ok(user), Ok(nonce)) = (std::str::from_utf8(user), std::str::from_utf8(nonce)) else { return Err(challenge(401, "Unauthorized")) };
        if !self.nonce_ok(nonce, from.ip(), now) {
            return Err(challenge(438, "Stale Nonce"));
        }
        let expired = user.split_once(':').and_then(|(t, _)| t.parse::<u64>().ok()).is_none_or(|t| t <= now);
        if expired || user.len() > 128 || mi.1.len() != 20 {
            return Err(challenge(401, "Unauthorized"));
        }
        let password = base64::engine::general_purpose::STANDARD.encode(hmac(&self.secret, &[user.as_bytes()]));
        let key: [u8; 16] = Md5::digest(format!("{user}:{REALM}:{password}").as_bytes()).into();
        // HMAC sur le message jusqu'à MESSAGE-INTEGRITY, la longueur de l'en-tête comptant cet attribut.
        let mut head = raw[..mi.2].to_vec();
        head[2..4].copy_from_slice(&u16::try_from(mi.2 + 24 - HEADER).unwrap_or(0).to_be_bytes());
        let mac = hmac(&key, &[&head]);
        if !constant_eq(&mac, mi.1) {
            return Err(challenge(401, "Unauthorized"));
        }
        Ok((user.to_owned(), key))
    }

    fn sweep(&mut self, now: Instant) {
        if now.duration_since(self.swept) < Duration::from_secs(5) {
            return;
        }
        self.swept = now;
        let dead: Vec<SocketAddr> = self.by_client.iter().filter(|(_, a)| a.expires <= now).map(|(c, _)| *c).collect();
        for c in dead {
            self.remove(c);
        }
    }

    fn remove(&mut self, client: SocketAddr) {
        if let Some(a) = self.by_client.remove(&client) {
            self.by_relay.remove(&a.relay.port());
        }
    }

    /// Un pair n'est admis que s'il est l'adresse de relais d'une autre allocation **de cette station**.
    fn internal(&self, peer: SocketAddr) -> bool {
        peer.ip() == IpAddr::V4(RELAY_IP) && self.by_relay.contains_key(&peer.port())
    }

    /// Traite un paquet reçu de `from` ; rend les paquets à émettre.
    pub fn handle(&mut self, p: &[u8], from: SocketAddr, now: Instant) -> Sends {
        self.sweep(now);
        if p.len() > MAX_PACKET {
            return Vec::new();
        }
        // ChannelData : deux premiers bits à 01.
        if p.len() >= 4 && p[0] & 0xC0 == 0x40 {
            return self.channel_data(p, from, now);
        }
        if let Some(reply) = stun::respond(p, from) {
            return if self.limiter.allow(from.ip(), now) { vec![(from, reply)] } else { Vec::new() };
        }
        if !self.relaying {
            return Vec::new();
        }
        let Some(m) = parse(p) else { return Vec::new() };
        if m.class == INDICATION {
            return if m.method == SEND { self.send_indication(&m, from, now) } else { Vec::new() };
        }
        if m.class != REQUEST {
            return Vec::new();
        }
        let unknown: Vec<u8> = m.attrs.iter().filter(|a| a.0 < 0x8000 && !UNDERSTOOD.contains(&a.0)).flat_map(|a| a.0.to_be_bytes()).collect();
        let reply = if !unknown.is_empty() {
            Out::new(m.method, ERROR, &m.txid).error(420, "Unknown Attribute").attr(0x000A, &unknown).finish(None)
        } else {
            match m.method {
                ALLOCATE => self.allocate(p, &m, from, now),
                REFRESH | CREATE_PERMISSION | CHANNEL_BIND => self.on_allocation(p, &m, from, now),
                _ => Out::new(m.method, ERROR, &m.txid).error(400, "Bad Request").finish(None),
            }
        };
        // Seules les réponses non signées (défis, erreurs) sont limitées par adresse : c'est tout ce qu'un inconnu peut provoquer.
        // Les échanges authentifiés d'un appel en cours passent toujours, même à plusieurs derrière une même adresse (NAT).
        if !signed(&reply) && !self.limiter.allow(from.ip(), now) {
            return Vec::new();
        }
        vec![(from, reply)]
    }

    fn allocate(&mut self, raw: &[u8], m: &Msg<'_>, from: SocketAddr, now: Instant) -> Vec<u8> {
        let (user, key) = match self.authenticate(raw, m, from, unix()) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let fail = |code: u16, reason: &str| Out::new(ALLOCATE, ERROR, &m.txid).error(code, reason).finish(Some(&key));
        if let Some(a) = self.by_client.get(&from) {
            // Retransmission de la même demande : même réponse ; sinon l'adresse a déjà son allocation.
            return if a.alloc_txid == m.txid && a.username == user { self.allocated(m, from, &key, a.relay, a.expires, now) } else { fail(437, "Allocation Mismatch") };
        }
        if m.attr(A_TRANSPORT).is_none_or(|v| v.first() != Some(&17)) {
            return fail(442, "Unsupported Transport Protocol");
        }
        let tag = |u: &str| u.split_once(':').map_or(String::new(), |(_, t)| t.to_owned());
        if self.by_client.len() >= MAX_ALLOCATIONS || self.by_client.values().filter(|a| tag(&a.username) == tag(&user)).count() >= MAX_PER_USER {
            return fail(486, "Allocation Quota Reached");
        }
        let Some(port) = self.free_port() else { return fail(508, "Insufficient Capacity") };
        let relay = SocketAddr::new(IpAddr::V4(RELAY_IP), port);
        let expires = now + Duration::from_secs(u64::from(lifetime(m, DEFAULT_LIFETIME)));
        self.by_relay.insert(port, from);
        self.by_client.insert(from, Allocation { username: user, relay, expires, alloc_txid: m.txid, permissions: HashMap::new(), channels: HashMap::new(), budget: (now, 0) });
        self.allocated(m, from, &key, relay, expires, now)
    }

    fn allocated(&self, m: &Msg<'_>, from: SocketAddr, key: &[u8; 16], relay: SocketAddr, expires: Instant, now: Instant) -> Vec<u8> {
        let left = u32::try_from(expires.saturating_duration_since(now).as_secs()).unwrap_or(0);
        Out::new(ALLOCATE, SUCCESS, &m.txid).attr(A_RELAYED, &xor_addr(relay, &m.txid)).attr(A_MAPPED, &xor_addr(from, &m.txid)).attr(A_LIFETIME, &left.to_be_bytes()).finish(Some(key))
    }

    fn free_port(&mut self) -> Option<u16> {
        for _ in 0..16_384u32 {
            let p = self.next_port;
            self.next_port = if p == u16::MAX { 49_152 } else { p + 1 };
            if !self.by_relay.contains_key(&p) {
                return Some(p);
            }
        }
        None
    }

    /// Refresh, CreatePermission, ChannelBind : sur l'allocation de `from`, avec le même identifiant.
    fn on_allocation(&mut self, raw: &[u8], m: &Msg<'_>, from: SocketAddr, now: Instant) -> Vec<u8> {
        let (user, key) = match self.authenticate(raw, m, from, unix()) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let fail = |code: u16, reason: &str| Out::new(m.method, ERROR, &m.txid).error(code, reason).finish(Some(&key));
        if self.by_client.get(&from).is_none_or(|a| a.username != user) {
            return fail(437, "Allocation Mismatch");
        }
        match m.method {
            REFRESH => {
                let life = lifetime(m, DEFAULT_LIFETIME);
                if life == 0 {
                    self.remove(from);
                } else if let Some(a) = self.by_client.get_mut(&from) {
                    a.expires = now + Duration::from_secs(u64::from(life));
                }
                Out::new(REFRESH, SUCCESS, &m.txid).attr(A_LIFETIME, &life.to_be_bytes()).finish(Some(&key))
            }
            CREATE_PERMISSION => {
                let peers: Vec<SocketAddr> = m.attrs.iter().filter(|a| a.0 == A_PEER).filter_map(|a| read_xor_addr(a.1, &m.txid)).collect();
                if peers.is_empty() {
                    return fail(400, "Bad Request");
                }
                // Le relais ne sert qu'entre les allocations de la station : jamais vers Internet ni le réseau local.
                if !peers.iter().all(|p| self.internal(*p)) {
                    return fail(403, "Forbidden");
                }
                if let Some(a) = self.by_client.get_mut(&from) {
                    for p in peers {
                        a.permissions.insert(p.ip(), now + Duration::from_secs(PERMISSION_SECS));
                    }
                }
                Out::new(CREATE_PERMISSION, SUCCESS, &m.txid).finish(Some(&key))
            }
            _ => {
                let (Some(ch), Some(peer)) = (m.attr(A_CHANNEL).filter(|v| v.len() == 4).map(|v| u16::from_be_bytes([v[0], v[1]])), m.attr(A_PEER).and_then(|v| read_xor_addr(v, &m.txid))) else {
                    return fail(400, "Bad Request");
                };
                if !(0x4000..=0x4FFF).contains(&ch) {
                    return fail(400, "Bad Request");
                }
                if !self.internal(peer) {
                    return fail(403, "Forbidden");
                }
                let Some(a) = self.by_client.get_mut(&from) else { return fail(437, "Allocation Mismatch") };
                // Un canal ne change pas de pair, et un pair n'a qu'un canal.
                if a.channels.get(&ch).is_some_and(|(p, _)| *p != peer) || a.channels.iter().any(|(c, (p, _))| *p == peer && *c != ch) {
                    return fail(400, "Bad Request");
                }
                a.channels.insert(ch, (peer, now + Duration::from_secs(CHANNEL_SECS)));
                a.permissions.insert(peer.ip(), now + Duration::from_secs(PERMISSION_SECS));
                Out::new(CHANNEL_BIND, SUCCESS, &m.txid).finish(Some(&key))
            }
        }
    }

    fn send_indication(&mut self, m: &Msg<'_>, from: SocketAddr, now: Instant) -> Sends {
        let (Some(peer), Some(data)) = (m.attr(A_PEER).and_then(|v| read_xor_addr(v, &m.txid)), m.attr(A_DATA)) else { return Vec::new() };
        self.forward(from, peer, data, now)
    }

    fn channel_data(&mut self, p: &[u8], from: SocketAddr, now: Instant) -> Sends {
        let ch = u16::from_be_bytes([p[0], p[1]]);
        let len = usize::from(u16::from_be_bytes([p[2], p[3]]));
        if 4 + len > p.len() {
            return Vec::new();
        }
        let Some(peer) = self.by_client.get(&from).and_then(|a| a.channels.get(&ch).filter(|(_, t)| *t > now).map(|(peer, _)| *peer)) else { return Vec::new() };
        self.forward(from, peer, &p[4..4 + len], now)
    }

    /// Donnée de l'allocation de `from` vers le relais `peer` : remise au navigateur qui le possède, en canal s'il en a un.
    fn forward(&mut self, from: SocketAddr, peer: SocketAddr, data: &[u8], now: Instant) -> Sends {
        let Some(src) = self.by_client.get_mut(&from) else { return Vec::new() };
        if !src.permitted(peer.ip(), now) || !src.spend(data.len(), now) {
            return Vec::new();
        }
        let src_relay = src.relay;
        if peer.ip() != IpAddr::V4(RELAY_IP) {
            return Vec::new();
        }
        let Some(&dst_client) = self.by_relay.get(&peer.port()) else { return Vec::new() };
        let Some(dst) = self.by_client.get(&dst_client) else { return Vec::new() };
        // Le destinataire doit avoir ouvert sa porte à cet expéditeur (règle des permissions TURN).
        if !dst.permitted(src_relay.ip(), now) {
            return Vec::new();
        }
        let out = match dst.channel_of(src_relay, now) {
            Some(ch) => {
                let mut b = Vec::with_capacity(4 + data.len());
                b.extend_from_slice(&ch.to_be_bytes());
                b.extend_from_slice(&u16::try_from(data.len()).unwrap_or(0).to_be_bytes());
                b.extend_from_slice(data);
                b
            }
            None => {
                let mut txid = [0u8; 12];
                if getrandom::fill(&mut txid).is_err() {
                    return Vec::new();
                }
                Out::new(DATA, INDICATION, &txid).attr(A_PEER, &xor_addr(src_relay, &txid)).attr(A_DATA, data).finish(None)
            }
        };
        vec![(dst_client, out)]
    }
}

fn lifetime(m: &Msg<'_>, default: u32) -> u32 {
    m.attr(A_LIFETIME).filter(|v| v.len() == 4).map_or(default, |v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]).min(MAX_LIFETIME))
}

/// La réponse porte-t-elle MESSAGE-INTEGRITY (requête authentifiée) ?
fn signed(reply: &[u8]) -> bool {
    parse(reply).is_some_and(|m| m.attr(A_INTEGRITY).is_some())
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Prises UDP du relais pour `addr` : une par adresse locale quand `addr` est « toutes les adresses » (0.0.0.0 ou ::), toutes
/// sur le même port. Une prise liée à toutes les adresses répondrait depuis l'adresse que choisit le noyau, pas toujours celle que
/// le navigateur a jointe : il ignorerait alors la réponse (machine à plusieurs adresses : box, VPN, conteneurs).
///
/// # Errors
/// Aucune prise n'a pu être ouverte.
pub async fn bind(addr: SocketAddr) -> std::io::Result<Vec<tokio::net::UdpSocket>> {
    if !addr.ip().is_unspecified() {
        return Ok(vec![tokio::net::UdpSocket::bind(addr).await?]);
    }
    let ips: Vec<IpAddr> = match local_ips(addr.is_ipv6()) {
        Ok(ips) => ips,
        Err(e) => {
            eprintln!("relais d'appel : adresses locales illisibles ({e}) : une seule prise sur {addr}");
            return Ok(vec![tokio::net::UdpSocket::bind(addr).await?]);
        }
    };
    let mut port = addr.port();
    let mut out = Vec::new();
    for ip in ips {
        match tokio::net::UdpSocket::bind(SocketAddr::new(ip, port)).await {
            Ok(s) => {
                port = s.local_addr()?.port(); // port 0 : la première prise fixe le port des suivantes
                out.push(s);
            }
            Err(e) => eprintln!("relais d'appel : udp/{ip}:{port} impossible : {e}"),
        }
    }
    if out.is_empty() {
        return Ok(vec![tokio::net::UdpSocket::bind(addr).await?]);
    }
    Ok(out)
}

/// Adresses locales utilisables pour un appel (IPv4, et IPv6 hors lien local si `v6`).
fn local_ips(v6: bool) -> std::io::Result<Vec<IpAddr>> {
    Ok(if_addrs::get_if_addrs()?
        .into_iter()
        .map(|i| i.ip())
        .filter(|ip| match ip {
            IpAddr::V4(_) => true,
            // Adresses de lien local (fe80::/10) : il leur faudrait un numéro d'interface ; inutiles pour un appel.
            IpAddr::V6(v) => v6 && (v.segments()[0] & 0xFFC0) != 0xFE80,
        })
        .collect())
}

/// Prises au plus (une par adresse locale vue depuis le démarrage).
const MAX_SOCKETS: usize = 64;

/// Suivi des adresses de la machine : une box qui redonne une autre adresse (DHCP), un passage de l'Ethernet au Wi-Fi, un VPN
/// qui démarre… Sans lui, le relais n'écouterait que les adresses du démarrage, et les appels échoueraient sans bruit.
pub struct Rebind {
    /// Intervalle entre deux lectures des adresses.
    pub every: Duration,
    /// Adresses actuelles de la machine.
    pub ips: Box<dyn Fn() -> Vec<IpAddr> + Send + Sync>,
}

impl Rebind {
    /// Suivi des vraies adresses toutes les 10 min, seulement pour une écoute « toutes les adresses » (0.0.0.0 ou ::).
    pub fn local(addr: SocketAddr) -> Option<Self> {
        let v6 = addr.is_ipv6();
        addr.ip().is_unspecified().then(|| Self { every: Duration::from_secs(600), ips: Box::new(move || local_ips(v6).unwrap_or_default()) })
    }
}

fn spawn_reader(i: usize, s: std::sync::Arc<tokio::net::UdpSocket>, tx: tokio::sync::mpsc::Sender<(usize, SocketAddr, Vec<u8>)>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_PACKET + 1];
        loop {
            match s.recv_from(&mut buf).await {
                Ok((n, from)) => {
                    if tx.send((i, from, buf[..n].to_vec())).await.is_err() {
                        return;
                    }
                }
                Err(e) => eprintln!("relais d'appel : réception impossible : {e}"),
            }
        }
    })
}

/// Sert STUN et TURN sur `sockets` jusqu'à l'arrêt : un lecteur par prise, un seul fil possède l'état du relais. Chaque navigateur
/// reçoit ses réponses et son média par la prise qu'il a jointe. Une erreur d'émission est dite, jamais fatale.
pub async fn serve(sockets: Vec<tokio::net::UdpSocket>, relay: Relay, stop: tokio::sync::watch::Receiver<bool>) {
    serve_rebinding(sockets, relay, stop, None).await;
}

/// Comme [`serve`], et avec `rebind`, une prise de plus (même port) sur chaque nouvelle adresse de la machine. Les prises des
/// adresses disparues restent, muettes : les retirer décalerait le routage des navigateurs déjà venus.
pub async fn serve_rebinding(sockets: Vec<tokio::net::UdpSocket>, mut relay: Relay, mut stop: tokio::sync::watch::Receiver<bool>, rebind: Option<Rebind>) {
    let port = sockets.first().and_then(|s| s.local_addr().ok()).map_or(0, |a| a.port());
    let mut known: std::collections::HashSet<IpAddr> = sockets.iter().filter_map(|s| s.local_addr().ok()).map(|a| a.ip()).collect();
    let mut sockets: Vec<std::sync::Arc<tokio::net::UdpSocket>> = sockets.into_iter().map(std::sync::Arc::new).collect();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(usize, SocketAddr, Vec<u8>)>(4096);
    let mut readers: Vec<_> = sockets.iter().enumerate().map(|(i, s)| spawn_reader(i, s.clone(), tx.clone())).collect();
    let every = rebind.as_ref().map_or(Duration::from_secs(3600), |r| r.every);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    // Par quelle prise joindre chaque navigateur (la dernière par laquelle il est venu), borné.
    let mut via: HashMap<SocketAddr, usize> = HashMap::new();
    loop {
        tokio::select! {
            _ = tick.tick(), if rebind.is_some() => {
                let Some(r) = &rebind else { continue };
                for ip in (r.ips)() {
                    if known.contains(&ip) || sockets.len() >= MAX_SOCKETS {
                        continue;
                    }
                    known.insert(ip); // un échec n'est dit qu'une fois
                    match tokio::net::UdpSocket::bind(SocketAddr::new(ip, port)).await {
                        Ok(s) => {
                            println!("relais d'appel : nouvelle adresse de la machine, écoute aussi sur udp/{ip}:{port}");
                            let s = std::sync::Arc::new(s);
                            readers.push(spawn_reader(sockets.len(), s.clone(), tx.clone()));
                            sockets.push(s);
                        }
                        Err(e) => eprintln!("relais d'appel : udp/{ip}:{port} impossible : {e}"),
                    }
                }
            }
            got = rx.recv() => {
                let Some((i, from, packet)) = got else { break };
                if via.len() > 4 * MAX_ALLOCATIONS + 1024 {
                    via.clear();
                }
                via.insert(from, i);
                for (to, bytes) in relay.handle(&packet, from, Instant::now()) {
                    let s = &sockets[via.get(&to).copied().unwrap_or(i)];
                    if let Err(e) = s.send_to(&bytes, to).await {
                        eprintln!("relais d'appel : envoi vers {to} impossible : {e}");
                    }
                }
            }
            _ = stop.changed() => break,
        }
    }
    readers.iter().for_each(tokio::task::JoinHandle::abort);
}
