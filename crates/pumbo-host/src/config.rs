//! Host sections of `pumboprox.yml` (plan §3.5): `plugins`, `services`,
//! `permissions`, `placeholders`, `server-group`, `style`. Other
//! sections of the file are ignored here.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::manifest::{EventKind, OnFailure};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct HostConfig {
    #[serde(default)]
    pub plugins: PluginsConfig,
    #[serde(default)]
    pub services: ServicesConfig,
    #[serde(default)]
    pub permissions: PermissionsConfig,
    #[serde(default)]
    pub placeholders: PlaceholdersConfig,
    #[serde(default, rename = "server-group")]
    pub server_groups: Vec<ServerGroup>,
    /// Server names from `servers` (the rest of each entry belongs to the proxy).
    #[serde(default)]
    pub servers: BTreeMap<String, serde_json::Value>,
    /// Style tags `<p>`, `<s>`, `<ok>`, `<warn>`, `<err>`, `<muted>` as
    /// MiniMessage opening tags (plan §6.6.1).
    #[serde(default = "default_style")]
    pub style: BTreeMap<String, String>,
}

impl HostConfig {
    pub fn parse(text: &str) -> Result<HostConfig, String> {
        pumbo_core::yaml::from_str(text)
    }

    /// Groups of a server in priority order (config order).
    pub fn groups_of(&self, server: &str) -> Vec<String> {
        self.server_groups
            .iter()
            .filter(|g| g.servers.iter().any(|s| s == server))
            .map(|g| g.name.clone())
            .collect()
    }
}

fn default_style() -> BTreeMap<String, String> {
    [
        ("p", "<aqua>"),
        ("s", "<gray>"),
        ("ok", "<green>"),
        ("warn", "<yellow>"),
        ("err", "<red>"),
        ("muted", "<dark_gray>"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerGroup {
    pub name: String,
    pub servers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PluginsConfig {
    pub dir: PathBuf,
    /// Missing or disabled gate from this list closes logins (plan §4.4 item 8a).
    pub required_gates: Vec<String>,
    /// The same for plugins that are not gates.
    pub required_plugins: Vec<String>,
    pub memory_mb: u32,
    /// Continuous guest work without returning to the host (D-E0-2).
    pub budget_ms: u32,
    /// How long gate events wait for a reloading or restarting gate plugin.
    pub gate_reload_wait_ms: u64,
    /// Hard limit of one gate, per gate name (default `gate-timeout-ms`).
    pub gate_timeout_ms: u64,
    pub gate_timeouts: BTreeMap<String, u64>,
    pub init_timeout_ms: u64,
    /// Event deadlines per kind, overriding the defaults.
    pub timeouts: BTreeMap<EventKind, u64>,
    /// Raises `on-failure` of an event kind for every plugin (never lowers it).
    pub on_failure: BTreeMap<EventKind, OnFailure>,
    /// HTTP from plugins to private and loopback addresses (plan §4.1).
    pub http_allow_private: bool,
    /// Tests only (not readable from the file): plain `http` URLs.
    #[doc(hidden)]
    #[serde(skip)]
    pub http_allow_plain_for_tests: bool,
    /// Plugins may change profiles of premium players.
    pub allow_premium_profile_changes: bool,
    /// Restart backoff after a trap (plan §4.4 item 7).
    pub restart_backoff_ms: Vec<u64>,
    pub max_failures: usize,
    pub failure_window_ms: u64,
    pub mailbox: usize,
    pub timers_per_plugin: usize,
    pub log_lines_per_second: u32,
    /// Messages of the host (MiniMessage with style tags).
    pub messages: HostMessages,
    /// `plugins.<id>`: where the plugin is enabled (plan §11, E5).
    #[serde(flatten)]
    pub scopes: BTreeMap<String, PluginScope>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        PluginsConfig {
            dir: PathBuf::from("plugins"),
            required_gates: Vec::new(),
            required_plugins: Vec::new(),
            memory_mb: 256,
            budget_ms: 200,
            gate_reload_wait_ms: 10_000,
            gate_timeout_ms: 300_000,
            gate_timeouts: BTreeMap::new(),
            init_timeout_ms: 10_000,
            timeouts: BTreeMap::new(),
            on_failure: BTreeMap::new(),
            http_allow_private: false,
            http_allow_plain_for_tests: false,
            allow_premium_profile_changes: false,
            restart_backoff_ms: vec![1000, 5000, 30_000],
            max_failures: 5,
            failure_window_ms: 300_000,
            mailbox: 4096,
            timers_per_plugin: 1024,
            log_lines_per_second: 100,
            messages: HostMessages::default(),
            scopes: BTreeMap::new(),
        }
    }
}

impl PluginsConfig {
    /// Deadline of an event kind.
    pub fn timeout(&self, kind: EventKind) -> Duration {
        let ms = self
            .timeouts
            .get(&kind)
            .copied()
            .unwrap_or_else(|| kind.default_timeout_ms());
        Duration::from_millis(ms)
    }

    pub fn gate_timeout(&self, gate: &str) -> Duration {
        Duration::from_millis(
            self.gate_timeouts
                .get(gate)
                .copied()
                .unwrap_or(self.gate_timeout_ms),
        )
    }

    pub fn data_dir(&self, id: &str) -> PathBuf {
        self.dir.join("data").join(id)
    }

    pub fn config_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct HostMessages {
    pub login_unavailable: String,
    pub command_unavailable: String,
    pub no_permission: String,
    pub gate_failed: String,
    pub permissions_unavailable: String,
}

impl Default for HostMessages {
    fn default() -> Self {
        HostMessages {
            login_unavailable:
                "<err>Logging in is temporarily unavailable. Please try again in a moment.".into(),
            command_unavailable: "<err>This command is temporarily unavailable.".into(),
            no_permission: "<err>You do not have permission to use this command.".into(),
            gate_failed: "<err>Could not verify the connection. Please try again.".into(),
            permissions_unavailable:
                "<err>Permissions are temporarily unavailable. Please try again in a moment.".into(),
        }
    }
}

/// Where a plugin is enabled. Empty `servers` and `groups` mean everywhere.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PluginScope {
    pub servers: Vec<String>,
    pub groups: Vec<String>,
    pub except: Vec<String>,
}

impl PluginScope {
    pub fn is_everywhere(&self) -> bool {
        self.servers.is_empty() && self.groups.is_empty() && self.except.is_empty()
    }

    /// Whether the plugin is enabled on `server` (with its groups). `except`
    /// lists servers or groups and wins over everything.
    pub fn allows(&self, server: &str, groups: &[String]) -> bool {
        let hit = |list: &[String]| list.iter().any(|s| s == server || groups.contains(s));
        if hit(&self.except) {
            return false;
        }
        if self.servers.is_empty() && self.groups.is_empty() {
            return true;
        }
        self.servers.iter().any(|s| s == server) || self.groups.iter().any(|g| groups.contains(g))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct ServicesConfig {
    pub max_depth: u32,
    pub default_timeout_ms: u32,
    pub queue_per_provider: usize,
    pub in_flight_per_pair: usize,
    pub restart_wait_ms: u64,
    /// Provider choice when several plugins declare a service.
    pub providers: BTreeMap<String, String>,
    /// Pending bus events per subscriber.
    pub bus_queue: usize,
}

impl Default for ServicesConfig {
    fn default() -> Self {
        ServicesConfig {
            max_depth: 8,
            default_timeout_ms: 1000,
            queue_per_provider: 1024,
            in_flight_per_pair: 64,
            restart_wait_ms: 2000,
            providers: BTreeMap::new(),
            bus_queue: 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LoadAt {
    AfterGates,
    AfterProfile,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OnLoadFailure {
    Deny,
    FileOnly,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PermissionsConfig {
    /// `auto` (the installed provider plugin, else the file), `file`, or
    /// the id of a provider plugin.
    pub provider: String,
    pub file: PathBuf,
    pub load_at: LoadAt,
    pub load_timeout_ms: u64,
    pub on_load_failure: OnLoadFailure,
    pub offline_timeout_ms: u32,
    pub offline_cache_ms: u64,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        PermissionsConfig {
            provider: "auto".into(),
            file: PathBuf::from("permissions.yml"),
            load_at: LoadAt::AfterGates,
            load_timeout_ms: 2000,
            on_load_failure: OnLoadFailure::Deny,
            offline_timeout_ms: 500,
            offline_cache_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Unresolved {
    Keep,
    Empty,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PlaceholdersConfig {
    pub unresolved: Unresolved,
    pub pull_timeout_ms: u64,
    pub aliases: BTreeMap<String, String>,
    /// Namespaces whose values may keep click and hover events.
    pub rich: BTreeMap<String, bool>,
    pub cache_entries: usize,
}

impl Default for PlaceholdersConfig {
    fn default() -> Self {
        PlaceholdersConfig {
            unresolved: Unresolved::Keep,
            pull_timeout_ms: 50,
            aliases: BTreeMap::new(),
            rich: BTreeMap::new(),
            cache_entries: 10_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sketch_sections_and_ignores_the_rest() {
        let cfg = HostConfig::parse(
            r#"
servers:
  lobby: { address: "127.0.0.1:25570" }

server-group:
  - name: survivals
    servers: [survival, survival2]
  - name: all
    servers: [survival, lobby]

plugins:
  dir: p
  required-gates: [auth]
  timeouts: { chat: 150 }
  pumbo-example:
    servers: [survival]

services:
  max-depth: 4
  providers:
    "pumboperms:ranks": pumbo-perms

permissions:
  provider: pumbo-perms
  on-load-failure: file-only

placeholders:
  aliases: { rank: pumboperms_rank }
"#,
        )
        .unwrap();
        assert_eq!(cfg.plugins.dir, PathBuf::from("p"));
        assert_eq!(cfg.plugins.required_gates, ["auth"]);
        assert_eq!(
            cfg.plugins.timeout(EventKind::Chat),
            Duration::from_millis(150)
        );
        assert_eq!(
            cfg.plugins.timeout(EventKind::PreLogin),
            Duration::from_secs(5)
        );
        assert_eq!(cfg.plugins.scopes["pumbo-example"].servers, ["survival"]);
        assert_eq!(cfg.services.max_depth, 4);
        assert_eq!(cfg.services.queue_per_provider, 1024);
        assert_eq!(cfg.permissions.on_load_failure, OnLoadFailure::FileOnly);
        assert_eq!(cfg.groups_of("survival"), ["survivals", "all"]);
        assert_eq!(cfg.placeholders.aliases["rank"], "pumboperms_rank");
        assert!(cfg.style.contains_key("err"));
    }

    #[test]
    fn scope_rules() {
        let everywhere = PluginScope::default();
        assert!(everywhere.allows("x", &[]));
        let s = PluginScope {
            servers: vec!["survival".into()],
            groups: vec!["minigames".into()],
            except: vec!["bedwars".into()],
        };
        assert!(s.allows("survival", &[]));
        assert!(s.allows("skywars", &["minigames".into()]));
        assert!(!s.allows("bedwars", &["minigames".into()]));
        assert!(!s.allows("lobby", &[]));
        let not_lobby = PluginScope {
            except: vec!["lobby".into()],
            ..PluginScope::default()
        };
        assert!(not_lobby.allows("survival", &[]));
        assert!(!not_lobby.allows("lobby", &[]));
    }
}
