//! Recorded frames: a small binary file format for golden tests.
//!
//! `PUMBOREC1\n`, then per frame: phase (u8), direction (u8), VarInt ID,
//! VarInt payload length, payload. Recordings of vanilla servers are generated
//! locally and never committed (plan §2.5).

use std::path::Path;

use bytes::Bytes;
use pumbo_protocol::types::{Reader, WriteExt};
use pumbo_protocol::{Direction, Phase};

const MAGIC: &[u8] = b"PUMBOREC1\n";

/// One frame as it crossed the wire (after decompression and decryption).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub phase: Phase,
    pub direction: Direction,
    pub id: i32,
    pub payload: Bytes,
}

fn phase_byte(p: Phase) -> u8 {
    Phase::ALL.iter().position(|x| *x == p).unwrap_or(0) as u8
}

pub fn encode(frames: &[Recorded]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    for f in frames {
        out.put_u8(phase_byte(f.phase));
        out.put_u8(u8::from(f.direction == Direction::Serverbound));
        out.put_varint(f.id);
        out.put_varint(i32::try_from(f.payload.len()).unwrap_or(i32::MAX));
        out.extend_from_slice(&f.payload);
    }
    out
}

pub fn decode(data: &[u8]) -> Option<Vec<Recorded>> {
    let rest = data.strip_prefix(MAGIC)?;
    let mut r = Reader::new(rest);
    let mut out = Vec::new();
    while !r.is_empty() {
        let phase = *Phase::ALL.get(usize::from(r.u8().ok()?))?;
        let direction = if r.u8().ok()? == 1 {
            Direction::Serverbound
        } else {
            Direction::Clientbound
        };
        let id = r.varint().ok()?;
        let len = usize::try_from(r.varint().ok()?).ok()?;
        let payload = Bytes::copy_from_slice(r.take(len).ok()?);
        out.push(Recorded {
            phase,
            direction,
            id,
            payload,
        });
    }
    Some(out)
}

pub fn write(path: &Path, frames: &[Recorded]) -> std::io::Result<()> {
    std::fs::write(path, encode(frames))
}

pub fn read(path: &Path) -> std::io::Result<Vec<Recorded>> {
    let data = std::fs::read(path)?;
    decode(&data).ok_or_else(|| std::io::Error::other(format!("{}: bad recording", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let frames = vec![
            Recorded {
                phase: Phase::Login,
                direction: Direction::Clientbound,
                id: 2,
                payload: Bytes::from_static(b"abc"),
            },
            Recorded {
                phase: Phase::Play,
                direction: Direction::Serverbound,
                id: 300,
                payload: Bytes::new(),
            },
        ];
        assert_eq!(decode(&encode(&frames)), Some(frames));
        assert_eq!(decode(b"nope"), None);
    }
}
