//! The sections of `pumboprox.yml` the proxy reads, one type each.

use std::collections::BTreeMap;

use pumbo_core::registry::ModuleConfig;
use serde::Deserialize;

pub use pumbo_core::engine::SignedChatCancel;

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

/// `versions`: the client versions players may join with.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct VersionsConfig {
    /// `all`, releases (`"1.21.4"`) and ranges (`"1.21.4-26.3"`).
    pub clients: crate::versions::ClientVersions,
    /// The refusal of any other version; `{versions}` names `clients`.
    /// Empty: the built-in text.
    pub unsupported_message: String,
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
    #[serde(default)]
    pub protocol: ServerProtocol,
    /// Pass the client's `chat_session_update` on; `false` makes this backend
    /// treat all chat as unsigned (§2.8 point 4). `None`: the engine's value.
    #[serde(default)]
    pub chat_session_forwarding: Option<bool>,
    /// What a cancelled signed message does on this backend once it got the
    /// player's chat session (§2.8 point 5, result of the E4 test). `None`:
    /// the engine's value.
    #[serde(default)]
    pub signed_chat_cancel: Option<SignedChatCancel>,
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

/// `servers.<name>.protocol`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServerProtocol {
    /// The proxy asks the server. Until it answers, like `Any`.
    #[default]
    Auto,
    /// The server takes clients of several versions itself: every client is
    /// sent to it in its own version and the proxy never asks.
    Any,
    /// The owner's entry, a protocol number or a release.
    Fixed(i32),
}

impl ServerProtocol {
    /// The protocol the entry names.
    pub fn fixed(self) -> Option<i32> {
        match self {
            Self::Fixed(p) => Some(p),
            Self::Auto | Self::Any => None,
        }
    }
}

impl<'de> Deserialize<'de> for ServerProtocol {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        /// A protocol number of an entry; every protocol is above 0.
        fn fixed<E: serde::de::Error>(
            n: Option<i32>,
            written: &dyn std::fmt::Display,
        ) -> Result<ServerProtocol, E> {
            n.filter(|n| *n > 0)
                .map(ServerProtocol::Fixed)
                .ok_or_else(|| E::custom(format!("protocol {written} is no protocol number")))
        }
        struct Entry;
        impl serde::de::Visitor<'_> for Entry {
            type Value = ServerProtocol;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("auto, any, a protocol number or a release in quotes such as \"26.3\"")
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ServerProtocol::Auto)
            }

            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ServerProtocol::Auto)
            }

            fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<Self::Value, E> {
                fixed(i32::try_from(n).ok(), &n)
            }

            fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<Self::Value, E> {
                fixed(i32::try_from(n).ok(), &n)
            }

            // YAML reads 1.20 as the number 1.2.
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Err(E::custom("write the release in quotes: protocol: \"26.3\""))
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Self::Value, E> {
                if s.eq_ignore_ascii_case("auto") {
                    return Ok(ServerProtocol::Auto);
                }
                if s.eq_ignore_ascii_case("any") {
                    return Ok(ServerProtocol::Any);
                }
                if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
                    return fixed(s.parse().ok(), &s);
                }
                crate::versions::protocol_of(s)
                    .map(|p| ServerProtocol::Fixed(p.0))
                    .ok_or_else(|| {
                        E::custom(format!(
                            "protocol \"{s}\" is not auto, any, a protocol number or a release this build knows ({})",
                            crate::versions::known()
                        ))
                    })
            }
        }
        d.deserialize_any(Entry)
    }
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
