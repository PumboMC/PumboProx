//! The running proxy: shared state, listeners, reload and shutdown.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use pumbo_core::listener::Listener;
use pumbo_core::profile::GameProfile;
use pumbo_core::routing::BackendInfo;
use pumbo_protocol::crypto::ServerKey;
use pumbo_protocol::{ProtocolVersion, VersionModule, VersionRegistry};
use pumbo_text::Component;
use tokio::sync::{Notify, Semaphore, mpsc, watch};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::{Config, ProxyProtocol, is_loopback_host, split_host_port};
use crate::limits::IpLimiter;
use crate::logging::IpDisplay;
use crate::metrics::Metrics;
use crate::modules::{self, Modules};
use crate::net::Cidr;

/// Everything that a reload replaces. Sessions in play follow the newest one
/// (`Play::follow_runtime`); a login finishes with the one it started with.
#[derive(Debug)]
pub struct Runtime {
    pub config: Config,
    pub modules: Modules,
    pub backends: Vec<BackendInfo>,
    pub motd: Component,
    /// `status.favicon` as a `data:` URL, read at start and reload.
    pub favicon: Option<String>,
    pub version_name: String,
    /// `commands.operators` parsed.
    pub operators: HashSet<Uuid>,
    /// `commands.sensitive` in lower case.
    pub sensitive: HashSet<String>,
    /// Servers of `managed-servers` added to `config.servers`.
    pub managed: Vec<String>,
}

impl Runtime {
    /// `releases`: first and last supported release, for the default version name.
    pub fn build(config: Config, releases: &(String, String)) -> Result<Self, String> {
        let registry = modules::builtin().map_err(|e| e.to_string())?;
        let modules = modules::build(&registry, &config).map_err(|e| e.to_string())?;
        for (name, s) in &config.servers {
            let (host, _) = split_host_port(&s.address).unwrap_or(("", 0));
            if modules.forwarding.requires_local_backend() && !is_loopback_host(host) {
                return Err(format!(
                    "server {name}: forwarding \"{}\" is only allowed for backends on 127.0.0.1",
                    modules.forwarding.name()
                ));
            }
        }
        let backends = config
            .servers
            .iter()
            .map(|(name, s)| BackendInfo {
                name: name.clone(),
                address: s.address.clone(),
                protocol: s.protocol.map(ProtocolVersion),
                online: true,
                players: 0,
            })
            .collect();
        let version_name = if config.status.version_name.is_empty() {
            format!("PumboProx {}-{}", releases.0, releases.1)
        } else {
            config.status.version_name.clone()
        };
        let operators = config
            .commands
            .operators
            .iter()
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect();
        let sensitive = config
            .commands
            .sensitive
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        Ok(Self {
            operators,
            sensitive,
            motd: crate::status::parse_text(&config.status.motd),
            favicon: crate::status::load_favicon(&config.status.favicon),
            config,
            modules,
            backends,
            version_name,
            managed: Vec::new(),
        })
    }
}

/// A semaphore of `from` permits to `to`. Permits held now stay valid; a
/// smaller limit takes free permits at once and the rest as they come back.
fn resize(sem: &Arc<Semaphore>, from: usize, to: usize) {
    if to >= from {
        sem.add_permits(to - from);
        return;
    }
    let rest = (from - to).saturating_sub(sem.forget_permits(from - to));
    if rest == 0 {
        return;
    }
    let (sem, rest) = (sem.clone(), u32::try_from(rest).unwrap_or(u32::MAX));
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        rt.spawn(async move {
            if let Ok(p) = sem.acquire_many(rest).await {
                p.forget();
            }
        });
    }
}

/// Whether two parsed settings differ (the config types have no `PartialEq`).
fn differs(a: &impl std::fmt::Debug, b: &impl std::fmt::Debug) -> bool {
    format!("{a:?}") != format!("{b:?}")
}

/// Settings of the proxy read once at start: a reload that changes them
/// names them. Everything else applies with the reload.
fn restart_needed(old: &Config, new: &Config) -> Vec<String> {
    let (o, n) = (&old.logging, &new.logging);
    [
        ("listener", differs(&old.listener, &new.listener)),
        ("logging.format", o.format != n.format),
        ("logging.log-ips", o.log_ips != n.log_ips),
        ("logging.metrics-bind", o.metrics_bind != n.metrics_bind),
        ("bridge", differs(&old.bridge, &new.bridge)),
        (
            "managed-servers",
            differs(&old.managed_servers, &new.managed_servers),
        ),
    ]
    .into_iter()
    .filter(|(_, changed)| *changed)
    .map(|(name, _)| name.to_string())
    .collect()
}

/// The same for the host sections, which the plugin host reads at start;
/// only `plugins.required-gates` applies with a reload.
fn host_restart_needed(old: &pumbo_host::HostConfig, new: &pumbo_host::HostConfig) -> Vec<String> {
    let plugins = |c: &pumbo_host::HostConfig| {
        let mut p = c.plugins.clone();
        p.required_gates.clear();
        p
    };
    [
        ("plugins", differs(&plugins(old), &plugins(new))),
        ("services", differs(&old.services, &new.services)),
        ("permissions", differs(&old.permissions, &new.permissions)),
        (
            "placeholders",
            differs(&old.placeholders, &new.placeholders),
        ),
        (
            "server-group",
            differs(&old.server_groups, &new.server_groups),
        ),
        ("style", differs(&old.style, &new.style)),
    ]
    .into_iter()
    .filter(|(_, changed)| *changed)
    .map(|(name, _)| name.to_string())
    .collect()
}

/// One listener's PROXY protocol settings.
#[derive(Debug)]
pub struct ListenerSettings {
    pub bind: String,
    pub proxy_protocol: ProxyProtocol,
    pub trusted: Vec<Cidr>,
    /// The first refused direct connection was logged at warn.
    pub warned: AtomicBool,
}

impl ListenerSettings {
    pub fn is_trusted(&self, ip: IpAddr) -> bool {
        self.trusted.iter().any(|c| c.contains(ip))
    }
}

/// What another task asks a player's session to do.
#[derive(Debug, Clone)]
pub enum SessionCmd {
    /// Move to this server; `quiet` skips "already connected" messages.
    Connect {
        server: String,
        quiet: bool,
    },
    /// Reconnect to the current server (§3.3, `players.reconnect` in E5).
    Reconnect,
    Message(Box<Component>),
    /// Title and subtitle; times in ticks (fade in, stay, fade out).
    Title {
        title: Box<Component>,
        subtitle: Box<Component>,
        times: (i32, i32, i32),
    },
    ActionBar(Box<Component>),
    Bossbar {
        id: Uuid,
        op: Box<crate::world::BossbarOp>,
    },
    /// PumboAPI: the virtual world (§5).
    Virtual(Box<crate::world::VirtualCmd>),
    /// Disconnect the player with this reason (`players.kick`).
    Kick(Box<Component>),
    ClearTitle,
    /// Proxy Tab header and footer; kept across server switches (§5.8.3).
    TabList {
        header: Box<Component>,
        footer: Box<Component>,
    },
    /// A plugin message from a plugin, to the client or to the backend.
    PluginMessage {
        channel: String,
        data: Vec<u8>,
        to_backend: bool,
    },
    /// A tab completion answer that came after the request (from a plugin).
    Suggestions(Box<pumbo_protocol::packets::play::CommandSuggestions>),
    /// Profile property change for the next forwarding (`None` value: remove).
    Property {
        name: String,
        value: Option<pumbo_core::profile::Property>,
    },
}

/// One online player in the registry.
#[derive(Debug, Clone)]
pub struct PlayerEntry {
    pub id: Uuid,
    pub name: String,
    /// Server the player is on (after its play `login`).
    pub server: Option<String>,
    pub address: SocketAddr,
    tx: mpsc::Sender<SessionCmd>,
    /// Permissions, context or plugin commands changed: the session rebuilds
    /// the command tree once for a series of changes, also with a full queue.
    commands_changed: Arc<Notify>,
    /// Teleports the client has not confirmed (PumboBridge holds its own).
    tp: crate::bridge::TpGate,
}

/// What a chat filter is asked about.
#[derive(Debug, Clone, Copy)]
pub enum ChatInput<'a> {
    Message(&'a str),
    /// A backend command without the slash.
    Command(&'a str),
}

/// Decides whether a player's chat message or backend command is cancelled
/// (`true`) before it reaches the backend. The plugin host's `on-chat` and
/// `on-backend-command` (E5) plug in here; without one nothing is cancelled.
pub type ChatFilter = dyn Fn(&GameProfile, ChatInput<'_>) -> bool + Send + Sync;

/// Commands per session waiting in its queue.
const SESSION_QUEUE: usize = 64;

pub struct Proxy {
    pub versions: VersionRegistry,
    newest: Arc<dyn VersionModule>,
    releases: (String, String),
    pub key: ServerKey,
    /// A watch, so sessions waiting in the Pumbox see a new server list at once.
    runtime: watch::Sender<Arc<Runtime>>,
    players: Mutex<HashMap<Uuid, PlayerEntry>>,
    chat_filter: RwLock<Option<Arc<ChatFilter>>>,
    /// Backends already reported, per reason (secure chat §2.8 point 1, D-COMPAT-1).
    warned: Mutex<HashSet<(&'static str, String)>>,
    pub metrics: Arc<Metrics>,
    pub ips: Arc<IpLimiter>,
    /// Connections before the end of login (§2.7). It outlives the runtime,
    /// so a reload resizes it instead of opening a fresh pool.
    pub pending: Arc<Semaphore>,
    /// Requests to the authentication service in flight, the same way.
    pub auth_slots: Arc<Semaphore>,
    /// Premium-name answers and their lookup limit; kept across reloads.
    pub premium: crate::limits::PremiumCache,
    ip_display: IpDisplay,
    shutdown: watch::Sender<bool>,
    active: AtomicUsize,
    config_path: Option<PathBuf>,
    /// One config change at a time: reload, refresh and edits of the file.
    config_lock: Mutex<()>,
    /// The plugin host (E5); unset without a plugin directory.
    pub plugins: OnceLock<Arc<crate::plugins::Plugins>>,
    /// PumboBridge sessions, when `[bridge] enabled`.
    pub bridge: OnceLock<Arc<crate::bridge::Bridge>>,
    /// Servers run by the proxy, when `managed-servers.enabled`.
    pub servers: OnceLock<Arc<pumbo_servers::Manager>>,
    /// Backends that failed their last logins (passive health).
    pub health: crate::health::Health,
    /// `/prox claim`: the operator code and the wrong tries so far; gone
    /// for good once the network has a server.
    pub(crate) claim: Mutex<Option<(String, u8)>>,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy").finish_non_exhaustive()
    }
}

/// A logged-in player, unregistered on drop.
#[derive(Debug)]
pub struct PlayerGuard {
    proxy: Arc<Proxy>,
    id: Uuid,
}

impl Drop for PlayerGuard {
    fn drop(&mut self) {
        if let Ok(mut p) = self.proxy.players.lock() {
            p.remove(&self.id);
        }
        if let Some(plugins) = self.proxy.plugins.get() {
            plugins.left(self.id);
        }
    }
}

impl Proxy {
    pub fn new(config: Config, config_path: Option<PathBuf>) -> Result<Arc<Self>, String> {
        let versions = modules::versions().map_err(|e| format!("protocol tables: {e}"))?;
        let newest = versions
            .newest()
            .and_then(|v| versions.get(v))
            .cloned()
            .ok_or("no protocol versions")?;
        let release = |v: Option<ProtocolVersion>, last: bool| {
            v.and_then(|v| versions.get(v))
                .and_then(|m| {
                    if last {
                        m.release_names().last()
                    } else {
                        m.release_names().first()
                    }
                })
                .cloned()
                .unwrap_or_default()
        };
        let releases = (
            release(versions.oldest(), false),
            release(versions.newest(), true),
        );
        let ip_display = IpDisplay::new(config.logging.log_ips);
        let pending = Arc::new(Semaphore::new(config.limits.max_pending_logins));
        let auth_slots = Arc::new(Semaphore::new(config.limits.max_concurrent_has_joined));
        let runtime = Runtime::build(config, &releases)?;
        let key = ServerKey::generate().map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            versions,
            newest,
            releases,
            key,
            runtime: watch::channel(Arc::new(runtime)).0,
            players: Mutex::new(HashMap::new()),
            chat_filter: RwLock::new(None),
            warned: Mutex::new(HashSet::new()),
            metrics: Arc::new(Metrics::default()),
            ips: Arc::new(IpLimiter::default()),
            pending,
            auth_slots,
            premium: crate::limits::PremiumCache::default(),
            ip_display,
            shutdown: watch::channel(false).0,
            active: AtomicUsize::new(0),
            config_path,
            config_lock: Mutex::new(()),
            plugins: OnceLock::new(),
            bridge: OnceLock::new(),
            servers: OnceLock::new(),
            health: crate::health::Health::default(),
            claim: Mutex::new(None),
        }))
    }

    pub fn runtime(&self) -> Arc<Runtime> {
        self.runtime.borrow().clone()
    }

    /// Changes after now of the runtime (reload, servers of the proxy).
    pub fn runtime_changes(&self) -> watch::Receiver<Arc<Runtime>> {
        self.runtime.subscribe()
    }

    /// Re-reads the config file. The whole file, the host sections too, is
    /// read and checked before anything changes; any error keeps the old
    /// config. Players keep their sessions and see the new server list and
    /// commands at once; `logging.level` and `plugins.required-gates` apply
    /// at once too. Returns the changed settings that wait for a restart.
    pub fn reload(&self) -> Result<Vec<String>, String> {
        off_worker(|| {
            let _one = self.config_lock();
            self.reload_locked()
        })
    }

    /// Holds off other reloads, refreshes and edits of the config file.
    pub(crate) fn config_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.config_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// [`Proxy::reload`] for a caller that holds [`Proxy::config_lock`].
    pub(crate) fn reload_locked(&self) -> Result<Vec<String>, String> {
        let path = self
            .config_path
            .as_ref()
            .ok_or("no config file to reload")?;
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        let config = Config::parse(&text).map_err(|e| at(&e))?;
        let unresolved = config.check_backend_names().map_err(|e| at(&e))?;
        let host = pumbo_host::HostConfig::parse(&text).map_err(|e| at(&e))?;
        let old = self.runtime();
        let mut restart = restart_needed(&old.config, &config);
        let level = config.logging.level.clone();
        if level != old.config.logging.level {
            crate::logging::check_level(&level).map_err(|e| at(&e))?;
        }
        let runtime = self.prepare(config)?;
        if let Some(p) = self.plugins.get() {
            restart.extend(host_restart_needed(p.host.config(), &host));
            // Last check: it applies the new permissions only when they load.
            p.host.reload_permissions()?;
            p.host.set_required_gates(host.plugins.required_gates);
        }
        self.commit(runtime);
        for w in unresolved {
            warn!("{w}");
        }
        if level != old.config.logging.level
            && let Err(e) = crate::logging::set_level(&level)
        {
            warn!("{e}");
        }
        Ok(restart)
    }

    /// The config file the proxy runs with (`/prox reload`, `/prox route`).
    pub fn config_path(&self) -> Option<&std::path::Path> {
        self.config_path.as_deref()
    }

    /// The server list again after a server of `managed-servers` was
    /// created or deleted; the rest of the config stays.
    pub fn refresh(&self) -> Result<(), String> {
        off_worker(|| {
            let _one = self.config_lock();
            let old = self.runtime();
            let mut config = old.config.clone();
            for name in &old.managed {
                config.servers.remove(name);
            }
            self.replace(config)
        })
    }

    /// A new runtime from `config` plus the servers of `managed-servers`
    /// (a server in the config file wins over one of the same name in any
    /// letter case).
    fn replace(&self, config: Config) -> Result<(), String> {
        let runtime = self.prepare(config)?;
        self.commit(runtime);
        Ok(())
    }

    /// The runtime of [`Proxy::replace`], built and checked; nothing changes yet.
    fn prepare(&self, mut config: Config) -> Result<Runtime, String> {
        let mut managed = Vec::new();
        if let Some(m) = self.servers.get() {
            for (name, s) in crate::servers::backends(self, m) {
                if let Some(own) = config
                    .servers
                    .keys()
                    .find(|k| k.eq_ignore_ascii_case(&name))
                {
                    if *own != name {
                        warn!("server {name} of managed-servers is left out: the config has {own}");
                    }
                    continue;
                }
                config.servers.insert(name.clone(), s);
                managed.push(name);
            }
        }
        // A server the proxy has not created yet is skipped until it exists.
        if config.managed_servers.enabled
            && let Err(e) = config.check_server_names()
        {
            warn!("{e}; players skip it until it exists");
        }
        let mut runtime = Runtime::build(config, &self.releases)?;
        runtime.managed = managed;
        Ok(runtime)
    }

    fn commit(&self, runtime: Runtime) {
        // The operator code is for a fresh network only.
        if !runtime.backends.is_empty() && self.claim.lock().is_ok_and(|mut c| c.take().is_some()) {
            info!("The operator code expired: the network has a server now.");
        }
        let (old, new) = (&self.runtime().config.limits, &runtime.config.limits);
        resize(
            &self.pending,
            old.max_pending_logins,
            new.max_pending_logins,
        );
        resize(
            &self.auth_slots,
            old.max_concurrent_has_joined,
            new.max_concurrent_has_joined,
        );
        self.runtime.send_replace(Arc::new(runtime));
        if let Some(p) = self.plugins.get() {
            p.servers_changed(self);
        }
    }

    /// A login to `server` got through (`reached`) or failed before the
    /// server answered: its health changes, and plugins hear when it goes
    /// offline or comes back.
    pub fn backend_login(&self, server: &str, reached: bool) {
        let changed = if reached {
            let back = self.health.succeeded(server);
            if back {
                info!("server {server} is back online");
            }
            back
        } else {
            let went = self.health.failed(server);
            if let Some(wait) = went {
                info!(
                    "server {server} is offline after {} failed logins in a row: players try the other servers first, and it again in {} s",
                    crate::health::FAILURES,
                    wait.as_secs()
                );
            }
            went.is_some()
        };
        if changed && let Some(p) = self.plugins.get() {
            p.servers_changed(self);
        }
    }

    /// Whether players try `server` in its place, not last: it is not offline
    /// (or its wait is over) and a server run by the proxy is ready.
    pub fn server_up(&self, rt: &Runtime, server: &str) -> bool {
        let ready = !rt.managed.iter().any(|m| m == server)
            || self
                .servers
                .get()
                .is_some_and(|m| m.state(server) == pumbo_servers::State::Ready);
        ready && self.health.usable(server)
    }

    pub fn newest(&self) -> Arc<dyn VersionModule> {
        self.newest.clone()
    }

    /// First release of the oldest and last release of the newest protocol.
    pub fn release_range(&self) -> &(String, String) {
        &self.releases
    }

    pub fn online(&self) -> usize {
        self.players.lock().map(|p| p.len()).unwrap_or(0)
    }

    /// Registers a logged-in player; `None` if the UUID is already online.
    /// The receiver is the session's command queue.
    pub fn register_player(
        self: &Arc<Self>,
        profile: &GameProfile,
        address: SocketAddr,
    ) -> Option<(PlayerGuard, mpsc::Receiver<SessionCmd>)> {
        let mut p = self.players.lock().ok()?;
        if p.contains_key(&profile.id) {
            return None;
        }
        let (tx, rx) = mpsc::channel(SESSION_QUEUE);
        p.insert(
            profile.id,
            PlayerEntry {
                id: profile.id,
                name: profile.name.clone(),
                server: None,
                address,
                tx,
                commands_changed: Arc::new(Notify::new()),
                tp: crate::bridge::new_gate(),
            },
        );
        Some((
            PlayerGuard {
                proxy: self.clone(),
                id: profile.id,
            },
            rx,
        ))
    }

    pub fn set_server(&self, id: Uuid, server: Option<&str>) {
        if let Ok(mut p) = self.players.lock()
            && let Some(e) = p.get_mut(&id)
        {
            e.server = server.map(str::to_string);
        }
        if let Some(plugins) = self.plugins.get() {
            plugins.server_changed(id, server);
        }
    }

    /// Online players, sorted by name.
    pub fn teleport_gate(&self, id: Uuid) -> Option<crate::bridge::TpGate> {
        self.players.lock().ok()?.get(&id).map(|e| e.tp.clone())
    }

    pub fn server_of(&self, id: Uuid) -> Option<String> {
        self.players.lock().ok()?.get(&id)?.server.clone()
    }

    pub fn players(&self) -> Vec<PlayerEntry> {
        let mut v: Vec<PlayerEntry> = self
            .players
            .lock()
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default();
        v.sort_by_key(|e| e.name.to_ascii_lowercase());
        v
    }

    /// A player by name, ignoring case.
    pub fn find_player(&self, name: &str) -> Option<PlayerEntry> {
        let p = self.players.lock().ok()?;
        p.values()
            .find(|e| e.name.eq_ignore_ascii_case(name))
            .cloned()
    }

    /// Queues a command for a player's session; `false` if the player is
    /// gone or the queue is full.
    pub fn send_to(&self, id: Uuid, cmd: SessionCmd) -> bool {
        let tx = self
            .players
            .lock()
            .ok()
            .and_then(|p| p.get(&id).map(|e| e.tx.clone()));
        tx.is_some_and(|tx| tx.try_send(cmd).is_ok())
    }

    /// The player's visible commands changed; `false` if the player is gone.
    pub fn commands_changed(&self, id: Uuid) -> bool {
        self.commands_signal(id).map(|n| n.notify_one()).is_some()
    }

    /// What the session of this player waits on for [`Proxy::commands_changed`].
    pub fn commands_signal(&self, id: Uuid) -> Option<Arc<Notify>> {
        self.players
            .lock()
            .ok()?
            .get(&id)
            .map(|e| e.commands_changed.clone())
    }

    /// Reconnects a player to the current server (§3.3).
    pub fn reconnect(&self, id: Uuid) -> bool {
        self.send_to(id, SessionCmd::Reconnect)
    }

    pub fn set_chat_filter(&self, filter: Option<Arc<ChatFilter>>) {
        *self
            .chat_filter
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = filter;
    }

    /// Whether the chat filter cancels this message or command.
    pub fn chat_cancelled(&self, profile: &GameProfile, input: ChatInput<'_>) -> bool {
        let filter = self
            .chat_filter
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        filter.is_some_and(|f| f(profile, input))
    }

    /// `true` the first time a backend is reported for `reason`.
    pub fn first_report(&self, reason: &'static str, server: &str) -> bool {
        self.warned
            .lock()
            .is_ok_and(|mut w| w.insert((reason, server.to_string())))
    }

    pub fn ip(&self, addr: SocketAddr) -> String {
        self.ip_display.show(addr.ip())
    }

    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }

    pub fn active_sessions(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    /// Opens every `listener`.
    pub async fn bind(&self) -> Result<Vec<(Box<dyn Listener>, Arc<ListenerSettings>)>, String> {
        let rt = self.runtime();
        let registry = modules::builtin().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for l in &rt.config.listener {
            let factory = registry
                .listeners
                .get(&l.transport)
                .map_err(|e| e.to_string())?;
            let listener = factory(&modules::listener_config(l))
                .await
                .map_err(|e| e.to_string())?;
            let trusted = rt.config.trusted(l).map_err(|e| e.to_string())?;
            info!("listening on {} ({})", listener.local_addr(), l.transport);
            if l.proxy_protocol == ProxyProtocol::Required {
                info!(
                    "listener {}: PROXY header required, only {} may connect; set proxy-protocol: optional to also accept direct connections",
                    l.bind,
                    l.trusted_proxies.join(", ")
                );
            }
            out.push((
                listener,
                Arc::new(ListenerSettings {
                    bind: l.bind.clone(),
                    proxy_protocol: l.proxy_protocol,
                    trusted,
                    warned: AtomicBool::new(false),
                }),
            ));
        }
        if out.is_empty() {
            return Err("no [[listener]] in the config".into());
        }
        Ok(out)
    }

    /// `pumboprox_backend_online{server="…"}`: 1 unless the server is
    /// offline after failed logins or a server run by the proxy is not ready.
    pub fn render_health(&self) -> String {
        use std::fmt::Write as _;
        let rt = self.runtime();
        let mut out = String::from("# TYPE pumboprox_backend_online gauge\n");
        for b in &rt.backends {
            let ready = !rt.managed.contains(&b.name)
                || self
                    .servers
                    .get()
                    .is_some_and(|m| m.state(&b.name) == pumbo_servers::State::Ready);
            let up = ready && self.health.online(&b.name);
            let name = b.name.replace('\\', "\\\\").replace('"', "\\\"");
            let _ = writeln!(
                out,
                "pumboprox_backend_online{{server=\"{name}\"}} {}",
                u8::from(up)
            );
        }
        out
    }

    /// Accepts connections on all listeners until [`Proxy::stop`], then waits
    /// up to five seconds for sessions to end.
    pub async fn serve(
        self: Arc<Self>,
        listeners: Vec<(Box<dyn Listener>, Arc<ListenerSettings>)>,
    ) {
        let metrics_bind = self.runtime().config.logging.metrics_bind.clone();
        if let Ok(addr) = metrics_bind.parse::<SocketAddr>() {
            match crate::metrics::bind(addr).await {
                Ok(l) => {
                    info!("metrics on http://{addr}/metrics");
                    let plugins = self.plugins.get().cloned();
                    let me = Arc::downgrade(&self);
                    let extra: Arc<dyn Fn() -> String + Send + Sync> = Arc::new(move || {
                        let mut out = me.upgrade().map(|p| p.render_health()).unwrap_or_default();
                        if let Some(p) = &plugins {
                            out.push_str(&p.host.render_metrics());
                        }
                        out
                    });
                    tokio::spawn(crate::metrics::serve(l, self.metrics.clone(), extra));
                }
                Err(e) => error!("metrics on {addr}: {e}"),
            }
        }
        let sweeper = {
            let ips = self.ips.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    ips.sweep();
                }
            })
        };
        let mut tasks = Vec::new();
        for (listener, settings) in listeners {
            let proxy = self.clone();
            tasks.push(tokio::spawn(async move {
                proxy.accept_loop(listener, settings).await
            }));
        }
        for t in tasks {
            let _ = t.await;
        }
        sweeper.abort();
        for _ in 0..50 {
            if self.active_sessions() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn accept_loop(
        self: Arc<Self>,
        listener: Box<dyn Listener>,
        settings: Arc<ListenerSettings>,
    ) {
        let mut stop = self.shutdown_signal();
        loop {
            tokio::select! {
                () = crate::session::stopped(&mut stop) => return,
                r = listener.accept() => match r {
                    Ok(incoming) => {
                        let proxy = self.clone();
                        let settings = settings.clone();
                        proxy.active.fetch_add(1, Ordering::Relaxed);
                        tokio::spawn(async move {
                            crate::session::handle(proxy.clone(), settings, incoming).await;
                            proxy.active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                    Err(e) => {
                        // E.g. out of file descriptors: back off instead of spinning.
                        warn!("accept on {}: {e}", settings.bind);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                },
            }
        }
    }
}

/// Runs `f`, which may wait on the config lock, the config file or DNS
/// (server names on reload), without holding up the other tasks of a
/// runtime worker: they move to another thread meanwhile.
pub(crate) fn off_worker<R>(f: impl FnOnce() -> R) -> R {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(h) if h.runtime_flavor() == RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refresh that waits for the config lock (held by a reload that
    /// resolves server names) leaves the other tasks of its worker running.
    #[test]
    fn waiting_for_the_config_lock_keeps_the_worker_running() {
        let cfg = Config::parse(
            "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n",
        )
        .unwrap();
        let proxy = Proxy::new(cfg, None).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let held = proxy.config_lock();
        let p = proxy.clone();
        let refresh = rt.spawn(async move { p.refresh() });
        std::thread::sleep(Duration::from_millis(200));
        let (tx, rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let _ = tx.send(());
        });
        let ran = rx.recv_timeout(Duration::from_secs(5)).is_ok();
        drop(held);
        assert!(ran, "the only worker is stuck waiting for the lock");
        rt.block_on(refresh).unwrap().unwrap();
    }
}
