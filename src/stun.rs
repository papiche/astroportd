//! Serveur STUN minimal (RFC 5389, requête « Binding » seulement) pour la visio : il dit à chaque navigateur sous quelle
//! adresse publique la station le voit, ce qui permet à deux navigateurs derrière leur box de s'appeler en direct.
//!
//! Le service lui-même est [`crate::turn::serve`] (STUN seul avec `--no-turn`). Il écoute en UDP sur le **même numéro de port** que l'interface web (une seule ouverture de port sur la box). Surface
//! exposée, donc : un paquet qui n'est pas une requête Binding bien formée est ignoré sans réponse, une réponse n'est jamais
//! plus de 2,2 fois plus grande que la requête, et chaque adresse est limitée en débit (pas d'amplification utilisable).
//! Le relais du son et de l'image (TURN), lui, n'est pas ici : sans lui, un appel entre deux réseaux très fermés échoue.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

/// Valeur fixe des en-têtes STUN (RFC 5389).
const MAGIC: u32 = 0x2112_A442;
/// Taille d'un en-tête STUN.
const HEADER: usize = 20;
/// Requête la plus longue acceptée (un MTU prudent).
const MAX_PACKET: usize = 548;
/// Requêtes par seconde et par adresse IP.
pub const PER_IP_PER_SEC: u32 = 20;
/// Adresses suivies au plus par le limiteur (au-delà, il repart de zéro).
const MAX_TRACKED: usize = 10_000;

/// Réponse à `packet` reçu de `from`, ou `None` si ce n'est pas une requête Binding bien formée (ignorée sans bruit).
pub fn respond(packet: &[u8], from: SocketAddr) -> Option<Vec<u8>> {
    if packet.len() < HEADER || packet.len() > MAX_PACKET {
        return None;
    }
    let ty = u16::from_be_bytes([packet[0], packet[1]]);
    let len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    let magic = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    // Binding Request, longueur exacte et multiple de 4, valeur fixe : sinon ce n'est pas du STUN.
    if ty != 0x0001 || magic != MAGIC || len % 4 != 0 || HEADER + len != packet.len() {
        return None;
    }
    let txid = &packet[8..HEADER];
    let port = from.port() ^ (MAGIC >> 16) as u16;
    let (family, addr): (u8, Vec<u8>) = match from.ip() {
        IpAddr::V4(ip) => (0x01, (u32::from(ip) ^ MAGIC).to_be_bytes().to_vec()),
        IpAddr::V6(ip) => {
            let mut key = MAGIC.to_be_bytes().to_vec();
            key.extend_from_slice(txid);
            (0x02, ip.octets().iter().zip(key).map(|(b, k)| b ^ k).collect())
        }
    };
    // Attribut XOR-MAPPED-ADDRESS (0x0020) : réservé, famille, port et adresse masqués par la valeur fixe.
    let mut attr = vec![0x00, family];
    attr.extend_from_slice(&port.to_be_bytes());
    attr.extend_from_slice(&addr);
    let mut out = Vec::with_capacity(HEADER + 4 + attr.len());
    out.extend_from_slice(&0x0101u16.to_be_bytes()); // Binding Success Response
    out.extend_from_slice(&u16::try_from(4 + attr.len()).ok()?.to_be_bytes());
    out.extend_from_slice(&MAGIC.to_be_bytes());
    out.extend_from_slice(txid);
    out.extend_from_slice(&0x0020u16.to_be_bytes());
    out.extend_from_slice(&u16::try_from(attr.len()).ok()?.to_be_bytes());
    out.extend_from_slice(&attr);
    Some(out)
}

/// Limiteur de débit par adresse IP (fenêtre d'une seconde).
#[derive(Default)]
pub struct Limiter {
    seen: HashMap<IpAddr, (Instant, u32)>,
}

impl Limiter {
    /// `true` si `ip` peut encore recevoir une réponse à l'instant `now`.
    pub fn allow(&mut self, ip: IpAddr, now: Instant) -> bool {
        if self.seen.len() >= MAX_TRACKED {
            self.seen.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(1));
            if self.seen.len() >= MAX_TRACKED {
                self.seen.clear();
            }
        }
        let e = self.seen.entry(ip).or_insert((now, 0));
        if now.duration_since(e.0) >= Duration::from_secs(1) {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= PER_IP_PER_SEC
    }
}
