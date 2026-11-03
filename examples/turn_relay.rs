//! Relais d'appel (STUN + TURN) seul, sans station : pour essayer la visio en navigateur à travers le relais.
//!
//! `cargo run --example turn_relay -- 0.0.0.0:3479` écoute en UDP et affiche, en JSON sur une ligne, le port et des identifiants
//! valables une heure pour les étiquettes `alice`, `bob` et `n1` (ce que l'API et l'offre d'appel donneraient aux navigateurs).
//! Le secret est fixe : ce relais n'est fait que pour les essais.

use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "0.0.0.0:0".to_owned());
    let socks = astroportd::turn::bind(addr.parse()?).await?;
    let secret = [42u8; 32];
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let creds: serde_json::Map<String, serde_json::Value> = ["alice", "bob", "n1"]
        .iter()
        .map(|t| {
            let (username, credential) = astroportd::turn::credentials(&secret, t, now, 3600);
            ((*t).to_owned(), serde_json::json!({ "username": username, "credential": credential }))
        })
        .collect();
    println!("{}", serde_json::json!({ "port": socks[0].local_addr()?.port(), "credentials": creds }));
    let (_keep, stop) = tokio::sync::watch::channel(false);
    astroportd::turn::serve(socks, astroportd::turn::Relay::new(secret), stop).await;
    Ok(())
}
