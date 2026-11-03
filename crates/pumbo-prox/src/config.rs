//! The `pumboprox.yml` config (sketch in §3.5). Every module section has a
//! field with the module name, and the rest of the section goes to that
//! module's factory without interpretation in the core.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

use pumbo_core::registry::ModuleConfig;
use serde::Deserialize;

pub use crate::keys::Seg;
use crate::net::Cidr;

/// The config a first start writes (`pumboprox` without a config file).
pub const DEFAULT_FILE: &str = include_str!("default-config.yml");

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
    /// Old keys still read, a newer `config-version`.
    pub warnings: Notes,
}

/// The config file as the start, `reload` and `check-config` read it: the
/// text (the plugin host reads its sections from it too), the typed config
/// and what the owner should hear about the file.
#[derive(Debug)]
pub struct Loaded {
    pub text: String,
    pub config: Config,
    /// Keys nothing reads: a warning at start and reload, an error in
    /// `check-config`. Each starts with the file and line.
    pub unknown: Vec<String>,
    /// Old keys still read (`pumboprox migrate-config` renames them).
    pub warnings: Vec<String>,
}

/// Reads and checks the config file; every reader of the file goes through
/// here, so the proxy and the plugin host see the same text.
pub fn load(path: &std::path::Path) -> Result<Loaded, String> {
    let text = read(path)?;
    let file = file_label(path);
    let (config, remarks) = Config::parse_checked(&text).map_err(|e| e.in_file(&file))?;
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
        text,
        config,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ListenerConfig {
    pub bind: String,
    /// Listener module; `java-tcp` by default.
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Read a PROXY protocol header (v1 or v2) from `trusted-proxies` (§3.4).
    #[serde(default)]
    pub proxy_protocol: ProxyProtocol,
    /// CIDR ranges allowed to send a PROXY header on this listener.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    /// Accept `trusted-proxies` wider than any provider's range (`0.0.0.0/0`).
    #[serde(default)]
    pub allow_any_proxy: bool,
    #[serde(flatten)]
    pub rest: ModuleConfig,
}

/// `proxy-protocol: true | required | optional | false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProxyProtocol {
    #[default]
    Off,
    /// `true` or `required`: only `trusted-proxies` may connect, with a header.
    Required,
    /// A header from `trusted-proxies`; other addresses connect directly.
    Optional,
}

impl ProxyProtocol {
    pub fn on(self) -> bool {
        self != Self::Off
    }
}

impl<'de> Deserialize<'de> for ProxyProtocol {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Bool(true) => Ok(Self::Required),
            Raw::Bool(false) => Ok(Self::Off),
            Raw::Text(t) => match t.as_str() {
                "true" | "required" => Ok(Self::Required),
                "optional" => Ok(Self::Optional),
                "false" => Ok(Self::Off),
                other => Err(serde::de::Error::custom(format!(
                    "proxy-protocol must be true, false, \"required\" or \"optional\", not \"{other}\""
                ))),
            },
        }
    }
}

fn default_transport() -> String {
    "java-tcp".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct StatusConfig {
    /// MOTD with `&` colour codes or text tags.
    pub motd: String,
    pub max_players: u32,
    /// Version name in the server list; empty = "PumboProx <oldest>-<newest>".
    pub version_name: String,
    /// Server icon: a 64x64 PNG (`server-icon.png` by default); empty =
    /// none.
    pub favicon: String,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            motd: concat!(
                "              &#F28C28&lPumbo&#39FF88&lProx &8┃ &7v",
                env!("CARGO_PKG_VERSION"),
                "\n      &fJava &#39FF881.21 &7– &#39FF8826.3 &8• &7built for &#F28C28Pumpkin"
            )
            .into(),
            max_players: 500,
            version_name: String::new(),
            favicon: crate::status::DEFAULT_FAVICON.into(),
        }
    }
}

/// `online-mode: per-player | true | false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineMode {
    /// Online for premium names, offline for the rest (§2.7).
    PerPlayer,
    Always,
    Never,
}

impl<'de> Deserialize<'de> for OnlineMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Bool(true) => Ok(Self::Always),
            Raw::Bool(false) => Ok(Self::Never),
            Raw::Text(t) => match t.as_str() {
                "per-player" => Ok(Self::PerPlayer),
                "true" => Ok(Self::Always),
                "false" => Ok(Self::Never),
                other => Err(serde::de::Error::custom(format!(
                    "online-mode must be \"per-player\", true or false, not \"{other}\""
                ))),
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct LoginConfig {
    /// `None` when the file does not set it: per-player then, with a
    /// warning at start; the file of a new install sets `true`.
    pub online_mode: Option<OnlineMode>,
    /// Encrypt offline connections too (`should_authenticate = false`).
    pub encrypt_offline: bool,
    /// Send the client IP to the sessionserver (`&ip=`).
    pub prevent_proxy_connections: bool,
    /// Client side compression threshold; negative = off.
    pub compression_threshold: i32,
    /// Accept handshakes with the transfer intent (3).
    pub accept_transfers: bool,
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            online_mode: None,
            encrypt_offline: false,
            prevent_proxy_connections: false,
            compression_threshold: 256,
            accept_transfers: false,
        }
    }
}

impl LoginConfig {
    pub fn online_mode(&self) -> OnlineMode {
        self.online_mode.unwrap_or(OnlineMode::PerPlayer)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthenticationConfig {
    /// Authenticator module (`mojang`).
    #[serde(default = "default_authenticator")]
    pub service: String,
    #[serde(flatten)]
    pub rest: ModuleConfig,
}

fn default_authenticator() -> String {
    "mojang".into()
}

impl Default for AuthenticationConfig {
    fn default() -> Self {
        Self {
            service: default_authenticator(),
            rest: ModuleConfig::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ForwardingConfig {
    #[serde(default = "default_forwarding")]
    pub mode: String,
    #[serde(flatten)]
    pub rest: ModuleConfig,
}

fn default_forwarding() -> String {
    "modern".into()
}

impl Default for ForwardingConfig {
    fn default() -> Self {
        Self {
            mode: default_forwarding(),
            rest: ModuleConfig::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ServerConfig {
    pub address: String,
    pub protocol: Option<i32>,
    /// Pass the client's `chat_session_update` on; `false` makes this backend
    /// treat all chat as unsigned (§2.8 point 4).
    #[serde(default = "yes")]
    pub chat_session_forwarding: bool,
    /// What a cancelled signed message does on this backend once it got the
    /// player's chat session (§2.8 point 5, result of the E4 test).
    #[serde(default)]
    pub signed_chat_cancel: SignedChatCancel,
    /// Pass this server's `transfer` packets on to the client. Without it
    /// the proxy drops them, or switches the player when one names a server
    /// of this proxy: a transfer could send players to any host.
    #[serde(default)]
    pub allow_transfers: bool,
    /// Allow an address outside loopback and private networks: forwarding
    /// answers travel unencrypted and could be replayed.
    #[serde(default)]
    pub allow_public: bool,
}

/// Cancelling a signed message leaves a gap in the signature chain. Vanilla
/// and Paper then reject every later signed message of that player until
/// they reconnect ("chat chain broken"); Pumpkin does not check (E4 test).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignedChatCancel {
    /// Drop it with a replacement `chat_ack`; the player stays.
    #[default]
    Drop,
    /// Disconnect the player with `messages.signed-chat-blocked`.
    Kick,
}

fn yes() -> bool {
    true
}

/// What happens to the previous server's resource packs on a switch (§3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PacksOnSwitch {
    Keep,
    Pop,
}

/// Server switching (§3.3).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct SwitchingConfig {
    /// Packets of the next server held while the client leaves the old one.
    pub config_buffer_bytes: usize,
    pub config_buffer_ms: u64,
    pub resource_packs_on_switch: PacksOnSwitch,
    /// Minimum time between two reconnects of one player to the same server.
    pub reconnect_cooldown_ms: u64,
    /// A kick whose text contains one of these (case-insensitive), or the
    /// vanilla shutdown message, moves the player to the next server instead
    /// of disconnecting (§3.2).
    pub fallback_reasons: Vec<String>,
}

impl Default for SwitchingConfig {
    fn default() -> Self {
        Self {
            config_buffer_bytes: 16 * 1024 * 1024,
            config_buffer_ms: 10_000,
            resource_packs_on_switch: PacksOnSwitch::Pop,
            reconnect_cooldown_ms: 30_000,
            fallback_reasons: [
                "server closed",
                "server stopped",
                "restarting",
                "shutting down",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

/// Tab header and footer of the proxy (§5.8.3): text tags (or `&` codes
/// without plugins) with placeholders, rendered per player from push and
/// cached values; empty = the backend decides. They replace the backend's.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct TabConfig {
    pub header: String,
    pub footer: String,
    pub refresh_ms: u64,
}

impl Default for TabConfig {
    fn default() -> Self {
        Self {
            header: String::new(),
            footer: String::new(),
            refresh_ms: 1000,
        }
    }
}

/// The virtual world (PumboAPI, §5).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct VirtualConfig {
    /// Tests only, until plugin gates (E5): every player first stands in a
    /// void world of the proxy (platform, title, bossbar, map in hand) and is
    /// released to a server after `test-gate-seconds` (0: only on
    /// `/gate release`). Console: `gate <player>` sends a player there from
    /// a server.
    pub test_gate: bool,
    pub test_gate_seconds: u64,
    /// Hard limit for a player held by the gates in a virtual world (§5.6),
    /// for the whole stay: it starts once, while each gate's own timeout
    /// (`plugins.gate-timeout-ms`) starts with that gate.
    pub gate_timeout_ms: u64,
}

impl Default for VirtualConfig {
    fn default() -> Self {
        Self {
            test_gate: false,
            test_gate_seconds: 10,
            gate_timeout_ms: 120_000,
        }
    }
}

/// The Pumbox: an empty world of the proxy where players wait while no
/// server can take them, instead of a "no server" kick.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PumboxConfig {
    pub enabled: bool,
}

impl Default for PumboxConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Built-in commands (§2.8).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct CommandsConfig {
    /// Player UUIDs with every `pumbo.proxy.*` permission. Stands in
    /// for the permission system until E5; `/server` is open to everyone.
    pub operators: Vec<String>,
    /// Command names that never reach a backend (e.g. `login`), also when no
    /// plugin handles them; plugin manifests add theirs (E5).
    pub sensitive: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PluginMessagesConfig {
    /// Appended to the server brand the client sees (F3); empty = unchanged.
    pub server_brand_suffix: String,
}

impl Default for PluginMessagesConfig {
    fn default() -> Self {
        Self {
            server_brand_suffix: " (PumboProx)".into(),
        }
    }
}

/// `messaging-channel`: the `bungeecord:main` channel for old backend
/// plugins (§2.8, decision §9 point 8).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct MessagingChannelConfig {
    pub enabled: bool,
    pub subchannels: Vec<String>,
}

pub const MESSAGING_SUBCHANNELS: &[&str] = &[
    "Connect",
    "ConnectOther",
    "IP",
    "UUID",
    "GetServer",
    "GetServers",
    "PlayerCount",
    "PlayerList",
];

impl Default for MessagingChannelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            subchannels: MESSAGING_SUBCHANNELS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// Texts of the proxy (`&` codes or text tags).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct MessagesConfig {
    /// No server could be reached.
    pub no_server: String,
    /// No server speaks the client's version; `{version}` is the client's
    /// release, `{supported}` the releases of the servers.
    pub no_server_for_version: String,
    /// Kick text for `signed-chat-cancel: kick`.
    pub signed_chat_blocked: String,
    /// `limits.max-players` reached.
    pub network_full: String,
    /// `limits.max-in-gates` reached.
    pub gates_full: String,
}

impl Default for MessagesConfig {
    fn default() -> Self {
        Self {
            no_server: "No server is available right now.".into(),
            no_server_for_version:
                "This network has no server for Minecraft {version}. Please join with {supported}."
                    .into(),
            signed_chat_blocked: "Your message was blocked. Please reconnect.".into(),
            network_full: "The network is full. Please try again later.".into(),
            gates_full: "Too many players are logging in right now. Please try again in a moment."
                .into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutingConfig {
    #[serde(default = "default_selector")]
    pub selector: String,
    #[serde(flatten)]
    pub rest: ModuleConfig,
}

fn default_selector() -> String {
    "try".into()
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            selector: default_selector(),
            rest: ModuleConfig::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TranslationConfig {
    /// Provider order; the first with a decision wins.
    #[serde(default = "default_chain")]
    pub chain: Vec<String>,
}

fn default_chain() -> Vec<String> {
    vec![
        "multiversion".into(),
        "viaproxy".into(),
        "passthrough".into(),
    ]
}

impl Default for TranslationConfig {
    fn default() -> Self {
        Self {
            chain: default_chain(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_store")]
    pub backend: String,
    #[serde(flatten)]
    pub rest: ModuleConfig,
}

fn default_store() -> String {
    "memory".into()
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: default_store(),
            rest: ModuleConfig::new(),
        }
    }
}

/// Native limits, before any plugin (§2.7).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct LimitsConfig {
    pub connections_per_ip_per_second: u32,
    pub concurrent_per_ip: u32,
    pub status_per_ip_per_second: u32,
    /// Connections that have not finished login yet, proxy-wide.
    pub max_pending_logins: usize,
    /// Requests to the authentication service in flight, proxy-wide.
    pub max_concurrent_has_joined: usize,
    pub client_packets_per_second: u32,
    pub client_bytes_per_second: u32,
    pub command_suggestions_per_second: u32,
    pub handshake_timeout_ms: u64,
    pub login_timeout_ms: u64,
    /// TCP connect plus login with a backend.
    pub backend_login_timeout_ms: u64,
    /// No data from a side for this long closes the session.
    pub idle_timeout_ms: u64,
    /// Players online at most; 0 = no limit. Operators and
    /// `pumbo.proxy.join-full` join anyway. `status.max-players` only shows.
    pub max_players: u32,
    /// Players a gate holds in a virtual world at once; 0 = no limit.
    pub max_in_gates: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            connections_per_ip_per_second: 3,
            concurrent_per_ip: 5,
            status_per_ip_per_second: 10,
            max_pending_logins: 2000,
            max_concurrent_has_joined: 64,
            client_packets_per_second: 500,
            client_bytes_per_second: 1_048_576,
            command_suggestions_per_second: 10,
            handshake_timeout_ms: 5000,
            login_timeout_ms: 30_000,
            backend_login_timeout_ms: 30_000,
            idle_timeout_ms: 30_000,
            max_players: 0,
            max_in_gates: 500,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct LoggingConfig {
    pub format: LogFormat,
    /// `error`, `warn`, `info`, `debug`, `trace` or `off`.
    pub level: String,
    /// Levels per module (`pumbo_host: debug`), over `level`.
    pub filters: BTreeMap<String, String>,
    /// `false`: addresses in logs become a keyed hash (random key per run).
    pub log_ips: bool,
    /// Prometheus endpoint with `/health` and `/ready`; empty = off.
    pub metrics_bind: String,
    /// `metrics-bind` may be an address other hosts reach.
    pub metrics_allow_remote: bool,
    /// File with the token `/metrics` wants as `Authorization: Bearer`;
    /// empty = none.
    pub metrics_token_file: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            format: LogFormat::Text,
            level: "info".into(),
            filters: BTreeMap::new(),
            log_ips: true,
            metrics_bind: String::new(),
            metrics_allow_remote: false,
            metrics_token_file: String::new(),
        }
    }
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
        let warnings = crate::migrate::apply(&mut cfg, &value, text);
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
