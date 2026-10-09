//! PumboProx plugin host (plan §4, §5.8, §6.6).
//!
//! - one wasmtime instance per plugin in an actor with a mailbox; every event
//!   is a separate task in the instance, so a plugin may call imports that
//!   lead to events of its own (scenario #2056) without deadlocks,
//! - imports never wait for events to be dispatched: they read host data,
//!   queue a command for the proxy ([`bridge::ProxyBridge`]) or are futures,
//! - permissions and placeholders are host data; checks never call plugins,
//! - every event with a result has a deadline and an `on-failure` rule,
//! - a trapped instance is replaced with backoff; required gates and plugins
//!   that are missing close logins (fail-closed).
//!
//! The proxy core talks to the host through [`Host`]: it pushes player and
//! server state, asks for event results and dispatches proxy commands.
//!
//! Design note: Pumpkin's host serialises synchronous guest roots across
//! stores (`LegacySyncReentry`), which deadlocks when a callback chain needs
//! a store that is busy further up the chain. Here there are no synchronous
//! cross-store calls at all: plugin-to-plugin traffic is queued into the
//! callee's mailbox and awaited as a future, and no host lock is held while a
//! plugin runs.

// Outcomes carry a text component (368 bytes) once per login step or event;
// boxing them would only add allocations.
#![allow(clippy::large_enum_variant, clippy::result_large_err)]

mod actor;
mod admin;
pub mod bridge;
mod commands;
pub mod config;
mod crypto;
mod dispatch;
mod http;
mod imports;
pub mod manifest;
mod permissions;
mod placeholders;
mod runtime;
mod schema;
mod services;
mod text;

#[allow(unsafe_code, missing_docs, clippy::all)]
mod wit_bindings {
    wasmtime::component::bindgen!({
        path: "../../wit",
        world: "plugin",
        exports: { default: async | store },
        with: {
            "pumbo:prox/bossbar.bar": crate::imports::BarHandle,
            "pumbo:prox/virtual.world": crate::imports::WorldHandle,
            "pumbo:prox/virtual.map-image": crate::imports::MapHandle,
        },
        additional_derives: [PartialEq],
    });
}

/// Types of the `pumbo:prox` interface, shared with the proxy core.
pub mod wit {
    pub use crate::wit_bindings::exports::pumbo::prox::events;
    pub use crate::wit_bindings::pumbo::prox::*;
}

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use pumbo_core::permissions::Subject;
use pumbo_text::{Component, StyleSheet};
use tokio::sync::{Semaphore, oneshot};

pub use actor::{CallError, PluginSlot, Status};
pub use admin::{HELP_PER_PAGE, HelpEntry, HelpHeader, help_page};
pub use bridge::{BossbarCommand, NoProxy, PlayerCommand, ProxyBridge};
pub use commands::{CommandOutcome, CommandSender, VisibleCommand};
pub use config::HostConfig;
pub use dispatch::{ConnectDecision, GateOutcome, KickDecision, PreLogin, StatusOutcome};
pub use manifest::Manifest;
pub use permissions::{Decision, Layer, levels, resolve};
pub use services::{Native, NativeService};

use crate::manifest::EventKind;
use crate::wit::admin::PluginDescription;
use crate::wit::servers::ServerInfo;
use crate::wit::types::{PlayerContext, PlayerId, PlayerInfo};

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("plugin host config: {0}")]
    Config(String),
    #[error("plugin {id}: {message}")]
    Plugin { id: String, message: String },
    #[error("wasm runtime: {0}")]
    Runtime(String),
}

/// Shared state of the host. Locks are std locks held only for short reads
/// and writes, never across an `await`.
pub(crate) struct HostInner {
    pub cfg: HostConfig,
    pub runtime: runtime::Runtime,
    pub bridge: Arc<dyn ProxyBridge>,
    pub styles: StyleSheet,
    /// By id; iteration order is the load order.
    pub plugins: BTreeMap<String, Arc<PluginSlot>>,
    pub players: RwLock<HashMap<PlayerId, PlayerInfo>>,
    pub servers: RwLock<BTreeMap<String, ServerInfo>>,
    pub commands: commands::Commands,
    pub perms: permissions::Permissions,
    /// Players held by a gate: (player, plugin id) → release signal.
    pub held: Mutex<HashMap<(PlayerId, String), oneshot::Sender<()>>>,
    pub http: http::Http,
    pub crypto: Semaphore,
    pub services: services::Services,
    pub placeholders: placeholders::Placeholders,
    pub metrics: admin::Metrics,
    /// Itself, for tasks and guards started from `&self`.
    pub me: OnceLock<Weak<HostInner>>,
    login_closed_logged: Mutex<Option<Instant>>,
    /// `plugins.required-gates`, changeable while running (`/prox route gates`).
    required_gates: RwLock<Vec<String>>,
    /// Plugin files that did not load (shown by `/pumbo`).
    pub failed: Vec<LoadFailure>,
    /// The permission provider plugin (`permissions.provider`, resolved at start).
    pub perm_provider: Option<String>,
    pub provider_down_logged: Mutex<Option<Instant>>,
}

/// Handle to the plugin host. Cheap to clone.
#[derive(Clone)]
pub struct Host {
    inner: Arc<HostInner>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("plugins", &self.inner.plugins.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Host {
    /// Loads manifests from the plugin directory and starts every plugin.
    /// Returns once the actors run; `wait_started` waits for their `init`.
    pub async fn start(cfg: HostConfig, bridge: Arc<dyn ProxyBridge>) -> Result<Host, HostError> {
        Host::build(cfg, bridge, Vec::new(), true).await
    }

    /// [`Host::start`] with services the host provides itself.
    pub async fn start_with(
        cfg: HostConfig,
        bridge: Arc<dyn ProxyBridge>,
        natives: Vec<Native>,
    ) -> Result<Host, HostError> {
        Host::build(cfg, bridge, natives, true).await
    }

    /// Loads manifests without starting plugins (for tools).
    async fn build(
        cfg: HostConfig,
        bridge: Arc<dyn ProxyBridge>,
        natives: Vec<Native>,
        spawn: bool,
    ) -> Result<Host, HostError> {
        let runtime = runtime::Runtime::new().map_err(|e| HostError::Runtime(format!("{e:#}")))?;
        let (file, warnings) = permissions::FileSource::load(&cfg.permissions.file, &cfg)
            .map_err(HostError::Config)?;
        for w in warnings {
            tracing::warn!("permissions: {w}");
        }
        let (manifests, failed) = load_manifests(&cfg.plugins.dir)?;
        let mut plugins = BTreeMap::new();
        let mut receivers = Vec::new();
        for (manifest, text, wasm) in manifests {
            let scope = cfg
                .plugins
                .scopes
                .get(&manifest.id)
                .cloned()
                .unwrap_or_default();
            let (slot, rx) = PluginSlot::new(manifest, text, wasm, scope, cfg.plugins.mailbox);
            receivers.push((Arc::clone(&slot), rx));
            plugins.insert(slot.id.clone(), slot);
        }
        let commands = commands::Commands::new(&plugins).map_err(HostError::Config)?;
        check_unique(&plugins)?;
        let services = services::Services::new(&plugins, &cfg.services.providers, natives)
            .map_err(HostError::Config)?;
        let placeholders =
            placeholders::Placeholders::new(&plugins, &cfg).map_err(HostError::Config)?;
        check_failures(&cfg.plugins, &plugins, &failed)?;
        let perm_provider = permission_provider(&cfg, &plugins);
        let blocked = services.blocked(&plugins);
        let styles = StyleSheet::new(cfg.style.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let mut http = http::Http::new(cfg.plugins.http_allow_private);
        http.allow_plain = cfg.plugins.http_allow_plain_for_tests;
        let inner = Arc::new(HostInner {
            runtime,
            bridge,
            styles,
            plugins,
            players: RwLock::new(HashMap::new()),
            servers: RwLock::new(BTreeMap::new()),
            commands,
            perms: permissions::Permissions {
                file: RwLock::new(file),
                online: RwLock::new(HashMap::new()),
            },
            held: Mutex::new(HashMap::new()),
            http,
            crypto: Semaphore::new(4),
            services,
            placeholders,
            metrics: admin::Metrics::default(),
            me: OnceLock::new(),
            login_closed_logged: Mutex::new(None),
            required_gates: RwLock::new(cfg.plugins.required_gates.clone()),
            failed,
            perm_provider,
            provider_down_logged: Mutex::new(None),
            cfg,
        });
        let _ = inner.me.set(Arc::downgrade(&inner));
        if spawn {
            for (slot, rx) in receivers {
                if let Some((_, why)) = blocked.iter().find(|(id, _)| *id == slot.id) {
                    tracing::error!(plugin = %slot.id, "not started: {why}");
                    slot.set_status(Status::Disabled(why.clone()));
                    continue;
                }
                if slot.status() == Status::Missing {
                    tracing::error!(plugin = %slot.id, "{} is missing", slot.wasm.display());
                }
                actor::spawn(Arc::clone(&inner), slot, rx);
            }
        }
        Ok(Host { inner })
    }

    /// Waits until no plugin is in `Starting` (at most `timeout`). A required
    /// gate or plugin that failed to load (its `.wasm` does not compile) is
    /// an error: the proxy does not start (fail-closed).
    pub async fn wait_started(&self, timeout: Duration) -> Result<(), HostError> {
        for slot in self.inner.plugins.values() {
            let mut rx = slot.subscribe_status();
            let _ = tokio::time::timeout(timeout, rx.wait_for(|s| *s != Status::Starting)).await;
            if let Status::Failed(why) = slot.status()
                && required(&self.inner.cfg.plugins, &slot.manifest)
            {
                return Err(required_error(&slot.id, &why));
            }
        }
        Ok(())
    }

    pub fn plugin(&self, id: &str) -> Option<Arc<PluginSlot>> {
        self.inner.plugins.get(id).cloned()
    }

    pub fn plugins(&self) -> Vec<Arc<PluginSlot>> {
        self.inner.plugins.values().cloned().collect()
    }

    /// Reloads a plugin from its file, or starts an unloaded, disabled or
    /// missing one (`/pumbo proxy plugin reload|load <id>`).
    pub async fn reload_plugin(&self, id: &str) -> Result<(), String> {
        let slot = self
            .plugin(id)
            .ok_or_else(|| format!("unknown plugin {id}"))?;
        slot.reload().await
    }

    /// Stops every plugin (shutdown export, bounded).
    pub async fn shutdown(&self) {
        for slot in self.inner.plugins.values() {
            slot.stop();
        }
        for slot in self.inner.plugins.values() {
            let mut rx = slot.subscribe_status();
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                rx.wait_for(|s| {
                    matches!(s, Status::Stopped | Status::Missing | Status::Disabled(_))
                }),
            )
            .await;
        }
    }

    /// `Ok` when every required gate and plugin is loaded (plan §4.4 item 8a);
    /// otherwise the message for the player. Status and ping work either way.
    pub fn login_open(&self) -> Result<(), Component> {
        let inner = &self.inner;
        let cfg = &inner.cfg.plugins;
        let mut missing = Vec::new();
        for gate in &self.required_gates() {
            let up = inner.plugins.values().any(|s| {
                s.manifest.gate.as_ref().is_some_and(|g| &g.name == gate) && s.status().is_coming()
            });
            if !up {
                missing.push(format!("gate {gate}"));
            }
        }
        for id in &cfg.required_plugins {
            if !inner
                .plugins
                .get(id)
                .is_some_and(|s| s.status().is_coming())
            {
                missing.push(format!("plugin {id}"));
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        if let Ok(mut last) = inner.login_closed_logged.lock()
            && last.is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
        {
            *last = Some(Instant::now());
            tracing::error!("logins closed, missing: {}", missing.join(", "));
        }
        Err(inner.message(&inner.cfg.plugins.messages.login_unavailable))
    }

    /// The proxy's admin commands from `/prox`: `plugin reload|load|unload
    /// <id>`, `perms list|check …`, `services`.
    pub fn proxy_admin(&self, sender: CommandSender, args: &[String]) -> CommandOutcome {
        admin::proxy_admin(&self.inner, sender, args)
    }

    /// `plugins.required-gates` as it is now.
    pub fn required_gates(&self) -> Vec<String> {
        self.inner
            .required_gates
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// New `plugins.required-gates` (a config reload): logins close at once
    /// while one of them is missing.
    pub fn set_required_gates(&self, gates: Vec<String>) {
        if let Ok(mut g) = self.inner.required_gates.write() {
            *g = gates;
        }
    }

    // --- state pushed by the proxy core ---

    /// A player logged in to the proxy (before gates). The host fills in the
    /// context from `server` and loads the file permission layer.
    pub fn player_joined(&self, mut info: PlayerInfo) {
        let inner = &self.inner;
        info.context = inner.context_of(info.server.as_deref());
        let who = subject(&info);
        inner.perms.load_file_layer(info.id, &who);
        if let Ok(mut p) = inner.players.write() {
            p.insert(info.id, info);
        }
    }

    /// Changes stored player data (brand, settings, profile, virtual flag).
    /// Server changes go through [`Host::player_server_changed`].
    pub fn player_update(&self, id: PlayerId, f: impl FnOnce(&mut PlayerInfo)) {
        if let Ok(mut p) = self.inner.players.write()
            && let Some(info) = p.get_mut(&id)
        {
            let server = info.server.clone();
            f(info);
            info.server = server;
        }
    }

    pub fn player(&self, id: PlayerId) -> Option<PlayerInfo> {
        self.inner.player(id)
    }

    /// The player left: `on-disconnect` to plugins, then cleanup.
    pub async fn player_left(&self, id: PlayerId) {
        self.inner.notify_disconnect(id).await;
        let inner = &self.inner;
        inner.perms.remove(id);
        if let Ok(mut h) = inner.held.lock() {
            h.retain(|(p, _), _| *p != id);
        }
        for slot in inner.plugins.values() {
            imports::forget_viewer(slot, id);
        }
        inner.placeholders.player_left(id);
        if let Ok(mut p) = inner.players.write() {
            p.remove(&id);
        }
    }

    /// Servers known to the proxy (with ping state); groups come from the host config.
    pub fn set_servers(&self, servers: Vec<ServerInfo>) {
        let inner = &self.inner;
        if let Ok(mut s) = inner.servers.write() {
            s.clear();
            for mut info in servers {
                info.groups = inner.cfg.groups_of(&info.name);
                s.insert(info.name.clone(), info);
            }
        }
    }

    /// Permission check in the player's current context (or explicit).
    pub fn has_permission(&self, id: PlayerId, node: &str, server: Option<&str>) -> bool {
        let inner = &self.inner;
        let lv = match server {
            Some(s) => permissions::levels(Some(s), &inner.cfg.groups_of(s)),
            None => inner.player_levels(id),
        };
        inner.has(id, node, &lv)
    }

    /// The permission decision of the host, `None` without a matching entry
    /// (the proxy keeps its own default then).
    pub fn permission_decision(
        &self,
        id: PlayerId,
        node: &str,
        server: Option<&str>,
    ) -> Option<bool> {
        let inner = &self.inner;
        let lv = match server {
            Some(s) => permissions::levels(Some(s), &inner.cfg.groups_of(s)),
            None => inner.player_levels(id),
        };
        inner.perms.decide(id, node, &lv).map(|d| d.value)
    }

    /// Server groups of a server (`[[server-group]]`, config order).
    pub fn groups_of(&self, server: &str) -> Vec<String> {
        self.inner.cfg.groups_of(server)
    }

    /// Decisions for a player on a backend (PumboBridge `perm-set`, spec
    /// §5.1): every node of `catalog` and every namespaced node (`ns:…`) of
    /// the player's entries, resolved in that server's context; only nodes
    /// with a decision. Wildcards of the table apply through the catalog.
    pub fn backend_permissions(
        &self,
        id: PlayerId,
        server: &str,
        catalog: &[String],
    ) -> BTreeMap<String, bool> {
        let inner = &self.inner;
        let lv = permissions::levels(Some(server), &inner.cfg.groups_of(server));
        let Ok(online) = inner.perms.online.read() else {
            return BTreeMap::new();
        };
        let Some(p) = online.get(&id) else {
            return BTreeMap::new();
        };
        let (file, provider) = p.layers();
        let mut nodes: std::collections::BTreeSet<String> =
            catalog.iter().map(|n| n.to_ascii_lowercase()).collect();
        nodes.extend(
            file.iter()
                .chain(provider)
                .filter(|e| e.node.contains(':') && !e.node.ends_with('*'))
                .map(|e| e.node.clone()),
        );
        nodes
            .into_iter()
            .filter_map(|n| {
                let d = permissions::resolve(file, provider, &n, &lv)?;
                Some((n, d.value))
            })
            .collect()
    }

    /// The permission table for a server behind the proxy as a PumboPerms
    /// export (JSON text, PumboBridge `perms-export`): the provider's data
    /// while it runs (marked `proxy-rules`), else `permissions.yml` in that
    /// server's context.
    pub async fn permissions_export(&self, server: &str) -> String {
        let inner = &self.inner;
        let groups = inner.cfg.groups_of(server);
        if inner.provider_slot().is_some() {
            let req = pumbo_contracts::PermissionsExportRequest {
                server: server.to_string(),
                groups: groups.clone(),
            };
            match inner
                .call_provider::<pumbo_contracts::PermissionsExport>(
                    pumbo_contracts::METHOD_EXPORT,
                    &req,
                    5000,
                )
                .await
            {
                Ok(x) => return x.data,
                Err(e) => tracing::warn!(
                    "{server}: the permission provider has no export ({e}), permissions.yml instead"
                ),
            }
        }
        inner
            .perms
            .file
            .read()
            .map(|f| f.export(server, &groups).to_string())
            .unwrap_or_default()
    }

    /// Publishes a host topic (`pumbo:bridge-event`, …) to its subscribers.
    pub fn publish(&self, topic: &pumbo_contracts::Contract, payload: Vec<u8>) {
        if let Err(e) = self.inner.publish_native(topic, payload) {
            tracing::warn!("bus {}: {e}", topic.name);
        }
    }

    /// `%server_<key>:<server>%` from PumboBridge (`None` clears).
    pub fn set_server_value(&self, server: &str, key: &str, value: Option<String>) {
        if let Ok(mut m) = self.inner.placeholders.server_values.lock() {
            let k = (server.to_string(), key.to_string());
            match value {
                Some(v) => m.insert(k, v),
                None => m.remove(&k),
            };
        }
    }

    /// `%player_<key>%` from PumboBridge (`None` clears).
    pub fn set_player_value(&self, id: PlayerId, key: &str, value: Option<String>) {
        if let Ok(mut m) = self.inner.placeholders.player_values.lock() {
            let k = (id, key.to_string());
            match value {
                Some(v) => m.insert(k, v),
                None => m.remove(&k),
            };
        }
    }

    /// A sensitive command name from a manifest (never sent to a backend).
    pub fn is_sensitive_command(&self, name: &str) -> bool {
        self.inner.commands.is_sensitive(&name.to_ascii_lowercase())
    }

    /// Whether any plugin may want plugin messages on this channel (cheap
    /// check before `on_plugin_message`).
    pub fn listens_channel(&self, channel: &str) -> bool {
        self.inner.listeners(EventKind::PluginMessage).any(|s| {
            s.channels
                .lock()
                .map(|c| c.contains(channel))
                .unwrap_or(false)
        })
    }

    /// Commands of the proxy and plugins visible to a player now (for the
    /// command tree).
    pub fn visible_commands(&self, id: PlayerId) -> Vec<VisibleCommand> {
        self.inner.commands.visible(&self.inner, id)
    }
}

/// Shared services and descriptions (part B of E5).
impl Host {
    /// Plugin metrics for the proxy's Prometheus endpoint (plan §2.11).
    pub fn render_metrics(&self) -> String {
        self.inner.render_metrics()
    }

    /// The description of a running plugin as JSON (plan §6.6.5).
    pub fn describe_json(&self, id: &str) -> Option<serde_json::Value> {
        let slot = self.plugin(id)?;
        let d = slot.description()?;
        Some(admin::description_json(&slot, &d))
    }

    /// The global config of a plugin with `x-pumbo-secret` fields hidden.
    pub fn config_view(&self, id: &str) -> Result<serde_json::Value, String> {
        let slot = self
            .plugin(id)
            .ok_or_else(|| format!("unknown plugin {id}"))?;
        self.inner.config_view(&slot)
    }

    /// Validates the config files of a plugin (and its overlays) against
    /// the schema of its description.
    pub fn validate_config(&self, id: &str) -> Result<(), String> {
        let slot = self
            .plugin(id)
            .ok_or_else(|| format!("unknown plugin {id}"))?;
        self.inner.validate_config(&slot)
    }

    /// `%proxy_max%`.
    pub fn set_max_players(&self, n: u32) {
        self.inner
            .placeholders
            .max_players
            .store(n, std::sync::atomic::Ordering::Relaxed);
    }

    /// `%player_ping%`.
    pub fn set_ping(&self, id: PlayerId, ms: u32) {
        if let Ok(mut p) = self.inner.placeholders.pings.lock() {
            p.insert(id, ms);
        }
    }

    /// A template of the proxy config for a recipient (kicks, `/alert`),
    /// waiting for pull values up to their deadline.
    pub async fn render(
        &self,
        mini: &str,
        args: &[(String, String)],
        player: Option<PlayerId>,
    ) -> Component {
        let t = wit::types::TextTemplate {
            mini: mini.to_string(),
            args: args.to_vec(),
        };
        let out = self
            .inner
            .resolve_template(&t, player, &wit::types::QueryContext::Current, None)
            .await
            .unwrap_or_else(|e| pumbo_text::template::escape_mini(&format!("({e:?})")));
        self.inner.message(&out)
    }

    /// The same without waiting (status, Tab): push and cached values only.
    pub fn render_now(
        &self,
        mini: &str,
        args: &[(String, String)],
        player: Option<PlayerId>,
    ) -> Component {
        let t = wit::types::TextTemplate {
            mini: mini.to_string(),
            args: args.to_vec(),
        };
        self.inner
            .message(&text::render_cached(&self.inner, &t, player, None))
    }
}

/// Description and config check of one plugin without starting it (`init`
/// does not run): for `pumboprox describe` and `check-config`.
pub async fn describe_offline(
    cfg: HostConfig,
    id: &str,
) -> Result<(serde_json::Value, Result<(), String>), String> {
    let host = Host::build(cfg, Arc::new(NoProxy), Vec::new(), false)
        .await
        .map_err(|e| e.to_string())?;
    describe_in(&host, id).await
}

/// Config check of every plugin (`pumboprox check-config`): manifests load,
/// descriptions are read without `init`, config files match the schemas.
pub async fn check_plugins(cfg: HostConfig) -> Result<Vec<(String, Result<(), String>)>, String> {
    let host = Host::build(cfg, Arc::new(NoProxy), Vec::new(), false)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for id in host.inner.plugins.keys() {
        let r = match describe_in(&host, id).await {
            Ok((_, check)) => check,
            Err(e) => Err(e),
        };
        out.push((id.clone(), r));
    }
    for f in &host.inner.failed {
        out.push((f.file.clone(), Err(f.reason.clone())));
    }
    Ok(out)
}

async fn describe_in(
    host: &Host,
    id: &str,
) -> Result<(serde_json::Value, Result<(), String>), String> {
    let inner = &host.inner;
    let slot = inner
        .plugins
        .get(id)
        .cloned()
        .ok_or_else(|| format!("unknown plugin {id}"))?;
    let component = inner
        .runtime
        .compile(&slot.wasm)
        .map_err(|e| format!("{e:#}"))?;
    let mut store = inner
        .runtime
        .store(inner, &slot)
        .map_err(|e| format!("{e:#}"))?;
    let instance =
        wit_bindings::Plugin::instantiate_async(&mut store, &component, &inner.runtime.linker)
            .await
            .map_err(|e| format!("{e:#}"))?;
    let desc = store
        .run_concurrent(async |acc| instance.pumbo_prox_events().call_describe(acc).await)
        .await
        .and_then(|r| r)
        .map_err(|e| format!("{e:#}"))?;
    let json = admin::description_json(&slot, &desc);
    if let Ok(mut d) = slot.description.write() {
        *d = Some(Arc::new(desc));
    }
    let check = inner.validate_config(&slot);
    Ok((json, check))
}

impl Drop for HostInner {
    fn drop(&mut self) {
        for slot in self.plugins.values() {
            slot.stop();
        }
    }
}

pub(crate) fn subject(info: &PlayerInfo) -> Subject {
    Subject {
        uuid: uuid_of(&info.profile.id),
        name: info.profile.name.clone(),
    }
}

pub fn uuid_of(u: &wit::types::Uuid) -> uuid::Uuid {
    uuid::Uuid::from_u64_pair(u.high, u.low)
}

pub fn wit_uuid(u: uuid::Uuid) -> wit::types::Uuid {
    let (high, low) = u.as_u64_pair();
    wit::types::Uuid { high, low }
}

impl HostInner {
    pub fn player(&self, id: PlayerId) -> Option<PlayerInfo> {
        self.players.read().ok()?.get(&id).cloned()
    }

    pub fn context_of(&self, server: Option<&str>) -> PlayerContext {
        PlayerContext {
            server: server.map(str::to_string),
            groups: server.map(|s| self.cfg.groups_of(s)).unwrap_or_default(),
        }
    }

    /// Levels of the player's current context; global in the virtual world
    /// and before the first backend.
    pub fn player_levels(&self, id: PlayerId) -> Vec<pumbo_core::permissions::PermContext> {
        match self.player(id) {
            Some(p) if !p.in_virtual => {
                permissions::levels(p.context.server.as_deref(), &p.context.groups)
            }
            _ => permissions::levels(None, &[]),
        }
    }

    pub fn has(
        &self,
        id: PlayerId,
        node: &str,
        lv: &[pumbo_core::permissions::PermContext],
    ) -> bool {
        match self.perms.decide(id, node, lv) {
            Some(d) => d.value,
            None => self.declared_default(node),
        }
    }

    /// Default of a node declared in a manifest (`permissions = [{ node, default }]`).
    pub fn declared_default(&self, node: &str) -> bool {
        self.plugins.values().any(|s| {
            s.manifest
                .permissions
                .iter()
                .any(|p| p.default && p.node.eq_ignore_ascii_case(node))
        })
    }

    /// A message from the config (MiniMessage with style tags).
    pub fn message(&self, mini: &str) -> Component {
        pumbo_text::parse_mini_styled(mini, &self.styles)
    }

    /// Whether a plugin gets events of this player now (plan §11, E5: plugins
    /// per server). Players without a server (login, gates, virtual world
    /// before a backend) count as inside every scope.
    pub fn in_scope(&self, slot: &PluginSlot, id: PlayerId) -> bool {
        if slot.scope.is_everywhere() {
            return true;
        }
        match self.player(id) {
            Some(p) => match &p.context.server {
                Some(server) if !p.in_virtual => slot.scope.allows(server, &p.context.groups),
                _ => true,
            },
            None => true,
        }
    }

    pub fn scope_allows_server(&self, slot: &PluginSlot, server: &str) -> bool {
        slot.scope.allows(server, &self.cfg.groups_of(server))
    }

    /// Called by the actor after `init` and `describe`.
    pub(crate) fn plugin_ready(
        &self,
        slot: &Arc<PluginSlot>,
        desc: PluginDescription,
    ) -> Result<(), String> {
        if !desc.config_schema.is_empty() {
            serde_json::from_str::<serde_json::Value>(&desc.config_schema)
                .map_err(|e| format!("describe: config schema is not JSON: {e}"))?;
        }
        if let Ok(mut d) = slot.description.write() {
            *d = Some(Arc::new(desc));
        }
        Ok(())
    }

    /// Called by the actor once the instance runs.
    pub(crate) fn plugin_running(&self, slot: &Arc<PluginSlot>) {
        tracing::info!(plugin = %slot.id, version = %slot.manifest.version, "plugin running");
        self.services_of_changed(slot, true);
        if self.is_permission_provider(slot)
            && let Some(me) = self.me.get().and_then(Weak::upgrade)
        {
            tokio::spawn(async move { me.provider_started().await });
        }
    }

    /// Called by the actor when an instance ends (trap, reload, stop).
    pub(crate) fn plugin_down(&self, slot: &Arc<PluginSlot>) {
        self.services_of_changed(slot, false);
        if self.is_permission_provider(slot) {
            // The file decides until the provider loads the players again.
            let ids = self.perms.drop_provider();
            if let Ok(mut last) = self.provider_down_logged.lock() {
                *last = None;
            }
            self.warn_file_fallback(&format!("is down ({} players online)", ids.len()));
            for id in ids {
                self.bridge.send(id, bridge::PlayerCommand::CommandsChanged);
            }
        }
        slot.cancel_timers();
        imports::hide_all_bars(self, slot);
        if let Ok(mut h) = self.held.lock() {
            h.retain(|(_, plugin), _| plugin != &slot.id);
        }
    }

    /// The player changed servers: cached placeholders of the old context go.
    pub(crate) fn context_changed(&self, id: PlayerId) {
        self.placeholders.context_changed(id);
    }

    pub(crate) fn listeners(&self, kind: EventKind) -> impl Iterator<Item = &Arc<PluginSlot>> {
        self.plugins
            .values()
            .filter(move |s| s.manifest.listens(kind))
    }
}

/// A plugin file that did not load. The proxy starts without it, like
/// Velocity skips a bad plugin, unless it is required (D-STD-4).
#[derive(Debug, Clone)]
pub struct LoadFailure {
    /// The file in the plugin directory.
    pub file: String,
    /// The manifest when it reads (also an invalid one).
    pub manifest: Option<Manifest>,
    pub reason: String,
}

/// A plugin that loads: its manifest (and the text of it) and module.
type Loaded = (Manifest, String, std::path::PathBuf);

/// Plugins in the directory: any `*.wasm` with its manifest built in
/// (`pumbo_sdk::embed!`); the id comes from the manifest, not the file name
/// (D-STD-5). A `<id>.yml` next to it overrides that manifest (builds without
/// one need it, found by the module's file name then); a `.yml` without a
/// `.wasm` reserves its names and shows the plugin as missing. Two files
/// with one id both fail. A `<id>.toml` (the format before YAML) is
/// reported, not read. A plugin that does not load is returned as a
/// failure; only an unreadable directory is an error.
fn load_manifests(dir: &Path) -> Result<(Vec<Loaded>, Vec<LoadFailure>), HostError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(e) => return Err(HostError::Config(format!("{}: {e}", dir.display()))),
    };
    let mut wasms = std::collections::BTreeSet::new();
    let mut ymls = std::collections::BTreeSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if ext == Some("toml") && !path.with_extension("yml").exists() {
            tracing::warn!(
                "found {}, this version reads YAML manifests (built into the .wasm) - delete it (see the plugin's README)",
                path.display()
            );
        }
        if path.is_file() {
            match ext {
                Some("wasm") => wasms.insert(path),
                Some("yml") => ymls.insert(path),
                _ => false,
            };
        }
    }
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let stem = |p: &Path| {
        p.file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut failed = Vec::new();
    let mut by_id: BTreeMap<String, Vec<Loaded>> = BTreeMap::new();
    let mut add = |file: String, text: Result<String, String>, id: Option<String>, wasm| {
        let parsed = text
            .map_err(|e| (None, e))
            .and_then(|t| match Manifest::parse_partial(&t) {
                Ok(m) => Ok((m, t)),
                Err((m, e)) => Err((m, e.to_string())),
            });
        match parsed {
            Ok((m, _)) if id.as_ref().is_some_and(|id| *id != m.id) => failed.push(LoadFailure {
                reason: format!("{file}: manifest id {} does not match the file name", m.id),
                file,
                manifest: Some(m),
            }),
            Ok((m, t)) => by_id.entry(m.id.clone()).or_default().push((m, t, wasm)),
            Err((manifest, e)) => failed.push(LoadFailure {
                reason: format!("{file}: {e}"),
                file,
                manifest,
            }),
        }
    };
    for wasm in wasms {
        let bytes = std::fs::read(&wasm).unwrap_or_default();
        let built_in = manifest::embedded(&bytes, manifest::MANIFEST_SECTION).map(|b| {
            String::from_utf8(b.to_vec()).map_err(|_| "the built-in manifest is not UTF-8".into())
        });
        // The override is `<id>.yml` by the built-in id, else by the file name.
        let id = match &built_in {
            Some(Ok(t)) => match Manifest::parse_partial(t) {
                Ok(m) | Err((Some(m), _)) => Some(m.id),
                Err(_) => None,
            },
            _ => None,
        }
        .unwrap_or_else(|| stem(&wasm));
        let file = dir.join(format!("{id}.yml"));
        if ymls.remove(&file) {
            if built_in.is_some() {
                tracing::warn!(
                    "{}: the manifest file overrides the one built into {}",
                    file.display(),
                    wasm.display()
                );
            }
            let text = std::fs::read_to_string(&file).map_err(|e| e.to_string());
            add(name(&file), text, Some(id), wasm);
        } else {
            let text = built_in.unwrap_or_else(|| {
                Err(format!(
                    "no manifest built in (pumbo_sdk::embed!) and no {id}.yml next to it"
                ))
            });
            add(name(&wasm), text, None, wasm);
        }
    }
    for file in ymls {
        let id = stem(&file);
        let wasm = dir.join(format!("{id}.wasm"));
        // A module of that name was taken by the id built into it.
        let text = match wasm.exists() {
            true => Err(format!("{id}.wasm next to it is another plugin")),
            false => std::fs::read_to_string(&file).map_err(|e| e.to_string()),
        };
        add(name(&file), text, Some(id), wasm);
    }
    let mut out = Vec::new();
    for (id, mut found) in by_id {
        if found.len() == 1 {
            out.extend(found.pop());
            continue;
        }
        let files: Vec<String> = found.iter().map(|(_, _, w)| name(w)).collect();
        for (m, _, w) in found {
            failed.push(LoadFailure {
                file: name(&w),
                manifest: Some(m),
                reason: format!("duplicate plugin id {id} in {}", files.join(" and ")),
            });
        }
    }
    Ok((out, failed))
}

/// Whether `plugins.required-plugins` or `required-gates` names the plugin.
fn required(cfg: &config::PluginsConfig, m: &Manifest) -> bool {
    cfg.required_plugins.contains(&m.id)
        || m.gate
            .as_ref()
            .is_some_and(|g| cfg.required_gates.contains(&g.name))
}

/// The start error of a required plugin that did not load (fail-closed: the
/// proxy does not run with logins closed for good).
fn required_error(id: &str, reason: &str) -> HostError {
    HostError::Plugin {
        id: id.to_string(),
        message: format!(
            "{reason} - it is required (plugins.required-gates / required-plugins in pumboprox.yml), so the proxy does not start"
        ),
    }
}

/// A failure that is (or, without a readable manifest, may be) a required
/// gate or plugin stops the start; the others are logged and skipped.
fn check_failures(
    cfg: &config::PluginsConfig,
    plugins: &BTreeMap<String, Arc<PluginSlot>>,
    failed: &[LoadFailure],
) -> Result<(), HostError> {
    let unmet: Vec<String> = cfg
        .required_plugins
        .iter()
        .filter(|id| !plugins.contains_key(*id))
        .map(|id| format!("plugin {id}"))
        .chain(
            cfg.required_gates
                .iter()
                .filter(|g| {
                    !plugins
                        .values()
                        .any(|s| s.manifest.gate.as_ref().is_some_and(|d| &d.name == *g))
                })
                .map(|g| format!("gate {g}")),
        )
        .collect();
    // Failures with a manifest first: they name themselves.
    let known = failed.iter().filter(|f| f.manifest.is_some());
    for f in known.chain(failed.iter().filter(|f| f.manifest.is_none())) {
        let hit = match &f.manifest {
            Some(m) => required(cfg, m),
            None => !unmet.is_empty(),
        };
        if hit {
            let reason = match &f.manifest {
                Some(_) => f.reason.clone(),
                None => format!("{} (it may be the missing {})", f.reason, unmet.join(", ")),
            };
            let id = f.manifest.as_ref().map_or(&f.file, |m| &m.id);
            return Err(required_error(id, &reason));
        }
    }
    for f in failed {
        tracing::error!(
            "plugin not loaded, the proxy starts without it: {}",
            f.reason
        );
    }
    Ok(())
}

/// The permission provider (`permissions.provider`): `auto` takes the plugin
/// with `permission-provider: true` that provides `pumbo:permissions` (of
/// several, the one chosen in `services.providers`), `file` none. A named
/// plugin that is not installed or no provider leaves `permissions.yml` in
/// charge, with a warning (PumboPerms spec §16).
fn permission_provider(
    cfg: &HostConfig,
    plugins: &BTreeMap<String, Arc<PluginSlot>>,
) -> Option<String> {
    let able: Vec<&String> = plugins
        .values()
        .filter(|s| {
            s.manifest.permission_provider
                && s.manifest
                    .provides
                    .iter()
                    .any(|p| p.service == pumbo_contracts::PERMISSIONS.name)
        })
        .map(|s| &s.id)
        .collect();
    let file = cfg.permissions.file.display();
    match cfg.permissions.provider.as_str() {
        "file" => None,
        "auto" => match able.as_slice() {
            [] => None,
            [one] => Some((*one).clone()),
            _ => {
                let chosen = cfg
                    .services
                    .providers
                    .get(pumbo_contracts::PERMISSIONS.name)
                    .filter(|id| able.contains(id))
                    .cloned();
                if chosen.is_none() {
                    tracing::warn!(
                        "permissions.provider: auto found several providers, using {file}; choose one in services.providers"
                    );
                }
                chosen
            }
        },
        id if able.iter().any(|a| *a == id) => Some(id.to_string()),
        id => {
            tracing::warn!(
                "permissions.provider: {id} is not installed or provides no permissions, using {file}"
            );
            None
        }
    }
}

/// Start errors for names two plugins cannot share.
fn check_unique(plugins: &BTreeMap<String, Arc<PluginSlot>>) -> Result<(), HostError> {
    let mut gates = BTreeMap::new();
    let mut short = BTreeMap::new();
    for s in plugins.values() {
        if let Some(g) = &s.manifest.gate
            && let Some(other) = gates.insert(g.name.clone(), s.id.clone())
        {
            return Err(HostError::Config(format!(
                "gate \"{}\" declared by {other} and {}",
                g.name, s.id
            )));
        }
        if let Some(n) = &s.manifest.short_name
            && let Some(other) = short.insert(n.clone(), s.id.clone())
        {
            return Err(HostError::Config(format!(
                "short-name \"{n}\" declared by {other} and {}",
                s.id
            )));
        }
        if let Some(a) = &s.manifest.short_alias
            && let Some(other) = short.insert(format!("alias {a}"), s.id.clone())
        {
            return Err(HostError::Config(format!(
                "short-alias \"{a}\" declared by {other} and {}",
                s.id
            )));
        }
    }
    Ok(())
}
