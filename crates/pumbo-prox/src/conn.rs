//! One side of a session: a byte stream with framing, compression and
//! encryption. Reads are cancel-safe, so a session can wait on both sides in
//! one `select!`.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use pumbo_core::listener::Transport;
use pumbo_core::translate::{PacketTranslator, Translated};
use pumbo_protocol::RawFrame;
use pumbo_protocol::crypto::{Cfb8Decrypt, Cfb8Encrypt, CryptoError, cfb8_pair};
use pumbo_protocol::frame::{self, FrameConfig, FrameError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A write that takes longer than this means the peer stopped reading.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const READ_CHUNK: usize = 32 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ConnError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame: {0}")]
    Frame(#[from] FrameError),
    #[error("connection closed")]
    Closed,
    #[error("write timed out")]
    WriteTimeout,
}

pub struct Conn {
    stream: Pin<Box<dyn Transport>>,
    rbuf: BytesMut,
    wbuf: BytesMut,
    pub inbound: FrameConfig,
    pub outbound: FrameConfig,
    decrypt: Option<Cfb8Decrypt>,
    encrypt: Option<Cfb8Encrypt>,
    translation: Option<Translation>,
}

/// Version translation of a backend connection (E3b): frames read from it
/// come out in the client's version, frames queued to it go out in the
/// backend's, so the rest of the session works in the client's version only.
struct Translation {
    translator: Box<dyn PacketTranslator>,
    ready: VecDeque<RawFrame>,
    out: Translated,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn")
            .field("buffered", &self.rbuf.len())
            .field("encrypted", &self.encrypt.is_some())
            .finish_non_exhaustive()
    }
}

impl Conn {
    pub fn new(
        stream: Pin<Box<dyn Transport>>,
        inbound: FrameConfig,
        outbound: FrameConfig,
    ) -> Self {
        Self {
            stream,
            rbuf: BytesMut::with_capacity(READ_CHUNK),
            wbuf: BytesMut::new(),
            inbound,
            outbound,
            decrypt: None,
            encrypt: None,
            translation: None,
        }
    }

    /// Translates everything from now on (after the backend login).
    pub fn set_translator(&mut self, translator: Box<dyn PacketTranslator>) {
        self.translation = Some(Translation {
            translator,
            ready: VecDeque::new(),
            out: Translated::default(),
        });
    }

    pub fn translates(&self) -> bool {
        self.translation.is_some()
    }

    /// Reads what is available (at least one byte). Cancel-safe: if the
    /// future is dropped, nothing was read.
    pub async fn fill(&mut self) -> Result<usize, ConnError> {
        let start = self.rbuf.len();
        self.rbuf.reserve(READ_CHUNK);
        let n = self.stream.read_buf(&mut self.rbuf).await?;
        if n == 0 {
            return Err(ConnError::Closed);
        }
        if let (Some(dec), Some(fresh)) = (&mut self.decrypt, self.rbuf.get_mut(start..)) {
            dec.apply(fresh);
        }
        Ok(n)
    }

    /// Bytes received but not yet decoded.
    pub fn buffered(&self) -> &[u8] {
        &self.rbuf
    }

    /// Drops `n` buffered bytes (e.g. a PROXY header).
    pub fn consume(&mut self, n: usize) {
        let _ = self.rbuf.split_to(n.min(self.rbuf.len()));
    }

    /// The next complete frame in the buffer, with its body as received
    /// (empty for a translated frame, which has no body to pass on).
    pub fn next_frame(&mut self) -> Result<Option<(RawFrame, Bytes)>, FrameError> {
        let Self {
            translation,
            rbuf,
            wbuf,
            inbound,
            outbound,
            ..
        } = self;
        let Some(t) = translation else {
            return frame::decode_raw(rbuf, inbound);
        };
        loop {
            if let Some(f) = t.ready.pop_front() {
                return Ok(Some((f, Bytes::new())));
            }
            let Some((f, _)) = frame::decode_raw(rbuf, inbound)? else {
                return Ok(None);
            };
            t.translator.backend_to_client(&f, &mut t.out);
            t.ready.extend(t.out.to_client.drain(..));
            // Answers of the translator itself (e.g. accepting what the client cannot show).
            for a in t.out.to_backend.drain(..) {
                frame::encode(wbuf, a.id, &a.payload, outbound)?;
            }
        }
    }

    pub async fn read_frame(&mut self) -> Result<(RawFrame, Bytes), ConnError> {
        loop {
            if let Some(f) = self.next_frame()? {
                return Ok(f);
            }
            self.fill().await?;
        }
    }

    pub fn queue(&mut self, id: i32, payload: &[u8]) -> Result<(), FrameError> {
        let Some(t) = self.translation.as_mut() else {
            return frame::encode(&mut self.wbuf, id, payload, &self.outbound);
        };
        let f = RawFrame {
            id,
            payload: Bytes::copy_from_slice(payload),
        };
        t.translator.client_to_backend(&f, &mut t.out);
        // ponytail: translators answer the client only from backend frames, so
        // `to_client` is empty here; route it to the client if one ever needs to.
        t.out.to_client.clear();
        for a in t.out.to_backend.drain(..) {
            frame::encode(&mut self.wbuf, a.id, &a.payload, &self.outbound)?;
        }
        Ok(())
    }

    /// Queues a body from the other side unchanged (same compression threshold).
    pub fn queue_body(&mut self, body: &[u8]) -> Result<(), FrameError> {
        frame::encode_body(&mut self.wbuf, body)
    }

    pub async fn flush(&mut self) -> Result<(), ConnError> {
        if self.wbuf.is_empty() {
            return Ok(());
        }
        if let Some(enc) = &mut self.encrypt {
            enc.apply(&mut self.wbuf);
        }
        let out = self.wbuf.split();
        tokio::time::timeout(WRITE_TIMEOUT, self.stream.write_all(&out))
            .await
            .map_err(|_| ConnError::WriteTimeout)??;
        Ok(())
    }

    pub async fn send(&mut self, id: i32, payload: &[u8]) -> Result<(), ConnError> {
        self.queue(id, payload)?;
        self.flush().await
    }

    /// AES-CFB8 on both directions from now on. Call right after decoding the
    /// encryption response: bytes still buffered came after it on the wire,
    /// so they are already encrypted and get decrypted here.
    pub fn enable_encryption(&mut self, secret: &[u8]) -> Result<(), CryptoError> {
        let (enc, _) = cfb8_pair(secret)?;
        let (_, mut dec) = cfb8_pair(secret)?;
        dec.apply(&mut self.rbuf);
        self.encrypt = Some(enc);
        self.decrypt = Some(dec);
        Ok(())
    }

    pub fn set_compression(&mut self, threshold: i32) {
        self.inbound = self.inbound.with_threshold(threshold);
        self.outbound = self.outbound.with_threshold(threshold);
    }

    pub async fn write_raw(&mut self, data: &[u8]) -> Result<(), ConnError> {
        tokio::time::timeout(WRITE_TIMEOUT, self.stream.write_all(data))
            .await
            .map_err(|_| ConnError::WriteTimeout)??;
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let _ = self.flush().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), self.stream.shutdown()).await;
    }
}
