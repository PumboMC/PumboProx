//! Player identifiers in one canonical form: nicknames, UUIDs and IP addresses.
//!
//! Every plugin stores and compares them the same way, so data written by one
//! plugin can be looked up by another:
//! - nicknames: [`name_key`] (trimmed, lowercase)
//! - UUIDs: [`Uuid`], written hyphenated in lowercase
//! - addresses: [`parse_ip`] (port and brackets removed, IPv4-mapped IPv6 as IPv4)
//!   and [`ip_key`], which groups IPv6 by /64, since one customer usually gets a
//!   whole /64 and can pick any address in it

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use md5::{Digest, Md5};

/// Storage key of a nickname: Java nicknames are case-insensitive.
pub fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Whether a name can belong to a Java account: 1 to 16 characters from
/// `[A-Za-z0-9_]`. Offline servers may let other names in.
pub fn is_java_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A UUID. Parses with or without hyphens, in any letter case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uuid(pub u128);

impl Uuid {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let hex: String = match text.len() {
            32 => text.to_string(),
            36 => {
                let dashes: Vec<usize> = text.match_indices('-').map(|(i, _)| i).collect();
                if dashes != [8, 13, 18, 23] {
                    return None;
                }
                text.replace('-', "")
            }
            _ => return None,
        };
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u128::from_str_radix(&hex, 16).ok().map(Uuid)
    }

    /// Version nibble: 3 for offline players, 4 for Mojang accounts.
    pub fn version(self) -> u8 {
        ((self.0 >> 76) & 0xF) as u8
    }

    /// The UUID an offline-mode server gives a nickname (version 3 of
    /// `OfflinePlayer:<name>`, the name as typed, case-sensitive).
    pub fn offline(name: &str) -> Self {
        let mut bytes: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}").as_bytes()).into();
        bytes[6] = (bytes[6] & 0x0F) | 0x30;
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        Uuid(u128::from_be_bytes(bytes))
    }

    /// 32 lowercase hex digits without hyphens (the form Mojang's API uses).
    pub fn simple(self) -> String {
        format!("{:032x}", self.0)
    }

    pub fn high_low(self) -> (u64, u64) {
        ((self.0 >> 64) as u64, self.0 as u64)
    }

    pub fn from_high_low(high: u64, low: u64) -> Self {
        Uuid((u128::from(high) << 64) | u128::from(low))
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.simple();
        let part = |a: usize, b: usize| s.get(a..b).unwrap_or("");
        write!(f, "{}-{}-{}-{}-{}", part(0, 8), part(8, 12), part(12, 16), part(16, 20), part(20, 32))
    }
}

impl FromStr for Uuid {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Uuid::parse(s).ok_or(())
    }
}

/// Whether a profile was authenticated by Mojang: a version 4 UUID with a signed
/// skin. A server in online mode and authenticated proxy forwarding (Velocity
/// modern, Vine, BungeeGuard) give exactly that; an offline login always gets a
/// version 3 UUID without a signature, so a client cannot fake it there.
pub fn is_authenticated_profile(uuid: &str, signed_skin: bool) -> bool {
    signed_skin && Uuid::parse(uuid).is_some_and(|u| u.version() == 4)
}

/// Removes the port from `host:port`, `[v6]:port` and `/ip:port`; other input is
/// returned trimmed.
pub fn strip_port(addr: &str) -> String {
    let addr = addr.trim();
    let addr = addr.strip_prefix('/').unwrap_or(addr);
    if let Some(rest) = addr.strip_prefix('[')
        && let Some((host, _)) = rest.split_once(']')
    {
        return host.to_string();
    }
    // Exactly one colon means IPv4 (or a hostname) with a port.
    if addr.matches(':').count() == 1
        && let Some((host, _)) = addr.rsplit_once(':')
    {
        return host.to_string();
    }
    addr.to_string()
}

/// Parses a client address as hosts report it (with or without port, brackets,
/// a leading `/` or an IPv6 zone). IPv4-mapped IPv6 addresses become IPv4.
pub fn parse_ip(addr: &str) -> Option<IpAddr> {
    let host = strip_port(addr);
    let host = host.split('%').next().unwrap_or("");
    let ip: IpAddr = host.parse().ok()?;
    Some(match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    })
}

/// Key under which an address is counted and stored: the IPv4 address itself,
/// or the /64 network of an IPv6 address (`2001:db8:1:2::/64`).
pub fn ip_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(_) => Cidr::new(ip, 64).to_string(),
    }
}

/// [`ip_key`] of a textual address; unparsable input is returned trimmed and
/// lowercased, so it still works as a key.
pub fn ip_key_str(addr: &str) -> String {
    parse_ip(addr).map_or_else(|| addr.trim().to_lowercase(), ip_key)
}

/// An address range such as `10.0.0.0/8` or `2001:db8::/32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// The range of `prefix` leading bits around `ip` (the prefix is capped at 32
    /// or 128 bits).
    pub fn new(ip: IpAddr, prefix: u8) -> Self {
        match ip {
            IpAddr::V4(v4) => {
                let prefix = prefix.min(32);
                Cidr { network: IpAddr::V4(Ipv4Addr::from(u32::from(v4) & mask32(prefix))), prefix }
            }
            IpAddr::V6(v6) => {
                let prefix = prefix.min(128);
                Cidr { network: IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask128(prefix))), prefix }
            }
        }
    }

    /// Parses `a.b.c.d/n`, `v6/n` or a single address (the whole address).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        match text.split_once('/') {
            Some((ip, bits)) => {
                let ip = parse_ip(ip)?;
                let bits: u8 = bits.parse().ok()?;
                let max = if ip.is_ipv4() { 32 } else { 128 };
                (bits <= max).then(|| Cidr::new(ip, bits))
            }
            None => parse_ip(text).map(|ip| Cidr::new(ip, if ip.is_ipv4() { 32 } else { 128 })),
        }
    }

    pub fn network(&self) -> IpAddr {
        self.network
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 => v4,
        };
        match (self.network, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => u32::from(a) & mask32(self.prefix) == u32::from(n),
            (IpAddr::V6(n), IpAddr::V6(a)) => u128::from(a) & mask128(self.prefix) == u128::from(n),
            _ => false,
        }
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

fn mask32(prefix: u8) -> u32 {
    if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix.min(32))) }
}

fn mask128(prefix: u8) -> u128 {
    if prefix == 0 { 0 } else { u128::MAX << (128 - u32::from(prefix.min(128))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_profile() {
        assert!(is_authenticated_profile("069a79f4-44e9-4726-a5be-fca90e38aaf5", true));
        assert!(!is_authenticated_profile("069a79f4-44e9-4726-a5be-fca90e38aaf5", false));
        let offline = Uuid::offline("Notch").to_string();
        assert!(!is_authenticated_profile(&offline, true));
        assert!(!is_authenticated_profile("nope", true));
    }

    #[test]
    fn names() {
        assert_eq!(name_key("  Steve "), "steve");
        assert!(is_java_name("Steve_01"));
        assert!(!is_java_name("a-b"));
        assert!(!is_java_name(""));
        assert!(!is_java_name("abcdefghijklmnopq"));
    }

    #[test]
    fn uuids() {
        let u = Uuid::parse("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap();
        assert_eq!(Uuid::parse("069A79F444E94726A5BEFCA90E38AAF5"), Some(u));
        assert_eq!(u.to_string(), "069a79f4-44e9-4726-a5be-fca90e38aaf5");
        assert_eq!(u.simple(), "069a79f444e94726a5befca90e38aaf5");
        assert_eq!(u.version(), 4);
        let (h, l) = u.high_low();
        assert_eq!(Uuid::from_high_low(h, l), u);
        assert_eq!("069a79f444e94726a5befca90e38aaf5".parse::<Uuid>(), Ok(u));
        assert!(Uuid::parse("nope").is_none());
        assert!(Uuid::parse("069a79f4-44e94726-a5be-fca90e38aaf5-").is_none());
        assert!(Uuid::parse("zz9a79f444e94726a5befca90e38aaf5").is_none());
    }

    #[test]
    fn offline_uuid_matches_the_server() {
        // Known value: offline UUID of "Notch" as vanilla and Pumpkin compute it.
        let u = Uuid::offline("Notch");
        assert_eq!(u.to_string(), "b50ad385-829d-3141-a216-7e7d7539ba7f");
        assert_eq!(u.version(), 3);
        assert_ne!(Uuid::offline("notch"), u);
    }

    #[test]
    fn ports_and_brackets() {
        assert_eq!(strip_port("127.0.0.1:53164"), "127.0.0.1");
        assert_eq!(strip_port("[::1]:25565"), "::1");
        assert_eq!(strip_port("::1"), "::1");
        assert_eq!(strip_port("10.0.0.1"), "10.0.0.1");
        assert_eq!(strip_port("/10.0.0.1:5"), "10.0.0.1");
        assert_eq!(parse_ip("[::ffff:1.2.3.4]:5"), Some("1.2.3.4".parse().unwrap()));
        assert_eq!(parse_ip("fe80::1%eth0"), Some("fe80::1".parse().unwrap()));
        assert_eq!(parse_ip("example.org:25565"), None);
    }

    #[test]
    fn ipv6_is_grouped_by_64() {
        let a = parse_ip("[2001:db8:1:2:aaaa::1]:1").unwrap();
        let b = parse_ip("2001:db8:1:2:ffff:ffff:ffff:ffff").unwrap();
        assert_eq!(ip_key(a), "2001:db8:1:2::/64");
        assert_eq!(ip_key(a), ip_key(b));
        assert_ne!(ip_key(a), ip_key_str("2001:db8:1:3::1"));
        assert_eq!(ip_key_str("1.2.3.4:80"), "1.2.3.4");
        assert_eq!(ip_key_str(" Weird "), "weird");
    }

    #[test]
    fn ranges() {
        let net = Cidr::parse("10.1.2.3/8").unwrap();
        assert_eq!(net.to_string(), "10.0.0.0/8");
        assert!(net.contains("10.200.0.1".parse().unwrap()));
        assert!(!net.contains("11.0.0.1".parse().unwrap()));
        assert!(net.contains("::ffff:10.0.0.9".parse().unwrap()));
        let v6 = Cidr::parse("2001:db8::/32").unwrap();
        assert!(v6.contains("2001:db8:ffff::1".parse().unwrap()));
        assert!(!v6.contains("10.0.0.1".parse().unwrap()));
        assert_eq!(Cidr::parse("1.2.3.4").unwrap().prefix(), 32);
        assert!(Cidr::parse("1.2.3.4/33").is_none());
        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains("8.8.8.8".parse().unwrap()));
        assert_eq!(Cidr::parse("::/0").unwrap().network(), "::".parse::<IpAddr>().unwrap());
    }
}
