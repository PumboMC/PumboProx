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
use pumbo_core::translate::{Dropped, PacketTranslator, Translated};
use pumbo_protocol::RawFrame;
use pumbo_protocol::crypto::{Cfb8Decrypt, Cfb8Encrypt, CryptoError, cfb8_pair};
use pumbo_protocol::frame::{self, FrameConfig, FrameError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

/// A write that takes longer than this means the peer stopped reading.
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a closing connection waits for the peer to close its side. A
/// client that got its kick closes at once; this only bounds one that does
/// not (under the stop's five-second wait for sessions).
const LINGER: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 32 * 1024;
/// A read buffer that grew over this (with the chunks of a join, say) gets
/// a fresh allocation once it is empty and the burst is over, so the burst
/// does not stay in memory. Two reads' worth: it grows past that only
/// around big frames.
const SHRINK_READ_ABOVE: usize = 2 * READ_CHUNK;
/// The same for the write buffers, once all they held is written.
const SHRINK_WRITE_ABOVE: usize = 256 * 1024;

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
    /// `rbuf` grew over [`SHRINK_READ_ABOVE`], `wbuf` and `obuf` over
    /// [`SHRINK_WRITE_ABOVE`].
    read_burst: bool,
    write_burst: bool,
    /// The last read took less than [`READ_CHUNK`]: the peer sent all it had.
    read_quiet: bool,
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
    on_drop: OnDrop,
}

/// Told of every packet the translator dropped.
pub type OnDrop = Box<dyn FnMut(Dropped) + Send>;

impl Translation {
    fn report_drops(&mut self) {
        for d in self.out.dropped.drain(..) {
            (self.on_drop)(d);
        }
    }
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
            read_burst: false,
            write_burst: false,
            read_quiet: false,
            inbound,
            outbound,
            decrypt: None,
            encrypt: None,
            translation: None,
        }
    }

    /// Translates everything from now on (after the backend login).
    pub fn set_translator(&mut self, translator: Box<dyn PacketTranslator>, on_drop: OnDrop) {
        self.translation = Some(Translation {
            translator,
            ready: VecDeque::new(),
            out: Translated::default(),
            on_drop,
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
    /// Without `read` it only writes: it resolves on an error or `room`.
    /// Cancel-safe like `fill`: written bytes are gone from the queue at once.
    pub async fn fill_writing(
        &mut self,
        room: Option<usize>,
        read: bool,
    ) -> Result<Option<usize>, ConnError> {
        poll_fn(|cx| {
            if let Poll::Ready(Err(e)) = self.poll_write_queued(cx) {
                return Poll::Ready(Err(e));
            }
            if room.is_some_and(|r| self.pending() < r) {
                return Poll::Ready(Ok(None));
            }
            if !read {
                return Poll::Pending;
            }
            self.poll_fill(cx).map_ok(Some)
        })
        .await
    }

    fn poll_fill(&mut self, cx: &mut Context<'_>) -> Poll<Result<usize, ConnError>> {
        // Empty, so no frame is half-read; quiet, so a burst does not get a
        // small buffer to grow again for each of its frames.
        if self.read_burst && self.read_quiet && self.rbuf.is_empty() {
            self.rbuf = BytesMut::new();
            self.read_burst = false;
        }
        let start = self.rbuf.len();
        self.rbuf.reserve(READ_CHUNK);
        // Frames are taken off the front, so the length stays small while
        // the allocation behind it grows.
        self.read_burst |= self.rbuf.capacity() > SHRINK_READ_ABOVE;
        let n = ready!(pin!(self.stream.read_buf(&mut self.rbuf)).poll(cx))?;
        if n == 0 {
            return Poll::Ready(Err(ConnError::Closed));
        }
        self.read_quiet = n < READ_CHUNK;
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
        // Everything queued passes here, so this sees the most that waited.
        self.write_burst |= self.pending() > SHRINK_WRITE_ABOVE;
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
        if self.write_burst {
            // Both views share the allocation the queue grew into.
            self.wbuf = BytesMut::new();
            self.obuf = BytesMut::new();
            self.write_burst = false;
        }
        Poll::Ready(Ok(()))
    }

    /// Writes what the socket takes now, without waiting for the rest.
    pub fn try_write(&mut self) -> Result<(), ConnError> {
        match self.poll_write_queued(&mut Context::from_waker(std::task::Waker::noop())) {
            Poll::Ready(Err(e)) => Err(e),
            _ => Ok(()),
        }
    }

    /// Bytes received but not yet decoded.
    pub fn buffered(&self) -> &[u8] {
        &self.rbuf
    }

    /// Drops `n` buffered bytes (e.g. a PROXY header).
    pub fn consume(&mut self, n: usize) {
        let _ = self.rbuf.split_to(n.min(self.rbuf.len()));
    }

    /// Decoded size of the next complete frame, before it is inflated.
    pub fn next_size(&self) -> Option<usize> {
        frame::decoded_size(&self.rbuf, &self.inbound)
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
            t.report_drops();
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
        t.report_drops();
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

    /// Sends what is buffered, ends the write side, then reads and drops what
    /// the peer still sends until it closes or [`LINGER`] passes. A socket
    /// closed with unread data sends a reset, and on a reset Windows throws
    /// away what the peer has not read yet: the kick reason would be lost.
    pub async fn shutdown(&mut self) {
        let _ = self.flush().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), self.stream.shutdown()).await;
        let mut sink = [0u8; 8192];
        let _ = tokio::time::timeout(LINGER, async {
            while matches!(self.stream.read(&mut sink).await, Ok(n) if n > 0) {}
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes for a moment, as a session waiting for the client's packets does.
    async fn write_a_moment(c: &mut Conn) {
        let _ = tokio::time::timeout(Duration::from_millis(50), c.fill_writing(None, true)).await;
    }

    /// Writes until the peer has taken `n` bytes, then a moment more. On a
    /// machine that stops the thread for longer than one moment the peer is
    /// still fed, so its read cannot wait for bytes nobody writes.
    async fn write_while_taking(c: &mut Conn, peer: &mut tokio::io::DuplexStream, n: usize) {
        let mut took = vec![0; n];
        let read = peer.read_exact(&mut took);
        tokio::pin!(read);
        loop {
            tokio::select! {
                r = &mut read => {
                    r.unwrap();
                    break;
                }
                () = write_a_moment(c) => {}
            }
        }
        write_a_moment(c).await;
    }

    /// Closing reads what the peer still sends, so the socket never closes
    /// with unread data (a reset, which loses the kick on Windows).
    #[tokio::test]
    async fn shutdown_reads_what_the_peer_still_sends() {
        let (a, mut peer) = tokio::io::duplex(1024);
        let mut c = Conn::new(
            Box::pin(a),
            FrameConfig::from_client(),
            FrameConfig::from_backend(),
        );
        // More than the pipe holds: the peer finishes only if someone reads.
        let sender = tokio::spawn(async move { peer.write_all(&[7; 64 * 1024]).await });
        c.shutdown().await;
        let sent = tokio::time::timeout(Duration::from_secs(1), sender).await;
        assert!(
            matches!(sent, Ok(Ok(Ok(())))),
            "the peer's data was not read"
        );
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
        write_while_taking(&mut c, &mut peer, 8 * 1024).await;
        assert_eq!(c.stalled_since(), Some(since));
        write_while_taking(&mut c, &mut peer, 40 * 1024).await;
        let since = c.stalled_since().unwrap();
        // 30 s on: the next write gives up.
        c.stalled = Some((since.checked_sub(WRITE_TIMEOUT).unwrap(), 0));
        let r = c.fill_writing(None, false).await;
        assert!(matches!(r, Err(ConnError::WriteTimeout)), "{r:?}");
        assert_eq!((c.pending(), c.stalled_since()), (0, None));
        // Everything taken: no stall.
        c.queue(0, &[7; 10_000]).unwrap();
        tokio::spawn(async move { tokio::io::copy(&mut peer, &mut tokio::io::sink()).await });
        c.flush().await.unwrap();
        assert_eq!((c.pending(), c.stalled_since()), (0, None));
    }

    /// Over [`SHRINK_WRITE_ABOVE`].
    const WRITE_BURST: usize = 300_000;
    /// Over [`SHRINK_READ_ABOVE`], under [`SHRINK_WRITE_ABOVE`].
    const READ_BURST: usize = 200_000;

    /// After a burst, empty buffers let go of the memory it took.
    #[tokio::test]
    async fn buffers_shrink_after_a_burst() {
        let (a, mut peer) = tokio::io::duplex(64 * 1024);
        let mut c = Conn::new(
            Box::pin(a),
            FrameConfig::from_client(),
            FrameConfig::from_client(),
        );
        // Out: a burst, all taken by the peer.
        c.queue(0, &[7; WRITE_BURST]).unwrap();
        let burst = c.wbuf.as_ptr_range();
        let (r, _) = tokio::join!(c.flush(), async {
            let mut took = vec![0; c_len(WRITE_BURST)];
            peer.read_exact(&mut took).await.unwrap();
        });
        r.unwrap();
        let kept = |p: *const u8| p >= burst.start && p <= burst.end;
        assert!(!kept(c.wbuf.as_ptr()) && !kept(c.obuf.as_ptr()));
        // In: a big frame, then a small one.
        let mut big = BytesMut::new();
        frame::encode(&mut big, 0, &[7; READ_BURST], &FrameConfig::from_client()).unwrap();
        frame::encode(&mut big, 0, &[7; 10], &FrameConfig::from_client()).unwrap();
        let (r, ()) = tokio::join!(peer.write_all(&big), async {
            let (f, _) = c.read_frame().await.unwrap();
            assert_eq!(f.payload.len(), READ_BURST);
            let (f, _) = c.read_frame().await.unwrap();
            assert_eq!(f.payload.len(), 10);
        });
        r.unwrap();
        // Then a quiet read: the next one starts in a fresh buffer.
        let mut small = BytesMut::new();
        frame::encode(&mut small, 0, &[7; 10], &FrameConfig::from_client()).unwrap();
        let (r, f) = tokio::join!(peer.write_all(&small), c.read_frame());
        r.unwrap();
        assert_eq!(f.unwrap().0.payload.len(), 10);
        let _ = tokio::time::timeout(Duration::from_millis(20), c.fill()).await;
        assert!(c.rbuf.capacity() <= READ_CHUNK, "{}", c.rbuf.capacity());
    }

    /// Length of a frame with an uncompressed body of `n` bytes and id 0.
    fn c_len(n: usize) -> usize {
        let mut f = BytesMut::new();
        frame::encode(&mut f, 0, &vec![0; n], &FrameConfig::from_client()).unwrap();
        f.len()
    }
}
