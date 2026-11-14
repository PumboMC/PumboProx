//! The client side: its frames, proxy commands, signed chat and Tab completion (§2.8).

use pumbo_protocol::packets;
use pumbo_protocol::packets::common::{
    CustomClickAction, KeepAlive, ResourcePackResponse, ServerboundCustomPayload,
};
use pumbo_protocol::packets::play::{
    Chat, ChatAck, ChatCommand, ChatCommandSigned, CommandSuggestion, CommandSuggestions,
    Suggestion,
};
use pumbo_protocol::{Direction, PacketKind, Phase, RawFrame};
use tokio::time::Instant;
use tracing::info;

use crate::bungee;
use crate::commands::{self, Source};
use crate::config::SignedChatCancel;
use crate::metrics::Metrics;
use crate::server::{ChatInput, SessionCmd};
use crate::session::{Fail, ctx, forward, queue, queue_out};
use crate::status::parse_text;
use pumbo_virtual::Input;

use super::{End, Play, Step, cancelled, end_kick, millis};

impl Play<'_> {
    pub(super) async fn client_frames(&mut self) -> Option<End> {
        self.follow_runtime();
        loop {
            // Charged before inflating: a small frame may declare 2 MiB.
            if let Some(n) = self.client.next_size()
                && !self.decoded.take(n as f64)
            {
                Metrics::inc(&self.proxy.metrics.kicked_packet_rate);
                return Some(end_kick("Too many packets."));
            }
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
        let module = self.module;
        let sb = ctx(module, Direction::Serverbound);
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
                // Or the lost server's own `start_configuration`, with the Pumbox to enter.
                let waited = self.backend.is_none() && self.enter_on_ack.is_some();
                if self.ack_deadline.take().is_some() || waited {
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
            // The proxy answers cookie requests itself, except for servers
            // that use the client's cookies.
            (_, Some(PacketKind::CookieResponse)) if self.cookie_jar_owner().is_some() => {
                Step::Drop
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
                self.module,
                Phase::Play,
                Direction::Serverbound,
                &ChatAck { offset },
            )?;
        }
        Ok(Step::Drop)
    }

    /// Tab completion for proxy commands; the rest goes to the backend.
    pub(super) fn suggest(&mut self, s: &CommandSuggestion) -> Result<Step, Fail> {
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
        // for a different command. Without our own: the plugin's `suggest`, else
        // its subcommands for the first word or online players, nothing for
        // passwords.
        let sensitive = self.rt.sensitive.contains(&root)
            || plugins.is_some_and(|p| p.host.is_sensitive_command(&root));
        let (start, matches) = match commands::suggest(self.proxy, &self.rt, &source, line) {
            Some(own) => own,
            None => {
                let Some(space) = line
                    .rfind(' ')
                    .filter(|_| plugin_cmd.is_some() && !sensitive)
                else {
                    return self.answer_suggestions(s, line.len(), Vec::new());
                };
                let start = space + 1;
                let first = line[..space].split_whitespace().count() == 1;
                match (plugin_cmd, plugins) {
                    (cmd, Some(p)) => {
                        // The plugin answers later; it filters by the player's rights.
                        let subs = cmd.map(|(_, subs)| subs).filter(|s| first && !s.is_empty());
                        let ask = p.suggest(self.player.profile.id, line);
                        let (proxy, id, text) =
                            (self.proxy.clone(), self.player.profile.id, s.text.clone());
                        let (req, line) = (s.id, line.to_string());
                        tokio::spawn(async move {
                            let matches = match ask.await {
                                Some(m) => m,
                                None => starting_with(
                                    subs.unwrap_or_else(|| {
                                        proxy.players().into_iter().map(|p| p.name).collect()
                                    }),
                                    &line[start..],
                                ),
                            };
                            let answer = suggestions(req, &text, &line, start, matches);
                            proxy.send_to(id, SessionCmd::Suggestions(Box::new(answer)));
                        });
                        return Ok(Step::Drop);
                    }
                    _ => (line.len(), Vec::new()),
                }
            }
        };
        self.answer_suggestions(s, start, matches)
    }

    fn answer_suggestions(
        &mut self,
        s: &CommandSuggestion,
        start: usize,
        matches: Vec<String>,
    ) -> Result<Step, Fail> {
        let line = s.text.strip_prefix('/').unwrap_or(&s.text);
        let answer = suggestions(s.id, &s.text, line, start, matches);
        queue_out(&mut self.client, self.module, Phase::Play, &answer)?;
        Ok(Step::Drop)
    }
}

/// The options that start with `word`, ignoring case.
fn starting_with(options: Vec<String>, word: &str) -> Vec<String> {
    let word = word.to_ascii_lowercase();
    options
        .into_iter()
        .filter(|o| o.to_ascii_lowercase().starts_with(&word))
        .collect()
}

/// The answer to request `id` for `text`; `line` is `text` without the slash
/// and `start` where the replaced word begins in it.
fn suggestions(
    id: i32,
    text: &str,
    line: &str,
    start: usize,
    matches: Vec<String>,
) -> CommandSuggestions {
    // Brigadier counts UTF-16 units of the whole text, slash included.
    let start = text.len() - line.len() + start;
    let before = text.get(..start).unwrap_or_default();
    let word = text.get(start..).unwrap_or_default();
    let units = |t: &str| i32::try_from(t.encode_utf16().count()).unwrap_or(i32::MAX);
    CommandSuggestions {
        id,
        start: units(before),
        length: units(word),
        matches: matches
            .into_iter()
            .map(|text| Suggestion {
                text,
                tooltip: None,
            })
            .collect(),
    }
}
