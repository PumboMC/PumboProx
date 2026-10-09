//! Writing helpers kept for callers outside the packet codec.

use crate::types::WriteExt;

/// Appends a VarInt.
pub fn write_varint(out: &mut Vec<u8>, value: i32) {
    out.put_varint(value);
}

/// Appends a String (VarInt byte length + UTF-8) without a length limit check.
pub fn write_string(out: &mut Vec<u8>, s: &str) {
    write_varint(out, i32::try_from(s.len()).unwrap_or(i32::MAX));
    out.extend_from_slice(s.as_bytes());
}
