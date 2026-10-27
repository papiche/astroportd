//! Envoi des enveloppes NIP-59 de FaceCloud vers les relais de la constellation.

use astro_facecloud::{Outbox, OutboxError};
use astro_nostr_proto::Event;

use crate::client::WsClient;

/// Publie chaque enveloppe sur les relais configurés ; réussit dès qu'un relai l'accepte (ou la connaît déjà).
///
/// Le relai du destinataire accepte un gift wrap d'un inconnu s'il vise un MULTIPASS qu'il héberge
/// (exception d'amorçage NIP-59), donc sans authentification de l'expéditeur.
pub struct RelayOutbox {
    client: WsClient,
    relays: Vec<String>,
}

impl RelayOutbox {
    /// `relays` : URL `ws://` ou `wss://` des relais de la constellation.
    pub fn new(client: WsClient, relays: Vec<String>) -> Self {
        Self { client, relays }
    }
}

impl Outbox for RelayOutbox {
    async fn send(&self, recipient_hex: &str, wrap: Event) -> Result<(), OutboxError> {
        let mut failures = Vec::new();
        for relay in &self.relays {
            match self.client.publish(relay, &wrap).await {
                Ok((true, _)) => return Ok(()),
                Ok((false, message)) => failures.push(format!("{relay} : {message}")),
                Err(e) => failures.push(format!("{relay} : {e}")),
            }
        }
        Err(OutboxError(format!("aucun relai n'a accepté l'enveloppe pour {}… ({})", &recipient_hex[..8.min(recipient_hex.len())], failures.join(" ; "))))
    }
}
