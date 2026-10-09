//! Java's modified UTF-8 (`DataOutput.writeUTF` without the length prefix):
//! U+0000 as `C0 80` and characters above U+FFFF as two 3-byte surrogates.

/// Encodes a string.
pub fn encode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut units = [0u16; 2];
    for c in s.chars() {
        for &u in c.encode_utf16(&mut units).iter() {
            match u {
                0x0001..=0x007F => out.push(u as u8),
                0x0000 | 0x0080..=0x07FF => {
                    out.push(0xC0 | ((u >> 6) as u8 & 0x1F));
                    out.push(0x80 | (u as u8 & 0x3F));
                }
                _ => {
                    out.push(0xE0 | ((u >> 12) as u8 & 0x0F));
                    out.push(0x80 | ((u >> 6) as u8 & 0x3F));
                    out.push(0x80 | (u as u8 & 0x3F));
                }
            }
        }
    }
    out
}

/// Decodes; `None` for malformed input or unpaired surrogates (a Rust string
/// cannot hold them).
pub fn decode(bytes: &[u8]) -> Option<String> {
    // Fast path: plain ASCII without NUL is the same in both encodings.
    if bytes.iter().all(|b| (1..0x80).contains(b)) {
        return String::from_utf8(bytes.to_vec()).ok();
    }
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut it = bytes.iter().copied();
    while let Some(b) = it.next() {
        let unit = match b >> 4 {
            0x0..=0x7 => u16::from(b),
            0xC | 0xD => {
                let b2 = continuation(it.next())?;
                (u16::from(b & 0x1F) << 6) | b2
            }
            0xE => {
                let b2 = continuation(it.next())?;
                let b3 = continuation(it.next())?;
                (u16::from(b & 0x0F) << 12) | (b2 << 6) | b3
            }
            _ => return None,
        };
        units.push(unit);
    }
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

fn continuation(b: Option<u8>) -> Option<u16> {
    let b = b?;
    (b & 0xC0 == 0x80).then_some(u16::from(b & 0x3F))
}
