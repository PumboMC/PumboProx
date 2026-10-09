//! Extension point: incoming connections (Java TCP today, Bedrock/RakNet later).

use std::net::SocketAddr;
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::BoxFuture;

/// Byte stream of a connection (TCP, later e.g. a stream from a RakNet session).
pub trait Transport: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Transport for T {}

/// Game edition on the other side of the connection. The core speaks the Java
/// protocol; a Bedrock connection needs a translator (§1.1) before it reaches a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    Java,
    Bedrock,
}

/// An accepted connection.
pub struct Incoming {
    pub edition: Edition,
    pub peer: SocketAddr,
    pub local: SocketAddr,
    pub transport: Pin<Box<dyn Transport>>,
}

impl std::fmt::Debug for Incoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Incoming")
            .field("edition", &self.edition)
            .field("peer", &self.peer)
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

/// A running listener.
pub trait Listener: Send + Sync {
    /// Address it listens on (after resolving port 0).
    fn local_addr(&self) -> SocketAddr;

    /// Next connection. Errors of single connections do not stop the listener.
    fn accept(&self) -> BoxFuture<'_, std::io::Result<Incoming>>;
}
