//! Serveur STUN de la visio : réponse conforme (IPv4, IPv6), paquets hostiles ignorés, débit limité par adresse.
#![allow(clippy::expect_used, clippy::panic)]
#![allow(clippy::unwrap_used)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use astroportd::stun::{Limiter, PER_IP_PER_SEC, respond};

fn request(txid: [u8; 12]) -> Vec<u8> {
    let mut p = vec![0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42];
    p.extend_from_slice(&txid);
    p
}

/// Adresse lue dans l'attribut XOR-MAPPED-ADDRESS d'une réponse (le décodage qu'en fait un navigateur).
fn mapped(reply: &[u8]) -> SocketAddr {
    assert_eq!(&reply[0..2], &[0x01, 0x01], "Binding Success Response");
    assert_eq!(&reply[20..22], &[0x00, 0x20], "XOR-MAPPED-ADDRESS");
    let port = u16::from_be_bytes([reply[26], reply[27]]) ^ 0x2112;
    let key: Vec<u8> = [0x21, 0x12, 0xA4, 0x42].iter().chain(&reply[8..20]).copied().collect();
    let ip = match reply[25] {
        0x01 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(reply[28..32].iter().zip(&key).map(|(a, k)| a ^ k).collect::<Vec<_>>()).unwrap())),
        0x02 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(reply[28..44].iter().zip(&key).map(|(a, k)| a ^ k).collect::<Vec<_>>()).unwrap())),
        f => panic!("famille inconnue {f}"),
    };
    SocketAddr::new(ip, port)
}

#[test]
fn a_binding_request_gets_the_public_address_back_in_ipv4_and_ipv6() {
    for from in ["203.0.113.7:51820".parse::<SocketAddr>().unwrap(), "[2001:db8::42]:40000".parse().unwrap()] {
        let req = request([7; 12]);
        let reply = respond(&req, from).expect("requête valide");
        assert_eq!(&reply[8..20], &[7; 12], "même identifiant de transaction");
        assert_eq!(mapped(&reply), from);
        assert!(reply.len() * 10 <= req.len() * 22, "jamais plus de 2,2 fois la requête : {} pour {}", reply.len(), req.len());
    }
}

#[test]
fn hostile_or_foreign_packets_are_ignored_without_reply() {
    let from: SocketAddr = "198.51.100.1:9".parse().unwrap();
    let good = request([1; 12]);
    let mut wrong_type = good.clone();
    wrong_type[1] = 0x11;
    let mut wrong_magic = good.clone();
    wrong_magic[4] = 0;
    let mut lying_length = good.clone();
    lying_length[3] = 8; // annonce 8 octets d'attributs absents
    let mut odd_length = good.clone();
    odd_length[3] = 3;
    odd_length.extend_from_slice(&[0, 0, 0]);
    let huge = { let mut p = good.clone(); p[2] = 0x02; p[3] = 0x14; p.resize(20 + 0x214, 0); p }; // 552 octets : au-delà des 548 acceptés
    for (why, p) in [("vide", vec![]), ("tronqué", good[..19].to_vec()), ("autre type", wrong_type), ("valeur fixe", wrong_magic), ("longueur mensongère", lying_length), ("longueur non alignée", odd_length), ("trop gros", huge), ("bruit", vec![0xFF; 64])] {
        assert!(respond(&p, from).is_none(), "{why} : aucune réponse");
    }
}

#[test]
fn each_address_is_rate_limited_and_the_tracker_stays_bounded() {
    let mut l = Limiter::default();
    let (a, b) = ("203.0.113.9".parse::<IpAddr>().unwrap(), "203.0.113.10".parse::<IpAddr>().unwrap());
    let t0 = Instant::now();
    assert_eq!((0..PER_IP_PER_SEC * 3).filter(|_| l.allow(a, t0)).count() as u32, PER_IP_PER_SEC, "au-delà du débit : silence");
    assert!(l.allow(b, t0), "une autre adresse n'est pas pénalisée");
    assert!(l.allow(a, t0 + Duration::from_millis(1001)), "une seconde plus tard, ça repart");
    // Une inondation d'adresses usurpées ne fait pas grossir la mémoire sans fin.
    for i in 0..30_000u32 {
        l.allow(IpAddr::V4(Ipv4Addr::from(i)), t0);
    }
    assert!(l.allow(a, t0 + Duration::from_secs(3)));
}
