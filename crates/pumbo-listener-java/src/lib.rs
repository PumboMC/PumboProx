//! Java Edition listener over TCP.

use std::net::SocketAddr;

use pumbo_core::BoxFuture;
use pumbo_core::listener::{Edition, Incoming, Listener};
use pumbo_core::registry::{ModuleConfig, ModuleError, ModuleRegistry, opt_str};
use tokio::net::TcpListener;

pub const NAME: &str = "java-tcp";

#[derive(Debug)]
pub struct JavaTcpListener {
    inner: TcpListener,
    local: SocketAddr,
}

impl JavaTcpListener {
    pub async fn bind(addr: &str) -> std::io::Result<Self> {
        let inner = TcpListener::bind(addr).await?;
        let local = inner.local_addr()?;
        Ok(Self { inner, local })
    }
}

impl Listener for JavaTcpListener {
    fn local_addr(&self) -> SocketAddr {
        self.local
    }

    fn accept(&self) -> BoxFuture<'_, std::io::Result<Incoming>> {
        Box::pin(async move {
            let (stream, peer) = self.inner.accept().await?;
            stream.set_nodelay(true)?;
            Ok(Incoming {
                edition: Edition::Java,
                peer,
                local: self.local,
                transport: Box::pin(stream),
            })
        })
    }
}

fn factory(cfg: &ModuleConfig) -> BoxFuture<'_, Result<Box<dyn Listener>, ModuleError>> {
    Box::pin(async move {
        let bind = opt_str(cfg, NAME, "bind")?.unwrap_or("0.0.0.0:25565");
        let listener = JavaTcpListener::bind(bind)
            .await
            .map_err(|source| ModuleError::Io {
                name: NAME.to_string(),
                source,
            })?;
        Ok(Box::new(listener) as Box<dyn Listener>)
    })
}

pub fn register(reg: &mut ModuleRegistry) -> Result<(), ModuleError> {
    reg.listeners.register(NAME, factory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn accepts_a_connection() {
        let mut reg = ModuleRegistry::new();
        register(&mut reg).unwrap();
        let mut cfg = ModuleConfig::new();
        cfg.insert("bind".into(), "127.0.0.1:0".into());
        let listener = (reg.listeners.get(NAME).unwrap())(&cfg).await.unwrap();
        let addr = listener.local_addr();
        let client = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.write_all(b"\x10\x00").await.unwrap();
        });
        let mut incoming = listener.accept().await.unwrap();
        assert_eq!(incoming.edition, Edition::Java);
        let mut buf = [0u8; 2];
        incoming.transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"\x10\x00");
        client.await.unwrap();
    }
}
