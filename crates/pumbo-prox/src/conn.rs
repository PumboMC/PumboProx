//! One side of a session: a byte stream with framing, compression and
//! encryption. Reads are cancel-safe, so a session can wait on both sides in
//! one `select!`.

use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use pumbo_core::listener::Transport;
use pumbo_core::translate::{PacketTranslator, Translated};
use pumbo_protocol::RawFrame;
use pumbo_protocol::crypto::{Cfb8Decrypt, Cfb8Encrypt, CryptoError, cfb8_pair};
use pumbo_protocol::frame::{self, FrameConfig, FrameError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

/// A write that takes longer than this means the peer stopped reading.
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
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
    /// Queued frames, not encrypted yet.
    wbuf: BytesMut,
    /// Encrypted bytes the socket has not taken yet.
    obuf: BytesMut,
    /// Since when bytes wait for the peer, and how many it took meanwhile.
    stalled: Option<(Instant, usize)>,
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
            obuf: BytesMut::new(),
            stalled: None,
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
        poll_fn(|cx| self.poll_fill(cx)).await
    }

    /// [`Conn::fill`] that meanwhile writes the queued bytes as the peer takes
    /// them, so a peer that reads slowly does not stop the reading. With
    /// `room`, it also resolves (`None`) once fewer bytes than that wait.
    /// Cancel-safe like `fill`: written bytes are gone from the queue at once.
    pub async fn fill_writing(&mut self, room: Option<usize>) -> Result<Option<usize>, ConnError> {
        poll_fn(|cx| {
            if let Poll::Ready(Err(e)) = self.poll_write_queued(cx) {
                return Poll::Ready(Err(e));
            }
            if room.is_some_and(|r| self.pending() < r) {
                return Poll::Ready(Ok(None));
            }
            self.poll_fill(cx).map_ok(Some)
        })
        .await
    }

    fn poll_fill(&mut self, cx: &mut Context<'_>) -> Poll<Result<usize, ConnError>> {
        let start = self.rbuf.len();
        self.rbuf.reserve(READ_CHUNK);
        let n = ready!(pin!(self.stream.read_buf(&mut self.rbuf)).poll(cx))?;
        if n == 0 {
            return Poll::Ready(Err(ConnError::Closed));
        }
        if let (Some(dec), Some(fresh)) = (&mut self.decrypt, self.rbuf.get_mut(start..)) {
            dec.apply(fresh);
        }
        Poll::Ready(Ok(n))
    }

    /// Bytes queued that the socket has not taken yet.
    pub fn pending(&self) -> usize {
        self.wbuf.len() + self.obuf.len()
    }

    /// Since when the peer has taken less than one read's worth (32 KiB)
    /// while bytes wait for it. [`WRITE_TIMEOUT`] later it counts as no
    /// longer reading (as a blocking write of one read found out before):
    /// the next write fails with [`ConnError::WriteTimeout`].
    pub fn stalled_since(&self) -> Option<Instant> {
        self.stalled.map(|(since, _)| since)
    }

    /// Writes queued bytes until none are left (`Ready`) or the socket takes
    /// no more (`Pending`). Each byte is encrypted once, when it leaves `wbuf`.
    fn poll_write_queued(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), ConnError>> {
        if self
            .stalled
            .is_some_and(|(since, _)| since.elapsed() >= WRITE_TIMEOUT)
        {
            // What waits is dropped, like the buffer of a write that timed out.
            self.wbuf.clear();
            self.obuf.clear();
            self.stalled = None;
            return Poll::Ready(Err(ConnError::WriteTimeout));
        }
        if !self.wbuf.is_empty() {
            let mut fresh = self.wbuf.split();
            if let Some(enc) = &mut self.encrypt {
                enc.apply(&mut fresh);
            }
            self.obuf.unsplit(fresh);
        }
        while !self.obuf.is_empty() {
            match self.stream.as_mut().poll_write(cx, &self.obuf) {
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(
                        std::io::Error::from(std::io::ErrorKind::WriteZero).into()
                    ));
                }
                Poll::Ready(Ok(n)) => {
                    self.obuf.advance(n);
                    match &mut self.stalled {
                        Some((_, took)) if *took + n < READ_CHUNK => *took += n,
                        s => *s = None,
                    }
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
                Poll::Pending => {
                    self.stalled.get_or_insert_with(|| (Instant::now(), 0));
                    return Poll::Pending;
                }
            }
        }
        self.stalled = None;
        Poll::Ready(Ok(()))
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
        // Translators answer the client only from backend frames, so
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
        if self.pending() == 0 {
            return Ok(());
        }
        tokio::time::timeout(WRITE_TIMEOUT, poll_fn(|cx| self.poll_write_queued(cx)))
            .await
            .map_err(|_| ConnError::WriteTimeout)?
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes for a moment, as a session waiting for the client's packets does.
    async fn write_a_moment(c: &mut Conn) {
        let _ = tokio::time::timeout(Duration::from_millis(50), c.fill_writing(None)).await;
    }

    /// A peer that takes less than one read's worth stays stalled since the
    /// first wait; one that takes more is reading; one stalled for the write
    /// timeout gets an error and its bytes are dropped.
    #[tokio::test]
    async fn a_stall_ends_with_a_read_worth_or_the_timeout() {
        let (a, mut peer) = tokio::io::duplex(1024);
        let mut c = Conn::new(
            Box::pin(a),
            FrameConfig::from_client(),
            FrameConfig::from_backend(),
        );
        c.queue(0, &[7; 100_000]).unwrap();
        write_a_moment(&mut c).await;
        let since = c.stalled_since().unwrap();
        let mut took = vec![0; 8 * 1024];
        let (_, r) = tokio::join!(write_a_moment(&mut c), peer.read_exact(&mut took));
        r.unwrap();
        assert_eq!(c.stalled_since(), Some(since));
        let mut took = vec![0; 40 * 1024];
        let (_, r) = tokio::join!(write_a_moment(&mut c), peer.read_exact(&mut took));
        r.unwrap();
        let since = c.stalled_since().unwrap();
        // 30 s on: the next write gives up.
        c.stalled = Some((since.checked_sub(WRITE_TIMEOUT).unwrap(), 0));
        let r = c.fill_writing(None).await;
        assert!(matches!(r, Err(ConnError::WriteTimeout)), "{r:?}");
        assert_eq!((c.pending(), c.stalled_since()), (0, None));
        // Everything taken: no stall.
        c.queue(0, &[7; 10_000]).unwrap();
        tokio::spawn(async move { tokio::io::copy(&mut peer, &mut tokio::io::sink()).await });
        c.flush().await.unwrap();
        assert_eq!((c.pending(), c.stalled_since()), (0, None));
    }
}
