//! Logs (§2.11): `tracing` as text or JSON on stderr, a `session` span with a
//! short random ID on every line of a connection, and addresses replaced by a
//! keyed hash with `log-ips: false`.

use std::net::IpAddr;

use aws_lc_rs::hmac;
use tracing::level_filters::LevelFilter;

use crate::config::{LogFormat, LoggingConfig};

/// Installs the global subscriber. Called once at startup; later calls (tests)
/// are ignored.
pub fn init(cfg: &LoggingConfig) {
    let level = cfg
        .level
        .parse::<LevelFilter>()
        .unwrap_or(LevelFilter::INFO);
    let builder = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .with_writer(std::io::stderr);
    let _ = match cfg.format {
        LogFormat::Text => builder.try_init(),
        LogFormat::Json => builder.json().try_init(),
    };
}

/// How addresses appear in logs.
#[derive(Debug)]
pub enum IpDisplay {
    Plain,
    /// HMAC-SHA256 with a key drawn at startup: the same address gives the
    /// same tag within one run, and the tag cannot be reversed afterwards.
    Hashed(Box<hmac::Key>),
}

impl IpDisplay {
    pub fn new(log_ips: bool) -> Self {
        if log_ips {
            return Self::Plain;
        }
        let mut key = [0u8; 32];
        // Without randomness the key is zero and tags are only a hash; still
        // no plain address in logs.
        let _ = aws_lc_rs::rand::fill(&mut key);
        Self::Hashed(Box::new(hmac::Key::new(hmac::HMAC_SHA256, &key)))
    }

    pub fn show(&self, ip: IpAddr) -> String {
        match self {
            Self::Plain => ip.to_string(),
            Self::Hashed(key) => {
                let tag = hmac::sign(key, ip.to_string().as_bytes());
                let hex: String = tag
                    .as_ref()
                    .iter()
                    .take(6)
                    .map(|b| format!("{b:02x}"))
                    .collect();
                format!("ip-{hex}")
            }
        }
    }
}

/// A short random session ID (8 hex digits).
pub fn session_id() -> String {
    let mut b = [0u8; 4];
    let _ = aws_lc_rs::rand::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_addresses() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(IpDisplay::new(true).show(ip), "203.0.113.7");
        let h = IpDisplay::new(false);
        let a = h.show(ip);
        assert!(a.starts_with("ip-") && !a.contains("203"));
        assert_eq!(a, h.show(ip));
        assert_ne!(a, h.show("203.0.113.8".parse().unwrap()));
        assert_eq!(session_id().len(), 8);
    }
}
