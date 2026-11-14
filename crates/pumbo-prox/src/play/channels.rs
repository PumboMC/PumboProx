//! Session commands, deferred messages, the Tab list and plugin messages.

use pumbo_protocol::packets;
use pumbo_protocol::packets::common::{
    self as pc, ClientboundCustomPayload, ServerboundCustomPayload,
};
use pumbo_protocol::packets::play::{
    BossAction, BossEvent, ClearTitles, SetActionBarText, SetSubtitleText, SetTitleText,
    SetTitlesAnimation, TabList,
};
use pumbo_protocol::types::Reader;
use pumbo_protocol::{Direction, Phase, VersionModule};
use pumbo_text::TextFormat;
use tracing::debug;
use uuid::Uuid;

use crate::conn::Conn;
use crate::server::SessionCmd;
use crate::session::{Fail, ctx, queue, queue_out};
use crate::status::parse_text;
use crate::world::BossbarOp;

use super::{End, MAX_DEFERRED, Play, Step};

impl Play<'_> {
    pub(super) fn command(&mut self, cmd: SessionCmd) -> Option<End> {
        self.follow_runtime();
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
            SessionCmd::Suggestions(answer) => {
                self.out(|m, c| queue_out(c, m, Phase::Play, &*answer));
            }
            SessionCmd::Property { name, value } => {
                let props = &mut self.player.profile.properties;
                props.retain(|p| p.name != name);
                props.extend(value);
            }
        }
        None
    }

    /// A kick or a gate release (outside the queue). Commands queued before a
    /// release go first, so a gate's last messages and worlds keep their order.
    pub(super) fn urgent_cmd(&mut self, cmd: SessionCmd) -> Option<End> {
        if !matches!(cmd, SessionCmd::Kick(_)) {
            while let Ok(queued) = self.cmds.try_recv() {
                if let Some(end) = self.command(queued) {
                    return Some(end);
                }
            }
        }
        self.command(cmd)
    }

    /// Permissions, context or plugin commands changed: the command tree and
    /// the PumboBridge permission set follow.
    pub(super) fn on_commands_changed(&mut self) {
        self.resend_commands();
        if let Some(b) = self.proxy.bridge.get() {
            b.perms_changed(self.player.profile.id, self.current().as_deref());
        }
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
    pub(super) fn after_join(&mut self) {
        for n in std::mem::take(&mut self.notices) {
            self.say(n);
        }
        for c in std::mem::take(&mut self.deferred) {
            self.command(c);
        }
        self.config_tab(true);
    }

    /// Whether the client is in play on a server or in a virtual world.
    pub(super) fn in_play(&self) -> bool {
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
        let module = self.module;
        if let Err(e) = f(module, &mut self.client) {
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

    pub(super) fn tab_configured(&self) -> bool {
        let tab = &self.rt.config.tab;
        !(tab.header.is_empty() && tab.footer.is_empty())
    }

    /// The client's settings for plugins (`player-info`).
    pub(super) fn settings_to_plugins(&self, payload: &[u8]) {
        let Some(p) = self.proxy.plugins.get() else {
            return;
        };
        let sb = ctx(self.module, Direction::Serverbound);
        if let Ok(c) = packets::decode::<pc::ClientInformation>(payload, &sb) {
            p.settings(self.player.profile.id, &c);
        }
    }

    /// `%player_ping%` from keep-alive round trips.
    pub(super) fn ping_to_plugins(&self, ms: u32) {
        if let Some(p) = self.proxy.plugins.get() {
            p.ping(self.player.profile.id, ms);
        }
    }

    pub(super) fn brand_to_plugins(&self, data: &[u8]) {
        if let Some(p) = self.proxy.plugins.get()
            && let Ok(brand) = Reader::new(data).string(256)
        {
            p.brand(self.player.profile.id, &brand);
        }
    }

    /// `tab` header and footer of the config (§5.8.3): rendered for this
    /// player (push and cached placeholders only) every `refresh-ms` and
    /// after a server change; sent when the text changed.
    pub(super) fn config_tab(&mut self, force: bool) {
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
        let module = self.module;
        let m = module;
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
    pub(super) fn resend_commands(&mut self) {
        if self.waiting() && self.in_play() && self.client_phase == Phase::Play {
            if let Err(e) = self.pumbox_tree() {
                debug!("command tree: {e:?}");
            }
            return;
        }
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
}
