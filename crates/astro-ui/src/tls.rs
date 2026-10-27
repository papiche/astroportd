//! Écouteur HTTPS : rustls (sans OpenSSL), certificat résolu à chaque poignée de main (rechargement à chaud).

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use rustls::server::ResolvesServerCert;
use rustls::ServerConfig;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

/// Délai maximal d'une poignée de main TLS (protège l'écouteur des clients qui ne finissent jamais).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Écouteur TLS pour `axum::serve`.
pub struct TlsListener {
    rx: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TlsListener {
    /// Écoute sur `addr` ; chaque connexion négocie TLS avec le certificat que `resolver` rend à cet instant.
    pub async fn bind(addr: SocketAddr, resolver: Arc<dyn ResolvesServerCert>) -> io::Result<Self> {
        let tcp = TcpListener::bind(addr).await?;
        let local = tcp.local_addr()?;
        let mut config = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_no_client_auth()
            .with_cert_resolver(resolver);
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let (tx, rx) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, peer)) = tcp.accept().await else { continue };
                let (acceptor, tx) = (acceptor.clone(), tx.clone());
                tokio::spawn(async move {
                    if let Ok(Ok(stream)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(socket)).await {
                        let _ = tx.send((stream, peer)).await;
                    }
                });
            }
        });
        Ok(Self { rx, local, task })
    }
}

impl Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.rx.recv().await {
                Some(conn) => return conn,
                None => std::future::pending::<()>().await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}

/// Adresse du client d'une connexion TLS (axum ne fournit `ConnectInfo<SocketAddr>` que pour `TcpListener`).
#[derive(Debug, Clone, Copy)]
pub struct TlsPeer(pub SocketAddr);

impl Connected<IncomingStream<'_, TlsListener>> for TlsPeer {
    fn connect_info(stream: IncomingStream<'_, TlsListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

/// Recopie `ConnectInfo<TlsPeer>` en `ConnectInfo<SocketAddr>` pour que les gestionnaires (limitation de débit
/// par IP du relai, `X-Forwarded-For` du proxy) voient la même chose en HTTP et en HTTPS.
pub async fn promote_peer(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    if let Some(axum::extract::ConnectInfo(TlsPeer(addr))) = req.extensions().get::<axum::extract::ConnectInfo<TlsPeer>>().copied() {
        req.extensions_mut().insert(axum::extract::ConnectInfo(addr));
    }
    next.run(req).await
}
