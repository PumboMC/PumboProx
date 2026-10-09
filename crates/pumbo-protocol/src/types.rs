//! Wire types: reading from a byte slice and writing to a `Vec<u8>`.
//!
//! Every length read from the wire is checked against an explicit maximum and
//! against the bytes actually left before anything is allocated.

use pumbo_nbt::{Limits as NbtLimits, NbtError, Tag};
use thiserror::Error;
use uuid::Uuid;

/// Longest string vanilla accepts by default (characters).
pub const MAX_STRING: usize = 32767;
/// Longest identifier.
pub const MAX_IDENTIFIER: usize = 32767;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("packet ends early")]
    Eof,
    #[error("VarInt longer than 5 bytes")]
    VarIntTooLong,
    #[error("VarLong longer than 10 bytes")]
    VarLongTooLong,
    #[error("negative length")]
    NegativeLength,
    #[error("string longer than {0} characters")]
    StringTooLong(usize),
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
    #[error("{what}: {got} exceeds the limit of {max}")]
    TooLong {
        what: &'static str,
        got: usize,
        max: usize,
    },
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("{0} bytes left after the packet")]
    TrailingBytes(usize),
    #[error("NBT: {0}")]
    Nbt(#[from] NbtError),
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum EncodeError {
    #[error("{what}: {got} exceeds the limit of {max}")]
    TooLong {
        what: &'static str,
        got: usize,
        max: usize,
    },
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("NBT: {0}")]
    Nbt(#[from] NbtError),
}

/// Number of UTF-16 code units, which is how Java measures string length.
pub fn java_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Reader over one packet's bytes.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Fails unless everything was read (vanilla rejects packets with extra bytes).
    pub fn finish(&self) -> Result<(), DecodeError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes(self.buf.len()))
        }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if n > self.buf.len() {
            return Err(DecodeError::Eof);
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let b = self.take(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(b);
        Ok(out)
    }

    /// Everything that is left.
    pub fn rest(&mut self) -> &'a [u8] {
        let all = self.buf;
        self.buf = &[];
        all
    }

    /// Everything that is left, at most `max` bytes.
    pub fn rest_max(&mut self, max: usize, what: &'static str) -> Result<&'a [u8], DecodeError> {
        if self.buf.len() > max {
            return Err(DecodeError::TooLong {
                what,
                got: self.buf.len(),
                max,
            });
        }
        Ok(self.rest())
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(u8::from_be_bytes(self.array()?))
    }

    pub fn i8(&mut self) -> Result<i8, DecodeError> {
        Ok(i8::from_be_bytes(self.array()?))
    }

    /// Any non-zero byte is `true`, as in vanilla.
    pub fn bool(&mut self) -> Result<bool, DecodeError> {
        Ok(self.u8()? != 0)
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub fn i16(&mut self) -> Result<i16, DecodeError> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_be_bytes(self.array()?))
    }

    pub fn f64(&mut self) -> Result<f64, DecodeError> {
        Ok(f64::from_be_bytes(self.array()?))
    }

    pub fn varint(&mut self) -> Result<i32, DecodeError> {
        let mut value: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            value |= u32::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(DecodeError::VarIntTooLong)
    }

    pub fn varlong(&mut self) -> Result<i64, DecodeError> {
        let mut value: u64 = 0;
        for i in 0..10 {
            let b = self.u8()?;
            value |= u64::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(value as i64);
            }
        }
        Err(DecodeError::VarLongTooLong)
    }

    /// A VarInt length or count, at most `max`.
    pub fn len(&mut self, max: usize, what: &'static str) -> Result<usize, DecodeError> {
        let n = usize::try_from(self.varint()?).map_err(|_| DecodeError::NegativeLength)?;
        if n > max {
            return Err(DecodeError::TooLong { what, got: n, max });
        }
        Ok(n)
    }

    /// A VarInt element count; each element takes at least `min_elem` bytes, so
    /// a count larger than the rest of the packet can fill is rejected before
    /// any allocation.
    pub fn count(
        &mut self,
        max: usize,
        min_elem: usize,
        what: &'static str,
    ) -> Result<usize, DecodeError> {
        let n = self.len(max, what)?;
        if n.saturating_mul(min_elem) > self.buf.len() {
            return Err(DecodeError::Eof);
        }
        Ok(n)
    }

    /// String with a maximum length in characters (UTF-16 units, as Java).
    pub fn string(&mut self, max_chars: usize) -> Result<String, DecodeError> {
        let n = self.len(max_chars.saturating_mul(3), "string bytes")?;
        let bytes = self.take(n)?;
        let s = std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?;
        if java_len(s) > max_chars {
            return Err(DecodeError::StringTooLong(max_chars));
        }
        Ok(s.to_string())
    }

    pub fn identifier(&mut self) -> Result<String, DecodeError> {
        self.string(MAX_IDENTIFIER)
    }

    /// VarInt-prefixed byte array.
    pub fn byte_array(&mut self, max: usize) -> Result<&'a [u8], DecodeError> {
        let n = self.len(max, "byte array")?;
        self.take(n)
    }

    pub fn uuid(&mut self) -> Result<Uuid, DecodeError> {
        Ok(Uuid::from_bytes(self.array()?))
    }

    /// Block position packed into a long: x 26 bits, z 26 bits, y 12 bits.
    pub fn position(&mut self) -> Result<Position, DecodeError> {
        Ok(Position::from_packed(self.i64()?))
    }

    /// BitSet as a VarInt count of longs and the longs.
    pub fn bitset(&mut self, max_longs: usize) -> Result<Vec<i64>, DecodeError> {
        let n = self.count(max_longs, 8, "bitset")?;
        (0..n).map(|_| self.i64()).collect()
    }

    /// Fixed BitSet of `bits` bits (no prefix), `ceil(bits / 8)` bytes.
    pub fn fixed_bitset(&mut self, bits: usize) -> Result<&'a [u8], DecodeError> {
        self.take(bits.div_ceil(8))
    }

    /// Network NBT; `None` for the `End` type.
    pub fn nbt(&mut self, limits: NbtLimits) -> Result<Option<Tag>, DecodeError> {
        let mut slice = self.buf;
        let tag = pumbo_nbt::read_network(&mut slice, limits)?;
        self.buf = slice;
        Ok(tag)
    }

    /// Network NBT that must be present.
    pub fn nbt_required(&mut self, limits: NbtLimits) -> Result<Tag, DecodeError> {
        self.nbt(limits)?.ok_or(DecodeError::Invalid("empty NBT"))
    }

    /// Value behind a boolean "present" prefix.
    pub fn option<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, DecodeError>,
    ) -> Result<Option<T>, DecodeError> {
        if self.bool()? {
            f(self).map(Some)
        } else {
            Ok(None)
        }
    }
}

/// Block position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Position {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Position {
    pub fn from_packed(v: i64) -> Self {
        Self {
            x: (v >> 38) as i32,
            y: ((v << 52) >> 52) as i32,
            z: ((v << 26) >> 38) as i32,
        }
    }

    pub fn packed(&self) -> i64 {
        ((i64::from(self.x) & 0x3FF_FFFF) << 38)
            | ((i64::from(self.z) & 0x3FF_FFFF) << 12)
            | (i64::from(self.y) & 0xFFF)
    }
}

/// Writing helpers on `Vec<u8>`.
pub trait WriteExt {
    fn put_u8(&mut self, v: u8);
    fn put_i8(&mut self, v: i8);
    fn put_bool(&mut self, v: bool);
    fn put_u16(&mut self, v: u16);
    fn put_i16(&mut self, v: i16);
    fn put_i32(&mut self, v: i32);
    fn put_i64(&mut self, v: i64);
    fn put_f32(&mut self, v: f32);
    fn put_f64(&mut self, v: f64);
    fn put_varint(&mut self, v: i32);
    fn put_varlong(&mut self, v: i64);
    fn put_uuid(&mut self, v: &Uuid);
    fn put_position(&mut self, v: Position);
    fn put_bytes(&mut self, v: &[u8]);
    /// VarInt length or count; fails above `max` or `i32::MAX`.
    fn put_len(&mut self, n: usize, max: usize, what: &'static str) -> Result<(), EncodeError>;
    fn put_string(&mut self, s: &str, max_chars: usize) -> Result<(), EncodeError>;
    fn put_identifier(&mut self, s: &str) -> Result<(), EncodeError>;
    fn put_byte_array(&mut self, v: &[u8], max: usize) -> Result<(), EncodeError>;
    fn put_bitset(&mut self, longs: &[i64], max_longs: usize) -> Result<(), EncodeError>;
    fn put_nbt(&mut self, tag: Option<&Tag>) -> Result<(), EncodeError>;
}

impl WriteExt for Vec<u8> {
    fn put_u8(&mut self, v: u8) {
        self.push(v);
    }
    fn put_i8(&mut self, v: i8) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_bool(&mut self, v: bool) {
        self.push(u8::from(v));
    }
    fn put_u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_i16(&mut self, v: i16) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_i32(&mut self, v: i32) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_i64(&mut self, v: i64) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_f32(&mut self, v: f32) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_f64(&mut self, v: f64) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_varint(&mut self, v: i32) {
        let mut v = v as u32;
        loop {
            let byte = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                self.push(byte);
                return;
            }
            self.push(byte | 0x80);
        }
    }
    fn put_varlong(&mut self, v: i64) {
        let mut v = v as u64;
        loop {
            let byte = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                self.push(byte);
                return;
            }
            self.push(byte | 0x80);
        }
    }
    fn put_uuid(&mut self, v: &Uuid) {
        self.extend_from_slice(v.as_bytes());
    }
    fn put_position(&mut self, v: Position) {
        self.put_i64(v.packed());
    }
    fn put_bytes(&mut self, v: &[u8]) {
        self.extend_from_slice(v);
    }
    fn put_len(&mut self, n: usize, max: usize, what: &'static str) -> Result<(), EncodeError> {
        let max = max.min(i32::MAX as usize);
        if n > max {
            return Err(EncodeError::TooLong { what, got: n, max });
        }
        self.put_varint(n as i32);
        Ok(())
    }
    fn put_string(&mut self, s: &str, max_chars: usize) -> Result<(), EncodeError> {
        let chars = java_len(s);
        if chars > max_chars {
            return Err(EncodeError::TooLong {
                what: "string",
                got: chars,
                max: max_chars,
            });
        }
        self.put_len(s.len(), usize::MAX, "string bytes")?;
        self.extend_from_slice(s.as_bytes());
        Ok(())
    }
    fn put_identifier(&mut self, s: &str) -> Result<(), EncodeError> {
        self.put_string(s, MAX_IDENTIFIER)
    }
    fn put_byte_array(&mut self, v: &[u8], max: usize) -> Result<(), EncodeError> {
        self.put_len(v.len(), max, "byte array")?;
        self.extend_from_slice(v);
        Ok(())
    }
    fn put_bitset(&mut self, longs: &[i64], max_longs: usize) -> Result<(), EncodeError> {
        self.put_len(longs.len(), max_longs, "bitset")?;
        for l in longs {
            self.put_i64(*l);
        }
        Ok(())
    }
    fn put_nbt(&mut self, tag: Option<&Tag>) -> Result<(), EncodeError> {
        pumbo_nbt::write_network(self, tag)?;
        Ok(())
    }
}

/// Length of a VarInt in bytes.
pub fn varint_len(v: i32) -> usize {
    match v as u32 {
        0..=0x7F => 1,
        0x80..=0x3FFF => 2,
        0x4000..=0x1F_FFFF => 3,
        0x20_0000..=0xFFF_FFFF => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn varint_and_varlong_vectors() {
        // Vectors from the protocol description (VarInt and VarLong page).
        for (v, bytes) in [
            (0, vec![0x00]),
            (1, vec![0x01]),
            (127, vec![0x7F]),
            (128, vec![0x80, 0x01]),
            (255, vec![0xFF, 0x01]),
            (25565, vec![0xDD, 0xC7, 0x01]),
            (2_097_151, vec![0xFF, 0xFF, 0x7F]),
            (i32::MAX, vec![0xFF, 0xFF, 0xFF, 0xFF, 0x07]),
            (-1, vec![0xFF, 0xFF, 0xFF, 0xFF, 0x0F]),
            (i32::MIN, vec![0x80, 0x80, 0x80, 0x80, 0x08]),
        ] {
            let mut out = Vec::new();
            out.put_varint(v);
            assert_eq!(out, bytes, "{v}");
            assert_eq!(varint_len(v), bytes.len());
            assert_eq!(Reader::new(&bytes).varint(), Ok(v));
        }
        for (v, bytes) in [
            (0i64, vec![0x00]),
            (2_147_483_647, vec![0xFF, 0xFF, 0xFF, 0xFF, 0x07]),
            (
                i64::MAX,
                vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F],
            ),
            (
                -1,
                vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01],
            ),
            (
                i64::MIN,
                vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
            ),
        ] {
            let mut out = Vec::new();
            out.put_varlong(v);
            assert_eq!(out, bytes, "{v}");
            assert_eq!(Reader::new(&bytes).varlong(), Ok(v));
        }
        assert_eq!(
            Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x01]).varint(),
            Err(DecodeError::VarIntTooLong)
        );
        assert_eq!(Reader::new(&[0x80]).varint(), Err(DecodeError::Eof));
    }

    #[test]
    fn strings_are_limited_in_java_characters() {
        let mut out = Vec::new();
        out.put_string("\u{1F600}", 2).unwrap(); // two UTF-16 units
        assert_eq!(Reader::new(&out).string(2).unwrap(), "\u{1F600}");
        // 4 bytes is more than 3 bytes per allowed character: rejected early.
        assert!(matches!(
            Reader::new(&out).string(1),
            Err(DecodeError::TooLong { .. })
        ));
        let mut two = Vec::new();
        two.put_string("ab", 2).unwrap();
        assert_eq!(
            Reader::new(&two).string(1),
            Err(DecodeError::StringTooLong(1))
        );
        assert!(Vec::new().put_string("abc", 2).is_err());
        // Declared length larger than the limit: rejected before reading.
        let mut bad = Vec::new();
        bad.put_varint(1_000_000);
        assert!(matches!(
            Reader::new(&bad).string(16),
            Err(DecodeError::TooLong { .. })
        ));
        assert_eq!(
            Reader::new(&[2, 0xC3, 0x28]).string(16),
            Err(DecodeError::InvalidUtf8)
        );
    }

    #[test]
    fn position_vector() {
        // Example from the protocol description: x=18357644, y=831, z=-20882616.
        let p = Position {
            x: 18_357_644,
            y: 831,
            z: -20_882_616,
        };
        let packed = p.packed();
        // x (26 bits), z (26 bits), y (12 bits), as in the description.
        let (x, z, y) = (
            0b01000110000001110110001100u64,
            0b10110000010101101101001000u64,
            0b001100111111u64,
        );
        assert_eq!(packed as u64, (x << 38) | (z << 12) | y);
        assert_eq!(Position::from_packed(packed), p);
    }

    #[test]
    fn counts_cannot_outgrow_the_packet() {
        let mut out = Vec::new();
        out.put_varint(1_000);
        assert_eq!(
            Reader::new(&out).count(10_000, 8, "longs"),
            Err(DecodeError::Eof)
        );
        assert!(matches!(
            Reader::new(&out).count(10, 1, "x"),
            Err(DecodeError::TooLong { .. })
        ));
        let mut neg = Vec::new();
        neg.put_varint(-5);
        assert_eq!(
            Reader::new(&neg).len(10, "x"),
            Err(DecodeError::NegativeLength)
        );
    }

    proptest! {
        #[test]
        fn varint_round_trip(v in any::<i32>()) {
            let mut out = Vec::new();
            out.put_varint(v);
            prop_assert_eq!(out.len(), varint_len(v));
            let mut r = Reader::new(&out);
            prop_assert_eq!(r.varint(), Ok(v));
            prop_assert!(r.is_empty());
        }

        #[test]
        fn varlong_round_trip(v in any::<i64>()) {
            let mut out = Vec::new();
            out.put_varlong(v);
            prop_assert_eq!(Reader::new(&out).varlong(), Ok(v));
        }

        #[test]
        fn string_round_trip(s in ".{0,64}") {
            let mut out = Vec::new();
            out.put_string(&s, MAX_STRING).unwrap();
            prop_assert_eq!(Reader::new(&out).string(MAX_STRING).unwrap(), s);
        }

        #[test]
        fn uuid_position_bitset_round_trip(
            u in any::<u128>(),
            x in -(1i32 << 25)..(1i32 << 25),
            y in -2048i32..2048,
            z in -(1i32 << 25)..(1i32 << 25),
            bits in proptest::collection::vec(any::<i64>(), 0..8),
        ) {
            let mut out = Vec::new();
            let uuid = Uuid::from_u128(u);
            out.put_uuid(&uuid);
            out.put_position(Position { x, y, z });
            out.put_bitset(&bits, 64).unwrap();
            let mut r = Reader::new(&out);
            prop_assert_eq!(r.uuid().unwrap(), uuid);
            prop_assert_eq!(r.position().unwrap(), Position { x, y, z });
            prop_assert_eq!(r.bitset(64).unwrap(), bits);
            prop_assert!(r.finish().is_ok());
        }

        #[test]
        fn readers_never_panic(data in proptest::collection::vec(any::<u8>(), 0..64)) {
            let mut r = Reader::new(&data);
            let _ = r.varint();
            let _ = r.string(16);
            let _ = r.bitset(4);
            let _ = r.nbt(NbtLimits::CLIENT);
            let _ = r.varlong();
        }
    }
}
