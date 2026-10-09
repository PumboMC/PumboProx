//! The `pumboprox.yml` config (sketch in §3.5). Every module section has a
//! field with the module name, and the rest of the section goes to that
//! module's factory without interpretation in the core.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

use pumbo_core::registry::ModuleConfig;
use serde::Deserialize;

use crate::net::Cidr;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Not valid YAML, or a value of the wrong type (with line and column).
    #[error("{0}")]
    Yaml(String),
    #[error("{0}")]
    Invalid(String),
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
    #[serde(default)]
    pub bungeecord_channel: BungeeCordConfig,
    #[serde(default)]
    pub messages: MessagesConfig,
    #[serde(default, rename = "virtual")]
    pub virtual_world: VirtualConfig,
    #[serde(default)]
    pub tab: TabConfig,
    /// `bridge`: PumboBridge sessions of the Pumpkin servers.
    #[serde(default)]
    pub bridge: crate::bridge::BridgeConfig,
    /// Servers the proxy downloads, creates and runs (`managed-servers`).
    #[serde(default)]
    pub managed_servers: pumbo_servers::Config,
    /// `server-group` (a list), read by the plugin host (§5.8.4).
    #[serde(default, rename = "server-group")]
    pub server_groups: Vec<ModuleConfig>,
    /// Other sections, e.g. `viaproxy` for the translation provider of that name.
    #[serde(flatten)]
    pub sections: BTreeMap<String, ModuleConfig>,
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
    pub proxy_protocol: bool,
    /// CIDR ranges allowed to send a PROXY header on this listener.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    #[serde(flatten)]
    pub rest: ModuleConfig,
}

fn default_transport() -> String {
    "java-tcp".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct StatusConfig {
    /// MOTD with `&` colour codes or MiniMessage tags.
    pub motd: String,
    pub max_players: u32,
    /// Version name in the server list; empty = "PumboProx <oldest>-<newest>".
    pub version_name: String,
    /// Server icon: a 64x64 PNG (like `server-icon.png` of Velocity and
    /// Paper); empty = none.
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
    pub online_mode: OnlineMode,
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
            online_mode: OnlineMode::PerPlayer,
            encrypt_offline: false,
            prevent_proxy_connections: false,
            compression_threshold: 256,
            accept_transfers: false,
        }
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

/// Tab header and footer of the proxy (§5.8.3): MiniMessage (or `&` codes
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
    /// Hard limit for a player held by a gate in a virtual world (§5.6).
    pub gate_timeout_ms: u64,
}

impl Default for VirtualConfig {
    fn default() -> Self {
        Self {
            test_gate: false,
            test_gate_seconds: 10,
            gate_timeout_ms: 600_000,
        }
    }
}

/// Built-in commands (§2.8).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct CommandsConfig {
    /// Player UUIDs with every `pumbo.proxy.*` permission. ponytail: stands in
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

/// `bungeecord:main` for old backend plugins (§2.8, decision §9 point 8).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct BungeeCordConfig {
    pub enabled: bool,
    pub subchannels: Vec<String>,
}

pub const BUNGEECORD_SUBCHANNELS: &[&str] = &[
    "Connect",
    "ConnectOther",
    "IP",
    "UUID",
    "GetServer",
    "GetServers",
    "PlayerCount",
    "PlayerList",
];

impl Default for BungeeCordConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            subchannels: BUNGEECORD_SUBCHANNELS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// Texts of the proxy (`&` codes or MiniMessage).
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
}

impl Default for MessagesConfig {
    fn default() -> Self {
        Self {
            no_server: "No server is available right now.".into(),
            no_server_for_version:
                "This network has no server for Minecraft {version}. Please join with {supported}."
                    .into(),
            signed_chat_blocked: "Your message was blocked. Please reconnect.".into(),
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
    /// No data from a side for this long closes the session.
    pub idle_timeout_ms: u64,
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
            idle_timeout_ms: 30_000,
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
    /// `error`, `warn`, `info`, `debug` or `trace`.
    pub level: String,
    /// `false`: addresses in logs become a keyed hash (random key per run).
    pub log_ips: bool,
    /// Prometheus endpoint, loopback only; empty = off.
    pub metrics_bind: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            format: LogFormat::Text,
            level: "info".into(),
            log_ips: true,
            metrics_bind: String::new(),
        }
    }
}

impl Config {
    /// Parses and checks everything that does not need the modules (§3.4:
    /// the proxy refuses to start on an unsafe setup).
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: Config = pumbo_core::yaml::from_str(text).map_err(ConfigError::Yaml)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        for l in &self.listener {
            let cidrs = self.trusted(l)?;
            if l.proxy_protocol && cidrs.is_empty() {
                return bad(format!(
                    "listener {}: proxy-protocol: true needs trusted-proxies",
                    l.bind
                ));
            }
        }
        if !self.logging.metrics_bind.is_empty() {
            match self.logging.metrics_bind.parse::<SocketAddr>() {
                Ok(a) if a.ip().is_loopback() => {}
                _ => {
                    return bad(format!(
                        "metrics-bind {} must be a loopback address with a port",
                        self.logging.metrics_bind
                    ));
                }
            }
        }
        for (name, s) in &self.servers {
            if split_host_port(&s.address).is_none() {
                return bad(format!(
                    "server {name}: address {} is not host:port",
                    s.address
                ));
            }
        }
        // Servers from the proxy are known only once they are added (`Proxy`).
        if !self.managed_servers.enabled {
            self.check_server_names()?;
        }
        for id in &self.commands.operators {
            if uuid::Uuid::parse_str(id).is_err() {
                return bad(format!("commands.operators: {id} is not a UUID"));
            }
        }
        for sub in &self.bungeecord_channel.subchannels {
            if !BUNGEECORD_SUBCHANNELS.contains(&sub.as_str()) {
                return bad(format!(
                    "bungeecord-channel: unknown subchannel {sub} (known: {})",
                    BUNGEECORD_SUBCHANNELS.join(", ")
                ));
            }
        }
        self.bridge.validate().map_err(ConfigError::Invalid)?;
        self.managed_servers
            .validate()
            .map_err(|e| ConfigError::Invalid(format!("managed-servers: {e}")))?;
        if self.limits.max_pending_logins == 0 || self.limits.max_concurrent_has_joined == 0 {
            return bad(
                "limits: max-pending-logins and max-concurrent-has-joined must be > 0".into(),
            );
        }
        Ok(())
    }

    /// `routing.try` and `forced-hosts` name only servers of `servers`.
    pub fn check_server_names(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        for n in self.try_order() {
            if !self.servers.contains_key(&n) {
                return bad(format!("routing.try names unknown server {n}"));
            }
        }
        for (host, order) in &self.forced_hosts {
            if let Some(n) = order.iter().find(|n| !self.servers.contains_key(*n)) {
                return bad(format!("forced-hosts.\"{host}\" names unknown server {n}"));
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
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback() || v4.is_private() || (a == 100 && (64..128).contains(&b))
        }
        Ok(IpAddr::V6(v6)) => v6.is_loopback() || v6.is_unique_local(),
        Err(_) => false,
    }
}

/// Whether a backend host is a loopback address.
pub fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}
