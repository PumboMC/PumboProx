//! The running proxy: shared state, listeners, reload and shutdown.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use pumbo_core::profile::GameProfile;
use pumbo_protocol::crypto::ServerKey;
use pumbo_protocol::{VersionModule, VersionRegistry};
use pumbo_text::Component;
use tokio::sync::{Notify, Semaphore, mpsc, watch};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::Config;
use crate::limits::IpLimiter;
use crate::logging::IpDisplay;
use crate::metrics::Metrics;
use crate::modules;

mod listen;
mod runtime;

pub use listen::ListenerSettings;
pub use runtime::Runtime;

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
    urgent: Arc<Urgent>,
}

/// A kick or a gate release for a session, kept apart from its queue so a
/// full queue cannot lose it: a ban that lands while the player waits in a
/// gate must not let the player in.
#[derive(Debug, Default)]
pub(crate) struct Urgent {
    cmd: Mutex<Option<SessionCmd>>,
    pub(crate) notify: Notify,
    /// A command was dropped from the full queue and logged.
    dropped: std::sync::atomic::AtomicBool,
}

impl Urgent {
    /// A kick replaces a waiting release; the first kick stays, so a ban
    /// reason is not replaced by a later text.
    fn put(&self, cmd: SessionCmd) {
        let mut slot = self
            .cmd
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*slot, Some(SessionCmd::Kick(_))) {
            *slot = Some(cmd);
        }
        drop(slot);
        self.notify.notify_one();
    }

    pub(crate) fn take(&self) -> Option<SessionCmd> {
        self.cmd
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

fn is_urgent(cmd: &SessionCmd) -> bool {
    match cmd {
        SessionCmd::Kick(_) => true,
        SessionCmd::Virtual(v) => matches!(**v, crate::world::VirtualCmd::Release),
        _ => false,
    }
}

/// When a dropped session command was last logged at `warn`, for all players.
static DROPPED_WARNED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// At most one `warn` about dropped session commands in this time.
const DROPPED_WARN_EVERY: Duration = Duration::from_secs(60);

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

/// Sessions get this long to end after the listeners close.
pub const SESSION_DRAIN: Duration = Duration::from_secs(5);
/// Plugins get this long for their `shutdown`, after the managed servers.
pub const PLUGIN_SHUTDOWN: Duration = Duration::from_secs(4);

/// Logged when the proxy starts stopping with managed servers: a container
/// that kills the proxy before this cuts a world save short (Docker waits
/// 10 s by default).
pub fn stop_budget(m: &pumbo_servers::Config) -> String {
    let worst = SESSION_DRAIN + m.shutdown_limit() + PLUGIN_SHUTDOWN;
    let grace = (worst.as_secs() + 5).div_ceil(10) * 10;
    format!(
        "stopping: managed servers get up to {} s (managed-servers.stop-timeout-secs); a container needs stop_grace_period of at least {grace}s",
        m.stop_timeout_secs
    )
}

pub struct Proxy {
    pub versions: VersionRegistry,
    newest: Arc<dyn VersionModule>,
    pub key: ServerKey,
    /// A watch, so sessions waiting in the Pumbox see a new server list at once.
    runtime: watch::Sender<Arc<Runtime>>,
    players: Mutex<HashMap<Uuid, PlayerEntry>>,
    chat_filter: RwLock<Option<Arc<ChatFilter>>>,
    /// Backends already reported, per reason (secure chat §2.8 point 1, D-COMPAT-1).
    warned: Mutex<HashSet<(&'static str, String)>>,
    pub metrics: Arc<Metrics>,
    pub translate_drops: crate::metrics::TranslateDrops,
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
    /// [`Proxy::serve`] accepts players.
    serving: AtomicBool,
    metrics_started: AtomicBool,
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
    /// Kicks and gate releases for the session.
    pub(crate) urgent: Arc<Urgent>,
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
        let ip_display = IpDisplay::new(config.logging.log_ips);
        let pending = Arc::new(Semaphore::new(config.limits.max_pending_logins));
        let auth_slots = Arc::new(Semaphore::new(config.limits.max_concurrent_has_joined));
        let runtime = Runtime::build(config)?;
        let key = ServerKey::generate().map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            versions,
            newest,
            key,
            runtime: watch::channel(Arc::new(runtime)).0,
            players: Mutex::new(HashMap::new()),
            chat_filter: RwLock::new(None),
            warned: Mutex::new(HashSet::new()),
            metrics: Arc::new(Metrics::default()),
            translate_drops: crate::metrics::TranslateDrops::default(),
            ips: Arc::new(IpLimiter::default()),
            pending,
            auth_slots,
            premium: crate::limits::PremiumCache::default(),
            ip_display,
            shutdown: watch::channel(false).0,
            serving: AtomicBool::new(false),
            metrics_started: AtomicBool::new(false),
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
        let urgent = Arc::new(Urgent::default());
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
                urgent: urgent.clone(),
            },
        );
        Some((
            PlayerGuard {
                proxy: self.clone(),
                id: profile.id,
                urgent,
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
    /// gone or the queue is full. Kicks and gate releases never wait in the
    /// queue, so a full one cannot drop them.
    pub fn send_to(&self, id: Uuid, cmd: SessionCmd) -> bool {
        let Some((tx, urgent, name)) = self.players.lock().ok().and_then(|p| {
            p.get(&id)
                .map(|e| (e.tx.clone(), e.urgent.clone(), e.name.clone()))
        }) else {
            return false;
        };
        if is_urgent(&cmd) {
            urgent.put(cmd);
            return true;
        }
        match tx.try_send(cmd) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Closed(_)) => false,
            Err(mpsc::error::TrySendError::Full(_)) => {
                Metrics::inc(&self.metrics.dropped_commands);
                if !urgent.dropped.swap(true, Ordering::Relaxed) {
                    debug!("the session of {name} is behind; commands for it are dropped");
                }
                let mut last = DROPPED_WARNED
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if last.is_none_or(|t| t.elapsed() >= DROPPED_WARN_EVERY) {
                    *last = Some(std::time::Instant::now());
                    warn!(
                        "commands for players whose sessions are behind were dropped (pumboprox_dropped_packets_total{{kind=\"session_command\"}})"
                    );
                }
                false
            }
        }
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

    /// `send_replace`, not `send`: with nobody subscribed yet (a stop right
    /// after the start) `send` fails and drops the value, so the stop is lost.
    pub fn stop(&self) {
        self.shutdown.send_replace(true);
    }

    pub fn active_sessions(&self) -> usize {
        self.active.load(Ordering::Relaxed)
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
    use crate::world::VirtualCmd;

    /// Stopping names the time a container must give the proxy, from
    /// `stop-timeout-secs`.
    #[test]
    fn the_stop_budget_follows_the_stop_timeout() {
        let mut m = pumbo_servers::Config::default();
        assert_eq!(
            stop_budget(&m),
            "stopping: managed servers get up to 30 s (managed-servers.stop-timeout-secs); a container needs stop_grace_period of at least 60s"
        );
        m.stop_timeout_secs = 120;
        let line = stop_budget(&m);
        assert!(
            line.contains("up to 120 s") && line.ends_with("at least 150s"),
            "{line}"
        );
    }

    /// A stop that comes before anything waits for it (a `stop` typed right
    /// after the start) is kept: the serve loop that subscribes later sees it.
    #[test]
    fn a_stop_before_anyone_listens_is_kept() {
        let cfg = Config::parse(
            "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n",
        )
        .unwrap();
        let proxy = Proxy::new(cfg, None).unwrap();
        proxy.stop();
        assert!(*proxy.shutdown_signal().borrow(), "the stop was lost");
    }

    #[test]
    fn a_kick_beats_a_release_and_the_first_kick_stays() {
        let u = Urgent::default();
        let release = || SessionCmd::Virtual(Box::new(VirtualCmd::Release));
        let kick = |t: &str| SessionCmd::Kick(Box::new(Component::text(t)));
        u.put(release());
        u.put(kick("You are banned."));
        u.put(release());
        u.put(kick("Kicked."));
        assert!(
            matches!(u.take(), Some(SessionCmd::Kick(t)) if t.plain_text() == "You are banned.")
        );
        assert!(u.take().is_none());
        u.put(release());
        assert!(matches!(u.take(), Some(SessionCmd::Virtual(_))));
        assert!(is_urgent(&kick("x")) && is_urgent(&release()));
        assert!(!is_urgent(&SessionCmd::Reconnect));
        assert!(!is_urgent(&SessionCmd::Virtual(Box::new(
            VirtualCmd::ClearInventory
        ))));
    }
}
