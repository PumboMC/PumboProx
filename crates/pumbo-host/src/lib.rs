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
mod host;
mod http;
mod imports;
mod inputs;
mod load;
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
pub use permissions::{Decision, Layer, Layers, levels, resolve};
pub use services::{Native, NativeService};

pub use load::LoadFailure;

use crate::manifest::EventKind;
use crate::wit::admin::PluginDescription;
use crate::wit::servers::ServerInfo;
use crate::wit::types::{PlayerContext, PlayerId, PlayerInfo};

/// Proxy nodes as strong as an operator: no wildcard grants them, only an
/// entry for exactly the node ([`Host::exact_permission_decision`]).
/// `pumbo.proxy.op` also needs an operator.
pub const EXACT_ONLY: &[&str] = &[
    "pumbo.proxy.op",
    "pumbo.proxy.bridge.key",
    "pumbo.proxy.route.edit",
];

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
    /// Built for a tool (`check-config`, `describe`): nothing is written,
    /// no plugin directories and no default files.
    pub offline: bool,
}

/// How often players without a set from the provider are loaded again.
const PROVIDER_RETRY: Duration = Duration::from_secs(5);

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
        .await
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

    /// The server list with player counts from the players online now
    /// (the proxy pushes the list without them).
    pub fn server_list(&self) -> Vec<ServerInfo> {
        let mut counts: HashMap<String, u32> = HashMap::new();
        if let Ok(players) = self.players.read() {
            for s in players.values().filter(|p| !p.in_virtual) {
                if let Some(s) = &s.server {
                    *counts.entry(s.clone()).or_default() += 1;
                }
            }
        }
        self.servers
            .read()
            .map(|s| {
                s.values()
                    .map(|i| ServerInfo {
                        players: counts.get(&i.name).copied().unwrap_or(0),
                        ..i.clone()
                    })
                    .collect()
            })
            .unwrap_or_default()
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

    /// A message from the config (text tags and style tags).
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
        if self.is_permission_provider(slot) {
            self.forget_offline(None);
            self.export_changed();
            if let Some(me) = self.me.get().and_then(Weak::upgrade) {
                let players = self.perms.eligible();
                let instance = self
                    .perms
                    .instance
                    .load(std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move { me.provider_started(players, instance).await });
            }
        }
    }

    /// Backends get the permission export again (PumboBridge `perms-export`).
    pub(crate) fn export_changed(&self) {
        self.bridge.permissions_export_changed();
    }

    /// Called by the actor when an instance ends (trap, reload, stop).
    pub(crate) fn plugin_down(&self, slot: &Arc<PluginSlot>) {
        self.services_of_changed(slot, false);
        if self.is_permission_provider(slot) {
            self.perms
                .instance
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Players keep the provider's last set (a down provider must not
            // give back ranks it took away through permissions.yml).
            self.forget_offline(None);
            if let Ok(mut a) = self.perms.accepted.lock() {
                *a = None;
            }
            self.export_changed();
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
