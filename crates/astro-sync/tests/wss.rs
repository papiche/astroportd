//! `wss://` : le crawler interroge un relai TLS (certificat auto-signé déclaré comme racine supplémentaire).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use astro_nostr_proto::{ClientMessage, Filter, RelayMessage};
use astro_sync::{CrawlConfig, Crawler, RelayClient, WsClient};
use common::*;
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_tungstenite::tungstenite::Message;

/// Relai TLS minimal : répond à chaque REQ par une note de Bob puis EOSE.
async fn tls_relay() -> (String, Vec<u8>) {
    let rcgen::CertifiedKey { cert, key_pair } = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert_der = cert.der().to_vec();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(cert_der.clone())], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der())))
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("wss://localhost:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else { return };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else { return };
                let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else { return };
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    if let Ok(ClientMessage::Req { sub_id, .. }) = ClientMessage::parse(text.as_str()) {
                        let note = ev(&BOB, 1, 1_800_000_000, vec![], "reçu via wss");
                        let _ = ws.send(Message::text(RelayMessage::Event { sub_id: sub_id.clone(), event: Box::new(note) }.to_json())).await;
                        let _ = ws.send(Message::text(RelayMessage::Eose(sub_id).to_json())).await;
                    }
                }
            });
        }
    });
    (url, cert_der)
}

#[tokio::test]
async fn fetches_events_over_tls_with_a_declared_root() {
    let (url, cert) = tls_relay().await;
    let client = WsClient::new(Duration::from_secs(10)).with_extra_root(cert);
    let events = client.fetch(&url, &Filter { kinds: Some(vec![1]), ..Filter::default() }).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].content, "reçu via wss");
}

#[tokio::test]
async fn an_unknown_certificate_authority_is_refused() {
    let (url, _cert) = tls_relay().await;
    let err = WsClient::new(Duration::from_secs(10)).fetch(&url, &Filter::default()).await.unwrap_err().to_string();
    assert!(err.to_lowercase().contains("certificate") || err.to_lowercase().contains("unknownissuer") || err.contains("invalid peer"), "{err}");
}

#[tokio::test]
async fn the_crawler_replicates_through_a_secure_relay() {
    let (url, cert) = tls_relay().await;
    let dir = TempDir::new("wss");
    let db = open_db(&dir);
    host(&db, &BOB).await;
    let client = WsClient::new(Duration::from_secs(10)).with_extra_root(cert);
    let crawler = Crawler::new(db.clone(), client, CrawlConfig { relays: vec![url], ..CrawlConfig::default() });
    let stats = crawler.run_once(&[]).await.unwrap();
    assert!(stats.errors.is_empty(), "{:?}", stats.errors);
    assert_eq!(stats.inserted, 1);
    let n: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM nostr_events WHERE content = 'reçu via wss'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(n, 1);
}
