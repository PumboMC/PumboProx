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

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use pumbo_core::profile::{ForwardedPlayer, GameProfile};
use pumbo_core::routing::{BackendInfo, SelectContext, SelectReason};
use pumbo_core::translate::TranslationPlan;
use pumbo_protocol::packets;
use pumbo_protocol::packets::commands::{
    Commands, FLAG_EXECUTABLE, FLAG_SUGGESTIONS, NODE_ARGUMENT, NODE_LITERAL, Node, Parser,
    ParserProperties,
};
use pumbo_protocol::packets::common::{
    self as pc, ClearDialog, ClientboundCustomPayload, CustomClickAction, CustomReportDetails,
    Disconnect, KeepAlive, Ping, Pong, ResourcePackPop, ResourcePackPush, ResourcePackResponse,
    ServerLinks, ServerboundCustomPayload,
};
use pumbo_protocol::packets::play::{
    BossAction, BossEvent, BundleDelimiter, Chat, ChatAck, ChatCommand, ChatCommandSigned,
    ClearTitles, CommandSuggestion, CommandSuggestions, Login, SetActionBarText, SetSubtitleText,
    SetTitleText, SetTitlesAnimation, StartConfiguration, Suggestion, SystemChat, TabList,
};
use pumbo_protocol::types::{Reader, WriteExt};
use pumbo_protocol::{Direction, PacketKind, Phase, RawFrame, VersionModule};
use pumbo_text::{Component, Content, TextFormat};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until, timeout};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::bungee;
use crate::commands::{self, Source};
use crate::config::{PacksOnSwitch, SignedChatCancel};
use crate::conn::{Conn, ConnError};
use crate::limits::TokenBucket;
use crate::metrics::{GaugeGuard, Metrics};
use crate::server::{ChatInput, PlayerGuard, Proxy, Runtime, SessionCmd};
use crate::session::{
    BACKEND_LOGIN_TIMEOUT, BackendFail, BackendVersion, Fail, backend_login, backend_version, ctx,
    forward, kick_in, queue, queue_out, random_bytes, stopped,
};
use crate::status::parse_text;
use crate::world::{BossbarOp, PlayerInput, VirtualCmd};
use pumbo_virtual::{Handled, Input, VirtualPlayer};

/// Time for the client to acknowledge `start_configuration` (§3.3 step 4).
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// Own keep-alives while the client waits without a server (§2.9).
const OWN_KEEP_ALIVE: Duration = Duration::from_secs(10);
/// Keep-alive IDs of the backend waiting for the client's answer (§2.9).
const MAX_PENDING_KEEP_ALIVES: usize = 32;

type LoginFuture = Pin<Box<dyn Future<Output = Result<Conn, BackendFail>> + Send>>;
/// A kick waiting for `on-server-kicked` (E5): decision, kick text, shutdown-like.
type KickFuture =
    Pin<Box<dyn Future<Output = (crate::plugins::KickDecision, Box<Component>, bool)> + Send>>;
/// Redirects by `on-server-connect` per request, against loops.
const MAX_REDIRECTS: u8 = 3;
/// Messages and titles, action bar and boss bars waiting for a client to be
/// in play; more are dropped.
const MAX_DEFERRED: usize = 64;

/// How a session ended.
/// How long the gates wait for `client_information` sent after the acknowledgement.
// ponytail: fixed; a config option when someone needs a slower link than this.
const SETTINGS_WAIT: Duration = Duration::from_millis(500);

pub(crate) enum End {
    Kick(Box<Component>),
    BackendKicked,
    ClientClosed,
    Protocol(String),
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

async fn next_event(next: &mut Next) -> NextEvent {
    match next {
        Next::Connecting(fut) => NextEvent::Login(fut.as_mut().await),
        Next::Ready { conn, .. } => NextEvent::Read(conn.fill().await),
        Next::Idle => std::future::pending().await,
    }
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

async fn fill_opt(conn: Option<&mut Conn>) -> Result<usize, ConnError> {
    match conn {
        Some(c) => c.fill().await,
        None => std::future::pending().await,
    }
}

/// Logs in to a backend in its own version (`module`); with a translator the
/// connection then speaks the client's version to the rest of the session (E3b).
fn login_future(
    rt: Arc<Runtime>,
    (module, translator): BackendVersion,
    address: String,
    player: ForwardedPlayer,
) -> LoginFuture {
    Box::pin(async move {
        let mut conn = timeout(
            BACKEND_LOGIN_TIMEOUT,
            backend_login(&rt, &*module, &address, &player),
        )
        .await
        .unwrap_or_else(|_| Err(BackendFail::Unavailable("login timed out".into())))?;
        if let Some(t) = translator {
            conn.set_translator(t);
        }
        Ok(conn)
    })
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
    module: Arc<dyn VersionModule>,
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
    packets: TokenBucket,
    bytes: TokenBucket,
    suggestions: TokenBucket,
    last_client: Instant,
    pending_kick: Option<KickFuture>,
    /// In a virtual world (PumboAPI): the proxy is the server.
    vworld: Option<InVirtual>,
    /// A gate holds the player before the first server (§5.6).
    holding: bool,
    gate_deadline: Option<Instant>,
    /// Enter a virtual world once the client acknowledged `start_configuration`.
    enter_on_ack: Option<Enter>,
    /// The server a virtual world was entered from; release goes back there.
    return_to: Option<String>,
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
    let mut play = Play {
        proxy,
        rt: rt.clone(),
        trail: pumpkin_trail(module.protocol()),
        module,
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
        cmds,
        packets: TokenBucket::new(limits.client_packets_per_second),
        bytes: TokenBucket::new(limits.client_bytes_per_second),
        suggestions: TokenBucket::new(limits.command_suggestions_per_second),
        last_client: now,
        vworld: None,
        holding: false,
        gate_deadline: None,
        enter_on_ack: None,
        return_to: None,
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
    let candidates = play.candidates(SelectReason::InitialJoin, None);
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
    } else if candidates.is_empty() {
        End::Kick(Box::new(play.no_server_text()))
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
    };
    let name = play.player.profile.name.clone();
    let reason = match end {
        End::Kick(text) => {
            let phase = play.out_phase;
            let _ = kick_in(&mut play.client, &*play.module, phase, &text).await;
            text.plain_text()
        }
        End::BackendKicked => "kicked by the backend".into(),
        End::ClientClosed => "left".into(),
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
        loop {
            if let Err(end) = self.progress() {
                return end;
            }
            if let Err(end) = self.flush().await {
                return end;
            }
            let deadline = self.deadline(idle);
            // While a kick waits for plugins the backend is not read any more.
            let kick_pending = self.pending_kick.is_some();
            tokio::select! {
                biased;
                () = stopped(&mut shutdown) => return end_kick("The proxy is shutting down."),
                r = self.client.fill() => {
                    let Ok(n) = r else { return End::ClientClosed };
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
                r = fill_opt(self.backend.as_mut().filter(|_| !kick_pending).map(|b| &mut b.conn)) => {
                    let end = match r {
                        Ok(n) => {
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
                () = sleep_until(deadline) => {
                    if let Some(end) = self.timers(idle) {
                        return end;
                    }
                }
            }
        }
    }

    fn deadline(&self, idle: Duration) -> Instant {
        let mut d = self.last_client + idle;
        if let Some(b) = &self.backend {
            d = d.min(b.last_read + idle);
        } else {
            d = d.min(self.next_own_keep_alive);
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
        if now >= self.last_client + idle {
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
        if self
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
            if let Err(e) = queue_out(&mut self.client, &*self.module, phase, &KeepAlive { id }) {
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

    async fn flush(&mut self) -> Result<(), End> {
        if self.client.flush().await.is_err() {
            return Err(End::ClientClosed);
        }
        if let Some(b) = self.backend.as_mut()
            && b.conn.flush().await.is_err()
            && let Some(end) = self.backend_lost(None)
        {
            return Err(end);
        }
        if let Next::Ready { conn, .. } = &mut self.next
            && conn.flush().await.is_err()
        {
            self.next_failed("connection closed".into());
            if let Some(end) = self.try_next() {
                return Err(end);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ switching

    /// Moves the state machine on: attaches a ready server to a client in
    /// configuration, or sends a playing client to configuration.
    fn progress(&mut self) -> Result<(), End> {
        let ready = matches!(self.next, Next::Ready { .. });
        if self.ack_deadline.is_some() {
            return Ok(());
        }
        // A virtual world still configuring the client finishes first.
        let configuring_virtual = self.vworld.as_ref().is_some_and(|v| !v.player.in_play());
        if ready
            && !configuring_virtual
            && self.client_phase == Phase::Configuration
            && self.out_phase == Phase::Configuration
        {
            return self.attach();
        }
        let wanted = ready
            || (self.backend.is_none() && self.vworld.is_none() && !self.holding)
            || self.reconnect_on_ack.is_some()
            || self.enter_on_ack.is_some();
        let in_bundle = self.backend.as_ref().is_some_and(|b| b.in_bundle);
        if wanted && !in_bundle && self.client_phase == Phase::Play && self.out_phase == Phase::Play
        {
            self.reconfigure()?;
        }
        Ok(())
    }

    /// Clears what would outlive the server and sends `start_configuration`
    /// (outside a bundle, §3.3 step 4).
    fn reconfigure(&mut self) -> Result<(), End> {
        let module = self.module.clone();
        let m = &*module;
        let c = &mut self.client;
        let p = Phase::Play;
        for id in std::mem::take(&mut self.tracked.bossbars) {
            queue_out(
                c,
                m,
                p,
                &BossEvent {
                    id,
                    action: BossAction::Remove,
                },
            )?;
        }
        if std::mem::take(&mut self.tracked.tab_list) {
            let empty = Component::text("").to_nbt(TextFormat::for_protocol(m.protocol().0));
            queue_out(
                c,
                m,
                p,
                &TabList {
                    header: empty.clone(),
                    footer: empty,
                },
            )?;
        }
        queue_out(c, m, p, &ClearTitles { reset: true })?;
        if std::mem::take(&mut self.tracked.dialog) {
            queue_out(c, m, p, &ClearDialog)?;
        }
        queue_out(c, m, p, &StartConfiguration)?;
        self.out_phase = Phase::Configuration;
        self.ack_deadline = Some(Instant::now() + ACK_TIMEOUT);
        Ok(())
    }

    /// The client acknowledged our `start_configuration`.
    fn after_ack(&mut self) -> Option<End> {
        if let Some(enter) = self.enter_on_ack.take() {
            self.return_to = self.drop_backend();
            return self.enter_now(enter).err();
        }
        // Leaving a virtual world for a server.
        self.vworld = None;
        if let Some(name) = self.reconnect_on_ack.take() {
            // The server would kick the old connection for the same UUID, so it
            // goes first (§3.3, reconnect).
            self.drop_backend();
            Metrics::inc(&self.proxy.metrics.reconnects);
            self.request = Some(Request {
                purpose: Purpose::Reconnect,
                candidates: VecDeque::from([name]),
                target: String::new(),
                refusal: None,
                redirects: 0,
            });
            return self.try_next();
        }
        if matches!(self.next, Next::Ready { .. }) {
            return None;
        }
        // The next server failed while the client was leaving: the old one
        // cannot take it back without a new login.
        let from = self.drop_backend();
        if self.request.is_none() {
            return self.fallback(from, Component::text("The server switch failed."));
        }
        None
    }

    /// The ready server gets the client (§3.3 step 5).
    fn attach(&mut self) -> Result<(), End> {
        let Next::Ready {
            mut conn,
            buffer,
            sent_settings,
            sent_brand,
            ..
        } = std::mem::replace(&mut self.next, Next::Idle)
        else {
            return Ok(());
        };
        if !sent_settings || !sent_brand {
            self.send_settings(&mut conn, !sent_settings, !sent_brand)?;
        }
        let name = self
            .request
            .as_ref()
            .map(|r| r.target.clone())
            .unwrap_or_default();
        self.drop_backend();
        self.epoch += 1;
        self.own_keep_alive = None;
        let server = self.rt.config.servers.get(&name);
        let chat_sessions = server.is_none_or(|s| s.chat_session_forwarding);
        let signed_cancel = server.map(|s| s.signed_chat_cancel).unwrap_or_default();
        debug!("{}: configuring on {name}", self.player.profile.name);
        self.backend = Some(Backend {
            conn,
            name,
            phase: Phase::Configuration,
            keep_alives: VecDeque::new(),
            in_bundle: false,
            joined: false,
            roots: None,
            tree: None,
            last_read: Instant::now(),
            chat_sessions,
            session_sent: false,
            signed_cancel,
        });
        for (f, body) in buffer {
            if let Some(end) = self.backend_frame(&f, &body) {
                return Err(end);
            }
        }
        // And what is still undecoded in its read buffer.
        match self.backend_frames() {
            Some(end) => Err(end),
            None => Ok(()),
        }
    }

    /// What a client sends right after login: settings and brand.
    fn send_settings(&self, conn: &mut Conn, settings: bool, brand: bool) -> Result<(), Fail> {
        let m = &*self.module;
        let mut send = |kind: PacketKind, payload: &Bytes| -> Result<(), Fail> {
            if let Some(id) = m.packet_id(Phase::Configuration, Direction::Serverbound, kind) {
                conn.queue(id, payload)?;
            }
            Ok(())
        };
        if let (true, Some(p)) = (settings, &self.tracked.client_information) {
            send(PacketKind::ClientInformation, p)?;
        }
        if let (true, Some(p)) = (brand, &self.tracked.brand) {
            send(PacketKind::CustomPayload, p)?;
        }
        Ok(())
    }

    /// Drops the current backend; returns its name.
    fn drop_backend(&mut self) -> Option<String> {
        let b = self.backend.take()?;
        self.next_own_keep_alive = Instant::now() + OWN_KEEP_ALIVE;
        self.proxy.set_server(self.player.profile.id, None);
        Some(b.name)
    }

    /// Candidate servers in order, without those the client cannot join.
    fn candidates(&self, reason: SelectReason, previous: Option<&str>) -> VecDeque<String> {
        let protocol = self.module.protocol();
        let ctx = SelectContext {
            reason,
            virtual_host: &self.host,
            client_protocol: protocol,
            previous,
        };
        self.rt
            .modules
            .selector
            .candidates(&ctx, &self.rt.backends)
            .into_iter()
            .filter(|name| {
                self.rt
                    .backends
                    .iter()
                    .any(|b| &b.name == name && self.reachable(b))
            })
            .collect()
    }

    /// The translation chain lets the client on this server: same version or
    /// translated in the proxy (not an external translator).
    fn reachable(&self, b: &BackendInfo) -> bool {
        matches!(
            self.rt
                .modules
                .translators
                .iter()
                .find_map(|t| t.plan(self.module.protocol(), b.protocol)),
            Some(TranslationPlan::Passthrough | TranslationPlan::Translate(_))
        )
    }

    /// "No server" text; names the versions when no server speaks the client's.
    fn no_server_text(&self) -> Component {
        let msgs = &self.rt.config.messages;
        if self.rt.backends.iter().any(|b| self.reachable(b)) {
            return parse_text(&msgs.no_server);
        }
        let mut protocols: Vec<_> = self.rt.backends.iter().filter_map(|b| b.protocol).collect();
        protocols.sort();
        protocols.dedup();
        let supported: Vec<String> = protocols
            .iter()
            .filter_map(|p| self.proxy.versions.get(*p))
            .map(|m| releases(m.release_names()))
            .collect();
        parse_text(
            &msgs
                .no_server_for_version
                .replace("{version}", &releases(self.module.release_names()))
                .replace("{supported}", &supported.join(", ")),
        )
    }

    /// Starts the next candidate of the request.
    fn try_next(&mut self) -> Option<End> {
        loop {
            let req = self.request.as_mut()?;
            let Some(name) = req.candidates.pop_front() else {
                return self.exhausted();
            };
            let Some(info) = self.rt.backends.iter().find(|b| b.name == name) else {
                continue;
            };
            let Some(version) =
                backend_version(self.proxy, &self.rt, self.module.protocol(), info.protocol)
            else {
                continue;
            };
            req.target = name.clone();
            // PumboBridge gets the player's permission set before the login.
            if let Some(b) = self.proxy.bridge.get() {
                b.connecting(self.player.profile.id, &name);
            }
            let reason = match req.purpose {
                Purpose::Join => "join",
                Purpose::Switch { .. } => "switch",
                Purpose::Fallback { .. } => "fallback",
                Purpose::Reconnect => "reconnect",
            };
            let login = login_future(
                self.rt.clone(),
                version,
                info.address.clone(),
                self.player.clone(),
            );
            // `on-server-connect` (E5) runs before the login, in this future.
            self.next = Next::Connecting(match self.proxy.plugins.get().cloned() {
                Some(p) => {
                    let id = self.player.profile.id;
                    Box::pin(async move {
                        use crate::plugins::ConnectDecision;
                        match p.server_connect(id, &name, reason).await {
                            ConnectDecision::Allow => login.await,
                            ConnectDecision::Deny(t) => {
                                Err(BackendFail::Refused(t.to_json(TextFormat::V770)))
                            }
                            ConnectDecision::Redirect(to) => Err(BackendFail::Redirect(to)),
                        }
                    })
                }
                None => login,
            });
            return None;
        }
    }

    /// No candidate left.
    fn exhausted(&mut self) -> Option<End> {
        let req = self.request.take()?;
        let text = req.refusal.unwrap_or_else(|| self.no_server_text());
        Metrics::inc(&self.proxy.metrics.switches_failed);
        match req.purpose {
            Purpose::Switch { quiet } if self.on_server() => {
                if !quiet {
                    self.say(
                        parse_text(&format!("&cCould not connect to {}: ", req.target))
                            .append(text),
                    );
                }
                None
            }
            // The client already left its server.
            Purpose::Switch { .. } | Purpose::Reconnect => self.fallback(Some(req.target), text),
            Purpose::Join => Some(End::Kick(Box::new(text))),
            Purpose::Fallback { reason } => Some(End::Kick(reason)),
        }
    }

    /// Moves the player to the next server after losing `from` (§3.2).
    fn fallback(&mut self, from: Option<String>, reason: Component) -> Option<End> {
        let candidates = self.candidates(SelectReason::Fallback, from.as_deref());
        if candidates.is_empty() {
            return Some(End::Kick(Box::new(reason)));
        }
        Metrics::inc(&self.proxy.metrics.fallbacks);
        self.request = Some(Request {
            purpose: Purpose::Fallback {
                reason: Box::new(reason),
            },
            candidates,
            target: String::new(),
            refusal: None,
            redirects: 0,
        });
        self.try_next()
    }

    /// The next server failed before it got the client.
    fn next_failed(&mut self, why: String) {
        self.next = Next::Idle;
        Metrics::inc(&self.proxy.metrics.backend_failures);
        let target = self
            .request
            .as_ref()
            .map(|r| r.target.as_str())
            .unwrap_or("");
        warn!("{}: backend {target}: {why}", self.player.profile.name);
    }

    /// The server refused the player: other candidates are tried only on fallback.
    fn refused(&mut self, text: Component) {
        if let Some(req) = self.request.as_mut() {
            if !matches!(req.purpose, Purpose::Fallback { .. }) {
                req.candidates.clear();
            }
            req.refusal = Some(text);
        }
    }

    fn on_next(&mut self, ev: NextEvent) -> Option<End> {
        match ev {
            NextEvent::Login(Ok(mut conn)) => {
                Metrics::inc(&self.proxy.metrics.backend_connects);
                // Like a client right after login_acknowledged (§3.3 step 5).
                let (s, b) = (
                    self.tracked.client_information.is_some(),
                    self.tracked.brand.is_some(),
                );
                if let Err(e) = self.send_settings(&mut conn, true, true) {
                    self.next_failed(format!("{e:?}"));
                    return self.try_next();
                }
                self.next = Next::Ready {
                    conn,
                    buffer: Vec::new(),
                    bytes: 0,
                    since: Instant::now(),
                    sent_settings: s,
                    sent_brand: b,
                };
                // Frames that came with the end of login are already buffered.
                self.buffer_next()
            }
            NextEvent::Login(Err(BackendFail::Refused(json))) => {
                self.next_failed(format!("refused: {json}"));
                let text =
                    Component::from_json(&json).unwrap_or_else(|_| Component::text("Disconnected"));
                self.refused(text);
                self.try_next()
            }
            NextEvent::Login(Err(BackendFail::Unavailable(why))) => {
                self.next_failed(why);
                self.try_next()
            }
            NextEvent::Login(Err(BackendFail::Redirect(to))) => {
                self.next = Next::Idle;
                if let Some(req) = self.request.as_mut()
                    && req.redirects < MAX_REDIRECTS
                {
                    req.redirects += 1;
                    req.candidates.push_front(to);
                }
                self.try_next()
            }
            NextEvent::Read(Ok(_)) => self.buffer_next(),
            NextEvent::Read(Err(_)) => {
                self.next_failed("connection closed".into());
                self.try_next()
            }
        }
    }

    /// Reads the ready server: answers keep-alives and pings, holds the rest.
    fn buffer_next(&mut self) -> Option<End> {
        let module = self.module.clone();
        let m = &*module;
        let limit = self.rt.config.switching.config_buffer_bytes;
        let cb = ctx(m, Direction::Clientbound);
        loop {
            let Next::Ready {
                conn,
                buffer,
                bytes,
                ..
            } = &mut self.next
            else {
                return None;
            };
            let (f, body) = match conn.next_frame() {
                Ok(Some(x)) => x,
                Ok(None) => return None,
                Err(e) => {
                    self.next_failed(e.to_string());
                    return self.try_next();
                }
            };
            let sb = Direction::Serverbound;
            let cfg = Phase::Configuration;
            let answered = match m.packet_kind(cfg, Direction::Clientbound, f.id) {
                Some(PacketKind::KeepAlive) => packets::decode::<KeepAlive>(&f.payload, &cb)
                    .map_err(Fail::from)
                    .and_then(|k| queue(conn, m, cfg, sb, &k)),
                Some(PacketKind::Ping) => packets::decode::<Ping>(&f.payload, &cb)
                    .map_err(Fail::from)
                    .and_then(|p| queue(conn, m, cfg, sb, &Pong { id: p.id })),
                Some(PacketKind::Disconnect) => {
                    let text = packets::decode::<Disconnect>(&f.payload, &cb)
                        .ok()
                        .and_then(|d| Component::from_nbt(&d.reason).ok())
                        .unwrap_or_else(|| Component::text("Disconnected"));
                    self.next_failed(format!("kicked in configuration: {}", text.plain_text()));
                    self.refused(text);
                    return self.try_next();
                }
                _ => {
                    // Held in memory decompressed, so that size counts.
                    *bytes += f.payload.len();
                    buffer.push((f, body));
                    if *bytes > limit {
                        self.next_failed(format!("configuration over {limit} bytes"));
                        return self.try_next();
                    }
                    Ok(())
                }
            };
            if let Err(e) = answered {
                self.next_failed(format!("{e:?}"));
                return self.try_next();
            }
        }
    }

    /// Whether the player plays on a server and nothing is switching.
    fn on_server(&self) -> bool {
        self.backend.as_ref().is_some_and(|b| b.joined)
            && self.request.is_none()
            && matches!(self.next, Next::Idle)
            && self.ack_deadline.is_none()
            && self.reconnect_on_ack.is_none()
            && self.client_phase == Phase::Play
    }

    fn current(&self) -> Option<String> {
        self.backend
            .as_ref()
            .filter(|b| b.joined)
            .map(|b| b.name.clone())
    }

    /// `/server`, `/send`, `bungeecord:main` Connect.
    fn switch_to(&mut self, target: &str, quiet: bool) -> Result<(), String> {
        let current = self.current();
        if current.as_deref() == Some(target) {
            return Err(format!("&cYou are already connected to {target}."));
        }
        if !self.on_server() {
            return Err("&cYou are already connecting to a server.".into());
        }
        let Some(info) = self.rt.backends.iter().find(|b| b.name == target) else {
            return Err(format!("&cThere is no server named {target}."));
        };
        if !self.reachable(info) {
            return Err(format!("&c{target} runs another Minecraft version."));
        }
        self.request = Some(Request {
            purpose: Purpose::Switch { quiet },
            candidates: VecDeque::from([target.to_string()]),
            target: String::new(),
            refusal: None,
            redirects: 0,
        });
        let _ = self.try_next();
        Ok(())
    }

    /// Reconnects to the current server (§3.3).
    fn reconnect(&mut self) -> Result<(), String> {
        let Some(name) = self.current().filter(|_| self.on_server()) else {
            return Err("not on a server or switching".into());
        };
        let cooldown = Duration::from_millis(self.rt.config.switching.reconnect_cooldown_ms);
        if self.last_reconnect.is_some_and(|t| t.elapsed() < cooldown) {
            return Err("reconnected too recently".into());
        }
        self.last_reconnect = Some(Instant::now());
        self.reconnect_on_ack = Some(name);
        Ok(())
    }

    fn command(&mut self, cmd: SessionCmd) -> Option<End> {
        if !self.in_play()
            && matches!(
                cmd,
                SessionCmd::Title { .. }
                    | SessionCmd::ActionBar(_)
                    | SessionCmd::ClearTitle
                    | SessionCmd::Bossbar { .. }
            )
        {
            self.defer(cmd);
            return None;
        }
        match cmd {
            SessionCmd::Connect { server, quiet } => {
                if let Err(msg) = self.switch_to(&server, quiet) {
                    if quiet {
                        debug!("{}: connect to {server}: {msg}", self.player.profile.name);
                    } else {
                        self.say(parse_text(&msg));
                    }
                }
            }
            SessionCmd::Reconnect => {
                if let Err(msg) = self.reconnect() {
                    debug!("{}: reconnect: {msg}", self.player.profile.name);
                }
            }
            SessionCmd::Message(text) => self.say(*text),
            SessionCmd::Title {
                title,
                subtitle,
                times,
            } => self.out(|m, c| {
                let f = TextFormat::for_protocol(m.protocol().0);
                queue_out(
                    c,
                    m,
                    Phase::Play,
                    &SetTitlesAnimation {
                        fade_in: times.0,
                        stay: times.1,
                        fade_out: times.2,
                    },
                )?;
                queue_out(
                    c,
                    m,
                    Phase::Play,
                    &SetSubtitleText {
                        text: subtitle.to_nbt(f),
                    },
                )?;
                queue_out(
                    c,
                    m,
                    Phase::Play,
                    &SetTitleText {
                        text: title.to_nbt(f),
                    },
                )
            }),
            SessionCmd::ActionBar(text) => self.out(|m, c| {
                let text = text.to_nbt(TextFormat::for_protocol(m.protocol().0));
                queue_out(c, m, Phase::Play, &SetActionBarText { text })
            }),
            SessionCmd::Bossbar { id, op } => self.bossbar(id, *op),
            SessionCmd::Virtual(cmd) => return self.virtual_cmd(*cmd),
            SessionCmd::Kick(text) => return Some(End::Kick(text)),
            SessionCmd::ClearTitle => {
                self.out(|m, c| queue_out(c, m, Phase::Play, &ClearTitles { reset: true }));
            }
            SessionCmd::TabList { header, footer } => {
                self.tracked.proxy_tab = Some((header, footer));
                self.send_proxy_tab();
            }
            SessionCmd::PluginMessage {
                channel,
                data,
                to_backend,
            } => self.plugin_message(channel, data, to_backend),
            SessionCmd::Property { name, value } => {
                let props = &mut self.player.profile.properties;
                props.retain(|p| p.name != name);
                props.extend(value);
            }
            SessionCmd::CommandsChanged => {
                self.resend_commands();
                if let Some(b) = self.proxy.bridge.get() {
                    b.perms_changed(self.player.profile.id, self.current().as_deref());
                }
            }
        }
        None
    }

    /// Keeps a title, the action bar or a boss bar change for the next join
    /// (a gate may send them before the client is in play). Only the newest
    /// title and action bar count, and boss bar changes fold into a waiting
    /// `Show`: a bar not shown in this world cannot change.
    fn defer(&mut self, cmd: SessionCmd) {
        let d = &mut self.deferred;
        match &cmd {
            SessionCmd::Title { .. } | SessionCmd::ClearTitle => {
                d.retain(|c| !matches!(c, SessionCmd::Title { .. }));
                if matches!(cmd, SessionCmd::ClearTitle) {
                    return;
                }
            }
            SessionCmd::ActionBar(_) => d.retain(|c| !matches!(c, SessionCmd::ActionBar(_))),
            SessionCmd::Bossbar { id, op } => {
                let waiting = d.iter_mut().find_map(|c| match c {
                    SessionCmd::Bossbar { id: i, op } if i == id => match op.as_mut() {
                        BossbarOp::Show {
                            title, progress, ..
                        } => Some((title, progress)),
                        _ => None,
                    },
                    _ => None,
                });
                match (op.as_ref(), waiting) {
                    (BossbarOp::Progress(p), Some((_, progress))) => *progress = *p,
                    (BossbarOp::Title(t), Some((title, _))) => *title = t.clone(),
                    (BossbarOp::Show { .. }, _) => {
                        let id = *id;
                        d.retain(|c| !matches!(c, SessionCmd::Bossbar { id: i, .. } if *i == id));
                        if d.len() < MAX_DEFERRED {
                            d.push(cmd);
                        }
                    }
                    (BossbarOp::Hide, _) => {
                        let id = *id;
                        d.retain(|c| !matches!(c, SessionCmd::Bossbar { id: i, .. } if *i == id));
                    }
                    _ => {}
                }
                return;
            }
            _ => {}
        }
        if d.len() < MAX_DEFERRED {
            d.push(cmd);
        }
    }

    /// The client is in play (on a server or in a virtual world): what
    /// waited for it goes out, the Tab header in the new context.
    fn after_join(&mut self) {
        for n in std::mem::take(&mut self.notices) {
            self.say(n);
        }
        for c in std::mem::take(&mut self.deferred) {
            self.command(c);
        }
        self.config_tab(true);
    }

    /// Whether the client is in play on a server or in a virtual world.
    fn in_play(&self) -> bool {
        self.out_phase == Phase::Play
            && self.ack_deadline.is_none()
            && (self.backend.as_ref().is_some_and(|b| b.joined)
                || self.vworld.as_ref().is_some_and(|v| v.player.in_play()))
    }

    /// Queues packets for a client in play; otherwise they are dropped.
    fn out(&mut self, f: impl FnOnce(&dyn VersionModule, &mut Conn) -> Result<(), Fail>) {
        if !self.in_play() {
            return;
        }
        let module = self.module.clone();
        if let Err(e) = f(&*module, &mut self.client) {
            debug!("{}: {e:?}", self.player.profile.name);
        }
    }

    /// Proxy boss bars; tracked like a backend's, so a switch removes them.
    fn bossbar(&mut self, id: Uuid, op: BossbarOp) {
        if !self.in_play() {
            return;
        }
        let f = TextFormat::for_protocol(self.module.protocol().0);
        let action = match op {
            BossbarOp::Show {
                title,
                progress,
                color,
                overlay,
            } => {
                if self.tracked.bossbars.contains(&id) {
                    BossAction::UpdateProgress(progress)
                } else {
                    self.tracked.bossbars.insert(id);
                    BossAction::Add {
                        title: title.to_nbt(f),
                        progress: progress.clamp(0.0, 1.0),
                        color,
                        overlay,
                        flags: 0,
                    }
                }
            }
            BossbarOp::Progress(p) if self.tracked.bossbars.contains(&id) => {
                BossAction::UpdateProgress(p.clamp(0.0, 1.0))
            }
            BossbarOp::Title(t) if self.tracked.bossbars.contains(&id) => {
                BossAction::UpdateTitle(t.to_nbt(f))
            }
            BossbarOp::Hide if self.tracked.bossbars.remove(&id) => BossAction::Remove,
            _ => return,
        };
        self.out(|m, c| queue_out(c, m, Phase::Play, &BossEvent { id, action }));
    }

    fn tab_configured(&self) -> bool {
        let tab = &self.rt.config.tab;
        !(tab.header.is_empty() && tab.footer.is_empty())
    }

    /// The client's settings for plugins (`player-info`).
    fn settings_to_plugins(&self, payload: &[u8]) {
        let Some(p) = self.proxy.plugins.get() else {
            return;
        };
        let sb = ctx(&*self.module, Direction::Serverbound);
        if let Ok(c) = packets::decode::<pc::ClientInformation>(payload, &sb) {
            p.settings(self.player.profile.id, &c);
        }
    }

    /// `%player_ping%` from keep-alive round trips.
    fn ping_to_plugins(&self, ms: u32) {
        if let Some(p) = self.proxy.plugins.get() {
            p.ping(self.player.profile.id, ms);
        }
    }

    fn brand_to_plugins(&self, data: &[u8]) {
        if let Some(p) = self.proxy.plugins.get()
            && let Ok(brand) = Reader::new(data).string(256)
        {
            p.brand(self.player.profile.id, &brand);
        }
    }

    /// `tab` header and footer of the config (§5.8.3): rendered for this
    /// player (push and cached placeholders only) every `refresh-ms` and
    /// after a server change; sent when the text changed.
    fn config_tab(&mut self, force: bool) {
        let tab = &self.rt.config.tab;
        if tab.header.is_empty() && tab.footer.is_empty() {
            if force {
                self.send_proxy_tab();
            }
            return;
        }
        let id = self.player.profile.id;
        let render = |t: &str| match self.proxy.plugins.get() {
            Some(p) => p.render(Some(id), t),
            None => parse_text(t),
        };
        let pair = (Box::new(render(&tab.header)), Box::new(render(&tab.footer)));
        if !force && self.tracked.proxy_tab.as_ref() == Some(&pair) {
            return;
        }
        self.tracked.proxy_tab = Some(pair);
        self.send_proxy_tab();
    }

    /// The proxy's Tab header and footer, if a plugin set them.
    fn send_proxy_tab(&mut self) {
        let Some((header, footer)) = self.tracked.proxy_tab.clone() else {
            return;
        };
        self.out(|m, c| {
            let f = TextFormat::for_protocol(m.protocol().0);
            queue_out(
                c,
                m,
                Phase::Play,
                &TabList {
                    header: header.to_nbt(f),
                    footer: footer.to_nbt(f),
                },
            )
        });
    }

    fn plugin_message(&mut self, channel: String, data: Vec<u8>, to_backend: bool) {
        let module = self.module.clone();
        let m = &*module;
        let result = if to_backend {
            match self.backend.as_mut() {
                Some(b) => queue(
                    &mut b.conn,
                    m,
                    b.phase,
                    Direction::Serverbound,
                    &ServerboundCustomPayload { channel, data },
                ),
                None => Ok(()),
            }
        } else {
            let phase = self.out_phase;
            queue_out(
                &mut self.client,
                m,
                phase,
                &ClientboundCustomPayload { channel, data },
            )
        };
        if let Err(e) = result {
            debug!("{}: plugin message: {e:?}", self.player.profile.name);
        }
    }

    /// The command tree again, merged with the commands this player may use
    /// now (§5.8.4: after a permission or context change).
    fn resend_commands(&mut self) {
        if !self.on_server() {
            return;
        }
        let Some(tree) = self.backend.as_ref().and_then(|b| b.tree.clone()) else {
            return;
        };
        match self.merge_commands(&tree) {
            Ok(Step::Forward) => {
                if let Err(e) = self.client.queue(tree.id, &tree.payload) {
                    debug!("command tree: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => debug!("command tree: {e:?}"),
        }
    }

    // ------------------------------------------------------------ virtual world

    /// Starts a virtual world for a client in configuration (§5.2).
    fn enter_now(&mut self, e: Enter) -> Result<(), End> {
        let profile = crate::session::to_wire(&self.player.profile);
        let mut player =
            VirtualPlayer::new(self.module.clone(), profile, e.world, e.at, e.commands);
        let mut out = Vec::new();
        player
            .start(&mut out)
            .map_err(|err| self.virtual_failed(&err))?;
        self.queue_virtual(out)?;
        self.vworld = Some(InVirtual {
            player,
            tag: e.tag,
            inputs: e.inputs,
        });
        Metrics::inc(&self.proxy.metrics.virtual_entries);
        if self.holding {
            self.gate_deadline = Some(
                Instant::now()
                    + Duration::from_millis(self.rt.config.virtual_world.gate_timeout_ms),
            );
        }
        Ok(())
    }

    fn virtual_failed(&self, err: &pumbo_virtual::Error) -> End {
        warn!("{}: virtual world: {err}", self.player.profile.name);
        end_kick("This server cannot show its login world to your game version.")
    }

    fn queue_virtual(&mut self, out: pumbo_virtual::Out) -> Result<(), End> {
        for (id, payload) in out {
            self.client
                .queue(id, &payload)
                .map_err(|e| End::Protocol(e.to_string()))?;
        }
        Ok(())
    }

    fn send_input(&mut self, input: Input) {
        let Some(v) = self.vworld.as_ref() else {
            return;
        };
        let event = PlayerInput {
            player: v.tag,
            input,
        };
        if v.inputs.try_send(event).is_err() {
            Metrics::inc(&self.proxy.metrics.dropped_inputs);
        }
    }

    fn virtual_cmd(&mut self, cmd: VirtualCmd) -> Option<End> {
        let mut out = Vec::new();
        if let VirtualCmd::Enter {
            world,
            at,
            commands,
            tag,
            inputs,
        } = cmd
        {
            let enter = Enter {
                world,
                at,
                commands,
                tag,
                inputs,
            };
            let fresh = self.backend.is_none()
                && self.client_phase == Phase::Configuration
                && self.out_phase == Phase::Configuration
                && matches!(self.next, Next::Idle);
            let on_server = self.on_server();
            let result = match self.vworld.as_mut() {
                // Another world in the same connection (§5.2).
                Some(v) => {
                    v.inputs = enter.inputs;
                    v.tag = enter.tag;
                    v.player
                        .change_world(enter.world, enter.at, enter.commands, &mut out)
                }
                None if fresh => return self.enter_now(enter).err(),
                None if on_server => {
                    // Through configuration, like a server switch (§5.2).
                    self.enter_on_ack = Some(enter);
                    Ok(())
                }
                None => {
                    debug!(
                        "{}: busy, virtual world not entered",
                        self.player.profile.name
                    );
                    Ok(())
                }
            };
            if let Err(e) = result {
                return Some(self.virtual_failed(&e));
            }
            return self.queue_virtual(out).err();
        }
        let result = match (cmd, self.vworld.as_mut()) {
            (VirtualCmd::Release, _) => return self.release(),
            (_, None) => return None,
            (VirtualCmd::Teleport { at, id }, Some(v)) => {
                v.player.teleport(at, id, &mut out).map(|_| ())
            }
            (VirtualCmd::ShowMap(image, hand), Some(v)) => v.player.show_map(image, hand, &mut out),
            (VirtualCmd::ClearInventory, Some(v)) => v.player.clear_inventory(&mut out),
            (VirtualCmd::Xp { bar, level }, Some(v)) if v.player.in_play() => {
                v.player.set_xp(bar, level, &mut out)
            }
            (VirtualCmd::Time(t), Some(v)) if v.player.in_play() => v.player.set_time(t, &mut out),
            (VirtualCmd::GameMode(g), Some(v)) if v.player.in_play() => {
                v.player.set_game_mode(g, &mut out)
            }
            (VirtualCmd::Flying { allow, flying }, Some(v)) if v.player.in_play() => {
                v.player.set_flying(allow, flying, &mut out)
            }
            (
                VirtualCmd::Sound {
                    name,
                    volume,
                    pitch,
                },
                Some(v),
            ) if v.player.in_play() => pumbo_virtual::player::sound(
                &*self.module,
                &name,
                v.player.position(),
                volume,
                pitch,
            )
            .and_then(|s| {
                let payload = packets::encode(&s, &ctx(&*self.module, Direction::Clientbound))?;
                let id = self
                    .module
                    .packet_id(Phase::Play, Direction::Clientbound, PacketKind::Sound)
                    .ok_or(pumbo_virtual::Error::NoPacket(PacketKind::Sound))?;
                out.push((id, Bytes::from(payload)));
                Ok(())
            }),
            _ => Ok(()),
        };
        if let Err(e) = result {
            return Some(self.virtual_failed(&e));
        }
        self.queue_virtual(out).err()
    }

    /// The gate is done (§5.6): the next server logs in in the background and
    /// the client goes there through `start_configuration` (§3.3).
    fn release(&mut self) -> Option<End> {
        if !self.holding && self.vworld.is_none() {
            return None;
        }
        self.holding = false;
        self.gate_deadline = None;
        Metrics::inc(&self.proxy.metrics.virtual_releases);
        let mut candidates = self.candidates(SelectReason::InitialJoin, None);
        if let Some(back) = self.return_to.take() {
            candidates.retain(|c| *c != back);
            candidates.push_front(back);
        }
        if candidates.is_empty() {
            return Some(End::Kick(Box::new(self.no_server_text())));
        }
        self.request = Some(Request {
            purpose: Purpose::Join,
            candidates,
            target: String::new(),
            refusal: None,
            redirects: 0,
        });
        self.try_next()
    }

    /// A client frame while in a virtual world.
    fn virtual_step(
        &mut self,
        phase: Phase,
        kind: Option<PacketKind>,
        f: &RawFrame,
    ) -> Result<Step, Fail> {
        let module = self.module.clone();
        let sb = ctx(&*module, Direction::Serverbound);
        match kind {
            Some(PacketKind::ClientInformation) => {
                self.tracked.client_information = Some(f.payload.clone());
                self.settings_to_plugins(&f.payload);
                self.send_input(Input::Settings);
                return Ok(Step::Drop);
            }
            Some(PacketKind::CustomPayload) => {
                let p: ServerboundCustomPayload = packets::decode(&f.payload, &sb)?;
                if bungee::is_channel(&p.channel) {
                    Metrics::inc(&self.proxy.metrics.dropped_payloads);
                } else if p.channel == "minecraft:brand" {
                    self.tracked.brand = Some(f.payload.clone());
                    self.brand_to_plugins(&p.data);
                    let brand = Reader::new(&p.data).string(256).unwrap_or_default();
                    self.send_input(Input::Brand(brand));
                } else {
                    self.send_input(Input::PluginMessage(p.channel, p.data));
                }
                return Ok(Step::Drop);
            }
            Some(PacketKind::CustomClickAction) => {
                let _: CustomClickAction = packets::decode(&f.payload, &sb)?;
                return Ok(Step::Drop);
            }
            Some(PacketKind::CommandSuggestion) => {
                if !self.suggestions.take(1.0) {
                    Metrics::inc(&self.proxy.metrics.dropped_suggestions);
                }
                return Ok(Step::Drop);
            }
            _ => {}
        }
        let mut out = Vec::new();
        let mut inputs = Vec::new();
        let Some(v) = self.vworld.as_mut() else {
            return Ok(Step::Drop);
        };
        let handled = v
            .player
            .on_frame(phase, kind, &f.payload, &mut out, &mut inputs)
            .map_err(|e| match e {
                pumbo_virtual::Error::Decode(d) => Fail::Protocol(d.to_string()),
                other => Fail::Kick(Box::new(Component::text(format!(
                    "This server cannot show its login world to your game version ({other})."
                )))),
            })?;
        if v.player.finishing() {
            // Our finish_configuration went out; what follows is play.
            self.out_phase = Phase::Play;
        }
        for (id, payload) in out {
            self.client.queue(id, &payload)?;
        }
        for i in inputs {
            self.send_input(i);
        }
        if handled == Handled::Joined {
            debug!("{}: in the virtual world", self.player.profile.name);
            self.after_join();
        }
        Ok(Step::Drop)
    }

    /// A system message now if the player is in play, otherwise after the next join.
    fn say(&mut self, text: Component) {
        if !self.in_play() {
            if self.notices.len() < MAX_DEFERRED {
                self.notices.push(text);
            }
            return;
        }
        let content = text.to_nbt(TextFormat::for_protocol(self.module.protocol().0));
        let msg = SystemChat {
            content,
            overlay: false,
        };
        if let Err(e) = queue_out(&mut self.client, &*self.module, Phase::Play, &msg) {
            debug!("system message: {e:?}");
        }
    }

    /// The answer of `on-server-kicked` (E5): the proxy's own handling,
    /// a disconnect with the plugin's text, or a move to another server.
    fn on_kick(
        &mut self,
        (decision, text, shutdown): (crate::plugins::KickDecision, Box<Component>, bool),
    ) -> Option<End> {
        use crate::plugins::KickDecision;
        match decision {
            KickDecision::Keep if shutdown => self.backend_lost(Some(*text)),
            KickDecision::Keep => Some(End::Kick(text)),
            KickDecision::Disconnect(t) => Some(End::Kick(Box::new(t))),
            KickDecision::Redirect(server, t) => {
                let from = self.drop_backend();
                self.request = Some(Request {
                    purpose: Purpose::Fallback {
                        reason: Box::new(t),
                    },
                    candidates: VecDeque::from([server]),
                    target: from.unwrap_or_default(),
                    refusal: None,
                    redirects: 0,
                });
                self.try_next()
            }
        }
    }

    /// The backend went away (closed, timed out or a shutdown kick, §3.2).
    fn backend_lost(&mut self, reason: Option<Component>) -> Option<End> {
        let b = self.backend.as_ref()?;
        let (name, in_bundle, phase) = (b.name.clone(), b.in_bundle, b.phase);
        let reason = reason.unwrap_or_else(|| Component::text("Lost connection to the server."));
        self.drop_backend();
        if self.ack_deadline.is_some() {
            // The client is leaving it anyway.
            return None;
        }
        info!(
            "{}: lost {name}: {}",
            self.player.profile.name,
            reason.plain_text()
        );
        if in_bundle && phase == Phase::Play && self.out_phase == Phase::Play {
            // Close the bundle the server left open before start_configuration.
            let _ = queue_out(
                &mut self.client,
                &*self.module,
                Phase::Play,
                &BundleDelimiter,
            );
        }
        if self.request.is_some() {
            // A switch in progress goes on; a join or fallback tries the next candidate.
            if matches!(self.next, Next::Idle) {
                if let Some(req) = self.request.as_mut() {
                    req.refusal.get_or_insert(reason);
                }
                return self.try_next();
            }
            return None;
        }
        self.fallback(Some(name), reason)
    }

    /// A kick that looks like a shutdown moves the player on (§3.2).
    fn shutdown_like(&self, text: &Component) -> bool {
        if let Content::Translatable { key, .. } = &text.content
            && key == "multiplayer.disconnect.server_shutdown"
        {
            return true;
        }
        let plain = text.plain_text().to_ascii_lowercase();
        self.rt
            .config
            .switching
            .fallback_reasons
            .iter()
            .any(|r| plain.contains(&r.to_ascii_lowercase()))
    }

    // ------------------------------------------------------------ client

    async fn client_frames(&mut self) -> Option<End> {
        loop {
            let (f, body) = match self.client.next_frame() {
                Ok(Some(x)) => x,
                Ok(None) => return None,
                Err(e) => return Some(End::Protocol(e.to_string())),
            };
            if !self.packets.take(1.0) {
                Metrics::inc(&self.proxy.metrics.kicked_packet_rate);
                return Some(end_kick("Too many packets."));
            }
            match self.client_step(&f).await {
                Ok(Step::Forward) => {
                    if let Some(b) = self.backend.as_mut() {
                        let same = self.client.inbound.threshold == b.conn.outbound.threshold;
                        if let Err(e) = forward(&mut b.conn, &f, &body, same) {
                            return Some(End::Protocol(e.to_string()));
                        }
                    }
                }
                Ok(Step::End(end)) => return Some(end),
                Ok(_) => {}
                Err(e) => return Some(e.into()),
            }
        }
    }

    async fn client_step(&mut self, f: &RawFrame) -> Result<Step, Fail> {
        let module = self.module.clone();
        let sb = ctx(&*module, Direction::Serverbound);
        let kind = module.packet_kind(self.client_phase, Direction::Serverbound, f.id);
        let m = &self.proxy.metrics;
        Ok(match (self.client_phase, kind) {
            (_, Some(PacketKind::KeepAlive)) => {
                let k: KeepAlive = packets::decode(&f.payload, &sb)?;
                if self.own_keep_alive == Some(k.id) {
                    self.own_keep_alive = None;
                    let rtt = millis(self.own_keep_alive_at);
                    self.send_input(Input::KeepaliveRtt(rtt));
                    self.ping_to_plugins(rtt);
                    return Ok(Step::Drop);
                }
                // Only answers to keep-alives the backend sent pass (§2.9).
                match self.backend.as_mut().and_then(|b| {
                    let i = b.keep_alives.iter().position(|(id, _)| *id == k.id)?;
                    b.keep_alives.remove(i)
                }) {
                    Some((_, sent)) => {
                        self.ping_to_plugins(millis(sent));
                        Step::Forward
                    }
                    None => {
                        Metrics::inc(&m.dropped_keep_alives);
                        Step::Drop
                    }
                }
            }
            (Phase::Play, Some(PacketKind::ConfigurationAcknowledged)) => {
                self.client_phase = Phase::Configuration;
                if self.ack_deadline.take().is_some() {
                    return Ok(match self.after_ack() {
                        Some(end) => Step::End(end),
                        None => Step::Drop,
                    });
                }
                Step::Forward
            }
            (Phase::Configuration, Some(PacketKind::FinishConfiguration)) => {
                self.client_phase = Phase::Play;
                if self.vworld.is_some() {
                    return self.virtual_step(Phase::Configuration, kind, f);
                }
                Step::Forward
            }
            // Play packets meant for the server the client is leaving.
            _ if self.ack_deadline.is_some() => Step::Drop,
            _ if self.vworld.is_some() => return self.virtual_step(self.client_phase, kind, f),
            (_, Some(PacketKind::ClientInformation)) => {
                self.tracked.client_information = Some(f.payload.clone());
                self.settings_to_plugins(&f.payload);
                Step::Forward
            }
            (_, Some(PacketKind::CustomPayload)) => {
                // Decoding enforces the 32 KiB limit for clients.
                let p: ServerboundCustomPayload = packets::decode(&f.payload, &sb)?;
                if bungee::is_channel(&p.channel) || crate::bridge::is_channel(&p.channel) {
                    // A client must not speak for its backend (§2.8) or as
                    // a bridge (PumboBridge spec §6.4).
                    Metrics::inc(&m.dropped_payloads);
                    return Ok(Step::Drop);
                }
                if p.channel == "minecraft:brand" {
                    self.tracked.brand = Some(f.payload.clone());
                    self.brand_to_plugins(&p.data);
                }
                if let Some(plugins) = self.proxy.plugins.get()
                    && !plugins
                        .plugin_message(self.player.profile.id, &p.channel, &p.data)
                        .await
                {
                    return Ok(Step::Drop);
                }
                Step::Forward
            }
            (Phase::Play, Some(PacketKind::AcceptTeleportation)) => {
                let a: pumbo_protocol::packets::world::AcceptTeleportation =
                    packets::decode(&f.payload, &sb)?;
                if let Some(g) = self.proxy.teleport_gate(self.player.profile.id) {
                    g.send_if_modified(|t| {
                        let done = *t == crate::bridge::Tp::Awaiting(a.id);
                        if done {
                            *t = crate::bridge::Tp::Confirmed(Instant::now());
                        }
                        done
                    });
                }
                Step::Forward
            }
            (_, Some(PacketKind::CustomClickAction)) => {
                // Decoding enforces the client NBT limits (depth 64, 32 KiB).
                let _: CustomClickAction = packets::decode(&f.payload, &sb)?;
                Step::Forward
            }
            (_, Some(PacketKind::ResourcePack)) => {
                let r: ResourcePackResponse = packets::decode(&f.payload, &sb)?;
                let epoch = self.epoch;
                match self.tracked.packs.iter_mut().find(|p| p.id == r.id) {
                    // Only the server that sent a pack hears about it (§3.3).
                    Some(p) if p.epoch != epoch => Step::Drop,
                    Some(p) => {
                        p.loaded = r.result == ResourcePackResponse::SUCCESSFULLY_LOADED;
                        Step::Forward
                    }
                    None => Step::Forward,
                }
            }
            (Phase::Play, Some(PacketKind::ChatCommand)) => {
                let c: ChatCommand = packets::decode(&f.payload, &sb)?;
                self.chat_command(&c.command).await
            }
            (Phase::Play, Some(PacketKind::ChatCommandSigned)) => {
                let c: ChatCommandSigned = packets::decode(&f.payload, &sb)?;
                let root = commands::root(&c.command);
                let sensitive = self.rt.sensitive.contains(&root)
                    || self
                        .proxy
                        .plugins
                        .get()
                        .is_some_and(|p| p.host.is_sensitive_command(&root));
                if sensitive {
                    self.sensitive(&root);
                }
                if sensitive
                    || cancelled(
                        self.proxy,
                        &self.player.profile,
                        self.current(),
                        ChatInput::Command(&c.command),
                        true,
                    )
                    .await
                {
                    self.cancel_signed(c.last_seen.offset, !c.arguments.is_empty())?
                } else {
                    Step::Forward
                }
            }
            (Phase::Play, Some(PacketKind::Chat)) => {
                let c: Chat = packets::decode(&f.payload, &sb)?;
                if cancelled(
                    self.proxy,
                    &self.player.profile,
                    self.current(),
                    ChatInput::Message(&c.message),
                    false,
                )
                .await
                {
                    self.cancel_signed(c.last_seen.offset, c.signature.is_some())?
                } else {
                    Step::Forward
                }
            }
            (Phase::Play, Some(PacketKind::ChatSessionUpdate)) => match self.backend.as_mut() {
                Some(b) if b.chat_sessions => {
                    b.session_sent = true;
                    Step::Forward
                }
                _ => Step::Drop,
            },
            (Phase::Play, Some(PacketKind::CommandSuggestion)) => {
                if !self.suggestions.take(1.0) {
                    // Dropped without an answer (§2.7).
                    Metrics::inc(&m.dropped_suggestions);
                    return Ok(Step::Drop);
                }
                let s: CommandSuggestion = packets::decode(&f.payload, &sb)?;
                self.suggest(&s)?
            }
            _ => Step::Forward,
        })
    }

    fn sensitive(&mut self, root: &str) {
        info!(
            "{} used /{root} (sensitive, arguments not logged, not sent to the server)",
            self.player.profile.name
        );
        self.say(parse_text("&cLogin is temporarily unavailable."));
    }

    /// A proxy command name that the backend does not have itself (§2.8).
    fn proxy_owns(&self, root: &str) -> bool {
        commands::NAMES.contains(&root)
            && !self
                .backend
                .as_ref()
                .and_then(|b| b.roots.as_ref())
                .is_some_and(|r| r.contains(root))
    }

    async fn chat_command(&mut self, line: &str) -> Step {
        let root = commands::root(line);
        // Commands of the plugin host (E5) first: `/pumbo`, plugin commands
        // and the sensitive names of their manifests.
        if let Some(lines) = self
            .proxy
            .plugins
            .get()
            .and_then(|p| p.command(self.player.profile.id, line))
        {
            for l in lines {
                self.say(l);
            }
            return Step::Drop;
        }
        if self.rt.sensitive.contains(&root) {
            self.sensitive(&root);
            return Step::Drop;
        }
        if self.proxy_owns(&root) {
            let current = self.current();
            let source = Source::Player {
                id: self.player.profile.id,
                server: current.as_deref(),
            };
            if let Some(reply) = commands::run(self.proxy, &self.rt, &source, line) {
                Metrics::inc(&self.proxy.metrics.proxy_commands);
                info!("{} issued proxy command /{line}", self.player.profile.name);
                for l in reply.lines {
                    self.say(l);
                }
                if let Some(s) = reply.connect
                    && let Err(msg) = self.switch_to(&s, false)
                {
                    self.say(parse_text(&msg));
                }
                return Step::Drop;
            }
        }
        if cancelled(
            self.proxy,
            &self.player.profile,
            self.current(),
            ChatInput::Command(line),
            false,
        )
        .await
        {
            // Unsigned commands carry no acknowledgements (§2.8).
            Metrics::inc(&self.proxy.metrics.chat_cancelled);
            return Step::Drop;
        }
        Step::Forward
    }

    /// Drops a message or command that carries acknowledgements and keeps
    /// the backend's count in step with a `chat_ack` (§2.8 point 2). A gap in
    /// the signature chain cannot be repaired, so `signed-chat-cancel =
    /// "kick"` disconnects instead when the backend has the chat session.
    fn cancel_signed(&mut self, offset: i32, signed: bool) -> Result<Step, Fail> {
        Metrics::inc(&self.proxy.metrics.chat_cancelled);
        if signed
            && self
                .backend
                .as_ref()
                .is_some_and(|b| b.session_sent && b.signed_cancel == SignedChatCancel::Kick)
        {
            let text = parse_text(&self.rt.config.messages.signed_chat_blocked);
            return Ok(Step::End(End::Kick(Box::new(text))));
        }
        if offset > 0
            && let Some(b) = self.backend.as_mut()
        {
            queue(
                &mut b.conn,
                &*self.module,
                Phase::Play,
                Direction::Serverbound,
                &ChatAck { offset },
            )?;
        }
        Ok(Step::Drop)
    }

    /// Tab completion for proxy commands; the rest goes to the backend.
    fn suggest(&mut self, s: &CommandSuggestion) -> Result<Step, Fail> {
        let line = s.text.strip_prefix('/').unwrap_or(&s.text);
        let root = commands::root(line);
        let plugins = self.proxy.plugins.get();
        let plugin_cmd = plugins.and_then(|p| {
            p.visible_commands(self.player.profile.id)
                .into_iter()
                .find(|(n, _)| *n == root)
        });
        if !self.proxy_owns(&root) && plugin_cmd.is_none() {
            return Ok(Step::Forward);
        }
        let current = self.current();
        let source = Source::Player {
            id: self.player.profile.id,
            server: current.as_deref(),
        };
        // The proxy runs this command, so a backend's suggestions for it would be
        // for a different command. Without our own: the plugin's subcommands for
        // the first word, online players after it, nothing for passwords.
        // ponytail: player names for every argument until the WIT has a suggestion event.
        let sensitive = self.rt.sensitive.contains(&root)
            || plugins.is_some_and(|p| p.host.is_sensitive_command(&root));
        let (start, matches) = commands::suggest(self.proxy, &self.rt, &source, line)
            .unwrap_or_else(|| {
                let Some(space) = line
                    .rfind(' ')
                    .filter(|_| plugin_cmd.is_some() && !sensitive)
                else {
                    return (line.len(), Vec::new());
                };
                let start = space + 1;
                let word = line[start..].to_ascii_lowercase();
                let first = line[..space].split_whitespace().count() == 1;
                let options = match plugin_cmd {
                    Some((_, subs)) if first && !subs.is_empty() => subs,
                    _ => self.proxy.players().into_iter().map(|p| p.name).collect(),
                };
                let matches = options
                    .into_iter()
                    .filter(|o| o.to_ascii_lowercase().starts_with(&word))
                    .collect();
                (start, matches)
            });
        // Brigadier counts UTF-16 units of the whole text, slash included.
        let start = s.text.len() - line.len() + start;
        let before = s.text.get(..start).unwrap_or_default();
        let word = s.text.get(start..).unwrap_or_default();
        let units = |t: &str| i32::try_from(t.encode_utf16().count()).unwrap_or(i32::MAX);
        let answer = CommandSuggestions {
            id: s.id,
            start: units(before),
            length: units(word),
            matches: matches
                .into_iter()
                .map(|text| Suggestion {
                    text,
                    tooltip: None,
                })
                .collect(),
        };
        queue_out(&mut self.client, &*self.module, Phase::Play, &answer)?;
        Ok(Step::Drop)
    }

    // ------------------------------------------------------------ backend

    fn backend_frames(&mut self) -> Option<End> {
        loop {
            let b = self.backend.as_mut()?;
            let (f, body) = match b.conn.next_frame() {
                Ok(Some(x)) => x,
                Ok(None) => return None,
                Err(e) => return Some(End::Protocol(format!("backend: {e}"))),
            };
            if let Some(end) = self.backend_frame(&f, &body) {
                return Some(end);
            }
        }
    }

    fn backend_frame(&mut self, f: &RawFrame, body: &Bytes) -> Option<End> {
        if self.ack_deadline.is_some() {
            // Play packets of the server the client is leaving.
            return None;
        }
        let step = match self.backend_step(f) {
            Ok(s) => s,
            Err(e) => Step::End(End::Protocol(format!("backend: {e:?}"))),
        };
        let fwd = |s: &mut Self| {
            let same = s
                .backend
                .as_ref()
                .is_some_and(|b| b.conn.inbound.threshold == s.client.outbound.threshold);
            forward(&mut s.client, f, body, same)
                .err()
                .map(|e| End::Protocol(format!("to client: {e}")))
        };
        match step {
            Step::Forward => fwd(self),
            Step::Drop => None,
            Step::ForwardAndEnd(end) => {
                let _ = fwd(self);
                Some(end)
            }
            Step::End(end) => Some(end),
            Step::Lost(reason) => self.backend_lost(Some(*reason)),
            Step::Joined => {
                if let Some(end) = fwd(self) {
                    return Some(end);
                }
                let cb = ctx(&*self.module, Direction::Clientbound);
                match packets::decode::<Login>(&f.payload, &cb) {
                    Ok(login) => self.joined(&login),
                    Err(e) => return Some(End::Protocol(format!("backend login: {e}"))),
                }
                None
            }
        }
    }

    fn backend_step(&mut self, f: &RawFrame) -> Result<Step, Fail> {
        let module = self.module.clone();
        let cb = ctx(&*module, Direction::Clientbound);
        let Some(phase) = self.backend.as_ref().map(|b| b.phase) else {
            return Ok(Step::Drop);
        };
        let kind = module.packet_kind(phase, Direction::Clientbound, f.id);
        let epoch = self.epoch;
        Ok(match (phase, kind) {
            (_, Some(PacketKind::KeepAlive)) => {
                let k: KeepAlive = packets::decode(&f.payload, &cb)?;
                if let Some(b) = self.backend.as_mut() {
                    if b.keep_alives.len() >= MAX_PENDING_KEEP_ALIVES {
                        b.keep_alives.pop_front();
                    }
                    b.keep_alives.push_back((k.id, Instant::now()));
                }
                Step::Forward
            }
            (Phase::Configuration, Some(PacketKind::FinishConfiguration)) => {
                self.before_finish()?;
                if let Some(b) = self.backend.as_mut() {
                    // The backend sends play packets only after the client's
                    // acknowledgement, so the stream switches here (as Velocity does).
                    b.phase = Phase::Play;
                }
                self.out_phase = Phase::Play;
                Step::Forward
            }
            (Phase::Play, Some(PacketKind::StartConfiguration)) => {
                if let Some(b) = self.backend.as_mut() {
                    b.phase = Phase::Configuration;
                    b.joined = false;
                }
                self.out_phase = Phase::Configuration;
                Step::Forward
            }
            (Phase::Play, Some(PacketKind::Login)) => Step::Joined,
            (_, Some(PacketKind::Disconnect)) => {
                let text = packets::decode::<Disconnect>(&f.payload, &cb)
                    .ok()
                    .and_then(|d| Component::from_nbt(&d.reason).ok());
                match (text, self.proxy.plugins.get().cloned(), self.current()) {
                    // `on-server-kicked` (E5) decides; the backend is not read meanwhile.
                    (Some(t), Some(p), Some(server)) => {
                        let shutdown = self.shutdown_like(&t);
                        let id = self.player.profile.id;
                        self.pending_kick = Some(Box::pin(async move {
                            let d = p.server_kicked(id, &server, &t).await;
                            (d, Box::new(t), shutdown)
                        }));
                        Step::Drop
                    }
                    (Some(t), _, _) if self.shutdown_like(&t) => Step::Lost(Box::new(t)),
                    _ => Step::ForwardAndEnd(End::BackendKicked),
                }
            }
            (_, Some(PacketKind::CustomPayload)) => self.backend_payload(phase, f)?,
            (Phase::Play, Some(PacketKind::PlayerPosition)) => {
                // A teleport the client must confirm: PumboBridge holds the
                // next one until then ("Wrong teleport id" otherwise).
                let p: pumbo_protocol::packets::world::PlayerPosition =
                    packets::decode(&f.payload, &cb)?;
                if let Some(g) = self.proxy.teleport_gate(self.player.profile.id) {
                    g.send_replace(crate::bridge::Tp::Awaiting(p.teleport_id));
                }
                Step::Forward
            }
            (Phase::Play, Some(PacketKind::Commands)) => {
                if let Some(b) = self.backend.as_mut() {
                    b.tree = Some(f.clone());
                }
                self.merge_commands(f)?
            }
            (Phase::Play, Some(PacketKind::BossEvent)) => {
                let e: BossEvent = packets::decode(&f.payload, &cb)?;
                match e.action {
                    BossAction::Add { .. } => {
                        self.tracked.bossbars.insert(e.id);
                    }
                    BossAction::Remove => {
                        self.tracked.bossbars.remove(&e.id);
                    }
                    _ => {}
                }
                Step::Forward
            }
            // The proxy's own header and footer replace the backend's (§5.8.3).
            (Phase::Play, Some(PacketKind::TabList)) if self.tracked.proxy_tab.is_some() => {
                Step::Drop
            }
            (Phase::Play, Some(PacketKind::TabList)) => {
                self.tracked.tab_list = true;
                Step::Forward
            }
            (_, Some(PacketKind::ShowDialog)) => {
                self.tracked.dialog = true;
                Step::Forward
            }
            (_, Some(PacketKind::ClearDialog)) => {
                self.tracked.dialog = false;
                Step::Forward
            }
            (_, Some(PacketKind::ResourcePackPush)) => {
                let p: ResourcePackPush = packets::decode(&f.payload, &cb)?;
                self.pack_push(phase, p)?
            }
            (_, Some(PacketKind::ResourcePackPop)) => {
                let p: ResourcePackPop = packets::decode(&f.payload, &cb)?;
                match p.id {
                    Some(id) => self.tracked.packs.retain(|k| k.id != id),
                    None => self.tracked.packs.clear(),
                }
                Step::Forward
            }
            (_, Some(PacketKind::ServerLinks)) => {
                self.tracked.links_epoch = Some(epoch);
                Step::Forward
            }
            (_, Some(PacketKind::CustomReportDetails)) => {
                self.tracked.report_epoch = Some(epoch);
                Step::Forward
            }
            (Phase::Play, Some(PacketKind::BundleDelimiter)) => {
                if let Some(b) = self.backend.as_mut() {
                    b.in_bundle = !b.in_bundle;
                }
                Step::Forward
            }
            (Phase::Play, Some(PacketKind::LevelParticles)) => self.level_particles(f)?,
            _ => Step::Forward,
        })
    }

    /// Pumpkin's `trail` particle without options kicks the client (Pumpkin
    /// issue #3065): repaired for clients without a translator, which repairs
    /// it itself (D-COMPAT-1). Correct packets pass unchanged.
    fn level_particles(&mut self, f: &RawFrame) -> Result<Step, Fail> {
        let Some(b) = self.backend.as_ref().filter(|b| !b.conn.translates()) else {
            return Ok(Step::Forward);
        };
        let first = self.module.protocol() >= pumbo_protocol::ProtocolVersion::V777;
        let Some(fixed) = self
            .trail
            .and_then(|t| pumbo_translate_mv::fix_empty_trail(&f.payload, t, first))
        else {
            return Ok(Step::Forward);
        };
        Metrics::inc(&self.proxy.metrics.fixed_trails);
        if self.proxy.first_report("trail", &b.name) {
            warn!(
                "fixed a malformed trail particle from {} (Pumpkin issue #3065); this stops by itself once the server sends correct packets",
                b.name
            );
        }
        self.client.queue(f.id, &fixed)?;
        Ok(Step::Drop)
    }

    /// The new server's configuration ends: drop what the old one left (§3.3).
    fn before_finish(&mut self) -> Result<(), Fail> {
        let module = self.module.clone();
        let m = &*module;
        let cfg = Phase::Configuration;
        let epoch = self.epoch;
        let t = &mut self.tracked;
        if self.rt.config.switching.resource_packs_on_switch == PacksOnSwitch::Pop {
            for p in t.packs.iter().filter(|p| p.epoch < epoch) {
                queue_out(
                    &mut self.client,
                    m,
                    cfg,
                    &ResourcePackPop { id: Some(p.id) },
                )?;
            }
            t.packs.retain(|p| p.epoch == epoch);
        }
        if t.links_epoch.is_some_and(|e| e < epoch) {
            t.links_epoch = None;
            queue_out(&mut self.client, m, cfg, &ServerLinks { links: Vec::new() })?;
        }
        if t.report_epoch.is_some_and(|e| e < epoch) {
            t.report_epoch = None;
            let empty = CustomReportDetails {
                details: Vec::new(),
            };
            queue_out(&mut self.client, m, cfg, &empty)?;
        }
        Ok(())
    }

    /// A pack the client already loaded from an earlier server is not sent
    /// again: the proxy answers the new server itself (§3.3).
    fn pack_push(&mut self, phase: Phase, p: ResourcePackPush) -> Result<Step, Fail> {
        let epoch = self.epoch;
        if let Some(k) = self
            .tracked
            .packs
            .iter_mut()
            .find(|k| k.id == p.id && k.hash == p.hash)
        {
            if k.loaded && k.epoch != epoch {
                k.epoch = epoch;
                if let Some(b) = self.backend.as_mut() {
                    for result in [
                        ResourcePackResponse::ACCEPTED,
                        ResourcePackResponse::DOWNLOADED,
                        ResourcePackResponse::SUCCESSFULLY_LOADED,
                    ] {
                        let r = ResourcePackResponse { id: p.id, result };
                        queue(
                            &mut b.conn,
                            &*self.module,
                            phase,
                            Direction::Serverbound,
                            &r,
                        )?;
                    }
                }
                return Ok(Step::Drop);
            }
            k.epoch = epoch;
            k.loaded = false;
            return Ok(Step::Forward);
        }
        self.tracked.packs.retain(|k| k.id != p.id);
        self.tracked.packs.push(Pack {
            id: p.id,
            hash: p.hash,
            epoch,
            loaded: false,
        });
        Ok(Step::Forward)
    }

    /// Brand rewrite, `bungeecord:main`, size limit (§2.8).
    fn backend_payload(&mut self, phase: Phase, f: &RawFrame) -> Result<Step, Fail> {
        let mut r = Reader::new(&f.payload);
        let channel = r.identifier()?;
        let suffix = &self.rt.config.plugin_messages.server_brand_suffix;
        if channel == "minecraft:brand" && !suffix.is_empty() {
            let brand = r.string(32_767)?;
            let mut data = Vec::new();
            data.put_string(&format!("{brand}{suffix}"), 32_767)?;
            let p = ClientboundCustomPayload { channel, data };
            queue_out(&mut self.client, &*self.module, phase, &p)?;
            return Ok(Step::Drop);
        }
        if bungee::is_channel(&channel) {
            if phase == Phase::Play {
                self.bungee(r.rest());
            }
            return Ok(Step::Drop);
        }
        if crate::bridge::is_channel(&channel) {
            // The bridge never uses plugin messages (PumboBridge spec §6.4).
            Metrics::inc(&self.proxy.metrics.dropped_payloads);
            return Ok(Step::Drop);
        }
        if f.payload.len() > pc::MAX_PAYLOAD_FROM_SERVER {
            // Decoding enforces the 1 MiB limit for backends.
            let cb = ctx(&*self.module, Direction::Clientbound);
            if packets::decode::<ClientboundCustomPayload>(&f.payload, &cb).is_err() {
                Metrics::inc(&self.proxy.metrics.dropped_payloads);
                return Ok(Step::Drop);
            }
        }
        Ok(Step::Forward)
    }

    fn bungee(&mut self, data: &[u8]) {
        let current = self.current();
        let sender = bungee::Sender {
            id: self.player.profile.id,
            address: self.peer,
            server: current.as_deref(),
        };
        match bungee::handle(self.proxy, &self.rt, &sender, data) {
            bungee::Action::Reply(data) => {
                let p = ServerboundCustomPayload {
                    channel: bungee::CHANNEL.into(),
                    data,
                };
                if let Some(b) = self.backend.as_mut()
                    && let Err(e) = queue(
                        &mut b.conn,
                        &*self.module,
                        Phase::Play,
                        Direction::Serverbound,
                        &p,
                    )
                {
                    debug!("bungeecord reply: {e:?}");
                }
            }
            bungee::Action::Connect(server) => {
                if let Err(msg) = self.switch_to(&server, true) {
                    debug!("bungeecord Connect {server}: {msg}");
                }
            }
            bungee::Action::None => {}
        }
    }

    /// Adds the proxy commands to the backend's tree (§2.8); a tree that
    /// cannot be read goes through as it is.
    fn merge_commands(&mut self, f: &RawFrame) -> Result<Step, Fail> {
        let module = self.module.clone();
        let m = &*module;
        let cb = ctx(m, Direction::Clientbound);
        let mut tree: Commands = match packets::decode(&f.payload, &cb) {
            Ok(t) => t,
            Err(e) => {
                debug!("command tree not merged: {e}");
                if let Some(b) = self.backend.as_mut() {
                    b.roots = None;
                }
                return Ok(Step::Forward);
            }
        };
        let root_at = usize::try_from(tree.root).unwrap_or(usize::MAX);
        let roots: HashSet<String> = tree
            .nodes
            .get(root_at)
            .map(|r| {
                r.children
                    .iter()
                    .filter_map(|c| tree.nodes.get(usize::try_from(*c).ok()?))
                    .filter(|n| n.kind() == NODE_LITERAL)
                    .filter_map(|n| n.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let current = self.current();
        let source = Source::Player {
            id: self.player.profile.id,
            server: current.as_deref(),
        };
        let mut names: Vec<(String, Vec<String>)> =
            commands::visible(self.proxy, &self.rt, &source)
                .into_iter()
                .map(|n| (n.to_string(), Vec::new()))
                .collect();
        // Plugin commands (E5) the player may use here.
        if let Some(p) = self.proxy.plugins.get() {
            names.extend(p.visible_commands(self.player.profile.id));
        }
        names.retain(|(n, _)| !roots.contains(n));
        if let Some(b) = self.backend.as_mut() {
            b.roots = Some(roots);
        }
        let string = (0..1024).find(|i| m.command_argument_type(*i) == Some("brigadier:string"));
        let (Some(string), false) = (string, names.is_empty()) else {
            return Ok(Step::Forward);
        };
        // A literal with a greedy string after it that asks the server for suggestions.
        let literal = |tree: &mut Commands, name: String, mut children: Vec<i32>| {
            let arg = i32::try_from(tree.nodes.len()).map_err(|_| Fail::Protocol("tree".into()))?;
            tree.nodes.push(Node {
                flags: NODE_ARGUMENT | FLAG_EXECUTABLE | FLAG_SUGGESTIONS,
                children: Vec::new(),
                redirect: None,
                name: Some("args".into()),
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
                .map(|s| literal(&mut tree, s, Vec::new()))
                .collect::<Result<Vec<_>, _>>()?;
            let at = literal(&mut tree, name, subs)?;
            if let Some(root) = tree.nodes.get_mut(root_at) {
                root.children.push(at);
            }
        }
        queue_out(&mut self.client, m, Phase::Play, &tree)?;
        Ok(Step::Drop)
    }

    /// The backend's play `login` reached the client (§3.3 steps 6–9).
    fn joined(&mut self, login: &Login) {
        let Some(b) = self.backend.as_mut() else {
            return;
        };
        b.joined = true;
        let server = b.name.clone();
        let player = self.player.profile.name.clone();
        // The spawn teleport is due; set before the server changes, so
        // PumboBridge's `send-to` never sees the new server with a free gate.
        if let Some(g) = self.proxy.teleport_gate(self.player.profile.id) {
            g.send_replace(crate::bridge::Tp::Sent(Instant::now()));
        }
        self.proxy.set_server(self.player.profile.id, Some(&server));
        if login.enforces_secure_chat && self.proxy.first_report("secure_chat", &server) {
            // Pumpkin 0.2.0 reports this even with allow_chat_reports = false,
            // where it checks nothing (E4 test, docs/przelaczanie-stan.md).
            warn!(
                "server {server} enforces secure chat: players without a chat key cannot chat there; on Paper or vanilla set enforce-secure-profile=false (§2.8)"
            );
        }
        match self.request.take().map(|r| r.purpose) {
            Some(Purpose::Join) | None => info!("{player} connected to {server}"),
            Some(Purpose::Switch { .. }) => {
                Metrics::inc(&self.proxy.metrics.switches_ok);
                info!("{player} switched to {server}");
            }
            Some(Purpose::Reconnect) => info!("{player} reconnected to {server}"),
            Some(Purpose::Fallback { reason }) => {
                Metrics::inc(&self.proxy.metrics.switches_ok);
                info!("{player} moved to {server}");
                self.notices.insert(
                    0,
                    parse_text(&format!("&eYou were moved to {server}: ")).append(*reason),
                );
            }
        }
        // The Tab header is rendered again in the new server's context (§5.8.3).
        self.after_join();
    }
}

/// The `trail` particle's id in 26.2 and 26.3, the versions of Pumpkin 0.1 and
/// 0.2 that send it without options (D-COMPAT-1).
fn pumpkin_trail(v: pumbo_protocol::ProtocolVersion) -> Option<i32> {
    use pumbo_protocol::ProtocolVersion as V;
    // ponytail: only Pumpkin's protocols so far; a later Pumpkin with the same bug
    // needs its protocol here (26.3's layout from 777 on, see `fix_empty_trail`).
    if v != V::V776 && v != V::V777 {
        return None;
    }
    let id = pumbo_data::tables(v)
        .ok()?
        .registry("particle_type")?
        .id("trail")?;
    i32::try_from(id).ok()
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

#[cfg(test)]
mod tests {
    use super::*;

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
