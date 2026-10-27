//! Identité de la station : découverte automatique des clés locales (`astro-lifecycle`).
//!
//! Plus aucune variable d'environnement n'est requise : le NODE fournit la clé de signature Nostr, la graine P2P
//! (`secret.NODE.dunikey`) et, par dérivation, le secret de station des jetons WebDAV. `ASTRO_STATION_SECRET` et
//! `ASTRO_P2P_SEED` restent des **surcharges** facultatives (conteneurs, tests) et l'emportent si elles sont définies.

use std::path::{Path, PathBuf};

use astro_lifecycle::discovery::{Discovered, Layout, discover};
use astro_sync::Signer;
use zeroize::Zeroizing;

type Failure = Box<dyn std::error::Error>;

/// Identité résolue au démarrage.
pub struct StationIdentity {
    /// Secret de station (jetons WebDAV, authentification entre stations).
    pub secret: Vec<u8>,
    /// Signature des réponses NIP-42 aux relais distants.
    pub signer: Option<Signer>,
    /// Clé Nostr du NODE : adresse et déchiffre les messages privés de la station (bus de commandes de BRO).
    pub node_secret: Option<Zeroizing<[u8; 32]>>,
    /// Graine ed25519 de la clé IPFS/libp2p.
    pub p2p_seed: Option<[u8; 32]>,
    /// Dossier des comptes MULTIPASS (`<game>/nostr`).
    pub nostr_dir: Option<PathBuf>,
    /// Identités locales détectées.
    pub discovered: Option<Discovered>,
}

fn hex_env(name: &str) -> Result<Option<Vec<u8>>, Failure> {
    let Ok(raw) = std::env::var(name) else { return Ok(None) };
    Ok(Some(hex::decode(raw.trim()).map_err(|_| format!("{name} doit être en hexadécimal"))?))
}

fn layout(game_dir: Option<&Path>) -> Result<(Layout, PathBuf), Failure> {
    let game = match game_dir {
        Some(dir) => dir.to_owned(),
        None => match std::env::var_os("ASTRO_GAME_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME introuvable : indiquer --game-dir ou ASTRO_GAME_DIR")?).join(".zen/game"),
        },
    };
    Ok((Layout::new(&game), game.join("nostr")))
}

/// Résout l'identité de la station. `required` : échoue si ni découverte ni variable d'environnement ne fournissent le secret.
pub fn resolve(game_dir: Option<&Path>, required: bool) -> Result<StationIdentity, Failure> {
    let env_secret = hex_env("ASTRO_STATION_SECRET")?;
    let env_seed = hex_env("ASTRO_P2P_SEED")?;
    let (layout, nostr_dir) = layout(game_dir)?;
    let discovered = match discover(&layout) {
        Ok(d) => {
            for p in &d.problems {
                eprintln!("identité : {} : {}", p.path.display(), p.what);
            }
            println!("identité : NODE {} (PeerID {}), {} MULTIPASS hébergé(s){}", &d.node.nostr_hex[..8], d.node.peer_id, d.multipasses.len(), d.captain_email.as_deref().map(|c| format!(", Capitaine {c}")).unwrap_or_default());
            Some(d)
        }
        Err(e) if env_secret.is_some() => {
            eprintln!("identité : découverte impossible ({e}) ; ASTRO_STATION_SECRET utilisé");
            None
        }
        Err(e) if required => return Err(format!("identité de la station introuvable : {e} (--game-dir ou ASTRO_GAME_DIR)").into()),
        Err(e) => {
            eprintln!("identité : {e}");
            None
        }
    };
    let secret = match (&env_secret, &discovered) {
        (Some(s), _) => s.clone(),
        (None, Some(d)) => d.node.station_secret().to_vec(),
        (None, None) => Vec::new(),
    };
    let signer = match (&env_secret, &discovered) {
        (Some(s), _) if s.len() == 32 => Some(Signer::new(s.as_slice().try_into().map_err(|_| "ASTRO_STATION_SECRET")?)),
        (_, Some(d)) => Some(Signer::new(*d.node.nostr_secret())),
        _ => None,
    };
    let node_secret = match (&env_secret, &discovered) {
        (Some(s), _) if s.len() == 32 => <[u8; 32]>::try_from(s.as_slice()).ok().map(Zeroizing::new),
        (_, Some(d)) => Some(Zeroizing::new(*d.node.nostr_secret())),
        _ => None,
    };
    let p2p_seed = match (env_seed, &discovered) {
        (Some(s), _) => Some(<[u8; 32]>::try_from(s).map_err(|_| "ASTRO_P2P_SEED doit faire 32 octets")?),
        (None, Some(d)) => Some(*d.node.p2p_seed()),
        (None, None) => None,
    };
    Ok(StationIdentity { secret, signer, node_secret, p2p_seed, nostr_dir: nostr_dir.is_dir().then_some(nostr_dir), discovered })
}
