//! The backend side: its frames, transfers, cookies, resource packs and plugin messages.

use std::collections::HashSet;

use bytes::Bytes;
use pumbo_core::routing::BackendInfo;
use pumbo_protocol::packets;
use pumbo_protocol::packets::commands::{Commands, NODE_LITERAL};
use pumbo_protocol::packets::common::{
    self as pc, ClientboundCustomPayload, CookieRequest, CookieResponse, CustomReportDetails,
    Disconnect, KeepAlive, ResourcePackPop, ResourcePackPush, ResourcePackResponse, ServerLinks,
    ServerboundCustomPayload, StoreCookie, Transfer,
};
use pumbo_protocol::packets::play::{BossAction, BossEvent, Login};
use pumbo_protocol::types::{Reader, WriteExt};
use pumbo_protocol::{Direction, PacketKind, Phase, RawFrame};
use pumbo_text::Component;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::bungee;
use crate::commands::{self, Source};
use crate::config::{PacksOnSwitch, split_host_port};
use crate::metrics::Metrics;
use crate::session::{Fail, ctx, forward, queue, queue_out};
use crate::status::parse_text;

use super::{
    End, MAX_COOKIES, MAX_PENDING_KEEP_ALIVES, MAX_REDIRECTS, Pack, Play, Purpose, Step,
    add_literals,
};

impl Play<'_> {
    pub(super) fn backend_frames(&mut self) -> Option<End> {
        self.follow_runtime();
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

    pub(super) fn backend_frame(&mut self, f: &RawFrame, body: &Bytes) -> Option<End> {
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
                let cb = ctx(self.module, Direction::Clientbound);
                match packets::decode::<Login>(&f.payload, &cb) {
                    Ok(login) => self.joined(&login),
                    Err(e) => return Some(End::Protocol(format!("backend login: {e}"))),
                }
                None
            }
        }
    }

    fn backend_step(&mut self, f: &RawFrame) -> Result<Step, Fail> {
        let module = self.module;
        let cb = ctx(module, Direction::Clientbound);
        let Some((phase, joined)) = self.backend.as_ref().map(|b| (b.phase, b.joined)) else {
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
                    // acknowledgement, so the stream switches here.
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
            (_, Some(PacketKind::StoreCookie)) => self.store_cookie(f),
            (_, Some(PacketKind::CookieRequest)) => self.cookie_request(phase, f)?,
            // Nothing else before its play `login`: the client has no level
            // yet, and vanilla disconnects on a world packet. Pumpkin 0.2.0
            // can send a sound first (it adds a joining player to the world
            // before the login).
            (Phase::Play, _) if !joined => {
                if let Some(b) = &self.backend
                    && self.proxy.first_report("before_login", &b.name)
                {
                    warn!(
                        "{} sent play packets before its login; they are dropped, the client has no world yet",
                        b.name
                    );
                }
                Step::Drop
            }
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
            (_, Some(PacketKind::Transfer)) => self.backend_transfer(phase, f)?,
            _ => Step::Forward,
        })
    }

    /// Whether `servers.<name>.allow-transfers` lets this server's
    /// `transfer` reach the client (GAP2-01); servers from the proxy never do.
    pub(super) fn transfers_allowed(&self, server: &str) -> bool {
        self.rt
            .config
            .servers
            .get(server)
            .is_some_and(|s| s.allow_transfers)
    }

    /// A `transfer` the client does not get: counted, reported once per
    /// server; returns the server of this proxy it names, if any.
    pub(super) fn transfer_blocked(&self, from: &str, t: &Transfer) -> Option<String> {
        let to = transfer_server(&self.rt.backends, &t.host, t.port);
        Metrics::inc(if to.is_some() {
            &self.proxy.metrics.transfers_switched
        } else {
            &self.proxy.metrics.transfers_blocked
        });
        if self.proxy.first_report("transfer", from) {
            warn!(
                "{from} tried to transfer {} to {}:{}; the proxy {} (set servers.{from}.allow-transfers: true to let its transfers through)",
                self.player.profile.name,
                t.host,
                t.port,
                match &to {
                    Some(name) => format!("switches the player to {name} instead"),
                    None => "dropped it".into(),
                }
            );
        }
        to
    }

    /// `transfer` from the current server. One to a server of this proxy
    /// becomes a switch; any other is dropped. In configuration the server
    /// waits for the client to leave, so it counts as lost and a join or
    /// switch goes on with the named server or the next candidate.
    fn backend_transfer(&mut self, phase: Phase, f: &RawFrame) -> Result<Step, Fail> {
        let Some(from) = self.backend.as_ref().map(|b| b.name.clone()) else {
            return Ok(Step::Drop);
        };
        if self.transfers_allowed(&from) {
            return Ok(Step::Forward);
        }
        let t: Transfer = packets::decode(&f.payload, &ctx(self.module, Direction::Clientbound))?;
        let to = self.transfer_blocked(&from, &t);
        if phase == Phase::Configuration {
            // A join or switch in progress tries the named server next.
            if let (Some(to), Some(req)) = (&to, self.request.as_mut())
                && req.redirects < MAX_REDIRECTS
            {
                req.redirects += 1;
                req.candidates.push_front(to.clone());
            }
            return Ok(Step::Lost(Box::new(Component::text(
                "The server tried to transfer you elsewhere.",
            ))));
        }
        // A transfer during a switch or before the login is dropped.
        if let Some(to) = to
            && self.on_server()
        {
            let _ = self.switch_to(&to, true);
        }
        Ok(Step::Drop)
    }

    /// The current server, unless `allow-transfers` lets it use the client's
    /// cookies (they travel with a transfer).
    pub(super) fn cookie_jar_owner(&self) -> Option<String> {
        let b = self.backend.as_ref()?;
        (!self.transfers_allowed(&b.name)).then(|| b.name.clone())
    }

    /// `store_cookie` from the server: kept for this server. What does not
    /// fit is dropped, never the player.
    fn store_cookie(&mut self, f: &RawFrame) -> Step {
        let Some(server) = self.cookie_jar_owner() else {
            return Step::Forward;
        };
        let name = &self.player.profile.name;
        match packets::decode::<StoreCookie>(&f.payload, &ctx(self.module, Direction::Clientbound))
        {
            Ok(c) => {
                let jar = self.tracked.cookies.entry(server.clone()).or_default();
                if jar.len() < MAX_COOKIES || jar.contains_key(&c.key) {
                    jar.insert(c.key, c.payload);
                } else {
                    debug!(
                        "{name}: {server} stores over {MAX_COOKIES} cookies, {} dropped",
                        c.key
                    );
                }
            }
            Err(e) => debug!("{name}: cookie from {server} dropped: {e}"),
        }
        Step::Drop
    }

    /// `cookie_request` from the server: answered with what it stored.
    fn cookie_request(&mut self, phase: Phase, f: &RawFrame) -> Result<Step, Fail> {
        let Some(server) = self.cookie_jar_owner() else {
            return Ok(Step::Forward);
        };
        let module = self.module;
        let c: CookieRequest = packets::decode(&f.payload, &ctx(module, Direction::Clientbound))?;
        let payload = self
            .tracked
            .cookies
            .get(&server)
            .and_then(|jar| jar.get(&c.key))
            .cloned();
        if let Some(b) = self.backend.as_mut() {
            let answer = CookieResponse {
                key: c.key,
                payload,
            };
            queue(&mut b.conn, module, phase, Direction::Serverbound, &answer)?;
        }
        Ok(Step::Drop)
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
        let module = self.module;
        let m = module;
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
                        queue(&mut b.conn, self.module, phase, Direction::Serverbound, &r)?;
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
            queue_out(&mut self.client, self.module, phase, &p)?;
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
            let cb = ctx(self.module, Direction::Clientbound);
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
                        self.module,
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
    pub(super) fn merge_commands(&mut self, f: &RawFrame) -> Result<Step, Fail> {
        let module = self.module;
        let m = module;
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
        if names.is_empty() || !add_literals(m, &mut tree, root_at, names)? {
            return Ok(Step::Forward);
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
pub(super) fn pumpkin_trail(v: pumbo_protocol::ProtocolVersion) -> Option<i32> {
    use pumbo_protocol::ProtocolVersion as V;
    // Only Pumpkin's protocols so far; a later Pumpkin with the same bug
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

/// The server whose address a `transfer` names: the same host (no DNS
/// lookup) and port, 25565 for an address without one.
fn transfer_server(backends: &[BackendInfo], host: &str, port: i32) -> Option<String> {
    backends
        .iter()
        .find(|b| {
            let (h, p) = split_host_port(&b.address).unwrap_or((b.address.as_str(), 25565));
            h.eq_ignore_ascii_case(host) && i32::from(p) == port
        })
        .map(|b| b.name.clone())
}
