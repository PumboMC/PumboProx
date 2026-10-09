//! Small helpers: hex encoding, constant-time comparison and `*` wildcards.

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS.get(usize::from(b >> 4)).copied().unwrap_or(b'0')));
        s.push(char::from(DIGITS.get(usize::from(b & 0x0F)).copied().unwrap_or(b'0')));
    }
    s
}

/// Constant-time comparison of two byte strings (the length is not secret).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Minimal glob: `*` matches any sequence, everything else matches literally.
/// Case-sensitive; lowercase both sides for case-insensitive matching.
pub fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(pos) => rest = rest.get(pos + part.len()..).unwrap_or(""),
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_ct_eq() {
        assert_eq!(hex(&[0x00, 0xAB, 0x7f]), "00ab7f");
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn globs() {
        assert!(glob("a*c", "abc"));
        assert!(glob("*", "x"));
        assert!(glob("ab", "ab"));
        assert!(!glob("ab", "abc"));
        assert!(glob("*b*", "abc"));
        assert!(!glob("a*d", "abc"));
        assert!(glob("pumbo.*", "pumbo.bans.ban"));
    }
}
