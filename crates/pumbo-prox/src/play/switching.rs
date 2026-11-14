//! Switching servers: the next server, fallback, reconnecting and kicks (§3.2, §3.3).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use pumbo_core::profile::ForwardedPlayer;
use pumbo_core::routing::{BackendInfo, SelectContext, SelectReason};
use pumbo_core::translate::TranslationPlan;
use pumbo_protocol::packets;
use pumbo_protocol::packets::common::{ClearDialog, Disconnect, KeepAlive, Ping, Pong, Transfer};
use pumbo_protocol::packets::play::{
    BossAction, BossEvent, BundleDelimiter, ClearTitles, StartConfiguration, TabList,
};
use pumbo_protocol::{Direction, PacketKind, Phase};
use pumbo_text::{Component, Content, TextFormat};
use tokio::time::{Instant, timeout};
use tracing::{debug, info, warn};

use crate::conn::Conn;
use crate::metrics::Metrics;
use crate::server::Runtime;
use crate::session::{
    BackendFail, BackendVersion, Fail, backend_login, backend_version, ctx, queue, queue_out,
};
use crate::status::parse_text;

use super::{
    ACK_TIMEOUT, Backend, End, LoginFuture, MAX_REDIRECTS, Next, NextEvent, OWN_KEEP_ALIVE, Play,
    Purpose, Request, releases,
};

impl Play<'_> {
    /// Moves the state machine on: attaches a ready server to a client in
    /// configuration, or sends a playing client to configuration.
    pub(super) fn progress(&mut self) -> Result<(), End> {
        // A new server list: a player in the Pumbox tries the servers again.
        if self.waiting()
            && let Some(seen) = &self.pumbox
            && (!Arc::ptr_eq(seen, &self.rt) || self.proxy.health.version() != self.pumbox_health)
        {
            self.pumbox = Some(self.rt.clone());
            self.pumbox_health = self.proxy.health.version();
            let candidates = self.candidates(SelectReason::InitialJoin, None);
            if !candidates.is_empty() {
                self.request = Some(Request {
                    purpose: Purpose::Join,
                    candidates,
                    target: String::new(),
                    refusal: None,
                    redirects: 0,
                });
                if let Some(end) = self.try_next() {
                    return Err(end);
                }
            }
        }
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
        let module = self.module;
        let m = module;
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
    pub(super) fn after_ack(&mut self) -> Option<End> {
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
        self.pumbox = None;
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
        let m = self.module;
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
    /// An offline server whose wait is over keeps its place only for this
    /// caller, so the list is picked only to be tried.
    pub(super) fn candidates(
        &self,
        reason: SelectReason,
        previous: Option<&str>,
    ) -> VecDeque<String> {
        let protocol = self.module.protocol();
        let ctx = SelectContext {
            reason,
            virtual_host: &self.host,
            client_protocol: protocol,
            previous,
        };
        let (mut up, down): (VecDeque<_>, VecDeque<_>) = self
            .rt
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
            .partition(|name| self.proxy.server_up(&self.rt, name));
        // Servers that are down are still tried, after the others.
        up.extend(down);
        up
    }

    /// The translation chain lets the client on this server: same version or
    /// translated in the proxy (not an external translator).
    pub(super) fn reachable(&self, b: &BackendInfo) -> bool {
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
    pub(super) fn no_server_text(&self) -> Component {
        let msgs = &self.rt.config.messages;
        if self.rt.backends.is_empty() || self.rt.backends.iter().any(|b| self.reachable(b)) {
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
    pub(super) fn try_next(&mut self) -> Option<End> {
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
                self.tracked.cookies.get(&name).cloned().unwrap_or_default(),
                {
                    let (proxy, server) = (self.proxy.clone(), name.clone());
                    Box::new(move |d| proxy.translate_drops.count(&server, d))
                },
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
            // A player from the Pumbox goes back there.
            _ if self.pumbox.is_some() => self.wait_in_pumbox(text),
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
            return self.wait_in_pumbox(reason);
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
    pub(super) fn next_failed(&mut self, why: String) {
        self.next = Next::Idle;
        Metrics::inc(&self.proxy.metrics.backend_failures);
        let target = self
            .request
            .as_ref()
            .map(|r| r.target.as_str())
            .unwrap_or("");
        warn!("{}: backend {target}: {why}", self.player.profile.name);
    }

    /// The login to the request's server got through or failed before the
    /// server answered (its health; a kick from the server does not count).
    fn login_result(&self, reached: bool) {
        if let Some(req) = &self.request {
            self.proxy.backend_login(&req.target, reached);
        }
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

    pub(super) fn on_next(&mut self, ev: NextEvent) -> Option<End> {
        self.follow_runtime();
        match ev {
            NextEvent::Login(Ok(mut conn)) => {
                Metrics::inc(&self.proxy.metrics.backend_connects);
                self.login_result(true);
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
                self.login_result(false);
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
        let module = self.module;
        let m = module;
        let limit = self.rt.config.switching.config_buffer_bytes;
        let cb = ctx(m, Direction::Clientbound);
        let target = self
            .request
            .as_ref()
            .map(|r| r.target.clone())
            .unwrap_or_default();
        let transfers = self.transfers_allowed(&target);
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
                // The server waits for the client to leave: a failed connect,
                // or a redirect when it names a server of this proxy.
                Some(PacketKind::Transfer) if !transfers => {
                    let to = packets::decode::<Transfer>(&f.payload, &cb)
                        .ok()
                        .and_then(|t| self.transfer_blocked(&target, &t));
                    self.next_failed("sent a transfer in configuration".into());
                    if let (Some(to), Some(req)) = (to, self.request.as_mut())
                        && req.redirects < MAX_REDIRECTS
                    {
                        req.redirects += 1;
                        req.candidates.push_front(to);
                    }
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
    pub(super) fn on_server(&self) -> bool {
        self.backend.as_ref().is_some_and(|b| b.joined)
            && self.request.is_none()
            && matches!(self.next, Next::Idle)
            && self.ack_deadline.is_none()
            && self.reconnect_on_ack.is_none()
            && self.client_phase == Phase::Play
    }

    pub(super) fn current(&self) -> Option<String> {
        self.backend
            .as_ref()
            .filter(|b| b.joined)
            .map(|b| b.name.clone())
    }

    /// `/server`, `/send`, `bungeecord:main` Connect.
    pub(super) fn switch_to(&mut self, target: &str, quiet: bool) -> Result<(), String> {
        let current = self.current();
        if current.as_deref() == Some(target) {
            return Err(format!("&cYou are already connected to {target}."));
        }
        if !self.on_server() && !self.waiting() {
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
    pub(super) fn reconnect(&mut self) -> Result<(), String> {
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

    /// The answer of `on-server-kicked` (E5): the proxy's own handling,
    /// a disconnect with the plugin's text, or a move to another server.
    pub(super) fn on_kick(
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
    pub(super) fn backend_lost(&mut self, reason: Option<Component>) -> Option<End> {
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
            let _ = queue_out(&mut self.client, self.module, Phase::Play, &BundleDelimiter);
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
    pub(super) fn shutdown_like(&self, text: &Component) -> bool {
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
}

pub(super) async fn next_event(next: &mut Next) -> NextEvent {
    match next {
        Next::Connecting(fut) => NextEvent::Login(fut.as_mut().await),
        Next::Ready { conn, .. } => NextEvent::Read(
            conn.fill_writing(None, true)
                .await
                .map(Option::unwrap_or_default),
        ),
        Next::Idle => std::future::pending().await,
    }
}

/// Logs in to a backend in its own version (`module`); with a translator the
/// connection then speaks the client's version to the rest of the session (E3b).
fn login_future(
    rt: Arc<Runtime>,
    (module, translator): BackendVersion,
    address: String,
    player: ForwardedPlayer,
    cookies: HashMap<String, Vec<u8>>,
    on_drop: crate::conn::OnDrop,
) -> LoginFuture {
    Box::pin(async move {
        let mut conn = timeout(
            Duration::from_millis(rt.config.limits.backend_login_timeout_ms),
            backend_login(&rt, &*module, &address, &player, &cookies),
        )
        .await
        .unwrap_or_else(|_| Err(BackendFail::Unavailable("login timed out".into())))?;
        if let Some(t) = translator {
            conn.set_translator(t, on_drop);
        }
        Ok(conn)
    })
}
