//! Publication d'un événement signé par la station.

use std::future::Future;

use astro_nostr_proto::Event;

/// Publie un événement signé (relai local, relais de la constellation).
pub trait Publisher: Send + Sync + 'static {
    /// Publie `event`.
    fn publish(&self, event: Event) -> impl Future<Output = Result<(), String>> + Send;
}
