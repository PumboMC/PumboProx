//! A client connection: framing, compression, optional encryption, packet
//! IDs from the version module, and a log of every frame.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use pumbo_protocol::crypto::{Cfb8Decrypt, Cfb8Encrypt, cfb8_pair};
use pumbo_protocol::frame::{self, FrameConfig, FrameError};
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::types::{DecodeError, EncodeError};
use pumbo_protocol::{Direction, PacketKind, Phase, RawFrame, VersionModule};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::recording::Recorded;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame: {0}")]
    Frame(#[from] FrameError),
    #[error("decode: {0}")]
    Decode(#[from] DecodeError),
    #[error("encode: {0}")]
    Encode(#[from] EncodeError),
    #[error("timed out waiting for the server")]
    Timeout,
    #[error("connection closed by the server")]
    Closed,
    #[error("{0}")]
    Protocol(String),
}

/// One connection to a server.
pub struct Client {
    stream: TcpStream,
    rbuf: BytesMut,
    module: Arc<dyn VersionModule>,
    pub phase: Phase,
    frame_in: FrameConfig,
    frame_out: FrameConfig,
    decrypt: Option<Cfb8Decrypt>,
    encrypt: Option<Cfb8Encrypt>,
    pub log: Vec<Recorded>,
    pub timeout: Duration,
    /// Keep-alives answered so far.
    pub keep_alives: u32,
}

/// PROXY protocol v2 header announcing `source` as the client address
/// (destination 0.0.0.0:0 or [::]:0, which nobody reads).
pub fn proxy_v2_header(source: SocketAddr) -> Vec<u8> {
    let mut out = b"\r\n\r\n\0\r\nQUIT\n".to_vec();
    out.push(0x21); // version 2, PROXY
    match source {
        SocketAddr::V4(a) => {
            out.push(0x11); // TCP over IPv4
            out.extend_from_slice(&12u16.to_be_bytes());
            out.extend_from_slice(&a.ip().octets());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&a.port().to_be_bytes());
            out.extend_from_slice(&[0; 2]);
        }
        SocketAddr::V6(a) => {
            out.push(0x21); // TCP over IPv6
            out.extend_from_slice(&36u16.to_be_bytes());
            out.extend_from_slice(&a.ip().octets());
            out.extend_from_slice(&[0; 16]);
            out.extend_from_slice(&a.port().to_be_bytes());
            out.extend_from_slice(&[0; 2]);
        }
    }
    out
}

impl Client {
    pub async fn connect(
        addr: SocketAddr,
        module: Arc<dyn VersionModule>,
    ) -> Result<Self, ClientError> {
        Self::connect_via(addr, module, None).await
    }

    /// Connects and, with `source`, first sends a PROXY v2 header for it.
    pub async fn connect_via(
        addr: SocketAddr,
        module: Arc<dyn VersionModule>,
        source: Option<SocketAddr>,
    ) -> Result<Self, ClientError> {
        let mut stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        if let Some(source) = source {
            stream.write_all(&proxy_v2_header(source)).await?;
        }
        Ok(Self {
            stream,
            rbuf: BytesMut::with_capacity(1 << 16),
            module,
            phase: Phase::Handshake,
            frame_in: FrameConfig::from_backend(),
            frame_out: FrameConfig::from_client(),
            decrypt: None,
            encrypt: None,
            log: Vec::new(),
            timeout: Duration::from_secs(30),
            keep_alives: 0,
        })
    }

    pub fn module(&self) -> &Arc<dyn VersionModule> {
        &self.module
    }

    pub fn ctx(&self, direction: Direction) -> Ctx<'_> {
        Ctx::new(self.module.as_ref(), direction)
    }

    pub fn set_compression(&mut self, threshold: i32) {
        self.frame_in = self.frame_in.with_threshold(threshold);
        self.frame_out = self.frame_out.with_threshold(threshold);
    }

    pub fn enable_encryption(&mut self, secret: &[u8]) -> Result<(), ClientError> {
        let (enc, _) = cfb8_pair(secret).map_err(|e| ClientError::Protocol(e.to_string()))?;
        let (_, dec) = cfb8_pair(secret).map_err(|e| ClientError::Protocol(e.to_string()))?;
        self.encrypt = Some(enc);
        self.decrypt = Some(dec);
        Ok(())
    }

    /// Sends a packet in the current phase.
    pub async fn send<P: Packet>(&mut self, packet: &P) -> Result<(), ClientError> {
        let (id, payload) = self.encode(packet)?;
        self.send_raw(id, &payload).await
    }

    fn encode<P: Packet>(&self, packet: &P) -> Result<(i32, Vec<u8>), ClientError> {
        let id = self
            .module
            .packet_id(self.phase, Direction::Serverbound, P::KIND)
            .ok_or_else(|| {
                ClientError::Protocol(format!(
                    "{} has no serverbound {:?} in {:?}",
                    self.module.protocol(),
                    P::KIND,
                    self.phase
                ))
            })?;
        Ok((
            id,
            packets::encode(packet, &self.ctx(Direction::Serverbound))?,
        ))
    }

    /// A packet of the current phase as a frame, not sent and not encrypted:
    /// several of them in one [`Client::write_bytes`] reach the server in one
    /// read, like the vanilla client's flush after `login_acknowledged`.
    pub fn frame<P: Packet>(&self, packet: &P) -> Result<Vec<u8>, ClientError> {
        let (id, payload) = self.encode(packet)?;
        let mut out = BytesMut::new();
        frame::encode(&mut out, id, &payload, &self.frame_out)?;
        Ok(out.to_vec())
    }

    pub async fn send_raw(&mut self, id: i32, payload: &[u8]) -> Result<(), ClientError> {
        let mut out = BytesMut::new();
        frame::encode(&mut out, id, payload, &self.frame_out)?;
        if let Some(enc) = &mut self.encrypt {
            enc.apply(&mut out);
        }
        self.stream.write_all(&out).await?;
        self.log.push(Recorded {
            phase: self.phase,
            direction: Direction::Serverbound,
            id,
            payload: Bytes::copy_from_slice(payload),
        });
        Ok(())
    }

    /// Writes bytes as they are (after encryption, if on): for malformed frames in tests.
    pub async fn write_bytes(&mut self, data: &[u8]) -> Result<(), ClientError> {
        let mut out = data.to_vec();
        if let Some(enc) = &mut self.encrypt {
            enc.apply(&mut out);
        }
        self.stream.write_all(&out).await?;
        Ok(())
    }

    /// Next frame from the server, with its kind if the proxy knows it.
    pub async fn recv(&mut self) -> Result<(Option<PacketKind>, RawFrame), ClientError> {
        self.recv_within(self.timeout).await
    }

    pub async fn recv_within(
        &mut self,
        wait: Duration,
    ) -> Result<(Option<PacketKind>, RawFrame), ClientError> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(f) = frame::decode(&mut self.rbuf, &self.frame_in)? {
                self.log.push(Recorded {
                    phase: self.phase,
                    direction: Direction::Clientbound,
                    id: f.id,
                    payload: f.payload.clone(),
                });
                let kind = self
                    .module
                    .packet_kind(self.phase, Direction::Clientbound, f.id);
                return Ok((kind, f));
            }
            let mut chunk = [0u8; 1 << 15];
            let n = tokio::time::timeout_at(deadline, self.stream.read(&mut chunk))
                .await
                .map_err(|_| ClientError::Timeout)??;
            if n == 0 {
                return Err(ClientError::Closed);
            }
            let data = chunk.get_mut(..n).unwrap_or_default();
            if let Some(dec) = &mut self.decrypt {
                dec.apply(data);
            }
            self.rbuf.extend_from_slice(data);
        }
    }

    /// Decodes a received frame as `P`.
    pub fn decode<P: Packet>(&self, frame: &RawFrame) -> Result<P, ClientError> {
        Ok(packets::decode(
            &frame.payload,
            &self.ctx(Direction::Clientbound),
        )?)
    }

    pub async fn close(mut self) -> Vec<Recorded> {
        let _ = self.stream.shutdown().await;
        self.log
    }
}
