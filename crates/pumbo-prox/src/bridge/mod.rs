//! PumboBridge, proxy side (see `docs/bridge-protocol.md`): the
//! `pumbobridge` plugin of every Pumpkin server connects here over
//! TCP, proves it has the bridge key, and is paired with its `servers`
//! entry by a status ping whose host name carries a nonce. Plugins reach the
//! bridges through the native service `pumbo:bridge@1.0`.
//!
//! The proxy also holds teleports: it sees every `player_position` of the
//! server and every `accept_teleportation` of the client, so a teleport for a
//! player with an unconfirmed one waits here (at most 10 s, then `expired`; a
//! newer one makes it `superseded`). Two teleports in one tick would kick
//! the player ("Wrong teleport id").
//!
//! Land protection belongs to the future PumboGuard: nothing here sends
//! rules (`guard-rules` is reserved in the contract).

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pumbo_bridge_proto::api::PermsMode;
use pumbo_bridge_proto::wire::{self, Msg};
use pumbo_host::wit::services::ServiceError;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{info, warn};
use uuid::Uuid;

use crate::server::Proxy;

mod admin;
mod pairing;
mod perms;
mod service;
mod session;

pub use admin::plain;
use service::Service;

/// `bridge` section of the proxy config.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct BridgeConfig {
    pub enabled: bool,
    /// Where bridges connect (`ip:port`); loopback or a private network.
    pub listen: String,
    /// The bridge key (64 hex digits), created on the first start.
    pub key_file: String,
    /// An environment variable with the key, used instead of `key-file`.
    pub key_env: String,
    pub command_timeout_ms: u64,
    /// Plugins that may send commands; every plugin with `uses` may query.
    pub trusted_plugins: Vec<String>,
    pub stats_ms: u32,
    pub player_stats_ms: u32,
    /// Allow `listen` outside loopback and private networks.
    pub allow_public: bool,
    /// For tests: how long a bridge may stay silent before it is closed. It
    /// is pinged every third of that.
    pub test_silence_ms: u64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        BridgeConfig {
            enabled: true,
            listen: "127.0.0.1:25578".into(),
            key_file: "bridge.key".into(),
            key_env: String::new(),
            command_timeout_ms: 5000,
            trusted_plugins: vec!["pumbo-core".into()],
            stats_ms: 5000,
            player_stats_ms: 2000,
            allow_public: false,
            test_silence_ms: 15_000,
        }
    }
}

impl BridgeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let Ok(addr) = self.listen.parse::<SocketAddr>() else {
            return Err(format!("bridge.listen {} is not ip:port", self.listen));
        };
        if !self.allow_public && !crate::config::is_private_ip(addr.ip()) {
            let what = if addr.ip().is_unspecified() {
                "listens on every interface, public ones too"
            } else {
                "is outside loopback and private networks"
            };
            return Err(format!(
                "bridge.listen {} {what}; bridge frames are signed, not encrypted. Bind a private address (10.x, 192.168.x, Tailscale 100.64.x) or set bridge.allow-public: true",
                self.listen
            ));
        }
        Ok(())
    }
}

const HANDSHAKE: Duration = Duration::from_secs(5);
/// A write to the bridge that makes no progress for this long (the server
/// stopped reading) ends the session; a slow link that keeps reading does not.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const PAIR_EVERY: Duration = Duration::from_secs(10);
const PAIR_TRIES: u32 = 3;
const MAX_UNAUTHENTICATED: usize = 8;
/// Handshake attempts per address in 10 s.
const MAX_TRIES_PER_IP: u32 = 10;
const OUT_QUEUE: usize = 4096;
const TELEPORT_WAIT: Duration = Duration::from_secs(10);
/// A teleport sent to a bridge counts as pending until the server's
/// `player_position` passes, at most this long.
const SENT_TTL: Duration = Duration::from_secs(2);
/// Two ticks after the client's confirmation.
pub const CONFIRM_GRACE: Duration = Duration::from_millis(100);
const ARRIVE_TTL_MS: u32 = 30_000;
const MISSING_AFTER: Duration = Duration::from_secs(60);
/// A rejection shows in the status this long (a bridge retries within 30 s).
const REJECTED_SHOWN: Duration = Duration::from_secs(120);

/// Teleport state of a player (kept by the play session, read here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tp {
    Free,
    /// A teleport went to the bridge (or the player joined a server): the
    /// server's `player_position` is due.
    Sent(Instant),
    /// The client has not confirmed this teleport id yet.
    Awaiting(i32),
    /// The client confirmed at this time; the server may not have read the
    /// confirmation yet (it reads packets in its tick), so the next teleport
    /// waits [`CONFIRM_GRACE`] more ("Wrong teleport id" otherwise).
    Confirmed(Instant),
}

impl Tp {
    /// When the state stops blocking a teleport by itself (`None`: free now;
    /// `Some(None)`: only a confirmation frees it).
    fn busy(self) -> Option<Option<Instant>> {
        match self {
            Tp::Free => None,
            Tp::Sent(t) if t.elapsed() >= SENT_TTL => None,
            Tp::Sent(t) => Some(Some(t + SENT_TTL)),
            Tp::Awaiting(_) => Some(None),
            Tp::Confirmed(t) if t.elapsed() >= CONFIRM_GRACE => None,
            Tp::Confirmed(t) => Some(Some(t + CONFIRM_GRACE)),
        }
    }
}

pub type TpGate = Arc<watch::Sender<Tp>>;

pub fn new_gate() -> TpGate {
    Arc::new(watch::channel(Tp::Free).0)
}

type Reply = Result<ciborium::Value, (String, Option<String>)>;

/// An authenticated session with one bridge.
struct Link {
    id: u64,
    peer: SocketAddr,
    out: mpsc::Sender<Msg>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    version: (String, String, String),
    proto: [u16; 2],
    ping_at: Mutex<Option<Instant>>,
    /// Round trip of the last ping (`u32::MAX`: none yet).
    rtt_ms: AtomicU32,
    caps: Mutex<Vec<String>>,
    catalog: Mutex<Vec<String>>,
    server: Mutex<Option<String>>,
    perms_mode: Mutex<PermsMode>,
    perms_sent: Mutex<HashMap<Uuid, SentPerms>>,
    /// Why the proxy closes the session (set once).
    closed: OnceLock<String>,
    close: Notify,
}

impl Link {
    fn send(&self, m: Msg) -> bool {
        self.out.try_send(m).is_ok()
    }

    fn close(&self, why: String) {
        let _ = self.closed.set(why);
        self.close.notify_one();
    }

    fn server(&self) -> Option<String> {
        self.server.lock().ok()?.clone()
    }

    fn mode(&self) -> PermsMode {
        self.perms_mode.lock().map(|m| *m).unwrap_or_default()
    }
}

/// The last `perm-set` sent for a player, and the newest info request.
#[derive(Default)]
struct SentPerms {
    nodes: BTreeMap<String, bool>,
    info: Option<String>,
    asked: u64,
}

#[derive(Default)]
struct Slot {
    link: Option<Arc<Link>>,
    since: Option<SystemTime>,
    ever: bool,
    warned: bool,
}

#[derive(Default)]
struct State {
    servers: BTreeMap<String, Slot>,
    /// Authenticated sessions waiting for pairing, with the pings tried.
    unpaired: HashMap<u64, (Arc<Link>, u32)>,
    /// Pairing nonce → server.
    nonces: HashMap<String, (String, Instant)>,
    unauthenticated: usize,
    tries: HashMap<IpAddr, (Instant, u32)>,
    /// Failed handshakes after a `hello`, by address: when and why.
    rejected: HashMap<IpAddr, (Instant, String)>,
    /// Held teleports: player → (generation, cancel).
    held: HashMap<Uuid, (u64, oneshot::Sender<()>)>,
}

pub struct Bridge {
    proxy: Weak<Proxy>,
    me: Weak<Bridge>,
    key: wire::Key,
    cfg: BridgeConfig,
    started: Instant,
    state: Mutex<State>,
    next: AtomicU64,
    wake: Notify,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("listen", &self.cfg.listen)
            .finish_non_exhaustive()
    }
}

fn now_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn cbor<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, ServiceError> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).map_err(|e| ServiceError::Rejected(e.to_string()))?;
    Ok(out)
}

fn rejected(code: &str, detail: Option<String>) -> ServiceError {
    ServiceError::Rejected(match detail {
        Some(d) => format!("{code}: {d}"),
        None => code.to_string(),
    })
}

/// Reads the bridge key, creating it (mode 0600) when missing; a key file
/// other users can read is set to 0600.
pub fn load_key(path: &str) -> Result<wire::Key, String> {
    if !std::path::Path::new(path).exists() {
        let mut k = [0u8; 32];
        aws_lc_rs::rand::fill(&mut k).map_err(|_| "random generator failed".to_string())?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(path).map_err(|e| format!("{path}: {e}"))?;
        std::io::Write::write_all(&mut f, wire::key_hex(&k).as_bytes())
            .map_err(|e| format!("{path}: {e}"))?;
        info!(
            "created a new bridge key in {path}; show it with `bridge key` in the console and put it in config.yml of PumboBridge"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            // A read-only mount or a file of another user stays as it is.
            match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
                Ok(()) => {
                    warn!("{path}: other users could read the bridge key, its mode is now 0600");
                }
                Err(e) => warn!(
                    "{path} is readable by other users (mode {mode:04o}) and cannot be changed ({e}); chmod 600 it or use bridge.key-env"
                ),
            }
        }
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    wire::parse_key(&text).ok_or_else(|| format!("{path}: not a bridge key (64 hex digits)"))
}

/// The bridge key from the environment variable `key-env` when it is set,
/// else from `key-file` ([`load_key`]).
pub fn key(cfg: &BridgeConfig) -> Result<wire::Key, String> {
    if cfg.key_env.is_empty() {
        return load_key(&cfg.key_file);
    }
    let var = &cfg.key_env;
    let text = std::env::var(var)
        .map_err(|_| format!("environment variable {var} (bridge.key-env) is not set"))?;
    wire::parse_key(text.trim()).ok_or_else(|| {
        format!("environment variable {var} (bridge.key-env): not a bridge key (64 hex digits)")
    })
}

impl Bridge {
    /// The module, without network yet (`start` listens).
    pub fn new(proxy: &Arc<Proxy>, cfg: BridgeConfig) -> Result<Arc<Bridge>, String> {
        let key = key(&cfg)?;
        Ok(Arc::new_cyclic(|me| Bridge {
            proxy: Arc::downgrade(proxy),
            me: me.clone(),
            key,
            cfg,
            started: Instant::now(),
            state: Mutex::new(State::default()),
            next: AtomicU64::new(1),
            wake: Notify::new(),
        }))
    }

    /// The key as 64 hex digits, for the files of new servers.
    pub fn key_hex(&self) -> String {
        wire::key_hex(&self.key)
    }

    pub fn native(self: &Arc<Self>) -> pumbo_host::Native {
        pumbo_host::Native {
            service: pumbo_bridge_proto::SERVICE,
            major: pumbo_bridge_proto::SERVICE_MAJOR,
            minor: pumbo_bridge_proto::SERVICE_MINOR,
            provider: Arc::new(Service(self.clone())),
        }
    }

    /// Opens the listener and starts the pairing task.
    pub async fn start(self: &Arc<Self>) -> Result<SocketAddr, String> {
        let listener = TcpListener::bind(&self.cfg.listen)
            .await
            .map_err(|e| format!("bridge listen {}: {e}", self.cfg.listen))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        if !crate::config::is_private_host(&addr.ip().to_string()) {
            warn!(
                "bridge listens on {addr}, outside loopback and private networks: bridge frames are signed, not encrypted; use a tunnel (WireGuard/Tailscale)"
            );
        }
        info!("bridge listening on {addr}");
        tokio::spawn(self.clone().accept_loop(listener));
        tokio::spawn(self.clone().pairing());
        Ok(addr)
    }

    fn proxy(&self) -> Option<Arc<Proxy>> {
        self.proxy.upgrade()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn link(&self, server: &str) -> Option<Arc<Link>> {
        self.lock().servers.get(server)?.link.clone()
    }
}

/// `pumbo:bridge*` is never relayed (spec §6.4): the bridge does not use
/// plugin messages, so a client or a backend on it is up to no good.
pub fn is_channel(channel: &str) -> bool {
    channel.starts_with("pumbo:bridge")
}

/// Writes all of `rest`, giving up only when the bridge takes nothing for
/// [`WRITE_TIMEOUT`]: a slow link that keeps reading is not cut off, however
/// long the whole batch takes.
async fn write_moving<W: tokio::io::AsyncWrite + Unpin>(
    wr: &mut W,
    mut rest: &[u8],
) -> Result<(), String> {
    while !rest.is_empty() {
        match tokio::time::timeout(WRITE_TIMEOUT, wr.write(rest)).await {
            Ok(Ok(0)) => return Err("write: the connection is closed".into()),
            Ok(Ok(n)) => rest = rest.get(n..).unwrap_or_default(),
            Ok(Err(e)) => return Err(format!("write: {e}")),
            Err(_) => return Err("write timed out (the bridge read nothing for 10 s)".into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// A batch that takes 30 s on a link the bridge keeps reading goes out
    /// whole; a bridge that reads nothing ends it after `WRITE_TIMEOUT`.
    #[tokio::test(start_paused = true)]
    async fn writes_time_out_only_without_progress() {
        let (mut wr, mut rd) = tokio::io::duplex(1024);
        let reader = tokio::spawn(async move {
            let (mut got, mut buf) = (0, [0u8; 1024]);
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break got,
                    Ok(n) => got += n,
                }
            }
        });
        let batch = vec![7u8; 30 * 1024];
        let started = Instant::now();
        assert_eq!(write_moving(&mut wr, &batch).await, Ok(()));
        assert!(started.elapsed() > WRITE_TIMEOUT * 2);
        drop(wr);
        assert_eq!(reader.await.unwrap(), batch.len());

        let (mut wr, _rd) = tokio::io::duplex(1024);
        let started = Instant::now();
        let err = write_moving(&mut wr, &batch).await.unwrap_err();
        assert!(err.starts_with("write timed out"), "{err}");
        assert!(started.elapsed() < WRITE_TIMEOUT + Duration::from_secs(1));
    }

    #[test]
    fn teleport_states() {
        assert_eq!(Tp::Free.busy(), None);
        assert_eq!(Tp::Awaiting(3).busy(), Some(None));
        let old = Instant::now() - SENT_TTL - Duration::from_millis(1);
        assert_eq!(Tp::Sent(old).busy(), None, "a lost teleport stops blocking");
        assert!(matches!(Tp::Sent(Instant::now()).busy(), Some(Some(_))));
        assert!(matches!(
            Tp::Confirmed(Instant::now()).busy(),
            Some(Some(_))
        ));
        let old = Instant::now() - CONFIRM_GRACE;
        assert_eq!(Tp::Confirmed(old).busy(), None);
        assert!(is_channel("pumbo:bridge") && is_channel("pumbo:bridge/x"));
        assert!(!is_channel("pumbo:other"));
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_readable_key_file_is_tightened() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = std::env::temp_dir().join(format!("pumbo-bridge-key-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();
        let mode = || std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let key = load_key(path_str).unwrap();
        assert_eq!(mode(), 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(load_key(path_str).unwrap(), key);
        assert_eq!(mode(), 0o600);
        // Not ours to change (root's /dev/null, 0666): a warning, no error.
        use std::os::unix::fs::MetadataExt as _;
        let ours = std::fs::metadata(&path).unwrap().uid();
        if ours != 0 && std::fs::metadata("/dev/null").unwrap().uid() != ours {
            let e = load_key("/dev/null").unwrap_err();
            assert!(e.contains("not a bridge key"), "{e}");
        }
        std::fs::remove_file(&path).unwrap();
        let unset = BridgeConfig {
            key_env: "PUMBO_TEST_NO_SUCH_VARIABLE".into(),
            ..BridgeConfig::default()
        };
        assert_eq!(
            super::key(&unset).unwrap_err(),
            "environment variable PUMBO_TEST_NO_SUCH_VARIABLE (bridge.key-env) is not set"
        );
    }
}
