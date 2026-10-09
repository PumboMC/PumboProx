//! Generated per-protocol tables and the version modules built from them.
//!
//! The tables under `tables/<protocol>/` come from `pumbo-datagen` (Mojang's
//! data generator run on the official server jars, plan §2.5). Only IDs and
//! names are in the repository (decision §9.2). `build.rs` embeds every
//! directory it finds, so a new protocol is a generator run, not a code change.

mod model;
mod module;
mod synced;

pub use model::{
    Block, BlockProperty, PacketAlias, PacketEntry, Registry, Release, Tables, full_name,
    short_name,
};
pub use module::DataVersion;
pub use synced::{Synced, SyncedRegistry};

use std::sync::Arc;

use pumbo_protocol::{ProtocolVersion, RegistryError, VersionRegistry};

/// Static registries kept in the tables (plan §2.5, E1).
pub const STATIC_REGISTRIES: &[&str] = &[
    "minecraft:attribute",
    "minecraft:block_entity_type",
    "minecraft:command_argument_type",
    "minecraft:custom_stat",
    "minecraft:data_component_type",
    "minecraft:entity_type",
    "minecraft:game_event",
    "minecraft:item",
    "minecraft:map_decoration_type",
    "minecraft:menu",
    "minecraft:mob_effect",
    "minecraft:particle_type",
    "minecraft:potion",
    "minecraft:recipe_serializer",
    "minecraft:sound_event",
    "minecraft:stat_type",
    "minecraft:villager_profession",
    "minecraft:villager_type",
];

/// Static registries that newer protocols added; kept where they exist
/// (the version translator remaps their IDs).
pub const LATER_REGISTRIES: &[&str] = &[
    "minecraft:consume_effect_type",
    "minecraft:recipe_book_category",
    "minecraft:recipe_display",
    "minecraft:slot_display",
];

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DataError {
    #[error("tables of protocol {protocol}, {file}:{line}: {message}")]
    Parse {
        protocol: i32,
        file: &'static str,
        line: usize,
        message: String,
    },
    #[error("no tables for protocol {0}")]
    Missing(i32),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

pub(crate) struct Embedded {
    pub protocol: i32,
    pub meta: &'static str,
    pub packets: &'static str,
    pub registries: &'static str,
    pub blocks: &'static str,
    /// `synced.txt`, if recorded.
    pub synced: Option<&'static str>,
    /// Full registry data (a `pumbo-testclient` recording), if generated
    /// locally; never in git.
    pub full: Option<&'static [u8]>,
}

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

/// Protocols with embedded tables, oldest first.
pub fn protocols() -> impl Iterator<Item = ProtocolVersion> {
    EMBEDDED.iter().map(|e| ProtocolVersion(e.protocol))
}

/// Tables of one protocol, parsed on first use.
pub fn tables(v: ProtocolVersion) -> Result<&'static Tables, DataError> {
    let i = EMBEDDED
        .iter()
        .position(|e| e.protocol == v.0)
        .ok_or(DataError::Missing(v.0))?;
    let (Some(e), Some(slot)) = (EMBEDDED.get(i), PARSED.get(i)) else {
        return Err(DataError::Missing(v.0));
    };
    slot.get_or_init(|| {
        let t = Tables::parse(e.meta, e.packets, e.registries, e.blocks)?;
        if t.protocol != v {
            return Err(DataError::Parse {
                protocol: v.0,
                file: "meta.txt",
                line: 1,
                message: format!("directory {} holds protocol {}", v.0, t.protocol),
            });
        }
        Ok(t)
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Registers a module for every embedded protocol.
pub fn register_all(reg: &mut VersionRegistry) -> Result<(), DataError> {
    for v in protocols() {
        reg.register(Arc::new(DataVersion::new(tables(v)?)))?;
    }
    Ok(())
}

/// Recorded configuration data of a protocol (`None` if not recorded yet).
pub fn synced(v: ProtocolVersion) -> Result<Option<&'static Synced>, DataError> {
    let i = EMBEDDED
        .iter()
        .position(|e| e.protocol == v.0)
        .ok_or(DataError::Missing(v.0))?;
    let (Some(e), Some(slot)) = (EMBEDDED.get(i), SYNCED.get(i)) else {
        return Err(DataError::Missing(v.0));
    };
    let Some(text) = e.synced else {
        return Ok(None);
    };
    slot.get_or_init(|| Synced::parse(v.0, text))
        .as_ref()
        .map(Some)
        .map_err(Clone::clone)
}

/// Full registry data for clients without known packs, as recorded frames
/// (`registry_data` and `update_tags`), if this build has it (§2.5, §2.6).
pub fn full_registry_data(v: ProtocolVersion) -> Option<&'static [u8]> {
    EMBEDDED.iter().find(|e| e.protocol == v.0)?.full
}
