//! What the translator needs to know about a protocol version. The caller
//! fills it from its own tables (IDs and names from Mojang's data generator
//! and recordings of vanilla servers); this crate carries no per-version tables
//! except the entity data fields (`data/entity_data.txt`) and the stand-ins
//! of ViaVersion Mappings (`data/via_mappings.txt`).

use std::collections::HashMap;

/// Connection phases the translator sees (login and handshake stay with the proxy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Configuration,
    Play,
}

impl Phase {
    pub(crate) fn index(self) -> usize {
        match self {
            Phase::Configuration => 0,
            Phase::Play => 1,
        }
    }
}

/// Packet direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bound {
    /// Server to client.
    Client,
    /// Client to server.
    Server,
}

impl Bound {
    pub(crate) fn index(self) -> usize {
        match self {
            Bound::Client => 0,
            Bound::Server => 1,
        }
    }
}

/// A block and its states: states are the cartesian product of the property
/// values in the given order, the last property varying fastest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Without the `minecraft:` namespace.
    pub name: String,
    pub first_state: u32,
    pub default_offset: u32,
    pub properties: Vec<(String, Vec<String>)>,
}

impl Block {
    pub(crate) fn state_count(&self) -> u32 {
        self.properties
            .iter()
            .map(|(_, v)| u32::try_from(v.len()).unwrap_or(u32::MAX))
            .fold(1u32, u32::saturating_mul)
    }

    /// Value index of every property at `offset`.
    pub(crate) fn value_indices(&self, offset: u32) -> Vec<usize> {
        let mut rest = offset;
        let mut out = vec![0; self.properties.len()];
        for (slot, (_, values)) in out.iter_mut().zip(&self.properties).rev() {
            let len = u32::try_from(values.len()).unwrap_or(1).max(1);
            *slot = (rest % len) as usize;
            rest /= len;
        }
        out
    }

    /// Property names and values at `offset`.
    pub(crate) fn values(&self, offset: u32) -> Vec<(&str, &str)> {
        self.properties
            .iter()
            .zip(self.value_indices(offset))
            .filter_map(|((name, values), i)| Some((name.as_str(), values.get(i)?.as_str())))
            .collect()
    }
}

/// Tags of one registry: tag name and entry IDs.
pub type TagList = Vec<(String, Vec<i32>)>;

/// Registries, tags and feature flags a vanilla server of this version sends
/// in configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Synced {
    /// Registry (no namespace) and entry names in ID order.
    pub registries: Vec<(String, Vec<String>)>,
    /// Registry and its tags with entry IDs.
    pub tags: Vec<(String, TagList)>,
    pub features: Vec<String>,
}

/// One protocol version.
#[derive(Debug, Clone, Default)]
pub struct VersionData {
    pub protocol: i32,
    /// Release names with this protocol, oldest first (known packs offer one
    /// `minecraft:core` per release).
    pub releases: Vec<String>,
    /// Packet names (current report names, no namespace) by ID:
    /// `[phase][bound]`, see [`Phase`] and [`Bound`].
    pub packets: [[Vec<String>; 2]; 2],
    /// Static registries (no namespace) with entry names in ID order.
    pub registries: HashMap<String, Vec<String>>,
    pub blocks: Vec<Block>,
    pub synced: Synced,
    /// `registry_data` payloads with full NBT, for clients that do not confirm
    /// the core pack; empty when the caller has none.
    pub full_registry_data: Vec<Vec<u8>>,
}

impl VersionData {
    pub(crate) fn registry(&self, name: &str) -> &[String] {
        self.registries.get(name).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn packet_name(&self, phase: Phase, bound: Bound, id: i32) -> Option<&str> {
        let list = self.packets.get(phase.index())?.get(bound.index())?;
        list.get(usize::try_from(id).ok()?).map(String::as_str)
    }

    pub(crate) fn block_state_count(&self) -> u32 {
        self.blocks.iter().map(Block::state_count).sum()
    }
}
