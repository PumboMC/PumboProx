//! One player in a virtual world (plan §5.2, §5.4, §5.5).
//!
//! The session calls [`VirtualPlayer::start`] while the client is in the
//! configuration phase, passes every client frame to
//! [`VirtualPlayer::on_frame`] and sends what lands in [`Out`]. Keep-alives,
//! client settings and plugin messages stay with the session (it tracks them
//! for backends anyway).
//!
//! Joining follows vanilla's order: `login`, abilities, held slot, command
//! tree, position, the player's own Tab entry (with the skin), time, spawn
//! point, "wait for chunks", chunk centre, one chunk batch. The world counts
//! as loaded on `player_loaded` (769+) or on the confirmed first teleport and
//! the acknowledged chunk batch (767, 768); a map is shown only then, since
//! the client draws none before its chunks (spec §2.4).

use std::collections::VecDeque;
use std::sync::Arc;

use bytes::Bytes;
use pumbo_protocol::packets::commands::{
    Commands, FLAG_EXECUTABLE, NODE_ARGUMENT, NODE_LITERAL, NODE_ROOT, Node, Parser,
    ParserProperties,
};
use pumbo_protocol::packets::common::{ClientboundCustomPayload, GameProfile};
use pumbo_protocol::packets::configuration::{FinishConfiguration, SelectKnownPacks};
use pumbo_protocol::packets::play::{Chat, ChatCommand, ChatCommandSigned, Login};
use pumbo_protocol::packets::world::{
    AcceptTeleportation, ChunkBatchFinished, ChunkBatchStart, ClockUpdate, ContainerSetSlot,
    GameEvent, InfoEntry, ItemStack, MovePlayer, PlayerAbilities, PlayerInfoUpdate, PlayerPosition,
    Respawn, SetChunkCacheCenter, SetDefaultSpawnPosition, SetExperience, SetHeldSlot, SetTime,
    Sound, SoundRef,
};
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::types::{Position as BlockPosition, WriteExt};
use pumbo_protocol::{Direction, PacketKind, Phase, VersionModule};

use crate::Error;
use crate::map::{MapImage, filled_map};
use crate::registry::{self, Prepared};
use crate::world::{GameMode, World};

/// Frames for the client: (packet ID, payload).
pub type Out = Vec<(i32, Bytes)>;

/// Entity ID of the player in the virtual world (nobody else is there).
const ENTITY_ID: i32 = 1;
/// Hotbar slot 0 in the player's inventory window; 45 is the off hand.
const HOTBAR_0: i16 = 36;
const OFF_HAND: i16 = 45;
/// Two level keys: a world change switches between them, so the client builds
/// a new level instead of keeping the old chunks.
const LEVELS: [&str; 2] = ["pumbo:virtual", "pumbo:virtual_b"];

/// A position with rotation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
}

impl Position {
    pub fn at(x: f64, y: f64, z: f64) -> Self {
        Self {
            x,
            y,
            z,
            ..Self::default()
        }
    }

    fn chunk(&self) -> (i32, i32) {
        ((self.x.floor() as i32) >> 4, (self.z.floor() as i32) >> 4)
    }

    fn block(&self) -> BlockPosition {
        BlockPosition {
            x: self.x.floor() as i32,
            y: self.y.floor() as i32,
            z: self.z.floor() as i32,
        }
    }
}

/// What the player did, for whoever drives the world (WIT `virtual.input`).
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A position report after the last teleport was confirmed.
    Moved {
        position: Position,
        on_ground: bool,
    },
    TeleportConfirmed(i32),
    /// The client shows the world (chunks there, first teleport confirmed).
    Loaded,
    Chat(String),
    /// A command line without the slash. Never logged by the host (it may be
    /// `/login <password>`).
    Command(String),
    Settings,
    Brand(String),
    PluginMessage(String, Vec<u8>),
    KeepaliveRtt(u32),
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    Main,
    Off,
}

/// Result of a client frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    /// Taken (or ignored on purpose).
    Done,
    /// The client acknowledged the end of configuration and joined the world.
    Joined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Known packs offered, waiting for the answer.
    Packs,
    /// Registries and `finish_configuration` sent.
    Finishing,
    Play,
}

/// One player in a virtual world.
#[derive(Debug)]
pub struct VirtualPlayer {
    module: Arc<dyn VersionModule>,
    profile: GameProfile,
    world: Arc<World>,
    at: Position,
    /// Command names in the tree (literal + greedy text).
    commands: Vec<String>,
    state: State,
    prepared: Option<Arc<Prepared>>,
    level: usize,
    next_teleport: i32,
    awaiting: Option<(i32, Position)>,
    queued: VecDeque<(Position, i32)>,
    batch_acknowledged: bool,
    first_teleport_done: bool,
    loaded: bool,
    map: Option<(MapImage, Hand)>,
    game_mode: GameMode,
    abilities: u8,
}

fn push<P: Packet>(out: &mut Out, m: &dyn VersionModule, phase: Phase, p: &P) -> Result<(), Error> {
    let id = m
        .packet_id(phase, Direction::Clientbound, P::KIND)
        .ok_or(Error::NoPacket(P::KIND))?;
    let payload = packets::encode(p, &Ctx::new(m, Direction::Clientbound))?;
    out.push((id, Bytes::from(payload)));
    Ok(())
}

impl VirtualPlayer {
    pub fn new(
        module: Arc<dyn VersionModule>,
        profile: GameProfile,
        world: Arc<World>,
        at: Position,
        commands: Vec<String>,
    ) -> Self {
        let game_mode = world.options.game_mode;
        Self {
            module,
            profile,
            world,
            at,
            commands,
            state: State::Packs,
            prepared: None,
            level: 0,
            next_teleport: 1,
            awaiting: None,
            queued: VecDeque::new(),
            batch_acknowledged: false,
            first_teleport_done: false,
            loaded: false,
            map: None,
            game_mode,
            abilities: PlayerAbilities::INVULNERABLE,
        }
    }

    pub fn module(&self) -> &Arc<dyn VersionModule> {
        &self.module
    }

    pub fn in_play(&self) -> bool {
        self.state == State::Play
    }

    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Our `finish_configuration` went out: the stream to the client is play.
    pub fn finishing(&self) -> bool {
        self.state != State::Packs
    }

    /// Last known position (teleport target or reported movement).
    pub fn position(&self) -> Position {
        self.at
    }

    /// The client is in configuration: brand, enabled features, known packs.
    pub fn start(&mut self, out: &mut Out) -> Result<(), Error> {
        let m = &*self.module.clone();
        let cfg = Phase::Configuration;
        let mut brand = Vec::new();
        brand.put_string("PumboProx", 32_767)?;
        push(
            out,
            m,
            cfg,
            &ClientboundCustomPayload {
                channel: "minecraft:brand".into(),
                data: brand,
            },
        )?;
        push(out, m, cfg, &registry::features(m)?)?;
        push(out, m, cfg, &registry::known_packs_offer(m))?;
        self.state = State::Packs;
        Ok(())
    }

    /// A frame from the client. Movement, teleport confirmations, chunk
    /// batches, `player_loaded`, chat and commands become inputs; the rest is
    /// ignored.
    pub fn on_frame(
        &mut self,
        phase: Phase,
        kind: Option<PacketKind>,
        payload: &[u8],
        out: &mut Out,
        inputs: &mut Vec<Input>,
    ) -> Result<Handled, Error> {
        let m = self.module.clone();
        let ctx = Ctx::new(&*m, Direction::Serverbound);
        let Some(kind) = kind else {
            return Ok(Handled::Done);
        };
        match (phase, kind) {
            (Phase::Configuration, PacketKind::SelectKnownPacks) if self.state == State::Packs => {
                let reply: SelectKnownPacks = packets::decode(payload, &ctx)?;
                let prepared = registry::prepared(&*m, registry::confirmed(&*m, &reply))?;
                out.extend(prepared.frames.iter().cloned());
                push(out, &*m, Phase::Configuration, &FinishConfiguration)?;
                self.prepared = Some(prepared);
                self.state = State::Finishing;
            }
            (Phase::Configuration, PacketKind::FinishConfiguration)
                if self.state == State::Finishing =>
            {
                self.state = State::Play;
                self.join(out)?;
                return Ok(Handled::Joined);
            }
            (Phase::Play, PacketKind::AcceptTeleportation) => {
                let t: AcceptTeleportation = packets::decode(payload, &ctx)?;
                if self.awaiting.is_some_and(|(id, _)| id == t.id) {
                    self.awaiting = None;
                    self.first_teleport_done = true;
                    inputs.push(Input::TeleportConfirmed(t.id));
                    if let Some((next, id)) = self.queued.pop_front() {
                        self.send_teleport(next, id, out)?;
                    }
                    self.check_loaded(out, inputs)?;
                }
            }
            (
                Phase::Play,
                PacketKind::MovePlayerPos
                | PacketKind::MovePlayerPosRot
                | PacketKind::MovePlayerRot
                | PacketKind::MovePlayerStatusOnly,
            ) => {
                let mv = MovePlayer::decode_kind(kind, payload)?;
                // Like vanilla: reports before the confirmation are stale.
                if self.awaiting.is_some() {
                    return Ok(Handled::Done);
                }
                if let Some((x, y, z)) = mv.position {
                    self.at.x = x;
                    self.at.y = y;
                    self.at.z = z;
                }
                if let Some((yaw, pitch)) = mv.rotation {
                    self.at.yaw = yaw;
                    self.at.pitch = pitch;
                }
                inputs.push(Input::Moved {
                    position: self.at,
                    on_ground: mv.on_ground(),
                });
            }
            (Phase::Play, PacketKind::ChunkBatchReceived) => {
                self.batch_acknowledged = true;
                self.check_loaded(out, inputs)?;
            }
            (Phase::Play, PacketKind::PlayerLoaded) => {
                if !self.loaded {
                    self.loaded = true;
                    inputs.push(Input::Loaded);
                    self.send_map(out)?;
                }
            }
            (Phase::Play, PacketKind::Chat) => {
                let c: Chat = packets::decode(payload, &ctx)?;
                inputs.push(Input::Chat(c.message));
            }
            (Phase::Play, PacketKind::ChatCommand) => {
                let c: ChatCommand = packets::decode(payload, &ctx)?;
                inputs.push(Input::Command(c.command));
            }
            (Phase::Play, PacketKind::ChatCommandSigned) => {
                let c: ChatCommandSigned = packets::decode(payload, &ctx)?;
                inputs.push(Input::Command(c.command));
            }
            _ => {}
        }
        Ok(Handled::Done)
    }

    /// 767 and 768 have no `player_loaded`: the world counts as shown once
    /// the chunks and the first teleport are acknowledged.
    fn check_loaded(&mut self, out: &mut Out, inputs: &mut Vec<Input>) -> Result<(), Error> {
        let has_packet = self
            .module
            .packet_id(
                Phase::Play,
                Direction::Serverbound,
                PacketKind::PlayerLoaded,
            )
            .is_some();
        if !self.loaded && !has_packet && self.batch_acknowledged && self.first_teleport_done {
            self.loaded = true;
            inputs.push(Input::Loaded);
            self.send_map(out)?;
        }
        Ok(())
    }

    fn prepared(&self) -> Result<&Prepared, Error> {
        self.prepared.as_deref().ok_or(Error::NotJoined)
    }

    /// The play part of joining (or of a world change after `respawn`).
    fn join(&mut self, out: &mut Out) -> Result<(), Error> {
        let m = self.module.clone();
        let m = &*m;
        let p = Phase::Play;
        let dimension_type = self.prepared()?.dimension_type;
        let view = i32::from(self.world.options.view_distance.clamp(2, 32));
        push(
            out,
            m,
            p,
            &Login {
                entity_id: ENTITY_ID,
                hardcore: false,
                dimensions: LEVELS.iter().map(|s| (*s).to_string()).collect(),
                max_players: 1,
                view_distance: view,
                simulation_distance: view,
                reduced_debug_info: false,
                respawn_screen: false,
                limited_crafting: false,
                dimension_type,
                dimension: self.level_name().into(),
                hashed_seed: 0,
                game_mode: self.game_mode.id(),
                previous_game_mode: None,
                debug: false,
                flat: true,
                death_location: None,
                portal_cooldown: 0,
                sea_level: 63,
                online_mode: false,
                enforces_secure_chat: false,
            },
        )?;
        self.after_spawn(out, true)
    }

    fn level_name(&self) -> &'static str {
        LEVELS.get(self.level).copied().unwrap_or(LEVELS[0])
    }

    /// Everything after `login`/`respawn`: abilities, commands, position,
    /// Tab entry, time, spawn point, chunks.
    fn after_spawn(&mut self, out: &mut Out, first: bool) -> Result<(), Error> {
        let m = self.module.clone();
        let m = &*m;
        let p = Phase::Play;
        self.loaded = false;
        self.batch_acknowledged = false;
        self.first_teleport_done = false;
        self.awaiting = None;
        self.queued.clear();
        self.send_abilities(out)?;
        push(out, m, p, &SetHeldSlot { slot: 0 })?;
        if first {
            push(out, m, p, &self.command_tree()?)?;
        }
        let id = self.next_teleport;
        self.next_teleport = self.next_teleport.wrapping_add(1).max(1);
        self.send_teleport(self.at, id, out)?;
        if first {
            self.tab_entry(out)?;
        }
        self.set_time(self.world.options.time, out)?;
        push(
            out,
            m,
            p,
            &SetDefaultSpawnPosition {
                dimension: self.level_name().into(),
                position: self.at.block(),
                yaw: self.at.yaw,
                pitch: 0.0,
            },
        )?;
        push(
            out,
            m,
            p,
            &GameEvent {
                event: GameEvent::LEVEL_CHUNKS_LOAD_START,
                value: 0.0,
            },
        )?;
        let (cx, cz) = self.at.chunk();
        push(out, m, p, &SetChunkCacheCenter { x: cx, z: cz })?;
        self.send_chunks(out)
    }

    fn send_chunks(&mut self, out: &mut Out) -> Result<(), Error> {
        let m = self.module.clone();
        let m = &*m;
        let biome = self.prepared()?.biome;
        let id = m
            .packet_id(
                Phase::Play,
                Direction::Clientbound,
                PacketKind::LevelChunkWithLight,
            )
            .ok_or(Error::NoPacket(PacketKind::LevelChunkWithLight))?;
        let r = i32::from(self.world.options.view_distance.clamp(1, 32));
        let (cx, cz) = self.at.chunk();
        push(out, m, Phase::Play, &ChunkBatchStart)?;
        let mut n = 0;
        for x in cx - r..=cx + r {
            for z in cz - r..=cz + r {
                out.push((id, self.world.chunk(m, biome, x, z)?));
                n += 1;
            }
        }
        push(out, m, Phase::Play, &ChunkBatchFinished { size: n })
    }

    fn command_tree(&self) -> Result<Commands, Error> {
        let m = &*self.module;
        let string = (0..1024).find(|i| m.command_argument_type(*i) == Some("brigadier:string"));
        let mut nodes = vec![Node {
            flags: NODE_ROOT,
            children: Vec::new(),
            redirect: None,
            name: None,
            parser: None,
            suggestions: None,
        }];
        for name in &self.commands {
            let at = i32::try_from(nodes.len()).map_err(|_| Error::NoData("command tree"))?;
            let mut children = Vec::new();
            if string.is_some() {
                children.push(at + 1);
            }
            nodes.push(Node {
                flags: NODE_LITERAL | FLAG_EXECUTABLE,
                children,
                redirect: None,
                name: Some(name.clone()),
                parser: None,
                suggestions: None,
            });
            if let Some(id) = string {
                nodes.push(Node {
                    flags: NODE_ARGUMENT | FLAG_EXECUTABLE,
                    children: Vec::new(),
                    redirect: None,
                    name: Some("args".into()),
                    parser: Some(Parser {
                        id,
                        // Greedy phrase.
                        properties: ParserProperties::String(2),
                    }),
                    suggestions: None,
                });
            }
            if let Some(root) = nodes.first_mut() {
                root.children.push(at);
            }
        }
        Ok(Commands { nodes, root: 0 })
    }

    /// The player's own Tab entry with its profile properties, so the skin
    /// shows in third person (PumboSkins sets `textures` before this).
    fn tab_entry(&self, out: &mut Out) -> Result<(), Error> {
        push(
            out,
            &*self.module,
            Phase::Play,
            &PlayerInfoUpdate {
                actions: PlayerInfoUpdate::ADD
                    | PlayerInfoUpdate::GAME_MODE
                    | PlayerInfoUpdate::LISTED
                    | PlayerInfoUpdate::LATENCY
                    | PlayerInfoUpdate::LIST_ORDER
                    | PlayerInfoUpdate::HAT,
                entries: vec![InfoEntry {
                    id: self.profile.id,
                    profile: Some(self.profile.clone()),
                    game_mode: self.game_mode.id(),
                    listed: true,
                    show_hat: true,
                    ..InfoEntry::default()
                }],
            },
        )
    }

    fn send_abilities(&self, out: &mut Out) -> Result<(), Error> {
        push(
            out,
            &*self.module,
            Phase::Play,
            &PlayerAbilities {
                flags: self.abilities,
                flying_speed: 0.05,
                fov_modifier: 0.1,
            },
        )
    }

    fn send_teleport(&mut self, at: Position, id: i32, out: &mut Out) -> Result<(), Error> {
        self.awaiting = Some((id, at));
        self.at = at;
        push(
            out,
            &*self.module,
            Phase::Play,
            &PlayerPosition {
                teleport_id: id,
                x: at.x,
                y: at.y,
                z: at.z,
                // Absolute and still: no drift after the teleport (spec §2.3, #80).
                velocity: (0.0, 0.0, 0.0),
                yaw: at.yaw,
                pitch: at.pitch,
                flags: 0,
            },
        )
    }

    /// Teleports the player; one teleport is pending at a time, the next ones
    /// wait (§5.4). `id` is what the confirmation carries (picked here when
    /// `None`); returns it, or `None` before the player is in the world.
    pub fn teleport(
        &mut self,
        at: Position,
        id: Option<i32>,
        out: &mut Out,
    ) -> Result<Option<i32>, Error> {
        if self.state != State::Play {
            self.at = at;
            return Ok(None);
        }
        let id = id.unwrap_or_else(|| {
            let id = self.next_teleport;
            self.next_teleport = self.next_teleport.wrapping_add(1).max(1);
            id
        });
        if self.awaiting.is_some() {
            self.queued.push_back((at, id));
            return Ok(Some(id));
        }
        self.send_teleport(at, id, out)?;
        Ok(Some(id))
    }

    /// A map in hand; sent once the world is loaded.
    pub fn show_map(&mut self, image: MapImage, hand: Hand, out: &mut Out) -> Result<(), Error> {
        self.map = Some((image, hand));
        if self.loaded {
            self.send_map(out)?;
        }
        Ok(())
    }

    fn send_map(&mut self, out: &mut Out) -> Result<(), Error> {
        let Some((image, hand)) = self.map.clone() else {
            return Ok(());
        };
        let m = &*self.module;
        let id = m
            .packet_id(Phase::Play, Direction::Clientbound, PacketKind::MapItemData)
            .ok_or(Error::NoPacket(PacketKind::MapItemData))?;
        out.push((id, image.payload.clone()));
        let slot = match hand {
            Hand::Main => HOTBAR_0,
            Hand::Off => OFF_HAND,
        };
        self.set_slot(slot, filled_map(m, image.id)?, out)?;
        if hand == Hand::Main {
            push(out, m, Phase::Play, &SetHeldSlot { slot: 0 })?;
        }
        Ok(())
    }

    fn set_slot(&self, slot: i16, item: ItemStack, out: &mut Out) -> Result<(), Error> {
        push(
            out,
            &*self.module,
            Phase::Play,
            &ContainerSetSlot {
                container: 0,
                state_id: 0,
                slot,
                item,
            },
        )
    }

    /// Empties the hotbar slot and the off hand the proxy uses.
    pub fn clear_inventory(&mut self, out: &mut Out) -> Result<(), Error> {
        self.map = None;
        if self.state != State::Play {
            return Ok(());
        }
        self.set_slot(HOTBAR_0, ItemStack::default(), out)?;
        self.set_slot(OFF_HAND, ItemStack::default(), out)
    }

    pub fn set_xp(&self, bar: f32, level: i32, out: &mut Out) -> Result<(), Error> {
        push(
            out,
            &*self.module,
            Phase::Play,
            &SetExperience {
                bar: bar.clamp(0.0, 1.0),
                level: level.max(0),
                total: 0,
            },
        )
    }

    /// Fixed time of day (the void has a fixed time; the clock does not run).
    pub fn set_time(&self, ticks: i64, out: &mut Out) -> Result<(), Error> {
        let m = &*self.module;
        let f = m.features();
        let clocks = if f.set_time_clocks {
            let n = pumbo_data::synced(m.protocol())?
                .and_then(|s| s.registry("minecraft:world_clock"))
                .map_or(0, |r| r.entries.len());
            (0..n)
                .map(|i| ClockUpdate {
                    clock: i32::try_from(i).unwrap_or(0),
                    ticks,
                    partial_tick: 0.0,
                    rate: 0.0,
                })
                .collect()
        } else {
            Vec::new()
        };
        push(
            out,
            m,
            Phase::Play,
            &SetTime {
                world_age: 0,
                // Before 768 a negative time of day stops the clock.
                time_of_day: if f.set_time_ticking_flag {
                    ticks
                } else {
                    -ticks.max(1)
                },
                ticking: false,
                clocks,
            },
        )
    }

    pub fn set_game_mode(&mut self, mode: GameMode, out: &mut Out) -> Result<(), Error> {
        self.game_mode = mode;
        push(
            out,
            &*self.module,
            Phase::Play,
            &GameEvent {
                event: GameEvent::CHANGE_GAME_MODE,
                value: mode.id() as f32,
            },
        )
    }

    /// May fly / is flying (e.g. so a player does not fall in an empty world).
    pub fn set_flying(&mut self, allow: bool, flying: bool, out: &mut Out) -> Result<(), Error> {
        self.abilities = PlayerAbilities::INVULNERABLE
            | if allow { PlayerAbilities::MAY_FLY } else { 0 }
            | if flying && allow {
                PlayerAbilities::FLYING
            } else {
                0
            };
        self.send_abilities(out)
    }

    /// Another world in the same connection: `respawn`, no reconfiguration
    /// (§5.2). The level key alternates, so the old chunks are dropped.
    pub fn change_world(
        &mut self,
        world: Arc<World>,
        at: Position,
        commands: Vec<String>,
        out: &mut Out,
    ) -> Result<(), Error> {
        self.world = world;
        self.at = at;
        self.game_mode = self.world.options.game_mode;
        // The next gate's commands (PumboFilter's /captcha, then PumboAuth's
        // /login): the client keeps the old tree until it gets a new one.
        let new_commands = self.commands != commands;
        self.commands = commands;
        if self.state != State::Play {
            return Ok(());
        }
        if new_commands {
            push(
                out,
                &*self.module.clone(),
                Phase::Play,
                &self.command_tree()?,
            )?;
        }
        self.level = (self.level + 1) % LEVELS.len();
        let dimension_type = self.prepared()?.dimension_type;
        push(
            out,
            &*self.module.clone(),
            Phase::Play,
            &Respawn {
                dimension_type,
                dimension: self.level_name().into(),
                hashed_seed: 0,
                game_mode: self.game_mode.id(),
                previous_game_mode: None,
                debug: false,
                flat: true,
                death_location: None,
                portal_cooldown: 0,
                sea_level: 63,
                data_kept: 0,
            },
        )?;
        self.after_spawn(out, false)?;
        // The map stays in hand across worlds.
        Ok(())
    }
}

/// `sound` by name at a position, with the ID of this version when it has one.
pub fn sound(
    module: &dyn VersionModule,
    name: &str,
    at: Position,
    volume: f32,
    pitch: f32,
) -> Result<Sound, Error> {
    let tables = pumbo_data::tables(module.protocol())?;
    let sound = match tables
        .registry("minecraft:sound_event")
        .and_then(|r| r.id(name))
    {
        Some(id) => SoundRef::Id(i32::try_from(id).unwrap_or(0)),
        None => SoundRef::Named {
            name: name.into(),
            range: None,
        },
    };
    Ok(Sound {
        sound,
        // "master"
        category: 0,
        x: (at.x * 8.0) as i32,
        y: (at.y * 8.0) as i32,
        z: (at.z * 8.0) as i32,
        volume,
        pitch,
        seed: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::WorldOptions;
    use pumbo_protocol::packets::world::{AcceptTeleportation, ChunkBatchReceived};
    use uuid::Uuid;

    fn frame<P: Packet>(m: &dyn VersionModule, p: &P) -> Vec<u8> {
        packets::encode(p, &Ctx::new(m, Direction::Serverbound)).unwrap()
    }

    #[test]
    fn join_teleport_load_map() {
        for v in pumbo_data::protocols() {
            let m: Arc<dyn VersionModule> =
                Arc::new(pumbo_data::DataVersion::new(pumbo_data::tables(v).unwrap()));
            let world = Arc::new(World::new(WorldOptions::default()));
            world
                .fill_layer((-2, -2), (2, 2), 64, "minecraft:smooth_stone")
                .unwrap();
            let profile = GameProfile {
                id: Uuid::from_u128(1),
                name: "Pumbo".into(),
                properties: Vec::new(),
            };
            let mut p = VirtualPlayer::new(
                m.clone(),
                profile,
                world,
                Position::at(0.5, 80.0, 0.5),
                vec!["gate".into()],
            );
            let (mut out, mut inputs) = (Vec::new(), Vec::new());
            p.start(&mut out).unwrap();
            let offer = registry::known_packs_offer(&*m);
            let cfg = Phase::Configuration;
            p.on_frame(
                cfg,
                Some(PacketKind::SelectKnownPacks),
                &frame(&*m, &offer),
                &mut out,
                &mut inputs,
            )
            .unwrap();
            let r = p
                .on_frame(
                    cfg,
                    Some(PacketKind::FinishConfiguration),
                    &[],
                    &mut out,
                    &mut inputs,
                )
                .unwrap();
            assert_eq!(r, Handled::Joined);
            p.show_map(MapImage::new(&[34; 16384]).unwrap(), Hand::Main, &mut out)
                .unwrap();
            let before = out.len();
            let pl = Phase::Play;
            p.on_frame(
                pl,
                Some(PacketKind::AcceptTeleportation),
                &frame(&*m, &AcceptTeleportation { id: 1, at: None }),
                &mut out,
                &mut inputs,
            )
            .unwrap();
            p.on_frame(
                pl,
                Some(PacketKind::ChunkBatchReceived),
                &frame(
                    &*m,
                    &ChunkBatchReceived {
                        chunks_per_tick: 9.0,
                    },
                ),
                &mut out,
                &mut inputs,
            )
            .unwrap();
            if v.0 >= 769 {
                assert!(!p.loaded());
                p.on_frame(
                    pl,
                    Some(PacketKind::PlayerLoaded),
                    &[],
                    &mut out,
                    &mut inputs,
                )
                .unwrap();
            }
            assert!(p.loaded(), "{v}");
            assert_eq!(inputs, vec![Input::TeleportConfirmed(1), Input::Loaded]);
            // Map data, the item and the held slot.
            assert_eq!(out.len() - before, 3, "{v}");
            // Another gate's world with other commands sends their tree.
            let tree = m
                .packet_id(Phase::Play, Direction::Clientbound, PacketKind::Commands)
                .unwrap();
            let next = Arc::new(World::new(WorldOptions::default()));
            let count = |out: &Out| out.iter().filter(|(id, _)| *id == tree).count();
            let mut out = Vec::new();
            p.change_world(
                next.clone(),
                Position::at(0.5, 80.0, 0.5),
                vec!["login".into()],
                &mut out,
            )
            .unwrap();
            assert_eq!(count(&out), 1, "{v}");
            let (_, body) = out.iter().find(|(id, _)| *id == tree).unwrap();
            assert!(body.windows(5).any(|w| w == b"login"), "{v}");
            let mut out = Vec::new();
            p.change_world(
                next,
                Position::at(0.5, 80.0, 0.5),
                vec!["login".into()],
                &mut out,
            )
            .unwrap();
            assert_eq!(count(&out), 0, "{v}");
        }
    }
}
