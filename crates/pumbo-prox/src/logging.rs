//! Logs (§2.11): `tracing` as text or JSON on stderr, a `session` span with a
//! short random ID on every line of a connection, and addresses replaced by a
//! keyed hash with `log-ips: false`.

use std::net::IpAddr;

use aws_lc_rs::hmac;
use std::sync::OnceLock;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Registry, reload};

use crate::config::{LogFormat, LoggingConfig};

/// The filter of the subscriber [`init`] installed, for [`set_filter`].
static FILTER: OnceLock<reload::Handle<Targets, Registry>> = OnceLock::new();

/// Installs the global subscriber. Called once at startup; later calls (tests)
/// are ignored.
pub fn init(cfg: &LoggingConfig) {
    let targets = filter(cfg).unwrap_or_else(|_| Targets::new().with_default(LevelFilter::INFO));
    let (filter, handle) = reload::Layer::new(targets);
    let fmt = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(std::io::stderr);
    let installed = match cfg.format {
        LogFormat::Text => Registry::default().with(filter).with(fmt).try_init(),
        LogFormat::Json => Registry::default().with(filter).with(fmt.json()).try_init(),
    };
    if installed.is_ok() {
        let _ = FILTER.set(handle);
    }
}

/// A level of `logging.level` or `logging.filters`; `warning` is `warn`.
pub fn check_level(level: &str) -> Result<LevelFilter, String> {
    let name = if level.eq_ignore_ascii_case("warning") {
        "warn"
    } else {
        level
    };
    name.parse().map_err(|_| {
        format!("\"{level}\" is not a level; use error, warn, info, debug, trace or off")
    })
}

/// `logging.level` with `logging.filters` (per module, e.g.
/// `pumbo_host: debug`); an error names its key.
pub fn filter(cfg: &LoggingConfig) -> Result<Targets, (Vec<String>, String)> {
    let mut targets = match check_level(&cfg.level) {
        Ok(level) => Targets::new().with_default(level),
        // `info,pumbo_host=debug`: older versions ran it as `info`.
        Err(_) if cfg.level.contains('=') => cfg.level.parse().map_err(|e| {
            (
                vec!["level".into()],
                format!("logging.level \"{}\": {e}", cfg.level),
            )
        })?,
        Err(e) => {
            return Err((
                vec!["level".into()],
                format!("logging.level {e} (per module: logging.filters)"),
            ));
        }
    };
    for (target, level) in &cfg.filters {
        let level = check_level(level).map_err(|e| {
            (
                vec!["filters".into(), target.clone()],
                format!("logging.filters.{target} {e}"),
            )
        })?;
        targets = targets.with_target(target.clone(), level);
    }
    Ok(targets)
}

/// Changes the filter of the running logs (`reload`).
pub fn set_filter(cfg: &LoggingConfig) -> Result<(), String> {
    let targets = filter(cfg).map_err(|(_, e)| e)?;
    if let Some(h) = FILTER.get() {
        h.modify(|f| *f = targets).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// [`set_filter`] with only a level.
pub fn set_level(level: &str) -> Result<(), String> {
    set_filter(&LoggingConfig {
        level: level.into(),
        ..LoggingConfig::default()
    })
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
    fn levels_and_filters() {
        let cfg = |level: &str, filters: &[(&str, &str)]| LoggingConfig {
            level: level.into(),
            filters: filters
                .iter()
                .map(|(t, l)| (t.to_string(), l.to_string()))
                .collect(),
            ..LoggingConfig::default()
        };
        assert!(filter(&cfg("WARNING", &[])).is_ok());
        assert!(filter(&cfg("info,pumbo_host=debug", &[])).is_ok());
        assert_eq!(
            filter(&cfg("verbose", &[])).unwrap_err().1,
            "logging.level \"verbose\" is not a level; use error, warn, info, debug, trace or off (per module: logging.filters)"
        );
        let (key, e) = filter(&cfg("info", &[("pumbo_host", "loud")])).unwrap_err();
        assert_eq!(key, ["filters", "pumbo_host"]);
        assert!(
            e.starts_with("logging.filters.pumbo_host \"loud\" is not a level"),
            "{e}"
        );
        let t = filter(&cfg("warn", &[("pumbo_host", "debug")])).unwrap();
        assert!(t.would_enable("pumbo_host::actor", &tracing::Level::DEBUG));
        assert!(!t.would_enable("pumbo_prox::server", &tracing::Level::INFO));
    }

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
