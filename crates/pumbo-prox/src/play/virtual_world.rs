//! Virtual worlds and the Pumbox: the proxy as the server (§5.2).

use std::time::Duration;

use bytes::Bytes;
use pumbo_core::routing::SelectReason;
use pumbo_protocol::packets;
use pumbo_protocol::packets::commands::{Commands, NODE_ROOT, Node};
use pumbo_protocol::packets::common::{CustomClickAction, ServerboundCustomPayload};
use pumbo_protocol::packets::play::{CommandSuggestion, SystemChat};
use pumbo_protocol::types::Reader;
use pumbo_protocol::{Direction, PacketKind, Phase, RawFrame};
use pumbo_text::{Component, TextFormat};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::bungee;
use crate::commands::{self, Source};
use crate::metrics::{GaugeGuard, Metrics};
use crate::session::{Fail, ctx, queue_out};
use crate::status::parse_text;
use crate::world::{PlayerInput, VirtualCmd};
use pumbo_virtual::{Handled, Input, VirtualPlayer};

use super::{
    End, Enter, InVirtual, MAX_DEFERRED, Next, Play, Purpose, Request, Step, add_literals, end_kick,
};

impl Play<'_> {
    /// Starts a virtual world for a client in configuration (§5.2).
    pub(super) fn enter_now(&mut self, e: Enter) -> Result<(), End> {
        if self.holding && self.in_gate.is_none() {
            let max = i64::from(self.rt.config.limits.max_in_gates);
            let Some(slot) =
                GaugeGuard::below(self.proxy.metrics.clone(), |m| &m.players_in_gates, max)
            else {
                return Err(End::Kick(Box::new(parse_text(
                    &self.rt.config.messages.gates_full,
                ))));
            };
            self.in_gate = Some(slot);
        }
        let profile = crate::session::to_wire(&self.player.profile);
        let mut player =
            VirtualPlayer::new(self.module_arc.clone(), profile, e.world, e.at, e.commands);
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

    pub(super) fn send_input(&mut self, input: Input) {
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

    pub(super) fn virtual_cmd(&mut self, cmd: VirtualCmd) -> Option<End> {
        let mut out = Vec::new();
        if let VirtualCmd::Enter {
            world,
            at,
            commands,
            tag,
            inputs,
        } = cmd
        {
            // Another driver takes the player out of the Pumbox.
            self.pumbox = None;
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
            ) if v.player.in_play() => {
                pumbo_virtual::player::sound(self.module, &name, v.player.position(), volume, pitch)
                    .and_then(|s| {
                        let payload =
                            packets::encode(&s, &ctx(&*self.module, Direction::Clientbound))?;
                        let id = self
                            .module
                            .packet_id(Phase::Play, Direction::Clientbound, PacketKind::Sound)
                            .ok_or(pumbo_virtual::Error::NoPacket(PacketKind::Sound))?;
                        out.push((id, Bytes::from(payload)));
                        Ok(())
                    })
            }
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
        self.in_gate = None;
        Metrics::inc(&self.proxy.metrics.virtual_releases);
        let mut candidates = self.candidates(SelectReason::InitialJoin, None);
        if let Some(back) = self.return_to.take() {
            candidates.retain(|c| *c != back);
            candidates.push_front(back);
        }
        if candidates.is_empty() {
            return self.wait_in_pumbox(self.no_server_text());
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

    /// No server can take the player: the Pumbox, an empty world of the
    /// proxy, until the server list changes. A kick with `text` when the
    /// Pumbox is off or no server speaks the client's version.
    pub(super) fn wait_in_pumbox(&mut self, text: Component) -> Option<End> {
        let unsupported =
            !self.rt.backends.is_empty() && !self.rt.backends.iter().any(|b| self.reachable(b));
        if !self.rt.config.pumbox.enabled || unsupported {
            return Some(End::Kick(Box::new(text)));
        }
        let was = self.pumbox.replace(self.rt.clone());
        self.pumbox_health = self.proxy.health.version();
        if was.is_none() {
            info!(
                "{} waits in the Pumbox: {}",
                self.player.profile.name,
                text.plain_text()
            );
        }
        self.say(text);
        if was.is_some() && (self.vworld.is_some() || self.enter_on_ack.is_some()) {
            return None;
        }
        let e = self.pumbox_enter();
        let fresh = self.backend.is_none()
            && self.client_phase == Phase::Configuration
            && self.out_phase == Phase::Configuration
            && matches!(self.next, Next::Idle);
        let mut out = Vec::new();
        match self.vworld.as_mut() {
            // From a gate's world.
            Some(v) => {
                v.inputs = e.inputs;
                v.tag = e.tag;
                if let Err(err) = v.player.change_world(e.world, e.at, e.commands, &mut out) {
                    return Some(self.virtual_failed(&err));
                }
                let end = self.queue_virtual(out).err();
                self.resend_commands();
                end
            }
            None if fresh => self.enter_now(e).err(),
            // Through configuration first (§5.2).
            None => {
                self.enter_on_ack = Some(e);
                None
            }
        }
    }

    /// The Pumbox world with its driver (`world::pumbox`).
    fn pumbox_enter(&self) -> Enter {
        let id = self.player.profile.id;
        let (tx, rx) = mpsc::channel(64);
        let name = self.player.profile.name.clone();
        tokio::spawn(crate::world::pumbox(self.proxy.clone(), id, name, rx));
        let (x, y, z) = crate::world::PUMBOX_SPAWN;
        Enter {
            world: crate::world::pumbox_world(),
            at: pumbo_virtual::Position::at(x, y, z),
            // The session sends the proxy commands' tree (`pumbox_tree`).
            commands: Vec::new(),
            tag: 0,
            inputs: tx,
        }
    }

    /// The proxy commands this player may use, as on a server: the Pumbox
    /// runs them (`world::pumbox`) and answers their Tab completion.
    pub(super) fn pumbox_tree(&mut self) -> Result<(), Fail> {
        let source = Source::Player {
            id: self.player.profile.id,
            server: None,
        };
        let names = commands::visible(self.proxy, &self.rt, &source)
            .into_iter()
            .map(|n| (n.to_string(), Vec::new()))
            .collect();
        let mut tree = Commands {
            nodes: vec![Node {
                flags: NODE_ROOT,
                children: Vec::new(),
                redirect: None,
                name: None,
                parser: None,
                suggestions: None,
            }],
            root: 0,
        };
        add_literals(self.module, &mut tree, 0, names)?;
        queue_out(&mut self.client, self.module, Phase::Play, &tree)
    }

    /// In the Pumbox with nothing going on.
    pub(super) fn waiting(&self) -> bool {
        self.pumbox.is_some()
            && self.vworld.is_some()
            && self.request.is_none()
            && matches!(self.next, Next::Idle)
    }

    /// A client frame while in a virtual world.
    pub(super) fn virtual_step(
        &mut self,
        phase: Phase,
        kind: Option<PacketKind>,
        f: &RawFrame,
    ) -> Result<Step, Fail> {
        let module = self.module;
        let sb = ctx(module, Direction::Serverbound);
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
                } else if self.pumbox.is_some() && phase == Phase::Play {
                    // Proxy commands complete as on a server; a gate's do not.
                    let s: CommandSuggestion = packets::decode(&f.payload, &sb)?;
                    self.suggest(&s)?;
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
            self.resend_commands();
            self.after_join();
        }
        Ok(Step::Drop)
    }

    /// A system message now if the player is in play, otherwise after the next join.
    pub(super) fn say(&mut self, text: Component) {
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
        if let Err(e) = queue_out(&mut self.client, self.module, Phase::Play, &msg) {
            debug!("system message: {e:?}");
        }
    }
}
