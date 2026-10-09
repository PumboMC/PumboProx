//! PumboAPI virtual world (plan §5.1–§5.7): a world served by the proxy
//! itself, before any backend sees the player or in the middle of a game.
//!
//! The crate has no I/O. [`VirtualPlayer`] turns client frames into
//! [`Input`]s and commands into frames, encoded per protocol version; the
//! proxy session owns the connection and sends what comes out.
//!
//! - [`registry`]: known packs, synchronized registries with or without data,
//!   the void dimension,
//! - [`world`]: blocks, structures, chunks per version (cached),
//! - [`map`]: map images encoded once, the filled map item per version,
//! - [`player`]: one player in a world: configuration, joining, teleports,
//!   chunks, maps, inventory, experience, time, game mode, world changes,
//! - [`physics`]: the vanilla falling curve (for the fall test of E7 and the
//!   tests here).

pub mod map;
pub mod physics;
pub mod player;
pub mod registry;
pub mod world;

pub use map::MapImage;
pub use player::{Hand, Handled, Input, Out, Position, VirtualPlayer};
pub use world::{BlockPos, GameMode, World, WorldOptions};

use std::sync::Arc;

use pumbo_protocol::PacketKind;
use pumbo_protocol::types::{DecodeError, EncodeError};

/// An input with the driver's tag for the player (the plugin host's player
/// ID, for instance).
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInput {
    pub player: u64,
    pub input: Input,
}

/// What a driver (the plugin host, the test gate) asks of a player's session
/// (WIT `virtual`, plan §5). The session answers with [`PlayerInput`]s on the
/// channel of the last `Enter`; a full channel drops inputs, so a slow
/// driver cannot grow the session's memory.
#[derive(Debug, Clone)]
pub enum Command {
    /// Into `world` at `at`: from the login (gate), from a server (through
    /// `start_configuration`, §5.2) or, already in a virtual world, a world
    /// change with `respawn`. `commands` form the command tree there.
    Enter {
        world: Arc<World>,
        at: Position,
        commands: Vec<String>,
        tag: u64,
        inputs: tokio::sync::mpsc::Sender<PlayerInput>,
    },
    /// A teleport; `id` is the one the confirmation will carry (the session
    /// picks one when `None`).
    Teleport {
        at: Position,
        id: Option<i32>,
    },
    ShowMap(MapImage, Hand),
    ClearInventory,
    Xp {
        bar: f32,
        level: i32,
    },
    Time(i64),
    GameMode(GameMode),
    Flying {
        allow: bool,
        flying: bool,
    },
    Sound {
        name: String,
        volume: f32,
        pitch: f32,
    },
    /// Out of the virtual world: the next gate or a server, without a
    /// disconnect screen (§5.6).
    Release,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("protocol data: {0}")]
    Data(#[from] pumbo_data::DataError),
    #[error("missing {0}")]
    NoData(&'static str),
    #[error("this version has no {0:?}")]
    NoPacket(PacketKind),
    #[error("encoding: {0}")]
    Encode(#[from] EncodeError),
    #[error("decoding: {0}")]
    Decode(#[from] DecodeError),
    #[error("unknown block state {0}")]
    Block(String),
    #[error("y = {0} is outside the world (0 to 255)")]
    OutOfWorld(i32),
    #[error("too many blocks in the world")]
    TooManyBlocks,
    #[error("more than 256 block states in one chunk section")]
    TooManyStates,
    #[error("structure file: {0}")]
    Structure(String),
    #[error("a map image has 16384 pixels, not {0}")]
    MapSize(usize),
    #[error("not in the virtual world yet")]
    NotJoined,
}
