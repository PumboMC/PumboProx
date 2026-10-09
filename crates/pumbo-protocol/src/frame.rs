//! Framing: VarInt length prefix, optional zlib compression (plan §2.2, §2.7).
//!
//! Decoding works on a growing buffer and returns a frame only when it is
//! complete. With compression on, the declared uncompressed length is checked
//! against the limit before anything is inflated, inflation stops at that
//! length, and frames that break the threshold rules are protocol errors.

use bytes::{Buf, Bytes, BytesMut};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use thiserror::Error;

use crate::RawFrame;
use crate::types::{DecodeError, Reader, WriteExt, varint_len};

/// Largest frame the length prefix allows (3-byte VarInt).
pub const MAX_FRAME: usize = 2_097_151;
/// Uncompressed limit for data from a client (plan §2.7).
pub const MAX_UNCOMPRESSED_FROM_CLIENT: usize = 2 * 1024 * 1024;
/// Uncompressed limit for data from a backend (as the vanilla client).
pub const MAX_UNCOMPRESSED_FROM_BACKEND: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame length prefix is not a valid 3-byte VarInt")]
    BadLength,
    #[error("frame of {got} bytes exceeds the limit of {max}")]
    TooLarge { got: usize, max: usize },
    #[error("declared uncompressed size {got} exceeds the limit of {max}")]
    UncompressedTooLarge { got: usize, max: usize },
    #[error("compressed frame declares {got} bytes, below the threshold {threshold}")]
    BelowThreshold { got: usize, threshold: usize },
    #[error("uncompressed frame of {got} bytes is above the threshold {threshold}")]
    AboveThreshold { got: usize, threshold: usize },
    #[error("compressed data does not match the declared size")]
    SizeMismatch,
    #[error("corrupt compressed data")]
    Corrupt,
    #[error("empty frame")]
    Empty,
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

/// Settings of one side of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameConfig {
    /// `None`: compression off. `Some(t)`: frames of `t` bytes or more are compressed.
    pub threshold: Option<usize>,
    /// Maximum frame length (after the prefix, before decompression).
    pub max_frame: usize,
    /// Maximum uncompressed length.
    pub max_uncompressed: usize,
    /// zlib level for encoding (0-9).
    pub level: u32,
    /// Reject uncompressed frames at or above the threshold (vanilla sends those
    /// compressed; a peer that does not is broken or hostile).
    pub strict: bool,
}

impl FrameConfig {
    pub const fn from_client() -> Self {
        Self {
            threshold: None,
            max_frame: MAX_FRAME,
            max_uncompressed: MAX_UNCOMPRESSED_FROM_CLIENT,
            level: 4,
            strict: true,
        }
    }

    pub const fn from_backend() -> Self {
        Self {
            threshold: None,
            max_frame: MAX_FRAME,
            max_uncompressed: MAX_UNCOMPRESSED_FROM_BACKEND,
            level: 4,
            strict: true,
        }
    }

    /// Compression threshold from `login_compression` (negative = off).
    pub fn with_threshold(mut self, threshold: i32) -> Self {
        self.threshold = usize::try_from(threshold).ok();
        self
    }
}

/// Reads the frame length prefix: `Ok(None)` if more bytes are needed.
fn peek_length(buf: &[u8]) -> Result<Option<(usize, usize)>, FrameError> {
    let mut value: u32 = 0;
    for i in 0..3 {
        let Some(&b) = buf.get(i) else {
            return Ok(None);
        };
        value |= u32::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(Some((value as usize, i + 1)));
        }
    }
    Err(FrameError::BadLength)
}

/// Takes one complete frame off the front of `buf`, if there is one.
pub fn decode(buf: &mut BytesMut, cfg: &FrameConfig) -> Result<Option<RawFrame>, FrameError> {
    Ok(decode_raw(buf, cfg)?.map(|(frame, _)| frame))
}

/// Like [`decode`], but also returns the frame body as received (after the
/// length prefix, still compressed). An unchanged frame can then go to a peer
/// with the same compression settings without being compressed again (§2.2).
pub fn decode_raw(
    buf: &mut BytesMut,
    cfg: &FrameConfig,
) -> Result<Option<(RawFrame, Bytes)>, FrameError> {
    let Some((len, prefix)) = peek_length(buf)? else {
        return Ok(None);
    };
    if len > cfg.max_frame {
        return Err(FrameError::TooLarge {
            got: len,
            max: cfg.max_frame,
        });
    }
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if buf.len() < prefix + len {
        return Ok(None);
    }
    buf.advance(prefix);
    let body = buf.split_to(len).freeze();
    let frame = decode_body(body.clone(), cfg)?;
    Ok(Some((frame, body)))
}

/// Appends a frame body taken from [`decode_raw`]: the length prefix and the
/// body unchanged. Only valid towards a peer with the same compression
/// threshold as the side the body came from.
pub fn encode_body(out: &mut BytesMut, body: &[u8]) -> Result<(), FrameError> {
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge {
            got: body.len(),
            max: MAX_FRAME,
        });
    }
    let mut prefix = Vec::with_capacity(3);
    prefix.put_varint(body.len() as i32);
    out.extend_from_slice(&prefix);
    out.extend_from_slice(body);
    Ok(())
}

/// Decodes a frame body (without the length prefix).
pub fn decode_body(body: Bytes, cfg: &FrameConfig) -> Result<RawFrame, FrameError> {
    let data = match cfg.threshold {
        None => body,
        Some(threshold) => {
            let mut r = Reader::new(&body);
            let declared = usize::try_from(r.varint()?).map_err(|_| FrameError::Corrupt)?;
            let header = body.len() - r.remaining();
            if declared == 0 {
                let raw = body.slice(header..);
                if cfg.strict && raw.len() >= threshold {
                    return Err(FrameError::AboveThreshold {
                        got: raw.len(),
                        threshold,
                    });
                }
                raw
            } else {
                if declared < threshold {
                    return Err(FrameError::BelowThreshold {
                        got: declared,
                        threshold,
                    });
                }
                if declared > cfg.max_uncompressed {
                    return Err(FrameError::UncompressedTooLarge {
                        got: declared,
                        max: cfg.max_uncompressed,
                    });
                }
                inflate(body.get(header..).unwrap_or_default(), declared)?.into()
            }
        }
    };
    let mut r = Reader::new(&data);
    let id = r.varint()?;
    let start = data.len() - r.remaining();
    Ok(RawFrame {
        id,
        payload: data.slice(start..),
    })
}

/// Inflates exactly `size` bytes; more or less is an error.
fn inflate(input: &[u8], size: usize) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::with_capacity(size);
    let mut d = Decompress::new(true);
    loop {
        let before_in = d.total_in();
        let before_out = d.total_out();
        let consumed = usize::try_from(before_in).map_err(|_| FrameError::Corrupt)?;
        let rest = input.get(consumed..).ok_or(FrameError::Corrupt)?;
        // `decompress_vec` never grows `out` past its capacity, so a stream that
        // inflates beyond `size` stops here.
        let status = d
            .decompress_vec(rest, &mut out, FlushDecompress::Finish)
            .map_err(|_| FrameError::Corrupt)?;
        match status {
            Status::StreamEnd => break,
            Status::Ok | Status::BufError => {
                if out.len() == out.capacity() {
                    // Full but the stream has not ended: it is larger than declared.
                    return Err(FrameError::SizeMismatch);
                }
                if d.total_in() == before_in && d.total_out() == before_out {
                    return Err(FrameError::Corrupt);
                }
            }
        }
    }
    let consumed = usize::try_from(d.total_in()).map_err(|_| FrameError::Corrupt)?;
    if out.len() != size || consumed != input.len() {
        return Err(FrameError::SizeMismatch);
    }
    Ok(out)
}

fn deflate(input: &[u8], level: u32) -> Result<Vec<u8>, FrameError> {
    let mut c = Compress::new(Compression::new(level.min(9)), true);
    let mut out = Vec::with_capacity(input.len() / 2 + 64);
    loop {
        let consumed = usize::try_from(c.total_in()).map_err(|_| FrameError::Corrupt)?;
        let rest = input.get(consumed..).ok_or(FrameError::Corrupt)?;
        if out.len() == out.capacity() {
            out.reserve(out.capacity().max(64));
        }
        match c
            .compress_vec(rest, &mut out, FlushCompress::Finish)
            .map_err(|_| FrameError::Corrupt)?
        {
            Status::StreamEnd => return Ok(out),
            Status::Ok | Status::BufError => {}
        }
    }
}

/// Appends a frame for `id` and `payload` to `out`.
pub fn encode(
    out: &mut BytesMut,
    id: i32,
    payload: &[u8],
    cfg: &FrameConfig,
) -> Result<(), FrameError> {
    let mut data = Vec::with_capacity(varint_len(id) + payload.len());
    data.put_varint(id);
    data.extend_from_slice(payload);
    encode_data(out, &data, cfg)
}

/// Appends a frame whose uncompressed data (ID and payload) is `data`.
pub fn encode_data(out: &mut BytesMut, data: &[u8], cfg: &FrameConfig) -> Result<(), FrameError> {
    let mut body = Vec::new();
    match cfg.threshold {
        None => body.extend_from_slice(data),
        Some(threshold) if data.len() < threshold => {
            body.put_varint(0);
            body.extend_from_slice(data);
        }
        Some(_) => {
            let len = i32::try_from(data.len()).map_err(|_| FrameError::TooLarge {
                got: data.len(),
                max: cfg.max_uncompressed,
            })?;
            body.put_varint(len);
            body.extend_from_slice(&deflate(data, cfg.level)?);
        }
    }
    encode_body(out, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn cfg(threshold: Option<usize>) -> FrameConfig {
        FrameConfig {
            threshold,
            ..FrameConfig::from_client()
        }
    }

    #[test]
    fn plain_and_compressed_round_trip() {
        for threshold in [None, Some(0), Some(64), Some(256)] {
            let c = cfg(threshold);
            let mut buf = BytesMut::new();
            encode(&mut buf, 0x05, b"short", &c).unwrap();
            encode(&mut buf, 0x7F, &[7u8; 1000], &c).unwrap();
            let a = decode(&mut buf, &c).unwrap().unwrap();
            let b = decode(&mut buf, &c).unwrap().unwrap();
            assert_eq!((a.id, a.payload.as_ref()), (5, &b"short"[..]));
            assert_eq!((b.id, b.payload.len()), (0x7F, 1000));
            assert!(buf.is_empty());
            assert_eq!(decode(&mut buf, &c), Ok(None));
        }
    }

    #[test]
    fn raw_body_passes_through_unchanged() {
        let c = cfg(Some(64));
        let mut buf = BytesMut::new();
        encode(&mut buf, 3, &[9u8; 500], &c).unwrap();
        let original = buf.clone();
        let (frame, body) = decode_raw(&mut buf, &c).unwrap().unwrap();
        assert_eq!((frame.id, frame.payload.len()), (3, 500));
        let mut out = BytesMut::new();
        encode_body(&mut out, &body).unwrap();
        assert_eq!(out, original);
    }

    #[test]
    fn partial_frames_wait_for_more_bytes() {
        let c = cfg(Some(16));
        let mut whole = BytesMut::new();
        encode(&mut whole, 1, &[1u8; 300], &c).unwrap();
        let mut buf = BytesMut::new();
        for (i, b) in whole.iter().enumerate() {
            buf.extend_from_slice(&[*b]);
            let r = decode(&mut buf, &c).unwrap();
            assert_eq!(r.is_some(), i + 1 == whole.len());
        }
    }

    #[test]
    fn compression_bomb_rejected_without_allocation() {
        // A frame that declares 2 GiB uncompressed: rejected from the header.
        let c = cfg(Some(256));
        let mut body = Vec::new();
        body.put_varint(i32::MAX);
        body.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00]);
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&[body.len() as u8]);
        buf.extend_from_slice(&body);
        assert_eq!(
            decode(&mut buf, &c),
            Err(FrameError::UncompressedTooLarge {
                got: i32::MAX as usize,
                max: MAX_UNCOMPRESSED_FROM_CLIENT
            })
        );
        // Declares 1000 bytes but inflates to 1 MiB: stops at 1000.
        let big = deflate(&vec![0u8; 1 << 20], 9).unwrap();
        let mut body = Vec::new();
        body.put_varint(1000);
        body.extend_from_slice(&big);
        assert_eq!(
            decode_body(Bytes::from(body), &c),
            Err(FrameError::SizeMismatch)
        );
    }

    #[test]
    fn threshold_rules() {
        let c = cfg(Some(256));
        // Compressed but declares less than the threshold.
        let mut body = Vec::new();
        body.put_varint(10);
        body.extend_from_slice(&deflate(&[0u8; 10], 6).unwrap());
        assert_eq!(
            decode_body(Bytes::from(body), &c),
            Err(FrameError::BelowThreshold {
                got: 10,
                threshold: 256
            })
        );
        // Uncompressed (declared 0) but above the threshold.
        let mut body = vec![0u8];
        body.extend_from_slice(&[1u8; 300]);
        assert!(matches!(
            decode_body(Bytes::from(body.clone()), &c),
            Err(FrameError::AboveThreshold { .. })
        ));
        let lenient = FrameConfig { strict: false, ..c };
        assert!(decode_body(Bytes::from(body), &lenient).is_ok());
    }

    #[test]
    fn length_limits() {
        let c = cfg(None);
        let mut buf = BytesMut::from(&[0xFF, 0xFF, 0xFF, 0x01][..]);
        assert_eq!(decode(&mut buf, &c), Err(FrameError::BadLength));
        let small = FrameConfig { max_frame: 10, ..c };
        let mut buf = BytesMut::new();
        encode(&mut buf, 0, &[0u8; 20], &c).unwrap();
        assert!(matches!(
            decode(&mut buf, &small),
            Err(FrameError::TooLarge { .. })
        ));
        let mut buf = BytesMut::from(&[0u8][..]);
        assert_eq!(decode(&mut buf, &c), Err(FrameError::Empty));
    }

    proptest! {
        #[test]
        fn round_trip(id in 0i32..300, payload in proptest::collection::vec(any::<u8>(), 0..2000),
                      threshold in proptest::option::of(0usize..512)) {
            let c = cfg(threshold);
            let mut buf = BytesMut::new();
            encode(&mut buf, id, &payload, &c).unwrap();
            let f = decode(&mut buf, &c).unwrap().unwrap();
            prop_assert_eq!(f.id, id);
            prop_assert_eq!(f.payload.as_ref(), payload.as_slice());
        }

        #[test]
        fn garbage_never_panics(data in proptest::collection::vec(any::<u8>(), 0..512),
                                threshold in proptest::option::of(0usize..64)) {
            let c = cfg(threshold);
            let mut buf = BytesMut::from(data.as_slice());
            while let Ok(Some(_)) = decode(&mut buf, &c) {}
        }
    }
}
