//! The part of a session after login (§3.3, §2.8, §2.9): relaying to one
//! backend at a time, switching servers, fallback, reconnecting to the same
//! server, proxy commands, chat cancelled with a replacement `chat_ack`,
//! plugin messages and `bungeecord:main`.
//!
//! Switching follows Velocity (GPL-3.0, `ClientPlaySessionHandler.doSwitch`
//! and the backend `ConfigSessionHandler`): the next server logs in while the
//! player still plays, then the client gets `start_configuration`, the old
//! server is dropped and the next one's configuration is relayed. Unlike
//! Velocity the proxy keeps reading the next server in the meantime, answers
//! its keep-alives and pings itself and holds the rest (§2.9, §3.3 step 3).
//! Everything runs in one `select!` loop per session.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use pumbo_core::profile::{ForwardedPlayer, GameProfile};
use pumbo_core::routing::SelectReason;
use pumbo_protocol::packets::commands::{
    Commands, FLAG_EXECUTABLE, FLAG_SUGGESTIONS, NODE_ARGUMENT, NODE_LITERAL, Node, Parser,
    ParserProperties,
};
use pumbo_protocol::packets::common::KeepAlive;
use pumbo_protocol::{Phase, RawFrame, VersionModule};
use pumbo_text::Component;
use tokio::sync::{Notify, mpsc};
use tokio::time::{Instant, sleep_until};
use tracing::info;
use uuid::Uuid;

use crate::commands;
use crate::config::SignedChatCancel;
use crate::conn::{Conn, ConnError, WRITE_TIMEOUT};
use crate::limits::TokenBucket;
use crate::metrics::{GaugeGuard, Metrics};
use crate::server::{ChatInput, PlayerGuard, Proxy, Runtime, SessionCmd, Urgent};
use crate::session::{BackendFail, Fail, kick_in, queue_out, random_bytes, stopped};
use crate::world::{PlayerInput, VirtualCmd};
use pumbo_virtual::{Input, VirtualPlayer};

mod backend;
mod channels;
mod client;
mod switching;
mod virtual_world;

use backend::pumpkin_trail;
use switching::next_event;

/// Time for the client to acknowledge `start_configuration` (§3.3 step 4).
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// Own keep-alives while the client waits without a server (§2.9).
const OWN_KEEP_ALIVE: Duration = Duration::from_secs(10);
/// Keep-alive IDs of the backend waiting for the client's answer (§2.9).
const MAX_PENDING_KEEP_ALIVES: usize = 32;
/// Bytes waiting for a slow peer above which the other side is not read
/// until the peer takes them (2 MiB).
const BACKLOG: usize = 2 << 20;

type LoginFuture = Pin<Box<dyn Future<Output = Result<Conn, BackendFail>> + Send>>;
/// A kick waiting for `on-server-kicked` (E5): decision, kick text, shutdown-like.
type KickFuture =
    Pin<Box<dyn Future<Output = (crate::plugins::KickDecision, Box<Component>, bool)> + Send>>;
/// Redirects by `on-server-connect` per request, against loops.
const MAX_REDIRECTS: u8 = 3;
/// Decompressed bytes a player may send per byte of `client-bytes-per-second`:
/// the byte limit counts compressed data, this bounds the inflating behind it.
const DECODED_PER_CLIENT_BYTE: f64 = 4.0;
/// Messages and titles, action bar and boss bars waiting for a client to be
/// in play; more are dropped.
const MAX_DEFERRED: usize = 64;
/// Cookies one server may store for a player; more are dropped.
const MAX_COOKIES: usize = 64;

/// How a session ended.
/// How long the gates wait for `client_information` sent after the acknowledgement.
// Fixed; a config option when someone needs a slower link than this.
const SETTINGS_WAIT: Duration = Duration::from_millis(500);

pub(crate) enum End {
    Kick(Box<Component>),
    BackendKicked,
    ClientClosed,
    Protocol(String),
    /// The client connection failed other than by a plain close.
    ClientLost(String),
}

impl End {
    /// A closed connection, a reset or a broken pipe is how clients usually
    /// leave; anything else (a write timeout, say) is worth its own text.
    fn client_lost(e: ConnError) -> Self {
        use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
        match e {
            ConnError::Closed => End::ClientClosed,
            ConnError::Io(io)
                if matches!(
                    io.kind(),
                    UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe
                ) =>
            {
                End::ClientClosed
            }
            e => End::ClientLost(e.to_string()),
        }
    }
}

fn end_kick(text: &str) -> End {
    End::Kick(Box::new(Component::text(text)))
}

impl From<Fail> for End {
    fn from(e: Fail) -> Self {
        match e {
            Fail::Kick(t) => End::Kick(t),
            Fail::Closed | Fail::Timeout => End::ClientClosed,
            Fail::Protocol(why) => End::Protocol(why),
        }
    }
}

/// Why the player is being connected to a server.
#[derive(Debug, Clone)]
enum Purpose {
    Join,
    Switch {
        quiet: bool,
    },
    /// The previous server went away.
    Fallback {
        reason: Box<Component>,
    },
    Reconnect,
}

/// Servers to try for one purpose, in order.
struct Request {
    purpose: Purpose,
    candidates: VecDeque<String>,
    /// The server being tried now.
    target: String,
    refusal: Option<Component>,
    redirects: u8,
}

/// The next server, before it gets the client. One per session, so the
/// size difference of the variants does not matter.
#[allow(clippy::large_enum_variant)]
enum Next {
    Idle,
    Connecting(LoginFuture),
    /// Logged in and in configuration; its packets wait here (§3.3 step 3).
    Ready {
        conn: Conn,
        buffer: Vec<(RawFrame, Bytes)>,
        bytes: usize,
        since: Instant,
        sent_settings: bool,
        sent_brand: bool,
    },
}

#[allow(clippy::large_enum_variant)]
enum NextEvent {
    Login(Result<Conn, BackendFail>),
    Read(Result<usize, ConnError>),
}

/// The chat filter (E4) and the plugins (`on-chat`, `on-backend-command`
/// for commands in a `command-filter`, E5) may cancel. A free function, so
/// the session's state is not borrowed across the plugin call.
async fn cancelled(
    proxy: &Proxy,
    profile: &GameProfile,
    server: Option<String>,
    input: ChatInput<'_>,
    signed: bool,
) -> bool {
    if proxy.chat_cancelled(profile, input) {
        return true;
    }
    let Some(p) = proxy.plugins.get() else {
        return false;
    };
    match input {
        ChatInput::Message(m) => p.chat(profile.id, m).await,
        ChatInput::Command(c) => {
            p.backend_command(profile.id, server.as_deref(), c, signed)
                .await
        }
    }
}

async fn kick_decision(
    f: &mut Option<KickFuture>,
) -> (crate::plugins::KickDecision, Box<Component>, bool) {
    match f {
        Some(fut) => fut.as_mut().await,
        None => std::future::pending().await,
    }
}

/// Writes the current server and, with `read`, reads it ([`Conn::fill_writing`]).
async fn backend_io(
    conn: Option<&mut Conn>,
    room: Option<usize>,
    read: bool,
) -> Result<Option<usize>, ConnError> {
    match conn {
        Some(c) => c.fill_writing(room, read).await,
        None => std::future::pending().await,
    }
}

/// The server the client is relayed to.
struct Backend {
    conn: Conn,
    name: String,
    /// Phase of its stream to the client.
    phase: Phase,
    /// With the time they reached the client (for `%player_ping%`).
    keep_alives: VecDeque<(i64, Instant)>,
    in_bundle: bool,
    /// Its play `login` arrived.
    joined: bool,
    /// Literal names under the root of its command tree; `None` until the
    /// tree arrives or when it could not be read.
    roots: Option<HashSet<String>>,
    /// The backend's last command tree, to merge again after a change.
    tree: Option<RawFrame>,
    last_read: Instant,
    chat_sessions: bool,
    /// The client's chat session went to this backend.
    session_sent: bool,
    signed_cancel: SignedChatCancel,
}

/// A resource pack the client has (§3.3).
struct Pack {
    id: Uuid,
    hash: String,
    /// Backend that sent it (see [`Play::epoch`]).
    epoch: u64,
    loaded: bool,
}

/// Client state that outlives a backend (§3.3), and what a new backend gets.
#[derive(Default)]
struct Tracked {
    client_information: Option<Bytes>,
    /// Payload of the client's `minecraft:brand` message.
    brand: Option<Bytes>,
    bossbars: HashSet<Uuid>,
    tab_list: bool,
    dialog: bool,
    packs: Vec<Pack>,
    links_epoch: Option<u64>,
    report_epoch: Option<u64>,
    /// Tab header and footer set by a plugin; sent again on every join.
    proxy_tab: Option<(Box<Component>, Box<Component>)>,
    /// Cookies each server stored, by server and key: kept by the proxy, so
    /// one server cannot read or replace another's (servers with
    /// `allow-transfers` use the client's own).
    cookies: HashMap<String, HashMap<String, Vec<u8>>>,
}

enum Step {
    Forward,
    Drop,
    /// Forward, then end the session.
    ForwardAndEnd(End),
    End(End),
    /// A kick that moves the player on (§3.2).
    Lost(Box<Component>),
    /// The play `login`: forward, then note the join.
    Joined,
}

struct Play<'a> {
    proxy: &'a Arc<Proxy>,
    rt: Arc<Runtime>,
    /// The client's version module, borrowed: packets read it without
    /// touching the shared `Arc`'s count, which every session of the same
    /// version would contend on.
    module: &'a dyn VersionModule,
    /// The same module, for what keeps its own handle (a virtual world).
    module_arc: Arc<dyn VersionModule>,
    player: ForwardedPlayer,
    peer: SocketAddr,
    host: String,
    client: Conn,
    /// Phase of the client's stream.
    client_phase: Phase,
    /// Phase of the stream to the client.
    out_phase: Phase,
    backend: Option<Backend>,
    request: Option<Request>,
    next: Next,
    /// `start_configuration` sent, waiting for the acknowledgement.
    ack_deadline: Option<Instant>,
    /// Reconnect to this server once the client is in configuration.
    reconnect_on_ack: Option<String>,
    /// Counts attached backends; tells the packs of the current one apart.
    epoch: u64,
    tracked: Tracked,
    own_keep_alive: Option<i64>,
    next_own_keep_alive: Instant,
    /// Messages for the player once it is in play.
    notices: Vec<Component>,
    /// Titles, action bar and boss bars for the player once it is in play.
    deferred: Vec<SessionCmd>,
    last_reconnect: Option<Instant>,
    cmds: mpsc::Receiver<SessionCmd>,
    /// Kicks and gate releases, outside the queue.
    urgent: Arc<Urgent>,
    commands_changed: Arc<Notify>,
    packets: TokenBucket,
    bytes: TokenBucket,
    decoded: TokenBucket,
    suggestions: TokenBucket,
    last_client: Instant,
    pending_kick: Option<KickFuture>,
    /// In a virtual world (PumboAPI): the proxy is the server.
    vworld: Option<InVirtual>,
    /// A gate holds the player before the first server (§5.6).
    holding: bool,
    gate_deadline: Option<Instant>,
    /// Counted in `limits.max-in-gates` while a gate holds it in a virtual world.
    in_gate: Option<GaugeGuard>,
    /// Enter a virtual world once the client acknowledged `start_configuration`.
    enter_on_ack: Option<Enter>,
    /// The server a virtual world was entered from; release goes back there.
    return_to: Option<String>,
    /// In the Pumbox (or on the way there) since this runtime: a newer one
    /// lets the player try the servers again.
    pumbox: Option<Arc<Runtime>>,
    /// The backend health seen then: a change (a server offline, back, or
    /// its wait over) also lets the player try again.
    pumbox_health: u64,
    own_keep_alive_at: Instant,
    /// Next render of the `tab` header and footer.
    next_tab: Instant,
    /// Id of the `trail` particle in the versions Pumpkin sends it broken in (D-COMPAT-1).
    trail: Option<i32>,
}

/// A player in a virtual world and its driver's input channel.
struct InVirtual {
    player: VirtualPlayer,
    tag: u64,
    inputs: mpsc::Sender<PlayerInput>,
}

/// A virtual world to enter.
struct Enter {
    world: Arc<pumbo_virtual::World>,
    at: pumbo_virtual::Position,
    commands: Vec<String>,
    tag: u64,
    inputs: mpsc::Sender<PlayerInput>,
}

/// Picks a server, logs in there and relays until the session ends.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    proxy: &Arc<Proxy>,
    rt: &Arc<Runtime>,
    client: Conn,
    module: Arc<dyn VersionModule>,
    profile: GameProfile,
    guard: PlayerGuard,
    cmds: mpsc::Receiver<SessionCmd>,
    peer: SocketAddr,
    host: &str,
) {
    let _online = GaugeGuard::new(proxy.metrics.clone(), |m| &m.players_online);
    let _guard = guard;
    let limits = &rt.config.limits;
    let now = Instant::now();
    let commands_changed = proxy.commands_signal(profile.id).unwrap_or_default();
    let mut play = Play {
        proxy,
        rt: rt.clone(),
        trail: pumpkin_trail(module.protocol()),
        module: &*module,
        module_arc: module.clone(),
        player: ForwardedPlayer {
            profile,
            address: peer.ip(),
        },
        peer,
        host: host.to_string(),
        client,
        client_phase: Phase::Configuration,
        out_phase: Phase::Configuration,
        backend: None,
        request: None,
        next: Next::Idle,
        ack_deadline: None,
        reconnect_on_ack: None,
        epoch: 0,
        tracked: Tracked::default(),
        own_keep_alive: None,
        next_own_keep_alive: now + OWN_KEEP_ALIVE,
        notices: Vec::new(),
        deferred: Vec::new(),
        last_reconnect: None,
        commands_changed,
        cmds,
        urgent: _guard.urgent.clone(),
        packets: TokenBucket::new(limits.client_packets_per_second),
        bytes: TokenBucket::new(limits.client_bytes_per_second),
        decoded: {
            let rate = f64::from(limits.client_bytes_per_second) * DECODED_PER_CLIENT_BYTE;
            TokenBucket::with_burst(rate, rate)
        },
        suggestions: TokenBucket::new(limits.command_suggestions_per_second),
        last_client: now,
        vworld: None,
        holding: false,
        gate_deadline: None,
        in_gate: None,
        enter_on_ack: None,
        return_to: None,
        pumbox: None,
        pumbox_health: 0,
        own_keep_alive_at: now,
        next_tab: now,
        pending_kick: None,
    };
    // The client sends its settings and brand with `login_acknowledged`; they
    // wait in the read buffer until it sends more. Read now, so the gates and
    // the first server see them (`player-info.settings`, §5.2).
    let mut early = play.client_frames().await;
    // A client may send its settings in a later TCP segment than the
    // acknowledgement: wait a moment for them, so the gates know the language.
    let settings_until = tokio::time::Instant::now() + SETTINGS_WAIT;
    while early.is_none() && play.tracked.client_information.is_none() {
        match tokio::time::timeout_at(settings_until, play.client.fill()).await {
            Ok(Ok(_)) => early = play.client_frames().await,
            // Timed out (a bot without settings) or the client went away; the
            // session loop sees the latter on its next read.
            _ => break,
        }
    }
    let end = if let Some(end) = early {
        end
    } else if rt.config.virtual_world.test_gate {
        // Tests only: a built-in gate with a virtual world.
        play.holding = true;
        crate::world::enter_test_gate(proxy, play.player.profile.id);
        play.run().await
    } else if let Some(plugins) = proxy.plugins.get().cloned() {
        // Plugin gates (§5.6), with the client in configuration: a gate that
        // holds may put it in a virtual world; passing all of them releases.
        play.holding = true;
        let (proxy, id) = (proxy.clone(), play.player.profile.id);
        tokio::spawn(async move {
            let cmd = match plugins.gates(id).await {
                Ok(()) => SessionCmd::Virtual(Box::new(VirtualCmd::Release)),
                Err(text) => SessionCmd::Kick(Box::new(text)),
            };
            proxy.send_to(id, cmd);
        });
        play.run().await
    } else {
        // Only here: picking the candidates hands out the one retry of an
        // offline server, which the gates' release picks again on its own.
        let candidates = play.candidates(SelectReason::InitialJoin, None);
        if candidates.is_empty() {
            let text = play.no_server_text();
            match play.wait_in_pumbox(text) {
                Some(end) => end,
                None => play.run().await,
            }
        } else {
            play.request = Some(Request {
                purpose: Purpose::Join,
                candidates,
                target: String::new(),
                refusal: None,
                redirects: 0,
            });
            match play.try_next() {
                Some(end) => end,
                None => play.run().await,
            }
        }
    };
    let name = play.player.profile.name.clone();
    let reason = match end {
        End::Kick(text) => {
            let phase = play.out_phase;
            let _ = kick_in(&mut play.client, play.module, phase, &text).await;
            text.plain_text()
        }
        End::BackendKicked => "kicked by the backend".into(),
        End::ClientClosed => "left".into(),
        End::ClientLost(why) => format!("connection lost: {why}"),
        End::Protocol(why) => {
            Metrics::inc(&proxy.metrics.protocol_errors);
            format!("protocol error: {why}")
        }
    };
    if let Some(v) = play.vworld.take() {
        let _ = v.inputs.try_send(PlayerInput {
            player: v.tag,
            input: Input::Left,
        });
    }
    play.client.shutdown().await;
    match play.backend.take() {
        Some(mut b) => {
            b.conn.shutdown().await;
            info!("{name} disconnected from {}: {reason}", b.name);
        }
        None => info!("{name} disconnected: {reason}"),
    }
}

impl Play<'_> {
    async fn run(&mut self) -> End {
        let m = self.proxy.metrics.clone();
        let idle = Duration::from_millis(self.rt.config.limits.idle_timeout_ms);
        let mut shutdown = self.proxy.shutdown_signal();
        let mut changes = self.proxy.runtime_changes();
        // Built once, not on every turn of the loop: each new timer and Notify
        // registration takes a lock in the runtime.
        let stop = stopped(&mut shutdown);
        let commands_changed = self.commands_changed.clone();
        let commands = commands_changed.notified();
        let timer = sleep_until(self.deadline(idle));
        let urgent = self.urgent.clone();
        let urgent_signal = urgent.notify.notified();
        tokio::pin!(stop, commands, timer, urgent_signal);
        loop {
            if let Some(cmd) = urgent.take()
                && let Some(end) = self.urgent_cmd(cmd)
            {
                return end;
            }
            self.follow_runtime();
            if let Err(end) = self.progress() {
                return end;
            }
            if let Err(end) = self.write_servers() {
                return end;
            }
            // Every client packet moves the deadline later. The armed timer is
            // only moved earlier, or re-armed once it fired: an early wake is
            // harmless, `timers` checks each deadline against the clock.
            let deadline = self.deadline(idle);
            if deadline < timer.deadline() || timer.is_elapsed() {
                timer.as_mut().reset(deadline);
            }
            // While a kick waits for plugins the backend is not read any more,
            // nor while the client has a backlog; the client is not read while
            // the backend has one. Both are still written.
            let kick_pending = self.pending_kick.is_some();
            let client_full = self.client_full();
            let backend_full = self.backend_full();
            tokio::select! {
                biased;
                () = &mut stop => return end_kick("The proxy is shutting down."),
                // Taken at the top of the loop.
                () = &mut urgent_signal => urgent_signal.set(urgent.notify.notified()),
                // The client is written here, so it is read while it takes its
                // data slowly: holding its packets back would pass them on in
                // one burst later, over the limits of the proxy and backend.
                r = self.client.fill_writing(client_full.then_some(BACKLOG), !backend_full) => {
                    let n = match r {
                        Ok(n) => n,
                        Err(e) => return End::client_lost(e),
                    };
                    // It took its backlog: the backend is read again.
                    let Some(n) = n else { continue };
                    self.last_client = Instant::now();
                    Metrics::add(&m.bytes_from_clients, n as u64);
                    if !self.bytes.take(n as f64) {
                        Metrics::inc(&m.kicked_packet_rate);
                        return end_kick("Too many packets.");
                    }
                    if let Some(end) = self.client_frames().await {
                        return end;
                    }
                }
                d = kick_decision(&mut self.pending_kick) => {
                    self.pending_kick = None;
                    if let Some(end) = self.on_kick(d) {
                        return end;
                    }
                }
                r = backend_io(
                    self.backend.as_mut().map(|b| &mut b.conn),
                    backend_full.then_some(BACKLOG),
                    !kick_pending && !client_full,
                ) => {
                    let end = match r {
                        // It took its backlog: the client is read again.
                        Ok(None) => continue,
                        Ok(Some(n)) => {
                            Metrics::add(&m.bytes_from_backends, n as u64);
                            if let Some(b) = self.backend.as_mut() {
                                b.last_read = Instant::now();
                            }
                            self.backend_frames()
                        }
                        Err(_) => self.backend_lost(None),
                    };
                    if let Some(end) = end {
                        return end;
                    }
                }
                ev = next_event(&mut self.next) => {
                    if let Some(end) = self.on_next(ev) {
                        return end;
                    }
                }
                Some(cmd) = self.cmds.recv() => {
                    if let Some(end) = self.command(cmd) {
                        return end;
                    }
                }
                () = &mut commands => {
                    // A change stored meanwhile is covered by this rebuild.
                    commands.set(commands_changed.notified());
                    if commands.as_mut().enable() {
                        commands.set(commands_changed.notified());
                    }
                    self.on_commands_changed();
                }
                // The Pumbox waits for a new server list (`progress`).
                _ = changes.changed(), if self.pumbox.is_some() => {}
                () = &mut timer => {
                    if let Some(end) = self.timers(idle) {
                        return end;
                    }
                }
            }
        }
    }

    /// A reload or a change of the servers run by the proxy replaced the
    /// runtime: the server list, commands and settings follow it at once (the
    /// idle limit at the next session), the client gets the command tree again.
    fn follow_runtime(&mut self) {
        let now = self.proxy.runtime();
        if !Arc::ptr_eq(&now, &self.rt) {
            self.rt = now;
            self.resend_commands();
        }
    }

    /// The client is behind by more than [`BACKLOG`].
    fn client_full(&self) -> bool {
        self.client.pending() > BACKLOG
    }

    /// The current server is behind by more than [`BACKLOG`].
    fn backend_full(&self) -> bool {
        self.backend
            .as_ref()
            .is_some_and(|b| b.conn.pending() > BACKLOG)
    }

    fn deadline(&self, idle: Duration) -> Instant {
        // Not read while its server has a backlog, so not idle either.
        let mut d = if self.backend_full() {
            Instant::now() + idle
        } else {
            self.last_client + idle
        };
        // The next write to a peer that stopped reading fails: the client
        // ends the session, a server is lost.
        let stalls = [
            self.client.stalled_since(),
            self.backend.as_ref().and_then(|b| b.conn.stalled_since()),
            match &self.next {
                Next::Ready { conn, .. } => conn.stalled_since(),
                _ => None,
            },
        ];
        for s in stalls.into_iter().flatten() {
            d = d.min(s + WRITE_TIMEOUT);
        }
        match &self.backend {
            // Not read meanwhile, so not idle either.
            Some(_) if self.client_full() => {}
            Some(b) => d = d.min(b.last_read + idle),
            None => d = d.min(self.next_own_keep_alive),
        }
        if let Some(a) = self.ack_deadline {
            d = d.min(a);
        }
        if let Some(g) = self.gate_deadline {
            d = d.min(g);
        }
        if self.tab_configured() {
            d = d.min(self.next_tab);
        }
        if let Next::Ready { since, .. } = &self.next {
            d = d.min(*since + Duration::from_millis(self.rt.config.switching.config_buffer_ms));
        }
        d
    }

    fn timers(&mut self, idle: Duration) -> Option<End> {
        let now = Instant::now();
        if !self.backend_full() && now >= self.last_client + idle {
            Metrics::inc(&self.proxy.metrics.timeouts_idle);
            return Some(end_kick("Timed out."));
        }
        if self.ack_deadline.is_some_and(|d| now >= d) {
            return Some(end_kick("Timed out while switching servers."));
        }
        if self.gate_deadline.is_some_and(|d| now >= d) {
            return Some(end_kick("You took too long to log in."));
        }
        if self.tab_configured() && now >= self.next_tab {
            self.next_tab = now + Duration::from_millis(self.rt.config.tab.refresh_ms.max(100));
            self.config_tab(false);
        }
        if !self.client_full()
            && self
                .backend
                .as_ref()
                .is_some_and(|b| now >= b.last_read + idle)
        {
            Metrics::inc(&self.proxy.metrics.timeouts_idle);
            return self.backend_lost(Some(Component::text("Timed out.")));
        }
        if self.backend.is_none() && now >= self.next_own_keep_alive {
            self.next_own_keep_alive = now + OWN_KEEP_ALIVE;
            let id = random_bytes::<8>().map(i64::from_be_bytes).unwrap_or(1);
            self.own_keep_alive = Some(id);
            self.own_keep_alive_at = now;
            let phase = self.out_phase;
            if let Err(e) = queue_out(&mut self.client, self.module, phase, &KeepAlive { id }) {
                return Some(e.into());
            }
        }
        let limit = Duration::from_millis(self.rt.config.switching.config_buffer_ms);
        if let Next::Ready { since, .. } = &self.next
            && now >= *since + limit
        {
            self.next_failed(format!("still configuring after {limit:?}"));
            return self.try_next();
        }
        None
    }

    /// Writes to the servers what their sockets take now; the rest goes out
    /// in `run` while the session goes on, as for the client.
    fn write_servers(&mut self) -> Result<(), End> {
        if let Some(b) = self.backend.as_mut()
            && b.conn.try_write().is_err()
            && let Some(end) = self.backend_lost(None)
        {
            return Err(end);
        }
        if let Next::Ready { conn, .. } = &mut self.next
            && conn.try_write().is_err()
        {
            self.next_failed("connection closed".into());
            if let Some(end) = self.try_next() {
                return Err(end);
            }
        }
        Ok(())
    }
}

fn millis(since: Instant) -> u32 {
    u32::try_from(since.elapsed().as_millis()).unwrap_or(u32::MAX)
}

/// Releases of a protocol for messages: "26.3" or "26.1–26.1.2".
fn releases(names: &[String]) -> String {
    match (names.first(), names.last()) {
        (Some(a), Some(b)) if a != b => format!("{a}–{b}"),
        (Some(a), _) => a.clone(),
        _ => "?".into(),
    }
}

/// Adds `names` (with their subcommands) under the root at `root_at` (§2.8):
/// each a literal with a greedy string after it that asks the server for
/// suggestions. False when this version has no string argument.
fn add_literals(
    m: &dyn VersionModule,
    tree: &mut Commands,
    root_at: usize,
    names: Vec<(String, Vec<String>)>,
) -> Result<bool, Fail> {
    let Some(string) = (0..1024).find(|i| m.command_argument_type(*i) == Some("brigadier:string"))
    else {
        return Ok(false);
    };
    let literal = |tree: &mut Commands, name: String, mut children: Vec<i32>| {
        let arg = i32::try_from(tree.nodes.len()).map_err(|_| Fail::Protocol("tree".into()))?;
        tree.nodes.push(Node {
            flags: NODE_ARGUMENT | FLAG_EXECUTABLE | FLAG_SUGGESTIONS,
            children: Vec::new(),
            redirect: None,
            name: Some(commands::arg_name(&name).into()),
            parser: Some(Parser {
                id: string,
                properties: ParserProperties::String(2),
            }),
            suggestions: Some("minecraft:ask_server".into()),
        });
        children.insert(0, arg);
        tree.nodes.push(Node {
            flags: NODE_LITERAL | FLAG_EXECUTABLE,
            children,
            redirect: None,
            name: Some(name),
            parser: None,
            suggestions: None,
        });
        Ok::<i32, Fail>(arg + 1)
    };
    for (name, subs) in names {
        let subs = subs
            .into_iter()
            .map(|s| literal(tree, s, Vec::new()))
            .collect::<Result<Vec<_>, _>>()?;
        let at = literal(tree, name, subs)?;
        if let Some(root) = tree.nodes.get_mut(root_at) {
            root.children.push(at);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_errors_beyond_a_plain_quit_keep_their_text() {
        use std::io::{Error, ErrorKind};
        let left = |e| matches!(End::client_lost(e), End::ClientClosed);
        assert!(left(ConnError::Closed));
        assert!(left(Error::from(ErrorKind::ConnectionReset).into()));
        assert!(left(Error::from(ErrorKind::BrokenPipe).into()));
        assert!(left(Error::from(ErrorKind::UnexpectedEof).into()));
        let text = |e| match End::client_lost(e) {
            End::ClientLost(why) => why,
            _ => String::new(),
        };
        assert_eq!(text(ConnError::WriteTimeout), "write timed out");
        assert!(text(Error::other("no route").into()).contains("no route"));
    }

    #[test]
    fn release_ranges() {
        assert_eq!(releases(&["26.3".into()]), "26.3");
        assert_eq!(
            releases(&["26.1".into(), "26.1.1".into(), "26.1.2".into()]),
            "26.1–26.1.2"
        );
        assert_eq!(releases(&[]), "?");
    }
}
