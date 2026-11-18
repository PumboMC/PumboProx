//! The `pumboprox.yml` config (sketch in §3.5). Every module section has a
//! field with the module name, and the rest of the section goes to that
//! module's factory without interpretation in the core.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

use pumbo_core::registry::ModuleConfig;
use pumbo_core::section::{SectionInfo, keys_schema};
use serde::Deserialize;
use serde_json::json;

pub use crate::keys::Seg;
use crate::net::Cidr;

mod types;
pub use types::*;

/// The config a first start writes (`pumboprox` without a config file).
pub const DEFAULT_FILE: &str = include_str!("../default-config.yml");

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Not valid YAML, or a value of the wrong type (with line and column).
    #[error("{0}")]
    Yaml(String),
    #[error("{0}")]
    Invalid(String),
    /// A bad value of `key`; [`Config::parse`] gives it the key's line.
    #[error("{message}")]
    Key { key: Vec<Seg>, message: String },
    #[error("line {line}: {message}")]
    At { line: usize, message: String },
}

impl ConfigError {
    fn key(key: &[&str], message: String) -> Self {
        Self::Key {
            key: key.iter().map(|k| Seg::from(*k)).collect(),
            message,
        }
    }

    /// `pumboprox.yml line 14: ...` or `pumboprox.yml: ...`.
    pub fn in_file(&self, file: &str) -> String {
        match self {
            Self::At { line, message } => format!("{file} line {line}: {message}"),
            other => format!("{file}: {other}"),
        }
    }
}

/// Messages about keys, each with the key's line when it was found.
pub type Notes = Vec<(Option<usize>, String)>;

/// What [`Config::parse_checked`] has to say about a file that loads.
#[derive(Debug, Default)]
pub struct Remarks {
    /// Keys nothing reads.
    pub unknown: Notes,
    /// Old keys still read or skipped, a newer `config-version`, an empty
    /// translation chain.
    pub warnings: Notes,
}

/// The config file as the start, `reload` and `check-config` read it: the
/// typed config of the proxy and of the plugin host, and what the owner
/// should hear about the file.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    /// The plugin host's sections, parsed from the same text. An error here
    /// is reported where they are used: at the start, on `reload` and in
    /// `check-config`.
    pub host: Result<pumbo_host::HostConfig, String>,
    /// Keys nothing reads: a warning at start and reload, an error in
    /// `check-config`. Each starts with the file and line.
    pub unknown: Vec<String>,
    /// Old keys still read or skipped and an empty translation chain
    /// (`pumboprox migrate-config` renames or removes the old keys).
    pub warnings: Vec<String>,
}

/// Reads and checks the config file for the start, `reload` and
/// `check-config`, which give the proxy and the plugin host their sections
/// from this one read; `describe`, `migrate-config` and the config edits of
/// commands read the file on their own.
pub fn load(path: &std::path::Path) -> Result<Loaded, String> {
    let text = read(path)?;
    let file = file_label(path);
    let (config, remarks) = Config::parse_checked(&text).map_err(|e| e.in_file(&file))?;
    let host = pumbo_host::HostConfig::parse(&text);
    let lines = |notes: &Notes| -> Vec<String> {
        notes
            .iter()
            .map(|(line, m)| match line {
                Some(n) => format!("{file} line {n}: {m}"),
                None => format!("{file}: {m}"),
            })
            .collect()
    };
    Ok(Loaded {
        unknown: lines(&remarks.unknown),
        warnings: lines(&remarks.warnings),
        config,
        host,
    })
}

/// The name of the config file in messages.
pub fn file_label(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The config file's text; a missing one next to a `.toml` of the same name
/// (the format before YAML) says so instead of only "not found".
pub fn read(path: &std::path::Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| {
        let old = path.with_extension("toml");
        if e.kind() == std::io::ErrorKind::NotFound && old.is_file() {
            format!(
                "{} not found; found {}, this version reads YAML - convert it to YAML with the same keys (see https://github.com/PumboMC/PumboProx)",
                path.display(),
                old.display()
            )
        } else {
            format!("{}: {e}", path.display())
        }
    })
}

/// The proxy's sections of `pumboprox.yml`. Every field but `check_only`
/// and `sections` is a section registered in [`sections()`] (`server-group`
/// by the plugin host): a new field is a new entry there.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    #[serde(default)]
    pub listener: Vec<ListenerConfig>,
    #[serde(default)]
    pub status: StatusConfig,
    #[serde(default)]
    pub login: LoginConfig,
    #[serde(default)]
    pub authentication: AuthenticationConfig,
    #[serde(default)]
    pub forwarding: ForwardingConfig,
    #[serde(default)]
    pub servers: BTreeMap<String, ServerConfig>,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub translation: TranslationConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Host from the handshake (normalized) → servers to try instead of `routing.try` (§3.2).
    #[serde(default)]
    pub forced_hosts: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub switching: SwitchingConfig,
    #[serde(default)]
    pub commands: CommandsConfig,
    #[serde(default)]
    pub plugin_messages: PluginMessagesConfig,
    /// `config-version`: 1 when missing (see `migrate`).
    #[serde(default)]
    pub config_version: Option<u64>,
    #[serde(default)]
    pub messaging_channel: MessagingChannelConfig,
    /// `bungeecord-channel`, the name before `config-version` 2; read into
    /// `messaging-channel` when that is not set.
    #[serde(default, rename = "bungeecord-channel")]
    pub legacy_messaging_channel: Option<MessagingChannelConfig>,
    #[serde(default)]
    pub messages: MessagesConfig,
    #[serde(default, rename = "virtual")]
    pub virtual_world: VirtualConfig,
    #[serde(default)]
    pub tab: TabConfig,
    #[serde(default)]
    pub pumbox: PumboxConfig,
    /// `bridge`: PumboBridge sessions of the Pumpkin servers.
    #[serde(default)]
    pub bridge: crate::bridge::BridgeConfig,
    /// Servers the proxy downloads, creates and runs (`managed-servers`).
    #[serde(default)]
    pub managed_servers: pumbo_servers::Config,
    /// `server-group` (a list), read by the plugin host (§5.8.4).
    #[serde(default, rename = "server-group")]
    pub server_groups: Vec<ModuleConfig>,
    /// Set by `check-config`: modules create no files (a missing secret is
    /// only reported).
    #[serde(skip)]
    pub check_only: bool,
    /// Other sections, e.g. `viaproxy` for the translation provider of that
    /// name; any value, so an unknown key is a warning, not a parse error.
    #[serde(flatten)]
    pub sections: BTreeMap<String, serde_json::Value>,
}

/// The sections the proxy reads, with the schema of their keys.
pub fn sections() -> Vec<SectionInfo> {
    // The typed keys of a section whose other keys go to its module.
    let module = |typed: &[&str]| {
        let properties: serde_json::Map<String, serde_json::Value> = typed
            .iter()
            .map(|k| ((*k).to_string(), json!({})))
            .collect();
        json!({ "type": "object", "properties": properties, "additionalProperties": true })
    };
    let section = |key, schema| SectionInfo { key, schema };
    vec![
        section(
            "listener",
            json!({ "type": "array", "items": module(crate::keys::LISTENER) }),
        ),
        section("status", keys_schema::<StatusConfig>()),
        section("login", keys_schema::<LoginConfig>()),
        section("authentication", module(&["service"])),
        section("forwarding", module(&["mode"])),
        section(
            "servers",
            json!({ "type": "object", "additionalProperties": keys_schema::<ServerConfig>() }),
        ),
        section("routing", module(&["selector"])),
        section("translation", keys_schema::<TranslationConfig>()),
        section("storage", module(&["backend"])),
        section("limits", keys_schema::<LimitsConfig>()),
        section("logging", keys_schema::<LoggingConfig>()),
        // Host names.
        section("forced-hosts", json!({ "type": "object" })),
        section("switching", keys_schema::<SwitchingConfig>()),
        section("commands", keys_schema::<CommandsConfig>()),
        section("plugin-messages", keys_schema::<PluginMessagesConfig>()),
        section("config-version", json!({})),
        section("messaging-channel", keys_schema::<MessagingChannelConfig>()),
        section(
            crate::migrate::OLD_CHANNEL,
            keys_schema::<MessagingChannelConfig>(),
        ),
        section("messages", keys_schema::<MessagesConfig>()),
        section("virtual", keys_schema::<VirtualConfig>()),
        section("tab", keys_schema::<TabConfig>()),
        section("pumbox", keys_schema::<PumboxConfig>()),
        section("bridge", keys_schema::<crate::bridge::BridgeConfig>()),
        section("managed-servers", keys_schema::<pumbo_servers::Config>()),
    ]
}

impl Config {
    /// Parses and checks everything that does not need the modules (§3.4:
    /// the proxy refuses to start on an unsafe setup).
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        Self::parse_checked(text).map(|(cfg, _)| cfg)
    }

    /// [`Config::parse`] plus what the owner should hear about the file:
    /// keys nothing reads and old keys, with their lines.
    pub fn parse_checked(text: &str) -> Result<(Self, Remarks), ConfigError> {
        let mut cfg: Config = pumbo_core::yaml::from_str(text).map_err(ConfigError::Yaml)?;
        let value: serde_json::Value =
            pumbo_core::yaml::from_str(text).map_err(ConfigError::Yaml)?;
        let mut warnings = crate::migrate::apply(&mut cfg, &value, text);
        if cfg.translation.chain.is_empty() {
            warnings.push((
                crate::keys::line_of(text, &["translation".into(), "chain".into()]),
                "translation.chain is empty, so no player can join: add passthrough".into(),
            ));
        }
        cfg.validate().map_err(|e| match e {
            ConfigError::Key { key, message } => match crate::keys::line_of(text, &key) {
                Some(line) => ConfigError::At { line, message },
                None => ConfigError::Invalid(message),
            },
            other => other,
        })?;
        let mut unknown: Notes = crate::keys::unknown(&value, &cfg)
            .iter()
            .map(|u| (crate::keys::line_of(text, &u.path), u.message()))
            .collect();
        unknown.sort_by_key(|(line, _)| line.unwrap_or(usize::MAX));
        Ok((cfg, Remarks { unknown, warnings }))
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        for (n, l) in self.listener.iter().enumerate() {
            let at = |key: &str, message: String| ConfigError::Key {
                key: vec!["listener".into(), Seg::Index(n), key.into()],
                message,
            };
            let cidrs = self
                .trusted(l)
                .map_err(|e| at("trusted-proxies", e.to_string()))?;
            if !l.proxy_protocol.on() {
                continue;
            }
            if cidrs.is_empty() {
                return Err(at(
                    "proxy-protocol",
                    format!("listener {}: proxy-protocol needs trusted-proxies", l.bind),
                ));
            }
            if let Some(i) = cidrs.iter().position(|c| c.too_wide())
                && !l.allow_any_proxy
            {
                return Err(at(
                    "trusted-proxies",
                    format!(
                        "listener {}: trusted-proxies {} lets anyone send a PROXY header with any address (IP bans and per-IP limits stop working); list your proxy's ranges or set allow-any-proxy: true",
                        l.bind,
                        l.trusted_proxies.get(i).map_or("", String::as_str)
                    ),
                ));
            }
        }
        if !self.logging.metrics_bind.is_empty() {
            match self.logging.metrics_bind.parse::<SocketAddr>() {
                Ok(a) if a.ip().is_loopback() || self.logging.metrics_allow_remote => {}
                Ok(a) => {
                    return bad(format!(
                        "logging.metrics-bind {a} is not a loopback address: anyone who reaches the port reads player counts and server names. Set logging.metrics-allow-remote: true (and logging.metrics-token-file)"
                    ));
                }
                Err(_) => {
                    return bad(format!(
                        "logging.metrics-bind {} must be an address with a port",
                        self.logging.metrics_bind
                    ));
                }
            }
        }
        let mut lower: BTreeMap<String, &str> = BTreeMap::new();
        for (name, s) in &self.servers {
            let at = |m: String| ConfigError::key(&["servers", name, "address"], m);
            let Some((host, _)) = split_host_port(&s.address) else {
                return Err(at(format!(
                    "server {name}: address {} is not host:port",
                    s.address
                )));
            };
            // Names are resolved by `check_backend_names`, at start and reload.
            if let Ok(ip) = host.parse::<IpAddr>() {
                check_private(name, s, &[ip]).map_err(at)?;
            }
            // Permission contexts name servers in lower case.
            if let Some(other) = lower.insert(name.to_ascii_lowercase(), name) {
                return Err(ConfigError::key(
                    &["servers", name],
                    format!("servers {other} and {name} differ only in letter case; rename one"),
                ));
            }
        }
        // Servers from the proxy are known only once they are added (`Proxy`).
        if !self.managed_servers.enabled {
            self.check_server_names()?;
        }
        for id in &self.commands.operators {
            if uuid::Uuid::parse_str(id).is_err() {
                return Err(ConfigError::key(
                    &["commands", "operators"],
                    format!("commands.operators: {id} is not a UUID"),
                ));
            }
        }
        for sub in &self.messaging_channel.subchannels {
            if !MESSAGING_SUBCHANNELS.contains(&sub.as_str()) {
                return Err(ConfigError::key(
                    &["messaging-channel", "subchannels"],
                    format!(
                        "messaging-channel: unknown subchannel {sub} (known: {})",
                        MESSAGING_SUBCHANNELS.join(", ")
                    ),
                ));
            }
        }
        self.bridge
            .validate()
            .map_err(|e| ConfigError::key(&["bridge", "listen"], e))?;
        self.managed_servers.validate().map_err(|e| {
            ConfigError::key(
                &["managed-servers", "ports"],
                format!("managed-servers: {e}"),
            )
        })?;
        if self.limits.max_pending_logins == 0 || self.limits.max_concurrent_has_joined == 0 {
            return Err(ConfigError::key(
                &["limits"],
                "limits: max-pending-logins and max-concurrent-has-joined must be > 0".into(),
            ));
        }
        if let Err((key, message)) = crate::logging::filter(&self.logging) {
            let mut path = vec![Seg::from("logging")];
            path.extend(key.iter().map(|k| Seg::from(k.as_str())));
            return Err(ConfigError::Key { key: path, message });
        }
        Ok(())
    }

    /// Servers given by a host name: every address the name resolves to has
    /// to pass the same check as an address in the file. A name that does
    /// not resolve now is a warning (returned), not an error. Blocking DNS:
    /// for the start and `reload`, not for every refresh of the server list.
    pub fn check_backend_names(&self) -> Result<Vec<String>, ConfigError> {
        use std::net::ToSocketAddrs;
        let mut warnings = Vec::new();
        for (name, s) in &self.servers {
            let Some((host, port)) = split_host_port(&s.address) else {
                continue;
            };
            if s.allow_public || host.parse::<IpAddr>().is_ok() {
                continue;
            }
            match (host, port).to_socket_addrs() {
                Ok(addrs) => {
                    let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
                    check_private(name, s, &ips).map_err(ConfigError::Invalid)?;
                }
                Err(e) => warnings.push(format!(
                    "server {name}: {host} does not resolve now ({e}); it is checked again on reload"
                )),
            }
        }
        Ok(warnings)
    }

    /// `routing.try` and `forced-hosts` name only servers of `servers`.
    pub fn check_server_names(&self) -> Result<(), ConfigError> {
        for n in self.try_order() {
            if !self.servers.contains_key(&n) {
                return Err(ConfigError::key(
                    &["routing", "try"],
                    format!("routing.try names unknown server {n}"),
                ));
            }
        }
        for (host, order) in &self.forced_hosts {
            if let Some(n) = order.iter().find(|n| !self.servers.contains_key(*n)) {
                return Err(ConfigError::key(
                    &["forced-hosts", host],
                    format!("forced-hosts.\"{host}\" names unknown server {n}"),
                ));
            }
        }
        Ok(())
    }

    /// `routing.try`, the servers in the order players try them.
    pub fn try_order(&self) -> Vec<String> {
        match self.routing.rest.get("try") {
            Some(serde_json::Value::Array(order)) => order
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn trusted(&self, l: &ListenerConfig) -> Result<Vec<Cidr>, ConfigError> {
        l.trusted_proxies
            .iter()
            .map(|c| {
                c.parse::<Cidr>()
                    .map_err(|e| ConfigError::Invalid(format!("listener {}: {e}", l.bind)))
            })
            .collect()
    }
}

/// `host:port` with a numeric port; IPv6 hosts in brackets.
pub fn split_host_port(address: &str) -> Option<(&str, u16)> {
    let (host, port) = address.rsplit_once(':')?;
    let port = port.parse().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then_some((host, port))
}

/// Whether a backend host is local, private or in a tunnel range (§3.4).
pub fn is_private_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse().is_ok_and(is_private_ip)
}

/// Loopback, a private network, CGNAT (Tailscale, 100.64.0.0/10) or IPv6
/// unique local.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback() || v4.is_private() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    }
}

/// A server whose address (or one its name resolves to) is public needs
/// `allow-public`: modern forwarding answers are signed, not encrypted, so
/// anyone on the way can read one and log in as that player.
fn check_private(name: &str, s: &ServerConfig, ips: &[IpAddr]) -> Result<(), String> {
    match ips.iter().find(|ip| !is_private_ip(**ip)) {
        Some(ip) if !s.allow_public => Err(format!(
            "server {name}: {} ({ip}) is outside loopback and private networks; forwarding answers are signed, not encrypted, and could be replayed. Reach the server over a private network or a tunnel (WireGuard, Tailscale 100.64.x) or set servers.{name}.allow-public: true",
            s.address
        )),
        _ => Ok(()),
    }
}

/// Whether a backend host is a loopback address.
pub fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}
