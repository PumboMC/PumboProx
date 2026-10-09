//! Reading and writing protocol primitives on raw payloads. Every read returns
//! `None` at the end of the input or on a malformed value; a translator that
//! cannot read a packet drops it.

/// Longest string the protocol allows (in bytes: 32767 UTF-16 units, 3 bytes each).
const MAX_STRING_BYTES: usize = 32767 * 3;

pub(crate) trait Get<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]>;
    fn get_u8(&mut self) -> Option<u8>;
    fn get_var_int(&mut self) -> Option<i32>;
    fn get_var_long(&mut self) -> Option<i64>;

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.bytes(N)?.try_into().ok()
    }
    fn get_i8(&mut self) -> Option<i8> {
        self.get_u8().map(|b| b as i8)
    }
    fn get_bool(&mut self) -> Option<bool> {
        self.get_u8().map(|b| b != 0)
    }
    fn get_i16(&mut self) -> Option<i16> {
        self.array().map(i16::from_be_bytes)
    }
    fn get_i32(&mut self) -> Option<i32> {
        self.array().map(i32::from_be_bytes)
    }
    fn get_i64(&mut self) -> Option<i64> {
        self.array().map(i64::from_be_bytes)
    }
    fn get_f32(&mut self) -> Option<f32> {
        self.array().map(f32::from_be_bytes)
    }
    fn get_f64(&mut self) -> Option<f64> {
        self.array().map(f64::from_be_bytes)
    }
    /// A non-negative VarInt as a length or count.
    fn get_len(&mut self) -> Option<usize> {
        usize::try_from(self.get_var_int()?).ok()
    }
    fn get_str(&mut self) -> Option<&'a str> {
        let len = self.get_len()?;
        if len > MAX_STRING_BYTES {
            return None;
        }
        std::str::from_utf8(self.bytes(len)?).ok()
    }
}

impl<'a> Get<'a> for &'a [u8] {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.split_at_checked(n)?;
        *self = rest;
        Some(head)
    }

    fn get_u8(&mut self) -> Option<u8> {
        let (&b, rest) = self.split_first()?;
        *self = rest;
        Some(b)
    }

    fn get_var_int(&mut self) -> Option<i32> {
        let mut value = 0u32;
        for shift in 0..5 {
            let b = self.get_u8()?;
            value |= u32::from(b & 0x7F) << (7 * shift);
            if b & 0x80 == 0 {
                return Some(value as i32);
            }
        }
        None
    }

    fn get_var_long(&mut self) -> Option<i64> {
        let mut value = 0u64;
        for shift in 0..10 {
            let b = self.get_u8()?;
            value |= u64::from(b & 0x7F) << (7 * shift);
            if b & 0x80 == 0 {
                return Some(value as i64);
            }
        }
        None
    }
}

pub(crate) trait Put {
    fn put_u8(&mut self, v: u8);
    fn put_slice(&mut self, v: &[u8]);

    fn put_bool(&mut self, v: bool) {
        self.put_u8(u8::from(v));
    }
    fn put_i8(&mut self, v: i8) {
        self.put_u8(v as u8);
    }
    fn put_i16(&mut self, v: i16) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_i32(&mut self, v: i32) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_i64(&mut self, v: i64) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_f32(&mut self, v: f32) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_f64(&mut self, v: f64) {
        self.put_slice(&v.to_be_bytes());
    }
    fn put_var_int(&mut self, v: i32) {
        let mut v = v as u32;
        loop {
            if v < 0x80 {
                self.put_u8(v as u8);
                return;
            }
            self.put_u8((v as u8 & 0x7F) | 0x80);
            v >>= 7;
        }
    }
    fn put_var_long(&mut self, v: i64) {
        let mut v = v as u64;
        loop {
            if v < 0x80 {
                self.put_u8(v as u8);
                return;
            }
            self.put_u8((v as u8 & 0x7F) | 0x80);
            v >>= 7;
        }
    }
    fn put_len(&mut self, n: usize) {
        self.put_var_int(i32::try_from(n).unwrap_or(i32::MAX));
    }
    fn put_str(&mut self, s: &str) {
        self.put_len(s.len());
        self.put_slice(s.as_bytes());
    }
}

impl Put for Vec<u8> {
    fn put_u8(&mut self, v: u8) {
        self.push(v);
    }
    fn put_slice(&mut self, v: &[u8]) {
        self.extend_from_slice(v);
    }
}

/// Bytes a VarInt takes on the wire.
pub(crate) fn var_int_len(v: i32) -> usize {
    let v = v as u32;
    match v {
        0..0x80 => 1,
        0x80..0x4000 => 2,
        0x4000..0x20_0000 => 3,
        0x20_0000..0x1000_0000 => 4,
        _ => 5,
    }
}

/// Copies `n` bytes from `r` to `out`.
pub(crate) fn copy(r: &mut &[u8], n: usize, out: &mut Vec<u8>) -> Option<()> {
    out.extend_from_slice(r.bytes(n)?);
    Some(())
}

pub(crate) fn copy_var_int(r: &mut &[u8], out: &mut Vec<u8>) -> Option<i32> {
    let v = r.get_var_int()?;
    out.put_var_int(v);
    Some(v)
}

pub(crate) fn copy_str(r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let s = r.get_str()?;
    out.put_str(s);
    Some(())
}

pub(crate) fn copy_bool(r: &mut &[u8], out: &mut Vec<u8>) -> Option<bool> {
    let v = r.get_bool()?;
    out.put_bool(v);
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn var_ints_round_trip() {
        for v in [
            0,
            1,
            127,
            128,
            255,
            25565,
            2_097_151,
            i32::MAX,
            -1,
            i32::MIN,
        ] {
            let mut out = Vec::new();
            out.put_var_int(v);
            assert_eq!(out.len(), var_int_len(v), "{v}");
            let mut r = out.as_slice();
            assert_eq!(r.get_var_int(), Some(v));
            assert!(r.is_empty());
        }
        for v in [0i64, 300, i64::MAX, -1, i64::MIN] {
            let mut out = Vec::new();
            out.put_var_long(v);
            assert_eq!((&mut out.as_slice()).get_var_long(), Some(v));
        }
        assert_eq!((&mut [0xFFu8; 6].as_slice()).get_var_int(), None);
    }
}
