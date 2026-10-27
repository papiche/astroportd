//! Clé de réseau privé (`swarm.key`) : isole une constellation des autres.

use libp2p::pnet::PreSharedKey;

use crate::P2pError;

/// Lit un fichier `swarm.key` au format IPFS :
/// ```text
/// /key/swarm/psk/1.0.0/
/// /base16/
/// <64 hexadécimaux>
/// ```
pub fn parse_swarm_key(text: &str) -> Result<PreSharedKey, P2pError> {
    text.trim().parse::<PreSharedKey>().map_err(|e| P2pError::SwarmKey(e.to_string()))
}

/// Génère le contenu d'un nouveau `swarm.key`.
pub fn generate_swarm_key() -> Result<String, P2pError> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| P2pError::SwarmKey(e.to_string()))?;
    Ok(format!("/key/swarm/psk/1.0.0/\n/base16/\n{}\n", hex::encode(key)))
}

/// Empreinte publique de la clé (pour vérifier que deux stations partagent le même réseau sans révéler la clé).
pub fn fingerprint(psk: &PreSharedKey) -> String {
    psk.fingerprint().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_round_trip_and_differ() {
        let (a, b) = (generate_swarm_key().unwrap(), generate_swarm_key().unwrap());
        assert_ne!(a, b);
        let (pa, pb) = (parse_swarm_key(&a).unwrap(), parse_swarm_key(&b).unwrap());
        assert_eq!(fingerprint(&pa), fingerprint(&parse_swarm_key(&a).unwrap()));
        assert_ne!(fingerprint(&pa), fingerprint(&pb));
        assert!(a.starts_with("/key/swarm/psk/1.0.0/\n/base16/\n"));
    }

    #[test]
    fn malformed_keys_are_refused() {
        for bad in ["", "n'importe quoi", "/key/swarm/psk/1.0.0/\n/base16/\nzz", "/key/swarm/psk/1.0.0/\n/base16/\nabcd"] {
            assert!(parse_swarm_key(bad).is_err(), "{bad:?}");
        }
    }
}
