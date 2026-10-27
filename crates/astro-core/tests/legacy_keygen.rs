//! Isomorphisme avec les sorties réelles de `tools/keygen` et `tools/atom4love_publish.py`.
//!
//! Les vecteurs ont été produits le 2026-10-26 par
//! `~/.astro/bin/python Astroport.ONE/tools/keygen -t {duniter,ipfs,nostr} [-s] -i <fichier de crédentiels>`,
//! avec `UPLANETNAME = UPlanetTestName`. Le test `live_*` (ignoré par défaut) rejoue `keygen`
//! sur des crédentiels aléatoires : `cargo test -p astro-core --test legacy_keygen -- --ignored`.

#![allow(clippy::unwrap_used)]

use astro_core::formula::resolve_formula;
use astro_core::kdf::{duniter_seed, love_credentials, love_stretch};
use astro_core::{FormulaInput, KeyFormula, KeyKind};

const UPLANETNAME: &str = "UPlanetTestName";

struct Vector {
    name: &'static str,
    formula: KeyFormula,
    raw_salt: &'static str,
    raw_pepper: &'static str,
    coop_suffix: Option<&'static str>,
    /// Crédentiels effectivement passés à keygen.
    salt: &'static str,
    pepper: &'static str,
    g1: &'static str,
    ipfs: &'static str,
    npub: &'static str,
    nsec: &'static str,
}

const VECTORS: &[Vector] = &[
    Vector { name: "raw_ascii", formula: KeyFormula::Raw, raw_salt: "salt-test", raw_pepper: "pepper-test", coop_suffix: None, salt: "salt-test", pepper: "pepper-test", g1: "6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN", ipfs: "12D3KooWFSoxH5cZqhpcywX4P4ZKgaU1yTzcopYEvVhSQfHdhwwJ", npub: "npub17rdaeqns9qtx296qszmztm67j04xd4y76epfxwx83la2xaeknwvqnz666t", nsec: "nsec1u45etzpcvk8s55322sgg5ch02q5p6v8rpyrvsegdhdyn7pcsj54q2w85sn" },
    Vector { name: "raw_unicode", formula: KeyFormula::Raw, raw_salt: "Sél€ Ünï", raw_pepper: "côté-pepper 🌍", coop_suffix: None, salt: "Sél€ Ünï", pepper: "côté-pepper 🌍", g1: "9EUzPLqpCBw4FnUfKi9ChFvDQVmo5HmamnB7LBe7GrR7", ipfs: "12D3KooWJ3rA8URXP6CYffz5xJtYU413tppS6yr4jNY8iAo1Ao93", npub: "npub1vnw42vhk2ja9dcs669m9unl87xszgasct7j0v2yqsu3ecpz7qxrq7yc4sh", nsec: "nsec169qry2xd79p9sfxtf8snf2nqtmggxj9h53epf20nf99r6u690duqrn779h" },
    Vector { name: "isolated", formula: KeyFormula::Isolated, raw_salt: "alice@example.org", raw_pepper: "PEPPERvalue123", coop_suffix: None, salt: "alice@example.org", pepper: "PEPPERvalue123_aeeb4f9fe9753492", g1: "6tUUePg7FMjgbQXpkxMPsczzUULurXJR361qFFMTBxcY", ipfs: "12D3KooWFhqePXFpSG1B1J3FPZ6jeR5pxoPYtDNtzgNrdEWM5uLU", npub: "npub14l4fy0c7rqps24a20w9t37elxuy2f3q00d800qrda0ppq4zrd2rs8ndxku", nsec: "nsec1aeyszua62hgj9z0t90dxj2xraawpcpa3f2yvqanyzd9yx5ddyz3sqguvce" },
    Vector { name: "legacy", formula: KeyFormula::Legacy, raw_salt: "alice@example.org", raw_pepper: "PEPPERvalue123", coop_suffix: None, salt: "alice@example.org", pepper: "PEPPERvalue123", g1: "DP35YSK14nALHCMLgRNuo2xvsexPgFY18ZAAJJwj5Eoe", ipfs: "12D3KooWNCQFHZtiFgRph5rmK28FZq3mMz12hwcV69XBgJ6cyBXa", npub: "npub1rlmcd8cgnldm6a6dfu49jl6qlfccuycxukz0p2fturtgv6s4pessnagrte", nsec: "nsec1qpm9w0fsfh8mjnj0mwfme7dvn9gq63qhx0ugl9sta64xalrfx7uslzvmu3" },
    Vector { name: "concat_swarm", formula: KeyFormula::ConcatSwarm, raw_salt: "0123456789abcdef0123456789abcdef", raw_pepper: "fedcba9876543210fedcba9876543210", coop_suffix: None, salt: "0123456789abcdef0123456789abcdefUPlanetTestName", pepper: "fedcba9876543210fedcba9876543210UPlanetTestName", g1: "77B4zLFrbtexADfZ5atZz8hdkbHxpFFvMBfXEjhW9pBW", ipfs: "12D3KooWFvYEjTqZnnvSa7AyiBdukvnUEvLbqwLQJn2YcirQ3kuS", npub: "npub1ju7un2p4zgngcatjajk8aywt0a09y55zqsra0wyagrl5tx2w224q9vjl42", nsec: "nsec1vwwkr883pfx9kp43pmhynhz7f58tfyed7dfmeelad9qhx3j0mfcsxg2fj2" },
    Vector { name: "coop_g1", formula: KeyFormula::CoopPrefix, raw_salt: "", raw_pepper: "", coop_suffix: Some("G1"), salt: "UPlanetTestName.G1", pepper: "UPlanetTestName.G1", g1: "EgemnJRqh59pMp7fV4YT3cVWmiUXyR9Q8kAG2Ckvrxpi", ipfs: "12D3KooWPW1wXS1YsyRJmhd67fHnpQaMG3XB17Dt6LXHQBupkuYe", npub: "npub1ares6j9pz7j3yx2xqmwmwfygl2sjhvnglljktyd0jf5yl8p8wfwszwgnn7", nsec: "nsec17ahxn5kxetwdzjyf09zz0afcc8usz4qfhk84qp7dhl5lwuezunssj7y5y2" },
    Vector { name: "coop_bare", formula: KeyFormula::CoopPrefix, raw_salt: "", raw_pepper: "", coop_suffix: None, salt: "UPlanetTestName", pepper: "UPlanetTestName", g1: "DvV7LxcfcpaqJX6cmXcEdmwUC6YFx9JLcriW6yFtZ7zt", ipfs: "12D3KooWNjrH66CNoirKiQc3Q8MaQa2JgRatyqNpaT5XUxQnT4ip", npub: "npub1h0s99t4d584z8ulkc83kngs9573c69xzlleczpnn8mgwrkrwk8rqmev53y", nsec: "nsec1eccygtdmm83ytun9ur8jjm8r0n0j24xqqt5k96yu76e3qwv8sussycdc8z" },
];

fn assert_keys(name: &str, salt: &str, pepper: &str, g1: &str, ipfs: &str, npub: &str, nsec: &str) {
    let seed = duniter_seed(salt, pepper).unwrap();
    assert_eq!(seed.g1_pubkey(), g1, "{name} : clé Ğ1");
    assert_eq!(seed.ipfs_peer_id(), ipfs, "{name} : PeerID IPFS");
    let nostr = seed.nostr_keys().unwrap();
    assert_eq!(nostr.npub().unwrap(), npub, "{name} : npub");
    assert_eq!(*nostr.nsec().unwrap(), nsec, "{name} : nsec");
}

#[test]
fn formulas_and_keys_match_legacy_keygen() {
    for v in VECTORS {
        let input = FormulaInput { salt: v.raw_salt, pepper: v.raw_pepper, uplanet_name: UPLANETNAME, coop_suffix: v.coop_suffix };
        let creds = v.formula.credentials(&input).unwrap();
        assert_eq!(creds.salt(), v.salt, "{} : salt", v.name);
        assert_eq!(creds.pepper(), v.pepper, "{} : pepper", v.name);
        assert_keys(v.name, creds.salt(), creds.pepper(), v.g1, v.ipfs, v.npub, v.nsec);
    }
}

#[test]
fn nostr_hex_is_the_bech32_payload_of_npub() {
    for v in VECTORS {
        let seed = duniter_seed(v.salt, v.pepper).unwrap();
        let (hrp, data) = bech32::decode(v.npub).unwrap();
        assert_eq!(hrp.as_str(), "npub");
        assert_eq!(seed.nostr_keys().unwrap().pubkey_hex(), hex::encode(data), "{}", v.name);
    }
}

#[test]
fn love_key_matches_atom4love_publish() {
    // Entrées brutes (naissance, conception) → `stretch` → keygen -t nostr (cf. derive_love_keypair).
    let (raw_salt, raw_pepper) = ("1990-04-12 Paris 48.85 2.35", "1992-07-03 Lyon 45.76 4.84 50 170");
    assert_eq!(*love_stretch(raw_salt), "j7jk-CeTzvvbYl3TzSwdP3PiMzeZ9DURRa_w2yxW1fQ");
    assert_eq!(*love_stretch(raw_pepper), "pwDxhpVitIRMQzow3ZmhkblVA57ElPFJ7XWsmdsoFpU");
    let creds = love_credentials(raw_salt, raw_pepper);
    assert_keys("love", creds.salt(), creds.pepper(), "7QiRxFhP5jjb83NsTjFwZVfrUJzHQ14Shz9gPTJ3qvLF", "12D3KooWGE5bhPH6Ge15XvtJ6L1HLHkgxe2vRh8vfaWhmSSwjs4B", "npub1en63efrrght0c0xvdtvlf7jjv56cukasqk274klzz9dacn9cdmzqkspdqu", "nsec1v76tdh2xc3tugk859wz47q0qg6a6tz609xw7mwvh0w8uljt4epvsc5hdue");
}

#[test]
fn resolve_formula_finds_isolated_then_legacy() {
    let (salt, pepper) = ("alice@example.org", "PEPPERvalue123");
    let iso = VECTORS.iter().find(|v| v.name == "isolated").unwrap();
    let leg = VECTORS.iter().find(|v| v.name == "legacy").unwrap();
    let r = |kind, reference: &str| resolve_formula(kind, salt, pepper, UPLANETNAME, reference).unwrap();
    assert_eq!(r(KeyKind::G1, iso.g1), Some(KeyFormula::Isolated));
    assert_eq!(r(KeyKind::G1, leg.g1), Some(KeyFormula::Legacy));
    // Même résolution à partir de l'adresse SS58 v2s de la clé de référence.
    let ss58 = astro_core::kdf::ss58_encode(astro_core::kdf::G1_SS58_PREFIX, &bs58::decode(iso.g1).into_vec().unwrap().try_into().unwrap()).unwrap();
    assert_eq!(r(KeyKind::G1, &ss58), Some(KeyFormula::Isolated));
    assert_eq!(r(KeyKind::Nostr, iso.npub), Some(KeyFormula::Isolated));
    assert_eq!(r(KeyKind::Ipns, &format!("/ipns/{}", leg.ipfs)), Some(KeyFormula::Legacy));
    let hex = hex::encode(bech32::decode(iso.npub).unwrap().1);
    assert_eq!(r(KeyKind::Nostr, &hex), Some(KeyFormula::Isolated));
    // Une clé étrangère ne correspond à aucune formule : on ne signe pas avec le mauvais wallet.
    assert_eq!(r(KeyKind::G1, "6dSnXx2reoZ8a41dkToyunPBV8wyn8TkxuLR2g8jp1DN"), None);
}

#[test]
fn resolve_formula_rejects_a_broken_npub() {
    assert!(resolve_formula(KeyKind::Nostr, "s", "p", UPLANETNAME, "npub1invalid").is_err());
}

/// Rejoue le vrai `keygen` sur des crédentiels aléatoires (nécessite `~/.astro/bin/python` et duniterpy).
#[test]
#[ignore = "exécute les outils legacy"]
fn live_keygen_agrees_on_random_credentials() {
    use std::io::Write;
    use std::process::Command;
    let home = std::env::var("HOME").unwrap();
    let tools = std::env::var("ASTRO_LEGACY_TOOLS")
        .unwrap_or_else(|_| format!("{}/../../../Astroport.ONE/tools", env!("CARGO_MANIFEST_DIR")));
    let python = format!("{home}/.astro/bin/python");
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ u64::from(std::process::id());
    let mut rnd = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for i in 0..5 {
        let salt = format!("salt-{i}-{:x} é", rnd());
        let pepper = format!("pep{:x}_{:x}", rnd(), rnd());
        let path = std::env::temp_dir().join(format!("astro-core-cred-{}-{i}", std::process::id()));
        std::fs::File::create(&path).unwrap().write_all(format!("{salt}\n{pepper}\n").as_bytes()).unwrap();
        let run = |t: &str, secret: bool| {
            let mut c = Command::new(&python);
            c.arg(format!("{tools}/keygen")).args(["-t", t, "-i"]).arg(&path);
            if secret {
                c.arg("-s");
            }
            String::from_utf8(c.output().unwrap().stdout).unwrap().trim().to_owned()
        };
        let (g1, ipfs, npub, nsec) = (run("duniter", false), run("ipfs", false), run("nostr", false), run("nostr", true));
        std::fs::remove_file(&path).unwrap();
        assert_keys(&format!("live-{i}"), &salt, &pepper, &g1, &ipfs, &npub, &nsec);
    }
}
