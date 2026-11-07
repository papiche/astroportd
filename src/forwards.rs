//! Accès locaux aux services d'un ami, ouverts à chaud depuis l'écran d'assistance : l'ami aidant clique « Se connecter »
//! au lieu de redémarrer avec `--p2p-forward`.
//!
//! L'écoute est **toujours** sur `127.0.0.1` (port choisi par le système) et se ferme seule à l'échéance de l'aide.
//! Elle ne donne aucun droit : c'est la politique de tunnels de la station aidée qui accepte ou refuse chaque flux.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use astro_p2p::tunnel::{Forward, forward};
use astro_p2p::{P2pHandle, PeerId};

struct Open {
    fwd: Forward,
    until: i64,
}

/// Accès locaux ouverts, par identifiant de demande.
pub struct Forwards {
    handle: Option<P2pHandle>,
    open: Arc<Mutex<HashMap<String, Open>>>,
}

impl Forwards {
    /// `handle` : le maillage P2P (`None` : rien ne peut être ouvert).
    pub fn new(handle: Option<P2pHandle>) -> Self {
        Self { handle, open: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Open>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ouvre (ou retrouve) l'accès local à `service` de la station `peer` pour la demande `id`, jusqu'à `until` (secondes Unix).
    pub async fn open(&self, id: &str, peer: &str, service: &str, until: i64, now: i64) -> Result<SocketAddr, String> {
        self.sweep(now);
        // Tout nom de service bien formé : c'est la politique de la station aidée qui décide (SSH, ComfyUI, Ollama, ou un
        // service que son Capitaine partage à chaud, comme un bureau à distance).
        if service.is_empty() || service.len() > 32 || !service.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
            return Err(format!("service « {service} » non pris en charge"));
        }
        let peer: PeerId = peer.parse().map_err(|_| "identifiant de station invalide".to_owned())?;
        if let Some(o) = self.lock().get(id) {
            return Ok(o.fwd.local_addr);
        }
        let handle = self.handle.as_ref().ok_or("maillage P2P inactif sur cette station")?;
        let fwd = forward(handle, SocketAddr::from(([127, 0, 0, 1], 0)), peer, service).await.map_err(|e| e.to_string())?;
        let addr = fwd.local_addr;
        self.lock().insert(id.to_owned(), Open { fwd, until });
        // L'accès se referme à l'échéance, même si personne n'ouvre plus l'écran d'assistance : sans minuteur, le port local
        // resterait actif jusqu'au prochain `open` ou `list`. Si la demande a été rouverte entre-temps (autre échéance), on n'y touche pas.
        let (map, key) = (self.open.clone(), id.to_owned());
        let wait = std::time::Duration::from_secs(u64::try_from(until.saturating_sub(now)).unwrap_or(0));
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let mut g = map.lock().unwrap_or_else(|e| e.into_inner());
            if g.get(&key).is_some_and(|o| o.until == until) {
                g.remove(&key);
            }
        });
        Ok(addr)
    }

    /// Ferme l'accès de la demande `id` (sans effet s'il n'est pas ouvert).
    pub fn close(&self, id: &str) {
        self.lock().remove(id);
    }

    /// Accès ouverts : `(id, adresse locale, échéance)`.
    pub fn list(&self, now: i64) -> Vec<(String, SocketAddr, i64)> {
        self.sweep(now);
        self.lock().iter().map(|(id, o)| (id.clone(), o.fwd.local_addr, o.until)).collect()
    }

    /// Referme ce qui est échu.
    fn sweep(&self, now: i64) {
        self.lock().retain(|_, o| o.until > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_malformed_services_bad_peers_and_missing_mesh() {
        let f = Forwards::new(None);
        for bad in ["../ssh", "Bureau", "", "a b", &"x".repeat(33)] {
            assert!(f.open("a", "12D3KooWx", bad, 100, 0).await.unwrap_err().contains("non pris en charge"), "{bad}");
        }
        // Un service partagé à chaud par l'ami (ex. son bureau) est un nom bien formé : c'est sa politique qui tranchera.
        assert!(f.open("a", "12D3KooWx", "bureau-vnc", 100, 0).await.unwrap_err().contains("invalide"), "seul le PeerID bloque ici");
        assert!(f.open("a", "pas-un-peer", "ssh", 100, 0).await.unwrap_err().contains("invalide"));
        let peer = astro_p2p::keypair_from_seed(&[1; 32]).unwrap().public().to_peer_id().to_string();
        assert!(f.open("a", &peer, "ssh", 100, 0).await.unwrap_err().contains("P2P inactif"));
        assert!(f.list(0).is_empty());
    }
}
