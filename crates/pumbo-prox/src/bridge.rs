//! PumboBridge, proxy side (pumbo-plugins `docs/pumbo-bridge-spec.md` §3,
//! §6): the `pumbobridge` plugin of every Pumpkin server connects here over
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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use pumbo_bridge_proto::api::{self, PermsMode};
use pumbo_bridge_proto::wire::{self, Codec, Event, Msg, Side};
use pumbo_bridge_proto::{PROTO, err, method};
use pumbo_host::wit::services::ServiceError;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::server::{Proxy, SessionCmd};

/// `bridge` section of the proxy config.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct BridgeConfig {
    pub enabled: bool,
    /// Where bridges connect (`ip:port`); loopback or a private network.
    pub listen: String,
    /// The bridge key (64 hex digits), created on the first start.
    pub key_file: String,
    pub command_timeout_ms: u64,
    /// Plugins that may send commands; every plugin with `uses` may query.
    pub trusted_plugins: Vec<String>,
    pub stats_ms: u32,
    pub player_stats_ms: u32,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        BridgeConfig {
            enabled: false,
            listen: "127.0.0.1:25578".into(),
            key_file: "bridge.key".into(),
            command_timeout_ms: 5000,
            trusted_plugins: vec!["pumbo-core".into()],
            stats_ms: 5000,
            player_stats_ms: 2000,
        }
    }
}

impl BridgeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && self.listen.parse::<SocketAddr>().is_err() {
            return Err(format!("bridge.listen {} is not ip:port", self.listen));
        }
        Ok(())
    }
}

const HANDSHAKE: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(5);
const SILENCE: Duration = Duration::from_secs(15);
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
    perms_sent: Mutex<HashMap<Uuid, BTreeMap<String, bool>>>,
    closed: AtomicBool,
    close: Notify,
}

impl Link {
    fn send(&self, m: Msg) -> bool {
        self.out.try_send(m).is_ok()
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.close.notify_one();
    }

    fn server(&self) -> Option<String> {
        self.server.lock().ok()?.clone()
    }

    fn mode(&self) -> PermsMode {
        self.perms_mode.lock().map(|m| *m).unwrap_or_default()
    }
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

/// Reads the bridge key, creating it (mode 0600) when missing.
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
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    wire::parse_key(&text).ok_or_else(|| format!("{path}: not a bridge key (64 hex digits)"))
}

impl Bridge {
    /// The module, without network yet (`start` listens).
    pub fn new(proxy: &Arc<Proxy>, cfg: BridgeConfig) -> Result<Arc<Bridge>, String> {
        let key = load_key(&cfg.key_file)?;
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

    async fn accept_loop(self: Arc<Self>, listener: TcpListener) {
        let mut stop = match self.proxy() {
            Some(p) => p.shutdown_signal(),
            None => return,
        };
        loop {
            tokio::select! {
                () = crate::session::stopped(&mut stop) => return,
                r = listener.accept() => match r {
                    Ok((sock, peer)) => {
                        if !self.admit(peer.ip()) {
                            continue;
                        }
                        tokio::spawn(self.clone().session(sock, peer));
                    }
                    Err(e) => {
                        warn!("bridge accept: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                },
            }
        }
    }

    /// Limits before the handshake (spec §7).
    fn admit(&self, ip: IpAddr) -> bool {
        let mut st = self.lock();
        if st.unauthenticated >= MAX_UNAUTHENTICATED {
            return false;
        }
        let e = st.tries.entry(ip).or_insert((Instant::now(), 0));
        if e.0.elapsed() > Duration::from_secs(10) {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
        if e.1 > MAX_TRIES_PER_IP {
            return false;
        }
        if st.tries.len() > 4096 {
            st.tries
                .retain(|_, (t, _)| t.elapsed() < Duration::from_secs(10));
        }
        st.unauthenticated += 1;
        true
    }

    async fn session(self: Arc<Self>, mut sock: TcpStream, peer: SocketAddr) {
        let mut said_hello = false;
        let r = tokio::time::timeout(HANDSHAKE, self.handshake(&mut sock, &mut said_hello)).await;
        self.lock().unauthenticated -= 1;
        let why = match r {
            Ok(Ok(x)) => Ok(x),
            Ok(Err(why)) => Err(why),
            Err(_) => Err("handshake timed out".to_string()),
        };
        let (hello, proto, codec, buf) = match why {
            Ok(x) => x,
            Err(why) if said_hello => {
                warn!("bridge from {peer} refused: {why}");
                self.reject(peer.ip(), why);
                return;
            }
            Err(why) => {
                debug!("bridge port: connection from {peer} dropped: {why}");
                return;
            }
        };
        let (tx, rx) = mpsc::channel(OUT_QUEUE);
        let link = Arc::new(Link {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            peer,
            out: tx,
            pending: Mutex::new(HashMap::new()),
            version: hello,
            proto,
            ping_at: Mutex::new(None),
            rtt_ms: AtomicU32::new(u32::MAX),
            caps: Mutex::new(Vec::new()),
            catalog: Mutex::new(Vec::new()),
            server: Mutex::new(None),
            perms_mode: Mutex::new(PermsMode::Bridge),
            perms_sent: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            close: Notify::new(),
        });
        debug!("bridge from {peer} authenticated, pairing");
        self.lock().unpaired.insert(link.id, (link.clone(), 0));
        self.wake.notify_one();
        self.run(&link, sock, codec, buf, rx).await;
        self.dropped(&link);
    }

    fn reject(&self, ip: IpAddr, why: String) {
        let mut st = self.lock();
        if st.rejected.len() > 1024 {
            st.rejected.clear();
        }
        st.rejected.insert(ip, (Instant::now(), why));
    }

    #[allow(clippy::type_complexity)]
    async fn handshake(
        &self,
        sock: &mut TcpStream,
        said_hello: &mut bool,
    ) -> Result<((String, String, String), [u16; 2], Codec, Vec<u8>), String> {
        let mut codec = Codec::new(Side::Proxy);
        let mut buf = Vec::new();
        let Msg::Hello {
            proto,
            bridge,
            pumpkin,
            mc,
            instance,
            nb,
        } = read_msg(sock, &mut codec, &mut buf).await?
        else {
            return Err("expected hello".into());
        };
        *said_hello = true;
        if proto[0] != PROTO[0] {
            return Err(format!(
                "bridge protocol {}.{}, this proxy speaks {}.x",
                proto[0], proto[1], PROTO[0]
            ));
        }
        let mut np = [0u8; 32];
        aws_lc_rs::rand::fill(&mut np).map_err(|_| "random generator failed".to_string())?;
        let proof = wire::proxy_proof(&self.key, &nb, &np, &instance);
        let frame = codec
            .encode(&Msg::Challenge { np, proof })
            .map_err(|e| e.to_string())?;
        sock.write_all(&frame).await.map_err(|e| e.to_string())?;
        // A bridge with another key sees a wrong proof here and hangs up.
        let auth = read_msg(sock, &mut codec, &mut buf)
            .await
            .map_err(|_| "closed after the challenge: wrong bridge key?".to_string())?;
        let Msg::Auth { proof } = auth else {
            return Err("expected auth".into());
        };
        if !wire::check_bridge_proof(&self.key, &np, &nb, &instance, &proof) {
            return Err("wrong bridge key".into());
        }
        codec.set_key(wire::session_key(&self.key, &nb, &np));
        Ok(((bridge, pumpkin, mc), proto, codec, buf))
    }

    async fn run(
        &self,
        link: &Arc<Link>,
        sock: TcpStream,
        mut codec: Codec,
        mut buf: Vec<u8>,
        mut out: mpsc::Receiver<Msg>,
    ) {
        let (mut rd, mut wr) = sock.into_split();
        let mut tx = codec.clone();
        let mut last_rx = Instant::now();
        let mut ping = tokio::time::interval(PING_EVERY);
        let why = 'session: loop {
            loop {
                match codec.decode(&buf) {
                    Ok(Some((m, n))) => {
                        buf.drain(..n);
                        last_rx = Instant::now();
                        self.on_msg(link, m);
                    }
                    Ok(None) => break,
                    Err(e) => break 'session e.to_string(),
                }
            }
            if link.closed.load(Ordering::Relaxed) {
                break "closed by the proxy".into();
            }
            tokio::select! {
                r = rd.read_buf(&mut buf) => match r {
                    Ok(0) => break "closed by the bridge".into(),
                    Ok(_) => {}
                    Err(e) => break e.to_string(),
                },
                Some(m) = out.recv() => {
                    let mut bytes = match tx.encode(&m) {
                        Ok(b) => b,
                        Err(e) => break e.to_string(),
                    };
                    while let Ok(m) = out.try_recv() {
                        match tx.encode(&m) {
                            Ok(b) => bytes.extend(b),
                            Err(e) => break 'session e.to_string(),
                        }
                    }
                    if let Err(e) = wr.write_all(&bytes).await {
                        break e.to_string();
                    }
                }
                _ = ping.tick() => {
                    if last_rx.elapsed() > SILENCE {
                        break "silent for 15 s".into();
                    }
                    if let Ok(mut p) = link.ping_at.lock() {
                        p.get_or_insert_with(Instant::now);
                    }
                    link.send(Msg::Ping);
                }
                () = link.close.notified() => break "closed by the proxy".into(),
            }
        };
        let server = link.server().unwrap_or_else(|| "?".into());
        debug!("bridge session {} ({server}) ended: {why}", link.peer);
    }

    /// A session ended: its server loses the bridge, calls in flight end
    /// with `disconnected`.
    fn dropped(&self, link: &Arc<Link>) {
        let mut lost = None;
        {
            let mut st = self.lock();
            st.unpaired.remove(&link.id);
            if let Some(server) = link.server()
                && let Some(slot) = st.servers.get_mut(&server)
                && slot.link.as_ref().is_some_and(|l| l.id == link.id)
            {
                slot.link = None;
                slot.since = Some(SystemTime::now());
                lost = Some(server);
            }
        }
        if let Ok(mut p) = link.pending.lock() {
            for (_, tx) in p.drain() {
                let _ = tx.send(Err((err::DISCONNECTED.into(), None)));
            }
        }
        if let Some(server) = lost {
            info!("bridge on {server} disconnected");
            if let Some(p) = self.proxy().and_then(|p| p.plugins.get().cloned()) {
                p.host.set_server_value(&server, "tps", None);
                p.host.set_server_value(&server, "mspt", None);
            }
            self.publish_status(&server);
        }
    }

    fn on_msg(&self, link: &Arc<Link>, m: Msg) {
        match m {
            Msg::Info { caps, catalog } => {
                if let Ok(mut c) = link.caps.lock() {
                    *c = caps;
                }
                if let Ok(mut c) = link.catalog.lock() {
                    *c = catalog;
                }
            }
            Msg::Seen { nonce } => self.pair(link, &nonce),
            Msg::Sync { .. } => self.synced(link),
            Msg::Res {
                id,
                ok,
                err: e,
                detail,
            } => {
                let tx = link.pending.lock().ok().and_then(|mut p| p.remove(&id));
                if let Some(tx) = tx {
                    let r = match e {
                        None => Ok(ok.unwrap_or(ciborium::Value::Null)),
                        Some(code) => Err((code, detail)),
                    };
                    let _ = tx.send(r);
                }
            }
            Msg::Ev { ev } => self.event(link, ev),
            Msg::StatsServer { tps, mspt, .. } => {
                if let (Some(server), Some(p)) = (
                    link.server(),
                    self.proxy().and_then(|p| p.plugins.get().cloned()),
                ) {
                    p.host
                        .set_server_value(&server, "tps", Some(format!("{tps:.1}")));
                    p.host
                        .set_server_value(&server, "mspt", Some(format!("{mspt:.1}")));
                }
            }
            Msg::StatsPlayer { players } => {
                let Some(p) = self.proxy().and_then(|p| p.plugins.get().cloned()) else {
                    return;
                };
                for s in players {
                    let gm = s.gamemode.map(|g| {
                        wire::value(&g)
                            .ok()
                            .and_then(|v| v.as_text().map(str::to_string))
                            .unwrap_or_default()
                    });
                    let vals = [
                        ("health", s.health.map(|h| format!("{h:.0}"))),
                        ("food", s.food.map(|f| f.to_string())),
                        ("level", s.level.map(|l| l.to_string())),
                        ("gamemode", gm),
                        ("world", s.world),
                    ];
                    for (k, v) in vals {
                        if let Some(v) = v {
                            p.set_player_value(s.player, k, v);
                        }
                    }
                }
            }
            Msg::Ping => {
                link.send(Msg::Pong);
            }
            Msg::Pong => {
                if let Some(t) = link.ping_at.lock().ok().and_then(|mut p| p.take()) {
                    let ms = u32::try_from(t.elapsed().as_millis()).unwrap_or(u32::MAX - 1);
                    link.rtt_ms.store(ms, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }

    fn pair(&self, link: &Arc<Link>, nonce: &str) {
        let (server, old) = {
            let mut st = self.lock();
            let Some((server, _)) = st.nonces.get(nonce).cloned() else {
                debug!("bridge {} saw an unknown pairing nonce", link.peer);
                return;
            };
            if st.unpaired.remove(&link.id).is_none() {
                if link.server().as_deref() != Some(server.as_str()) {
                    warn!(
                        "the bridge paired with {} also answers for {server}: one Pumpkin under two names is not supported, {server} stays without a bridge",
                        link.server().unwrap_or_default()
                    );
                }
                return;
            }
            let slot = st.servers.entry(server.clone()).or_default();
            let old = slot.link.replace(link.clone());
            slot.since = Some(SystemTime::now());
            slot.ever = true;
            (server, old)
        };
        if let Some(old) = old {
            info!("a newer bridge session replaces the old one on {server}");
            old.close();
        }
        if let Ok(mut s) = link.server.lock() {
            *s = Some(server.clone());
        }
        let groups = self
            .proxy()
            .and_then(|p| p.plugins.get().map(|pl| pl.host.groups_of(&server)))
            .unwrap_or_default();
        link.send(Msg::Welcome {
            server: server.clone(),
            groups,
            stats_ms: self.cfg.stats_ms,
            player_stats_ms: self.cfg.player_stats_ms,
        });
        let (b, p, mc) = &link.version;
        info!(
            "bridge on {server} paired ({}, PumboBridge {b}, Pumpkin {p}, Minecraft {mc})",
            link.peer
        );
        self.publish_status(&server);
    }

    /// After `sync`: permission sets of the players there and the export.
    fn synced(&self, link: &Arc<Link>) {
        let (Some(server), Some(proxy)) = (link.server(), self.proxy()) else {
            return;
        };
        for p in proxy.players() {
            if p.server.as_deref() == Some(server.as_str()) {
                self.send_perms(link, &server, p.id);
            }
        }
        self.send_export(link, &server);
    }

    fn event(&self, link: &Arc<Link>, ev: Event) {
        let Some(server) = link.server() else {
            return;
        };
        match &ev {
            Event::Join { player, .. } => self.send_perms(link, &server, *player),
            Event::Leave { player, .. } => {
                if let Ok(mut s) = link.perms_sent.lock() {
                    s.remove(player);
                }
            }
            Event::PermsMode { mode } => {
                let old = link
                    .perms_mode
                    .lock()
                    .map(|mut m| std::mem::replace(&mut *m, *mode))
                    .unwrap_or(*mode);
                if old != *mode {
                    match mode {
                        PermsMode::Local => {
                            info!("{server}: ranks from the local PumboPerms");
                            self.send_export(link, &server);
                        }
                        PermsMode::Bridge => {
                            info!("{server}: ranks from the proxy again");
                            if let Ok(mut s) = link.perms_sent.lock() {
                                s.clear();
                            }
                            self.synced(link);
                        }
                    }
                }
            }
            _ => {}
        }
        if let Some(p) = self.proxy().and_then(|p| p.plugins.get().cloned()) {
            let e = wire::BusEvent { server, ev };
            if let Ok(b) = cbor(&e) {
                p.host.publish(&pumbo_contracts::BRIDGE_EVENT, b);
            }
        }
    }

    fn publish_status(&self, server: &str) {
        let Some(p) = self.proxy().and_then(|p| p.plugins.get().cloned()) else {
            return;
        };
        if let Some(s) = self.status(Some(server)).into_iter().next()
            && let Ok(b) = cbor(&s)
        {
            p.host.publish(&pumbo_contracts::BRIDGE_STATUS, b);
        }
    }

    /// A command whose answer nobody waits for (the bridge still answers).
    fn notify(&self, link: &Link, m: &str, args: ciborium::Value) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        link.send(Msg::Cmd {
            id,
            method: m.into(),
            args,
        });
    }

    /// Sends the player's decisions for `server` unless they did not change
    /// (spec §5.2). A local PumboPerms rules there: nothing to send.
    fn send_perms(&self, link: &Link, server: &str, player: Uuid) {
        if link.mode() == PermsMode::Local {
            return;
        }
        let Some(plugins) = self.proxy().and_then(|p| p.plugins.get().cloned()) else {
            return;
        };
        let catalog = link.catalog.lock().map(|c| c.clone()).unwrap_or_default();
        let Some(nodes) = plugins.backend_permissions(player, server, &catalog) else {
            return;
        };
        if let Ok(mut sent) = link.perms_sent.lock() {
            if sent.get(&player) == Some(&nodes) {
                return;
            }
            sent.insert(player, nodes.clone());
        }
        let set = api::PermSet {
            player,
            nodes,
            extra: BTreeMap::new(),
        };
        if let Ok(v) = wire::value(&set) {
            self.notify(link, method::PERM_SET, v);
        }
    }

    /// The table for the server (from the permission provider when it runs,
    /// so asked in a task).
    fn send_export(&self, link: &Arc<Link>, server: &str) {
        let Some(plugins) = self.proxy().and_then(|p| p.plugins.get().cloned()) else {
            return;
        };
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (link, server) = (Arc::clone(link), server.to_string());
        tokio::spawn(async move {
            let data = plugins.host.permissions_export(&server).await;
            let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, data.as_bytes());
            let fingerprint = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
            if let Ok(args) = wire::value(&api::PermsExport { fingerprint, data }) {
                link.send(Msg::Cmd {
                    id,
                    method: method::PERMS_EXPORT.into(),
                    args,
                });
            }
        });
    }

    /// The session asks: the player is about to log in to `server` (before
    /// forwarding), so its permission set goes first.
    pub fn connecting(&self, player: Uuid, server: &str) {
        if let Some(link) = self.link(server) {
            self.send_perms(&link, server, player);
        }
    }

    /// The player's permissions or context changed.
    pub fn perms_changed(&self, player: Uuid, server: Option<&str>) {
        if let Some(server) = server {
            self.connecting(player, server);
        }
    }

    fn link(&self, server: &str) -> Option<Arc<Link>> {
        self.lock().servers.get(server)?.link.clone()
    }

    /// The pairing task: while authenticated sessions wait, every server gets
    /// a status ping with a fresh nonce every 10 s (at once for a new
    /// session); a session that matches nothing after 3 rounds is closed.
    async fn pairing(self: Arc<Self>) {
        loop {
            tokio::select! {
                () = self.wake.notified() => {}
                () = tokio::time::sleep(PAIR_EVERY) => {}
            }
            let Some(proxy) = self.proxy() else { return };
            let rt = proxy.runtime();
            self.missing(&rt);
            let (targets, give_up) = {
                let mut st = self.lock();
                st.nonces
                    .retain(|_, (_, t)| t.elapsed() < PAIR_EVERY * (PAIR_TRIES + 1));
                if st.unpaired.is_empty() {
                    continue;
                }
                let mut give_up = Vec::new();
                for (link, tries) in st.unpaired.values_mut() {
                    *tries += 1;
                    if *tries > PAIR_TRIES {
                        give_up.push(link.clone());
                    }
                }
                for l in &give_up {
                    st.unpaired.remove(&l.id);
                }
                let mut targets = Vec::new();
                for (name, s) in &rt.config.servers {
                    let mut n = [0u8; 16];
                    if aws_lc_rs::rand::fill(&mut n).is_err() {
                        continue;
                    }
                    let nonce: String = n.iter().map(|b| format!("{b:02x}")).collect();
                    st.nonces
                        .insert(nonce.clone(), (name.clone(), Instant::now()));
                    targets.push((s.address.clone(), nonce));
                }
                (targets, give_up)
            };
            for l in give_up {
                warn!(
                    "bridge from {} does not match any server in servers:, closing it",
                    l.peer
                );
                self.reject(l.peer.ip(), "matches no server in servers:".into());
                l.close();
            }
            for (address, nonce) in targets {
                tokio::spawn(async move {
                    if let Err(e) = status_ping(&address, &nonce).await {
                        debug!("pairing ping to {address}: {e}");
                    }
                });
            }
        }
    }

    /// One line per server that never had a bridge, a minute after start.
    fn missing(&self, rt: &crate::server::Runtime) {
        if self.started.elapsed() < MISSING_AFTER {
            return;
        }
        let mut st = self.lock();
        for name in rt.config.servers.keys() {
            let slot = st.servers.entry(name.clone()).or_default();
            if !slot.ever && !slot.warned {
                slot.warned = true;
                info!(
                    "no bridge on {name}: server commands follow op levels there, bridge calls answer no-bridge"
                );
            }
        }
    }

    /// `status` of the service and `/pumbo bridge`.
    pub fn status(&self, only: Option<&str>) -> Vec<api::ServerStatus> {
        let Some(proxy) = self.proxy() else {
            return Vec::new();
        };
        let rt = proxy.runtime();
        let st = self.lock();
        let host_ip = |address: &str| {
            let (host, _) = crate::config::split_host_port(address)?;
            if host.eq_ignore_ascii_case("localhost") {
                return Some(IpAddr::from([127, 0, 0, 1]));
            }
            host.parse::<IpAddr>().ok()
        };
        let same = |a: IpAddr, b: IpAddr| a == b || (a.is_loopback() && b.is_loopback());
        // Servers without a session, by address: a rejection from that
        // address is theirs only when it is the only such server there.
        let without: Vec<(&String, Option<IpAddr>)> = rt
            .config
            .servers
            .iter()
            .filter(|(n, _)| st.servers.get(*n).is_none_or(|s| s.link.is_none()))
            .map(|(n, s)| (n, host_ip(&s.address)))
            .collect();
        let rejection = |name: &str| {
            let ip = without.iter().find(|(n, _)| n.as_str() == name)?.1?;
            let (_, (_, why)) = st
                .rejected
                .iter()
                .filter(|(_, (t, _))| t.elapsed() < REJECTED_SHOWN)
                .find(|(peer, _)| same(**peer, ip))?;
            let candidates = without
                .iter()
                .filter(|(_, i)| i.is_some_and(|i| same(i, ip)))
                .count();
            (candidates == 1).then(|| why.clone())
        };
        rt.config
            .servers
            .keys()
            .filter(|n| only.is_none_or(|o| o == n.as_str()))
            .map(|name| {
                let slot = st.servers.get(name);
                let link = slot.and_then(|s| s.link.clone());
                let rejected = if link.is_none() {
                    rejection(name)
                } else {
                    None
                };
                let state = match (&link, &rejected, slot.is_some_and(|s| s.ever)) {
                    (Some(_), _, _) => api::State::Connected,
                    (None, Some(_), _) => api::State::Rejected,
                    (None, None, true) => api::State::Lost,
                    (None, None, false) => api::State::None,
                };
                let (bridge, pumpkin, mc) =
                    link.as_ref().map(|l| l.version.clone()).unwrap_or_default();
                let rtt = link
                    .as_ref()
                    .map(|l| l.rtt_ms.load(Ordering::Relaxed))
                    .filter(|ms| *ms != u32::MAX);
                api::ServerStatus {
                    server: name.clone(),
                    state,
                    since: slot.and_then(|s| s.since).map(now_ms),
                    bridge,
                    pumpkin,
                    mc,
                    caps: link
                        .as_ref()
                        .and_then(|l| l.caps.lock().ok().map(|c| c.clone()))
                        .unwrap_or_default(),
                    perms_mode: link.as_ref().map(|l| l.mode()).unwrap_or_default(),
                    proto: link
                        .as_ref()
                        .map(|l| format!("{}.{}", l.proto[0], l.proto[1]))
                        .unwrap_or_default(),
                    latency_ms: rtt,
                    detail: rejected,
                }
            })
            .collect()
    }

    /// `/pumbo bridge [status|key]` as MiniMessage lines (plain columns for
    /// the console). `secrets`: the console or `pumbo.proxy.bridge.key`.
    pub fn admin(&self, args: &[String], console: bool, secrets: bool) -> Vec<String> {
        let esc = |s: &str| pumbo_text::template::escape_mini(s);
        match args.first().map(String::as_str) {
            Some("key") if secrets => vec![
                "<p>Bridge key</p> <s>(config.yml of PumboBridge on every server):".into(),
                format!("<ok>{}", wire::key_hex(&self.key)),
            ],
            Some("key") => vec!["<err>You may not see the bridge key.".into()],
            None | Some("status") => {
                let now = SystemTime::now();
                let mut rows = vec![[
                    ("server".to_string(), "s"),
                    ("bridge".into(), "s"),
                    ("version / contract".into(), "s"),
                    ("ping".into(), "s"),
                    ("since".into(), "s"),
                ]];
                for s in self.status(None) {
                    let (state, style) = match s.state {
                        api::State::Connected => ("connected", "ok"),
                        api::State::Lost => ("lost", "warn"),
                        api::State::None => ("none", "muted"),
                        api::State::Rejected => ("rejected", "err"),
                    };
                    let connected = s.state == api::State::Connected;
                    let since = match (s.since, connected || s.state == api::State::Lost) {
                        (Some(ms), true) => {
                            let t = UNIX_EPOCH + Duration::from_millis(ms);
                            ago(now.duration_since(t).unwrap_or_default())
                        }
                        _ => "-".into(),
                    };
                    rows.push([
                        (s.server.clone(), "p"),
                        (state.into(), style),
                        (
                            if connected {
                                format!("{} / {}", s.bridge, s.proto)
                            } else {
                                s.detail.clone().unwrap_or_else(|| "-".into())
                            },
                            if connected { "s" } else { style },
                        ),
                        (
                            s.latency_ms.map_or("-".into(), |ms| format!("{ms} ms")),
                            "s",
                        ),
                        (since, "s"),
                    ]);
                }
                let mut lines = vec![format!(
                    "<p>PumboBridge</p> <s>listening on {}",
                    esc(&self.cfg.listen)
                )];
                lines.extend(columns(&rows, console));
                lines
            }
            Some(_) => vec!["<p>/pumbo bridge</p> <s>[status | key]".into()],
        }
    }

    /// A call of `pumbo:bridge@1.0` by plugin `caller`.
    async fn service(
        &self,
        caller: &str,
        m: &str,
        payload: &[u8],
    ) -> Result<Vec<u8>, ServiceError> {
        let is_command = method::COMMANDS.contains(&m);
        if !is_command && !method::QUERIES.contains(&m) {
            return Err(
                if [method::PERM_SET, method::PERMS_EXPORT, method::GUARD_RULES].contains(&m) {
                    rejected(err::NOT_ALLOWED, None)
                } else {
                    ServiceError::UnknownMethod
                },
            );
        }
        if is_command && !self.cfg.trusted_plugins.iter().any(|p| p == caller) {
            return Err(rejected(err::NOT_ALLOWED, None));
        }
        let bad = |e: String| rejected(err::BAD_ARGS, Some(e));
        if m == method::STATUS {
            let s: api::Status = ciborium::from_reader(payload).map_err(|e| bad(e.to_string()))?;
            return cbor(&self.status(s.server.as_deref()));
        }
        if m == method::SEND_TO {
            let s: api::SendTo = ciborium::from_reader(payload).map_err(|e| bad(e.to_string()))?;
            return self.send_to(s);
        }
        let args: ciborium::Value =
            ciborium::from_reader(payload).map_err(|e| bad(e.to_string()))?;
        let route: api::Route = args.deserialized().unwrap_or_default();
        let server = match route.server {
            Some(s) => s,
            None => {
                let who = route
                    .player
                    .or(route.viewer)
                    .ok_or_else(|| bad("server or player needed".into()))?;
                self.proxy()
                    .and_then(|p| p.server_of(who))
                    .ok_or_else(|| rejected(err::OFFLINE, None))?
            }
        };
        let v = self
            .exec(&server, m, args, route.player)
            .await
            .map_err(|(c, d)| rejected(&c, d))?;
        cbor(&v)
    }

    /// Runs one method on a server's bridge (teleports wait for the
    /// previous one first).
    async fn exec(
        &self,
        server: &str,
        m: &str,
        args: ciborium::Value,
        player: Option<Uuid>,
    ) -> Reply {
        let link = self
            .link(server)
            .ok_or_else(|| (err::NO_BRIDGE.to_string(), None))?;
        let caps_known = link
            .caps
            .lock()
            .map(|c| !c.is_empty() && !c.iter().any(|x| x == m));
        if caps_known.unwrap_or(false) {
            return Err((err::UNSUPPORTED.into(), None));
        }
        let gate = match (m == method::TELEPORT, player) {
            (true, Some(p)) => {
                let gate = self.proxy().and_then(|x| x.teleport_gate(p));
                if let Some(g) = &gate {
                    self.hold(p, g).await.map_err(|c| (c.to_string(), None))?;
                }
                gate
            }
            _ => None,
        };
        let r = self.call(&link, m, args).await;
        if let Some(g) = gate
            && r.is_err()
        {
            g.send_if_modified(|t| {
                let sent = matches!(t, Tp::Sent(_));
                if sent {
                    *t = Tp::Free;
                }
                sent
            });
        }
        r
    }

    async fn call(&self, link: &Link, m: &str, args: ciborium::Value) -> Reply {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = link.pending.lock() {
            p.insert(id, tx);
        }
        let sent = link.send(Msg::Cmd {
            id,
            method: m.into(),
            args,
        });
        let forget = || {
            if let Ok(mut p) = link.pending.lock() {
                p.remove(&id);
            }
        };
        if !sent {
            forget();
            return Err((err::BUSY.into(), None));
        }
        let wait = Duration::from_millis(self.cfg.command_timeout_ms);
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err((err::DISCONNECTED.into(), None)),
            Err(_) => {
                // A late answer finds no waiter and is dropped.
                forget();
                Err((err::TIMEOUT.into(), None))
            }
        }
    }

    /// Waits until the player has no unconfirmed teleport (spec §4.2).
    async fn hold(&self, player: Uuid, gate: &TpGate) -> Result<(), &'static str> {
        let generation = self.next.fetch_add(1, Ordering::Relaxed);
        let (cancel_tx, mut cancel) = oneshot::channel();
        if let Some((_, old)) = self.lock().held.insert(player, (generation, cancel_tx)) {
            let _ = old.send(());
        }
        let deadline = Instant::now() + TELEPORT_WAIT;
        let mut rx = gate.subscribe();
        let r = loop {
            let state = *rx.borrow_and_update();
            let Some(until) = state.busy() else {
                break Ok(());
            };
            let wake = until.unwrap_or(deadline).min(deadline);
            tokio::select! {
                _ = &mut cancel => break Err(err::SUPERSEDED),
                () = tokio::time::sleep_until(deadline) => break Err(err::EXPIRED),
                r = rx.changed() => if r.is_err() { break Ok(()) },
                () = tokio::time::sleep_until(wake) => {}
            }
        };
        {
            let mut st = self.lock();
            if st.held.get(&player).is_some_and(|(g, _)| *g == generation) {
                st.held.remove(&player);
            }
        }
        if r.is_ok() {
            gate.send_replace(Tp::Sent(Instant::now()));
        }
        r
    }

    /// `send-to`: `no-bridge` before the player moves; the actions run on
    /// the target after the player joined and confirmed its spawn teleport.
    fn send_to(&self, s: api::SendTo) -> Result<Vec<u8>, ServiceError> {
        if self.link(&s.server).is_none() {
            return Err(rejected(err::NO_BRIDGE, None));
        }
        let proxy = self.proxy().ok_or_else(|| rejected(err::OFFLINE, None))?;
        let queued = proxy.send_to(
            s.player,
            SessionCmd::Connect {
                server: s.server.clone(),
                quiet: false,
            },
        );
        if !queued {
            return Err(rejected(err::OFFLINE, None));
        }
        if !s.actions.is_empty()
            && let Some(me) = self.me.upgrade()
        {
            tokio::spawn(async move { me.arrive(s).await });
        }
        cbor(&())
    }

    async fn arrive(self: Arc<Self>, s: api::SendTo) {
        let deadline =
            Instant::now() + Duration::from_millis(u64::from(s.ttl_ms.unwrap_or(ARRIVE_TTL_MS)));
        let arrived = loop {
            let Some(proxy) = self.proxy() else { return };
            // Server first: the session marks the spawn teleport as due
            // before it changes the server.
            let there = proxy.server_of(s.player).as_deref() == Some(s.server.as_str());
            let free = proxy
                .teleport_gate(s.player)
                .is_some_and(|g| g.borrow().busy().is_none());
            if there && free {
                break true;
            }
            if Instant::now() >= deadline || proxy.teleport_gate(s.player).is_none() {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        if !arrived {
            debug!(
                "send-to {}: {} did not arrive in time, {} actions dropped",
                s.server,
                s.player,
                s.actions.len()
            );
            return;
        }
        for a in s.actions {
            if let Err((c, d)) = self
                .exec(&s.server, &a.method, a.args, Some(s.player))
                .await
            {
                warn!(
                    "send-to {}: {} for {} failed: {c} {}",
                    s.server,
                    a.method,
                    s.player,
                    d.unwrap_or_default()
                );
            }
        }
    }
}

async fn read_msg(
    sock: &mut TcpStream,
    codec: &mut Codec,
    buf: &mut Vec<u8>,
) -> Result<Msg, String> {
    loop {
        if let Some((m, n)) = codec.decode(buf).map_err(|e| e.to_string())? {
            buf.drain(..n);
            return Ok(m);
        }
        if sock.read_buf(buf).await.map_err(|e| e.to_string())? == 0 {
            return Err("closed during the handshake".into());
        }
    }
}

fn varint(out: &mut Vec<u8>, v: i32) {
    let mut v = v as u32;
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// A status ping whose host name is `pumbo-bridge.<nonce>` (spec §3.4).
async fn status_ping(address: &str, nonce: &str) -> Result<(), String> {
    let (host, port) =
        crate::config::split_host_port(address).ok_or_else(|| "bad address".to_string())?;
    let mut sock = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect((host, port)))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let name = format!("{}{nonce}", pumbo_bridge_proto::PING_PREFIX);
    let mut hs = Vec::new();
    varint(&mut hs, 0);
    varint(&mut hs, 767);
    varint(&mut hs, name.len() as i32);
    hs.extend_from_slice(name.as_bytes());
    hs.extend_from_slice(&port.to_be_bytes());
    varint(&mut hs, 1);
    let mut out = Vec::new();
    varint(&mut out, hs.len() as i32);
    out.extend(hs);
    out.extend([1, 0]);
    sock.write_all(&out).await.map_err(|e| e.to_string())?;
    // The answer is not needed; read a little so the server is not cut off.
    let mut buf = [0u8; 512];
    let _ = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf)).await;
    Ok(())
}

/// The native provider of `pumbo:bridge@1.0`.
struct Service(Arc<Bridge>);

impl pumbo_host::NativeService for Service {
    fn call(
        &self,
        caller: String,
        method: String,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<Vec<u8>, ServiceError>> {
        let b = self.0.clone();
        Box::pin(async move { b.service(&caller, &method, &payload).await })
    }

    fn admin(&self, args: &[String], console: bool, secrets: bool) -> Vec<String> {
        self.0.admin(args, console, secrets)
    }
}

/// A line of [`Bridge::admin`] without tags (console).
pub fn plain(line: &str) -> String {
    let none =
        pumbo_text::StyleSheet::new(["p", "s", "ok", "warn", "err", "muted"].map(|t| (t, "")));
    pumbo_text::parse_mini_styled(line, &none).plain_text()
}

/// `5m 03s`.
fn ago(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// Width in the default Minecraft font (as `pumbo_common::rich`).
fn px(text: &str) -> u32 {
    text.chars()
        .map(|c| match c {
            '!' | ',' | '.' | ':' | ';' | '|' | 'i' => 2,
            '\'' | '`' | 'l' => 3,
            ' ' | 'I' | '[' | ']' | 't' => 4,
            '"' | '(' | ')' | '*' | '<' | '>' | 'f' | 'k' | '{' | '}' => 5,
            '@' | '~' => 7,
            _ => 6,
        })
        .sum()
}

/// Spaces `w` pixels wide: 4 px normal and 5 px bold ones.
fn pad(w: u32) -> String {
    let bold = (0..=3u32)
        .find(|b| 5 * b <= w && (w - 5 * b).is_multiple_of(4))
        .unwrap_or(0);
    let normal = (w.saturating_sub(5 * bold)) / 4;
    let mut out = " ".repeat(normal as usize);
    if bold > 0 {
        out.push_str(&format!("<b>{}</b>", " ".repeat(bold as usize)));
    }
    out
}

/// Rows of styled cells as aligned lines: pixels in chat, characters in the
/// console (styles are tags of the host style sheet).
fn columns<const N: usize>(rows: &[[(String, &str); N]], console: bool) -> Vec<String> {
    let width = |t: &str| {
        if console {
            t.chars().count() as u32
        } else {
            px(t)
        }
    };
    let mut widths = [0u32; N];
    for row in rows {
        for (w, (t, _)) in widths.iter_mut().zip(row) {
            *w = (*w).max(width(t));
        }
    }
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (i, ((t, style), w)) in row.iter().zip(widths).enumerate() {
                let text = pumbo_text::template::escape_mini(t);
                line.push_str(&format!("<{style}>{text}</{style}>"));
                if i + 1 < N {
                    let gap = w - width(t);
                    if console {
                        line.push_str(&" ".repeat(gap as usize + 2));
                    } else {
                        line.push_str(&pad(gap + 8));
                    }
                }
            }
            line
        })
        .collect()
}

/// `pumbo:bridge*` is never relayed (spec §6.4): the bridge does not use
/// plugin messages, so a client or a backend on it is up to no good.
pub fn is_channel(channel: &str) -> bool {
    channel.starts_with("pumbo:bridge")
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
