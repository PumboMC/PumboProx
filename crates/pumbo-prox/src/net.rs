//! Address helpers: CIDR ranges and the PROXY protocol header (v1 and v2,
//! HAProxy's specification "The PROXY protocol"), accepted only from trusted
//! addresses (§3.4).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

/// An address range such as `10.0.0.0/8` or `::1/128` (a bare address is /32 or /128).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

impl FromStr for Cidr {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (addr, prefix) = s.split_once('/').unwrap_or((s, ""));
        let addr: IpAddr = addr.parse().map_err(|_| format!("bad address in {s}"))?;
        let addr = addr.to_canonical();
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = if prefix.is_empty() {
            max
        } else {
            prefix.parse().map_err(|_| format!("bad prefix in {s}"))?
        };
        if prefix > max {
            return Err(format!("prefix /{prefix} too long in {s}"));
        }
        Ok(Self { addr, prefix })
    }
}

const V2_SIGNATURE: &[u8; 12] = b"\r\n\r\n\0\r\nQUIT\n";
const V1_PREFIX: &[u8] = b"PROXY ";
/// Longest v1 line including CRLF (specification §2.1).
const V1_MAX: usize = 107;
/// Largest v2 header we accept (16 bytes + addresses + TLVs).
const V2_MAX: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProxyHeaderError {
    #[error("no PROXY protocol header")]
    Missing,
    #[error("malformed PROXY protocol header")]
    Malformed,
}

/// A complete header: bytes it took and the client address it announces
/// (`None` for `LOCAL`/`UNKNOWN`, e.g. health checks of the load balancer).
pub type ProxyHeader = (usize, Option<SocketAddr>);

/// Parses a header at the start of `buf`. `Ok(None)`: more bytes needed.
pub fn parse_proxy_header(buf: &[u8]) -> Result<Option<ProxyHeader>, ProxyHeaderError> {
    let n = buf.len().min(V2_SIGNATURE.len());
    if buf.get(..n) == V2_SIGNATURE.get(..n) && n > 0 {
        return parse_v2(buf);
    }
    let n = buf.len().min(V1_PREFIX.len());
    if buf.get(..n) == V1_PREFIX.get(..n) && n > 0 {
        return parse_v1(buf);
    }
    if buf.is_empty() {
        return Ok(None);
    }
    Err(ProxyHeaderError::Missing)
}

fn parse_v1(buf: &[u8]) -> Result<Option<ProxyHeader>, ProxyHeaderError> {
    let window = buf.get(..buf.len().min(V1_MAX)).unwrap_or(buf);
    let Some(end) = window.windows(2).position(|w| w == b"\r\n") else {
        return if buf.len() >= V1_MAX {
            Err(ProxyHeaderError::Malformed)
        } else {
            Ok(None)
        };
    };
    let line = std::str::from_utf8(window.get(..end).unwrap_or_default())
        .map_err(|_| ProxyHeaderError::Malformed)?;
    let parts: Vec<&str> = line.split(' ').collect();
    let source = match parts.as_slice() {
        ["PROXY", "UNKNOWN", ..] => None,
        ["PROXY", proto @ ("TCP4" | "TCP6"), src, _dst, sport, _dport] => {
            let ip: IpAddr = src.parse().map_err(|_| ProxyHeaderError::Malformed)?;
            if ip.is_ipv4() != (*proto == "TCP4") {
                return Err(ProxyHeaderError::Malformed);
            }
            let port: u16 = sport.parse().map_err(|_| ProxyHeaderError::Malformed)?;
            Some(SocketAddr::new(ip, port))
        }
        _ => return Err(ProxyHeaderError::Malformed),
    };
    Ok(Some((end + 2, source)))
}

fn parse_v2(buf: &[u8]) -> Result<Option<ProxyHeader>, ProxyHeaderError> {
    let Some(head) = buf.get(..16) else {
        return Ok(None);
    };
    let (Some(&ver_cmd), Some(&family), Some(len)) = (
        head.get(12),
        head.get(13),
        head.get(14..16)
            .and_then(|b| <[u8; 2]>::try_from(b).ok())
            .map(|b| usize::from(u16::from_be_bytes(b))),
    ) else {
        return Err(ProxyHeaderError::Malformed);
    };
    if ver_cmd >> 4 != 2 || 16 + len > V2_MAX {
        return Err(ProxyHeaderError::Malformed);
    }
    let Some(body) = buf.get(16..16 + len) else {
        return Ok(None);
    };
    let total = 16 + len;
    match ver_cmd & 0x0F {
        0 => return Ok(Some((total, None))), // LOCAL
        1 => {}                              // PROXY
        _ => return Err(ProxyHeaderError::Malformed),
    }
    let port = |at: usize| {
        body.get(at..at + 2)
            .and_then(|b| <[u8; 2]>::try_from(b).ok())
            .map(u16::from_be_bytes)
            .ok_or(ProxyHeaderError::Malformed)
    };
    let source = match family >> 4 {
        1 => {
            let ip: [u8; 4] = body
                .get(..4)
                .and_then(|b| b.try_into().ok())
                .ok_or(ProxyHeaderError::Malformed)?;
            Some(SocketAddr::new(Ipv4Addr::from(ip).into(), port(8)?))
        }
        2 => {
            let ip: [u8; 16] = body
                .get(..16)
                .and_then(|b| b.try_into().ok())
                .ok_or(ProxyHeaderError::Malformed)?;
            Some(SocketAddr::new(Ipv6Addr::from(ip).into(), port(32)?))
        }
        // AF_UNSPEC and AF_UNIX carry no usable client address.
        0 | 3 => None,
        _ => return Err(ProxyHeaderError::Malformed),
    };
    Ok(Some((total, source)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn cidr_ranges() {
        let c: Cidr = "10.0.0.0/8".parse().unwrap();
        assert!(c.contains("10.200.1.1".parse().unwrap()));
        assert!(!c.contains("11.0.0.1".parse().unwrap()));
        assert!(c.contains("::ffff:10.0.0.1".parse().unwrap()));
        let one: Cidr = "127.0.0.1".parse().unwrap();
        assert!(one.contains("127.0.0.1".parse().unwrap()));
        assert!(!one.contains("127.0.0.2".parse().unwrap()));
        let all: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains("203.0.113.9".parse().unwrap()));
        assert!(!all.contains("::1".parse().unwrap()));
        let v6: Cidr = "2001:db8::/32".parse().unwrap();
        assert!(v6.contains("2001:db8:1::5".parse().unwrap()));
        assert!("10.0.0.0/33".parse::<Cidr>().is_err());
        assert!("nope/8".parse::<Cidr>().is_err());
    }

    #[test]
    fn v1_headers() {
        let h = b"PROXY TCP4 203.0.113.7 10.0.0.1 51234 25565\r\n\x10\x00";
        assert_eq!(
            parse_proxy_header(h),
            Ok(Some((
                h.len() - 2,
                Some("203.0.113.7:51234".parse().unwrap())
            )))
        );
        let h = b"PROXY TCP6 2001:db8::1 ::1 4000 25565\r\n";
        assert_eq!(
            parse_proxy_header(h).unwrap().unwrap().1,
            Some("[2001:db8::1]:4000".parse().unwrap())
        );
        assert_eq!(
            parse_proxy_header(b"PROXY UNKNOWN\r\n"),
            Ok(Some((15, None)))
        );
        assert_eq!(parse_proxy_header(b"PROXY TCP4 1.2.3.4"), Ok(None));
        assert_eq!(parse_proxy_header(b"PRO"), Ok(None));
        assert_eq!(
            parse_proxy_header(b"PROXY TCP4 ::1 ::1 1 2\r\n"),
            Err(ProxyHeaderError::Malformed)
        );
        assert_eq!(
            parse_proxy_header(&[b'P'; 200]),
            Err(ProxyHeaderError::Missing)
        );
        let mut long = b"PROXY ".to_vec();
        long.extend_from_slice(&[b'1'; 120]);
        assert_eq!(parse_proxy_header(&long), Err(ProxyHeaderError::Malformed));
    }

    #[test]
    fn v2_headers() {
        let mut h = V2_SIGNATURE.to_vec();
        h.extend_from_slice(&[
            0x21, 0x11, 0, 12, 203, 0, 113, 7, 10, 0, 0, 1, 0x1F, 0x90, 0x63, 0xDD,
        ]);
        h.extend_from_slice(b"rest");
        assert_eq!(
            parse_proxy_header(&h),
            Ok(Some((28, Some("203.0.113.7:8080".parse().unwrap()))))
        );
        for cut in 0..28 {
            assert_eq!(
                parse_proxy_header(h.get(..cut).unwrap()),
                Ok(None),
                "cut {cut}"
            );
        }
        // LOCAL (health check).
        let mut local = V2_SIGNATURE.to_vec();
        local.extend_from_slice(&[0x20, 0x00, 0, 0]);
        assert_eq!(parse_proxy_header(&local), Ok(Some((16, None))));
        // Wrong version.
        let mut bad = V2_SIGNATURE.to_vec();
        bad.extend_from_slice(&[0x11, 0x11, 0, 12]);
        assert_eq!(parse_proxy_header(&bad), Err(ProxyHeaderError::Malformed));
        // A Minecraft handshake is not a header.
        assert_eq!(
            parse_proxy_header(&[0x10, 0x00, 0xF5, 0x05]),
            Err(ProxyHeaderError::Missing)
        );
    }

    proptest! {
        #[test]
        fn garbage_never_panics(data in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = parse_proxy_header(&data);
            let mut v2 = V2_SIGNATURE.to_vec();
            v2.extend_from_slice(&data);
            if let Ok(Some((n, _))) = parse_proxy_header(&v2) {
                prop_assert!(n <= v2.len());
            }
        }
    }
}
