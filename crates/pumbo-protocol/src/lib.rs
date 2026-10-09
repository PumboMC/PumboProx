//! Minecraft Java protocol: versions, phases, raw frames and the version
//! module interface.
//!
//! The proxy core knows no concrete version. Each protocol version is a module
//! implementing [`VersionModule`] (ID tables from the generator and layout
//! features), registered in a [`VersionRegistry`] at startup. Adding a
//! Minecraft version means a new module or new data, without core changes
//! (plan §2.10).

pub mod crypto;
pub mod features;
pub mod frame;
pub mod kinds;
pub mod packets;
pub mod types;
pub mod version;
pub mod wire;

pub use features::VersionFeatures;
pub use kinds::{KNOWN_PACKETS, PacketKind};
pub use version::{ProtocolVersion, RegistryError, VersionModule, VersionRegistry};

use bytes::Bytes;

/// Connection phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Phase {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
}

impl Phase {
    pub const ALL: [Phase; 5] = [
        Phase::Handshake,
        Phase::Status,
        Phase::Login,
        Phase::Configuration,
        Phase::Play,
    ];

    /// Name used in Mojang's `packets.json` report.
    pub const fn report_name(self) -> &'static str {
        match self {
            Phase::Handshake => "handshake",
            Phase::Status => "status",
            Phase::Login => "login",
            Phase::Configuration => "configuration",
            Phase::Play => "play",
        }
    }

    pub fn from_report_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.report_name() == name)
    }
}

/// Packet direction, named as in Mojang's reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Direction {
    /// Server to client.
    Clientbound,
    /// Client to server.
    Serverbound,
}

impl Direction {
    pub const ALL: [Direction; 2] = [Direction::Clientbound, Direction::Serverbound];

    pub const fn report_name(self) -> &'static str {
        match self {
            Direction::Clientbound => "clientbound",
            Direction::Serverbound => "serverbound",
        }
    }

    pub fn from_report_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.report_name() == name)
    }
}

/// A frame after removing length, compression and encryption: ID and raw
/// payload. Packets the proxy does not understand pass in this form (§2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub id: i32,
    pub payload: Bytes,
}
