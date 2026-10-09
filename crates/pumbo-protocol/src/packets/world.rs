//! Play packets of the virtual world (plan §2.3 "PumboAPI", §5): the proxy
//! sends them itself and reads the client's movement. Layouts per version
//! follow minecraft.wiki ("Java Edition protocol/Packets") and Pumpkin's
//! `pumpkin-protocol` (GPL-3.0) for 26.3; the vanilla recordings decide
//! (golden test).

use uuid::Uuid;

use super::common::{GameProfile, empty_packet};
use super::{Ctx, Packet, Text};
use crate::PacketKind;
use crate::types::{DecodeError, EncodeError, MAX_STRING, Position, Reader, WriteExt};

/// Longest BitSet we read (in longs); chunk light masks need one.
const MAX_BITSET_LONGS: usize = 64;
/// Sections, light arrays and block entities in one chunk packet.
const MAX_CHUNK_ITEMS: usize = 4096;
const LIGHT_ARRAY: usize = 2048;
/// Chunk section data from a backend may be large; ours is small.
const MAX_SECTION_BYTES: usize = 2 * 1024 * 1024;

fn bitset(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Vec<i64>, DecodeError> {
    if !ctx.features.bitset_bytes {
        return r.bitset(MAX_BITSET_LONGS);
    }
    let bytes = r.byte_array(MAX_BITSET_LONGS * 8)?;
    Ok(bytes
        .chunks(8)
        .map(|c| {
            let mut b = [0u8; 8];
            for (d, s) in b.iter_mut().zip(c) {
                *d = *s;
            }
            i64::from_le_bytes(b)
        })
        .collect())
}

fn put_bitset(out: &mut Vec<u8>, longs: &[i64], ctx: &Ctx<'_>) -> Result<(), EncodeError> {
    if !ctx.features.bitset_bytes {
        return out.put_bitset(longs, MAX_BITSET_LONGS);
    }
    let mut bytes: Vec<u8> = longs.iter().flat_map(|l| l.to_le_bytes()).collect();
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    out.put_byte_array(&bytes, MAX_BITSET_LONGS * 8)
}

/// `player_abilities` (clientbound).
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerAbilities {
    /// 0x01 invulnerable, 0x02 flying, 0x04 may fly, 0x08 instant break.
    pub flags: u8,
    pub flying_speed: f32,
    pub fov_modifier: f32,
}

impl PlayerAbilities {
    pub const INVULNERABLE: u8 = 0x01;
    pub const FLYING: u8 = 0x02;
    pub const MAY_FLY: u8 = 0x04;
}

impl Packet for PlayerAbilities {
    const KIND: PacketKind = PacketKind::PlayerAbilities;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            flags: r.u8()?,
            flying_speed: r.f32()?,
            fov_modifier: r.f32()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_u8(self.flags);
        out.put_f32(self.flying_speed);
        out.put_f32(self.fov_modifier);
        Ok(())
    }
}

/// `set_default_spawn_position`.
#[derive(Debug, Clone, PartialEq)]
pub struct SetDefaultSpawnPosition {
    /// From 773.
    pub dimension: String,
    pub position: Position,
    pub yaw: f32,
    /// From 773.
    pub pitch: f32,
}

impl Packet for SetDefaultSpawnPosition {
    const KIND: PacketKind = PacketKind::SetDefaultSpawnPosition;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        if ctx.features.spawn_position_dimension {
            Ok(Self {
                dimension: r.identifier()?,
                position: r.position()?,
                yaw: r.f32()?,
                pitch: r.f32()?,
            })
        } else {
            Ok(Self {
                dimension: String::new(),
                position: r.position()?,
                yaw: r.f32()?,
                pitch: 0.0,
            })
        }
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        if ctx.features.spawn_position_dimension {
            out.put_identifier(&self.dimension)?;
        }
        out.put_position(self.position);
        out.put_f32(self.yaw);
        if ctx.features.spawn_position_dimension {
            out.put_f32(self.pitch);
        }
        Ok(())
    }
}

/// `game_event`.
#[derive(Debug, Clone, PartialEq)]
pub struct GameEvent {
    pub event: u8,
    pub value: f32,
}

impl GameEvent {
    pub const CHANGE_GAME_MODE: u8 = 3;
    /// "Start waiting for level chunks" (the loading screen waits for the
    /// player's chunk).
    pub const LEVEL_CHUNKS_LOAD_START: u8 = 13;
}

impl Packet for GameEvent {
    const KIND: PacketKind = PacketKind::GameEvent;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            event: r.u8()?,
            value: r.f32()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_u8(self.event);
        out.put_f32(self.value);
        Ok(())
    }
}

/// `set_chunk_cache_center`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetChunkCacheCenter {
    pub x: i32,
    pub z: i32,
}

impl Packet for SetChunkCacheCenter {
    const KIND: PacketKind = PacketKind::SetChunkCacheCenter;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            x: r.varint()?,
            z: r.varint()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.x);
        out.put_varint(self.z);
        Ok(())
    }
}

empty_packet!(
    /// `chunk_batch_start`.
    ChunkBatchStart,
    ChunkBatchStart
);

/// `chunk_batch_finished`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkBatchFinished {
    pub size: i32,
}

impl Packet for ChunkBatchFinished {
    const KIND: PacketKind = PacketKind::ChunkBatchFinished;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self { size: r.varint()? })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.size);
        Ok(())
    }
}

/// Heightmaps of a chunk: NBT before 770, a list of (type, longs) after.
#[derive(Debug, Clone, PartialEq)]
pub enum Heightmaps {
    Nbt(Text),
    List(Vec<(i32, Vec<i64>)>),
}

/// A block entity in a chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkBlockEntity {
    /// Section-relative x in the high nibble, z in the low one.
    pub xz: u8,
    pub y: i16,
    pub kind: i32,
    pub data: Option<Text>,
}

/// Light of a chunk: masks over the sections plus one below and one above
/// the world, and 2048-byte nibble arrays for the set bits of the first two.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LightData {
    pub sky_mask: Vec<i64>,
    pub block_mask: Vec<i64>,
    pub empty_sky_mask: Vec<i64>,
    pub empty_block_mask: Vec<i64>,
    pub sky: Vec<Vec<u8>>,
    pub block: Vec<Vec<u8>>,
}

fn light_arrays(r: &mut Reader<'_>) -> Result<Vec<Vec<u8>>, DecodeError> {
    let n = r.count(MAX_CHUNK_ITEMS, 1, "light arrays")?;
    (0..n)
        .map(|_| Ok(r.byte_array(LIGHT_ARRAY)?.to_vec()))
        .collect()
}

fn put_light_arrays(out: &mut Vec<u8>, arrays: &[Vec<u8>]) -> Result<(), EncodeError> {
    out.put_len(arrays.len(), MAX_CHUNK_ITEMS, "light arrays")?;
    for a in arrays {
        out.put_byte_array(a, LIGHT_ARRAY)?;
    }
    Ok(())
}

impl LightData {
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            sky_mask: bitset(r, ctx)?,
            block_mask: bitset(r, ctx)?,
            empty_sky_mask: bitset(r, ctx)?,
            empty_block_mask: bitset(r, ctx)?,
            sky: light_arrays(r)?,
            block: light_arrays(r)?,
        })
    }

    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        put_bitset(out, &self.sky_mask, ctx)?;
        put_bitset(out, &self.block_mask, ctx)?;
        put_bitset(out, &self.empty_sky_mask, ctx)?;
        put_bitset(out, &self.empty_block_mask, ctx)?;
        put_light_arrays(out, &self.sky)?;
        put_light_arrays(out, &self.block)
    }
}

/// `level_chunk_with_light`. The sections stay bytes here; their format per
/// version is in `pumbo-virtual` (vanilla 770 pads them with zeros, so a
/// parsed form would not give the same bytes back).
#[derive(Debug, Clone, PartialEq)]
pub struct LevelChunkWithLight {
    pub x: i32,
    pub z: i32,
    pub heightmaps: Heightmaps,
    pub sections: Vec<u8>,
    pub block_entities: Vec<ChunkBlockEntity>,
    pub light: LightData,
}

impl Packet for LevelChunkWithLight {
    const KIND: PacketKind = PacketKind::LevelChunkWithLight;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let x = r.i32()?;
        let z = r.i32()?;
        let heightmaps = if ctx.features.chunk_heightmaps_list {
            let n = r.count(64, 2, "heightmaps")?;
            Heightmaps::List(
                (0..n)
                    .map(|_| Ok((r.varint()?, r.bitset(MAX_CHUNK_ITEMS)?)))
                    .collect::<Result<_, DecodeError>>()?,
            )
        } else {
            Heightmaps::Nbt(r.nbt_required(ctx.nbt)?)
        };
        let sections = r.byte_array(MAX_SECTION_BYTES)?.to_vec();
        let n = r.count(MAX_CHUNK_ITEMS * 16, 4, "block entities")?;
        let block_entities = (0..n)
            .map(|_| {
                Ok(ChunkBlockEntity {
                    xz: r.u8()?,
                    y: r.i16()?,
                    kind: r.varint()?,
                    data: r.nbt(ctx.nbt)?,
                })
            })
            .collect::<Result<_, DecodeError>>()?;
        Ok(Self {
            x,
            z,
            heightmaps,
            sections,
            block_entities,
            light: LightData::decode(r, ctx)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_i32(self.x);
        out.put_i32(self.z);
        match (&self.heightmaps, ctx.features.chunk_heightmaps_list) {
            (Heightmaps::List(maps), true) => {
                out.put_len(maps.len(), 64, "heightmaps")?;
                for (kind, longs) in maps {
                    out.put_varint(*kind);
                    out.put_bitset(longs, MAX_CHUNK_ITEMS)?;
                }
            }
            (Heightmaps::Nbt(tag), false) => out.put_nbt(Some(tag))?,
            _ => return Err(EncodeError::Invalid("heightmaps in the wrong format")),
        }
        out.put_byte_array(&self.sections, MAX_SECTION_BYTES)?;
        out.put_len(
            self.block_entities.len(),
            MAX_CHUNK_ITEMS * 16,
            "block entities",
        )?;
        for b in &self.block_entities {
            out.put_u8(b.xz);
            out.put_i16(b.y);
            out.put_varint(b.kind);
            out.put_nbt(b.data.as_ref())?;
        }
        self.light.encode(out, ctx)
    }
}

/// `forget_level_chunk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetLevelChunk {
    pub x: i32,
    pub z: i32,
}

impl Packet for ForgetLevelChunk {
    const KIND: PacketKind = PacketKind::ForgetLevelChunk;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        let z = r.i32()?;
        Ok(Self { x: r.i32()?, z })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_i32(self.z);
        out.put_i32(self.x);
        Ok(())
    }
}

/// `player_position`: a teleport the client must confirm.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerPosition {
    pub teleport_id: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// From 768; before, absolute coordinates reset the velocity.
    pub velocity: (f64, f64, f64),
    pub yaw: f32,
    pub pitch: f32,
    /// Relative flags (a byte before 768).
    pub flags: i32,
}

impl Packet for PlayerPosition {
    const KIND: PacketKind = PacketKind::PlayerPosition;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        if ctx.features.player_position_velocity {
            Ok(Self {
                teleport_id: r.varint()?,
                x: r.f64()?,
                y: r.f64()?,
                z: r.f64()?,
                velocity: (r.f64()?, r.f64()?, r.f64()?),
                yaw: r.f32()?,
                pitch: r.f32()?,
                flags: r.i32()?,
            })
        } else {
            let (x, y, z) = (r.f64()?, r.f64()?, r.f64()?);
            let (yaw, pitch) = (r.f32()?, r.f32()?);
            let flags = i32::from(r.u8()?);
            Ok(Self {
                teleport_id: r.varint()?,
                x,
                y,
                z,
                velocity: (0.0, 0.0, 0.0),
                yaw,
                pitch,
                flags,
            })
        }
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        if ctx.features.player_position_velocity {
            out.put_varint(self.teleport_id);
        }
        out.put_f64(self.x);
        out.put_f64(self.y);
        out.put_f64(self.z);
        if ctx.features.player_position_velocity {
            out.put_f64(self.velocity.0);
            out.put_f64(self.velocity.1);
            out.put_f64(self.velocity.2);
        }
        out.put_f32(self.yaw);
        out.put_f32(self.pitch);
        if ctx.features.player_position_velocity {
            out.put_i32(self.flags);
        } else {
            out.put_u8(self.flags as u8);
            out.put_varint(self.teleport_id);
        }
        Ok(())
    }
}

/// One world clock in `set_time` (775+).
#[derive(Debug, Clone, PartialEq)]
pub struct ClockUpdate {
    /// ID in `minecraft:world_clock`.
    pub clock: i32,
    pub ticks: i64,
    pub partial_tick: f32,
    pub rate: f32,
}

/// `set_time`.
#[derive(Debug, Clone, PartialEq)]
pub struct SetTime {
    pub world_age: i64,
    /// Before 775.
    pub time_of_day: i64,
    /// 768 to 774: the time of day advances.
    pub ticking: bool,
    /// From 775.
    pub clocks: Vec<ClockUpdate>,
}

impl Packet for SetTime {
    const KIND: PacketKind = PacketKind::SetTime;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let f = ctx.features;
        let world_age = r.i64()?;
        if f.set_time_clocks {
            let n = r.count(256, 7, "clocks")?;
            let clocks = (0..n)
                .map(|_| {
                    Ok(ClockUpdate {
                        clock: r.varint()?,
                        ticks: r.varlong()?,
                        partial_tick: r.f32()?,
                        rate: r.f32()?,
                    })
                })
                .collect::<Result<_, DecodeError>>()?;
            return Ok(Self {
                world_age,
                time_of_day: 0,
                ticking: false,
                clocks,
            });
        }
        let time_of_day = r.i64()?;
        let ticking = if f.set_time_ticking_flag {
            r.bool()?
        } else {
            false
        };
        Ok(Self {
            world_age,
            time_of_day,
            ticking,
            clocks: Vec::new(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        let f = ctx.features;
        out.put_i64(self.world_age);
        if f.set_time_clocks {
            out.put_len(self.clocks.len(), 256, "clocks")?;
            for c in &self.clocks {
                out.put_varint(c.clock);
                out.put_varlong(c.ticks);
                out.put_f32(c.partial_tick);
                out.put_f32(c.rate);
            }
            return Ok(());
        }
        out.put_i64(self.time_of_day);
        if f.set_time_ticking_flag {
            out.put_bool(self.ticking);
        }
        Ok(())
    }
}

/// A decoration on a map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapIcon {
    pub kind: i32,
    pub x: i8,
    pub z: i8,
    pub direction: u8,
    pub name: Option<Text>,
}

/// Changed pixels of a map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapPatch {
    pub columns: u8,
    pub rows: u8,
    pub x: u8,
    pub z: u8,
    pub colors: Vec<u8>,
}

/// `map_item_data`; the same layout in 767–777.
#[derive(Debug, Clone, PartialEq)]
pub struct MapItemData {
    pub map_id: i32,
    pub scale: i8,
    pub locked: bool,
    pub icons: Option<Vec<MapIcon>>,
    pub patch: Option<MapPatch>,
}

impl Packet for MapItemData {
    const KIND: PacketKind = PacketKind::MapItemData;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let map_id = r.varint()?;
        let scale = r.i8()?;
        let locked = r.bool()?;
        let icons = r.option(|r| {
            let n = r.count(1024, 5, "map icons")?;
            (0..n)
                .map(|_| {
                    Ok(MapIcon {
                        kind: r.varint()?,
                        x: r.i8()?,
                        z: r.i8()?,
                        direction: r.u8()?,
                        name: r.option(|r| r.nbt_required(ctx.nbt))?,
                    })
                })
                .collect::<Result<Vec<_>, DecodeError>>()
        })?;
        let columns = r.u8()?;
        let patch = if columns == 0 {
            None
        } else {
            Some(MapPatch {
                columns,
                rows: r.u8()?,
                x: r.u8()?,
                z: r.u8()?,
                colors: r.byte_array(128 * 128)?.to_vec(),
            })
        };
        Ok(Self {
            map_id,
            scale,
            locked,
            icons,
            patch,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.map_id);
        out.put_i8(self.scale);
        out.put_bool(self.locked);
        out.put_bool(self.icons.is_some());
        if let Some(icons) = &self.icons {
            out.put_len(icons.len(), 1024, "map icons")?;
            for i in icons {
                out.put_varint(i.kind);
                out.put_i8(i.x);
                out.put_i8(i.z);
                out.put_u8(i.direction);
                out.put_bool(i.name.is_some());
                if let Some(n) = &i.name {
                    out.put_nbt(Some(n))?;
                }
            }
        }
        match &self.patch {
            None => out.put_u8(0),
            Some(p) => {
                out.put_u8(p.columns);
                out.put_u8(p.rows);
                out.put_u8(p.x);
                out.put_u8(p.z);
                out.put_byte_array(&p.colors, 128 * 128)?;
            }
        }
        Ok(())
    }
}

/// An item stack as it goes over the wire (1.20.5+): count, then item ID and
/// component patches. The components stay bytes: their layout depends on the
/// component type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemStack {
    pub count: i32,
    pub item: i32,
    /// Components added and removed, and their encoded data.
    pub added: i32,
    pub removed: i32,
    pub components: Vec<u8>,
}

impl ItemStack {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let count = r.varint()?;
        if count <= 0 {
            return Ok(Self::default());
        }
        Ok(Self {
            count,
            item: r.varint()?,
            added: r.varint()?,
            removed: r.varint()?,
            components: r.rest_max(MAX_STRING, "item components")?.to_vec(),
        })
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.put_varint(self.count.max(0));
        if self.count > 0 {
            out.put_varint(self.item);
            out.put_varint(self.added);
            out.put_varint(self.removed);
            out.put_bytes(&self.components);
        }
    }
}

/// `container_set_slot`. The container ID is a byte before 768 and a VarInt
/// after; both are one byte for the player's inventory (0) and other IDs
/// below 128.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSetSlot {
    pub container: i32,
    pub state_id: i32,
    pub slot: i16,
    pub item: ItemStack,
}

impl Packet for ContainerSetSlot {
    const KIND: PacketKind = PacketKind::ContainerSetSlot;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            container: i32::from(r.u8()?),
            state_id: r.varint()?,
            slot: r.i16()?,
            item: ItemStack::decode(r)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        if !(0..128).contains(&self.container) {
            return Err(EncodeError::Invalid("container ID over 127"));
        }
        out.put_u8(self.container as u8);
        out.put_varint(self.state_id);
        out.put_i16(self.slot);
        self.item.encode(out);
        Ok(())
    }
}

/// `set_held_slot`. A byte before 770 and a VarInt after: the same byte for
/// hotbar slots 0–8, the only values the proxy sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetHeldSlot {
    pub slot: u8,
}

impl Packet for SetHeldSlot {
    const KIND: PacketKind = PacketKind::SetHeldSlot;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        let slot = r.u8()?;
        if slot > 8 {
            return Err(DecodeError::Invalid("hotbar slot"));
        }
        Ok(Self { slot })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        if self.slot > 8 {
            return Err(EncodeError::Invalid("hotbar slot"));
        }
        out.put_u8(self.slot);
        Ok(())
    }
}

/// `set_experience`.
#[derive(Debug, Clone, PartialEq)]
pub struct SetExperience {
    pub bar: f32,
    pub level: i32,
    pub total: i32,
}

impl Packet for SetExperience {
    const KIND: PacketKind = PacketKind::SetExperience;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            bar: r.f32()?,
            level: r.varint()?,
            total: r.varint()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_f32(self.bar);
        out.put_varint(self.level);
        out.put_varint(self.total);
        Ok(())
    }
}

/// A sound by registry ID or by name.
#[derive(Debug, Clone, PartialEq)]
pub enum SoundRef {
    Id(i32),
    Named { name: String, range: Option<f32> },
}

/// `sound` at a position (coordinates × 8).
#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    pub sound: SoundRef,
    pub category: i32,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub volume: f32,
    pub pitch: f32,
    pub seed: i64,
}

impl Packet for Sound {
    const KIND: PacketKind = PacketKind::Sound;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        let id = r.varint()?;
        let sound = if id == 0 {
            SoundRef::Named {
                name: r.identifier()?,
                range: r.option(|r| r.f32())?,
            }
        } else {
            SoundRef::Id(id - 1)
        };
        Ok(Self {
            sound,
            category: r.varint()?,
            x: r.i32()?,
            y: r.i32()?,
            z: r.i32()?,
            volume: r.f32()?,
            pitch: r.f32()?,
            seed: r.i64()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        match &self.sound {
            SoundRef::Id(id) => out.put_varint(id.saturating_add(1)),
            SoundRef::Named { name, range } => {
                out.put_varint(0);
                out.put_identifier(name)?;
                out.put_bool(range.is_some());
                if let Some(r) = range {
                    out.put_f32(*r);
                }
            }
        }
        out.put_varint(self.category);
        out.put_i32(self.x);
        out.put_i32(self.y);
        out.put_i32(self.z);
        out.put_f32(self.volume);
        out.put_f32(self.pitch);
        out.put_i64(self.seed);
        Ok(())
    }
}

/// `respawn`: the spawn part of play `login` and the data kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Respawn {
    pub dimension_type: i32,
    pub dimension: String,
    pub hashed_seed: i64,
    pub game_mode: i32,
    pub previous_game_mode: Option<i32>,
    pub debug: bool,
    pub flat: bool,
    pub death_location: Option<super::play::DeathLocation>,
    pub portal_cooldown: i32,
    /// From 768.
    pub sea_level: i32,
    pub data_kept: u8,
}

impl Packet for Respawn {
    const KIND: PacketKind = PacketKind::Respawn;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let f = ctx.features;
        let dimension_type = r.varint()?;
        let dimension = r.identifier()?;
        let hashed_seed = r.i64()?;
        let (game_mode, previous_game_mode) = super::play::read_game_modes(r, f)?;
        Ok(Self {
            dimension_type,
            dimension,
            hashed_seed,
            game_mode,
            previous_game_mode,
            debug: r.bool()?,
            flat: r.bool()?,
            death_location: r.option(|r| {
                Ok(super::play::DeathLocation {
                    dimension: r.identifier()?,
                    position: r.position()?,
                })
            })?,
            portal_cooldown: r.varint()?,
            sea_level: if f.play_login_sea_level {
                r.varint()?
            } else {
                0
            },
            data_kept: r.u8()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        let f = ctx.features;
        out.put_varint(self.dimension_type);
        out.put_identifier(&self.dimension)?;
        out.put_i64(self.hashed_seed);
        super::play::put_game_modes(out, f, self.game_mode, self.previous_game_mode);
        out.put_bool(self.debug);
        out.put_bool(self.flat);
        out.put_bool(self.death_location.is_some());
        if let Some(d) = &self.death_location {
            out.put_identifier(&d.dimension)?;
            out.put_position(d.position);
        }
        out.put_varint(self.portal_cooldown);
        if f.play_login_sea_level {
            out.put_varint(self.sea_level);
        }
        out.put_u8(self.data_kept);
        Ok(())
    }
}

/// A chat session in `player_info_update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoChatSession {
    pub id: Uuid,
    pub expires_at: i64,
    pub key: Vec<u8>,
    pub signature: Vec<u8>,
}

/// One player in `player_info_update`; fields are present for the actions
/// of the packet.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InfoEntry {
    pub id: Uuid,
    /// Add player: name and properties.
    pub profile: Option<GameProfile>,
    pub chat: Option<InfoChatSession>,
    pub game_mode: i32,
    pub listed: bool,
    pub latency: i32,
    pub display_name: Option<Text>,
    pub list_order: i32,
    pub show_hat: bool,
}

/// `player_info_update`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfoUpdate {
    /// Action bits: 0x01 add, 0x02 chat session, 0x04 game mode, 0x08
    /// listed, 0x10 latency, 0x20 display name, 0x40 list order (768+),
    /// 0x80 show hat (769+).
    pub actions: u8,
    pub entries: Vec<InfoEntry>,
}

impl PlayerInfoUpdate {
    pub const ADD: u8 = 0x01;
    pub const CHAT: u8 = 0x02;
    pub const GAME_MODE: u8 = 0x04;
    pub const LISTED: u8 = 0x08;
    pub const LATENCY: u8 = 0x10;
    pub const DISPLAY_NAME: u8 = 0x20;
    pub const LIST_ORDER: u8 = 0x40;
    pub const HAT: u8 = 0x80;

    /// Actions this version knows.
    pub fn supported(ctx: &Ctx<'_>) -> u8 {
        let f = ctx.features;
        0x3F | if f.player_info_list_order {
            Self::LIST_ORDER
        } else {
            0
        } | if f.player_info_hat { Self::HAT } else { 0 }
    }
}

impl Packet for PlayerInfoUpdate {
    const KIND: PacketKind = PacketKind::PlayerInfoUpdate;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let actions = r.u8()?;
        if actions & !Self::supported(ctx) != 0 {
            return Err(DecodeError::Invalid("player info action"));
        }
        let n = r.count(4096, 16, "player info entries")?;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let mut e = InfoEntry {
                id: r.uuid()?,
                ..InfoEntry::default()
            };
            if actions & Self::ADD != 0 {
                let name = r.string(16)?;
                let properties = GameProfile::decode_properties(r)?;
                e.profile = Some(GameProfile {
                    id: e.id,
                    name,
                    properties,
                });
            }
            if actions & Self::CHAT != 0 {
                e.chat = r.option(|r| {
                    Ok(InfoChatSession {
                        id: r.uuid()?,
                        expires_at: r.i64()?,
                        key: r.byte_array(512)?.to_vec(),
                        signature: r.byte_array(4096)?.to_vec(),
                    })
                })?;
            }
            if actions & Self::GAME_MODE != 0 {
                e.game_mode = r.varint()?;
            }
            if actions & Self::LISTED != 0 {
                e.listed = r.bool()?;
            }
            if actions & Self::LATENCY != 0 {
                e.latency = r.varint()?;
            }
            if actions & Self::DISPLAY_NAME != 0 {
                e.display_name = r.option(|r| r.nbt_required(ctx.nbt))?;
            }
            if actions & Self::LIST_ORDER != 0 {
                e.list_order = r.varint()?;
            }
            if actions & Self::HAT != 0 {
                e.show_hat = r.bool()?;
            }
            entries.push(e);
        }
        Ok(Self { actions, entries })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        let actions = self.actions & Self::supported(ctx);
        out.put_u8(actions);
        out.put_len(self.entries.len(), 4096, "player info entries")?;
        for e in &self.entries {
            out.put_uuid(&e.id);
            if actions & Self::ADD != 0 {
                let p = e
                    .profile
                    .as_ref()
                    .ok_or(EncodeError::Invalid("add player without a profile"))?;
                out.put_string(&p.name, 16)?;
                p.encode_properties(out)?;
            }
            if actions & Self::CHAT != 0 {
                out.put_bool(e.chat.is_some());
                if let Some(c) = &e.chat {
                    out.put_uuid(&c.id);
                    out.put_i64(c.expires_at);
                    out.put_byte_array(&c.key, 512)?;
                    out.put_byte_array(&c.signature, 4096)?;
                }
            }
            if actions & Self::GAME_MODE != 0 {
                out.put_varint(e.game_mode);
            }
            if actions & Self::LISTED != 0 {
                out.put_bool(e.listed);
            }
            if actions & Self::LATENCY != 0 {
                out.put_varint(e.latency);
            }
            if actions & Self::DISPLAY_NAME != 0 {
                out.put_bool(e.display_name.is_some());
                if let Some(t) = &e.display_name {
                    out.put_nbt(Some(t))?;
                }
            }
            if actions & Self::LIST_ORDER != 0 {
                out.put_varint(e.list_order);
            }
            if actions & Self::HAT != 0 {
                out.put_bool(e.show_hat);
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------ serverbound

/// `accept_teleportation`; from 777 with the position and rotation the
/// client took.
#[derive(Debug, Clone, PartialEq)]
pub struct AcceptTeleportation {
    pub id: i32,
    /// x, y, z, yaw, pitch (777+).
    pub at: Option<(f64, f64, f64, f32, f32)>,
}

impl Packet for AcceptTeleportation {
    const KIND: PacketKind = PacketKind::AcceptTeleportation;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let id = r.varint()?;
        let at = if ctx.features.accept_teleport_position {
            Some((r.f64()?, r.f64()?, r.f64()?, r.f32()?, r.f32()?))
        } else {
            None
        };
        Ok(Self { id, at })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.id);
        if ctx.features.accept_teleport_position {
            let (x, y, z, yaw, pitch) = self.at.unwrap_or_default();
            out.put_f64(x);
            out.put_f64(y);
            out.put_f64(z);
            out.put_f32(yaw);
            out.put_f32(pitch);
        }
        Ok(())
    }
}

/// Movement flags: bit 0 on ground; from 768 bit 1 horizontal collision
/// (before, the same byte is the on-ground boolean).
pub const MOVE_ON_GROUND: u8 = 0x01;

/// `move_player_pos`, `move_player_pos_rot`, `move_player_rot` and
/// `move_player_status_only` in one shape.
#[derive(Debug, Clone, PartialEq)]
pub struct MovePlayer {
    pub position: Option<(f64, f64, f64)>,
    pub rotation: Option<(f32, f32)>,
    pub flags: u8,
}

impl MovePlayer {
    /// The packet kind for the fields present.
    pub fn kind(&self) -> PacketKind {
        match (self.position.is_some(), self.rotation.is_some()) {
            (true, true) => PacketKind::MovePlayerPosRot,
            (true, false) => PacketKind::MovePlayerPos,
            (false, true) => PacketKind::MovePlayerRot,
            (false, false) => PacketKind::MovePlayerStatusOnly,
        }
    }

    /// Decodes a payload of one of the four kinds.
    pub fn decode_kind(kind: PacketKind, payload: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(payload);
        let position = match kind {
            PacketKind::MovePlayerPos | PacketKind::MovePlayerPosRot => {
                Some((r.f64()?, r.f64()?, r.f64()?))
            }
            PacketKind::MovePlayerRot | PacketKind::MovePlayerStatusOnly => None,
            _ => return Err(DecodeError::Invalid("not a movement packet")),
        };
        let rotation = match kind {
            PacketKind::MovePlayerPosRot | PacketKind::MovePlayerRot => Some((r.f32()?, r.f32()?)),
            _ => None,
        };
        let flags = r.u8()?;
        r.finish()?;
        Ok(Self {
            position,
            rotation,
            flags,
        })
    }

    pub fn encode_body(&self, out: &mut Vec<u8>) {
        if let Some((x, y, z)) = self.position {
            out.put_f64(x);
            out.put_f64(y);
            out.put_f64(z);
        }
        if let Some((yaw, pitch)) = self.rotation {
            out.put_f32(yaw);
            out.put_f32(pitch);
        }
        out.put_u8(self.flags);
    }

    pub fn on_ground(&self) -> bool {
        self.flags & MOVE_ON_GROUND != 0
    }
}

macro_rules! move_packet {
    ($name:ident, $kind:ident) => {
        /// One movement packet kind; see [`MovePlayer`].
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name(pub MovePlayer);

        impl Packet for $name {
            const KIND: PacketKind = PacketKind::$kind;
            fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
                let m = MovePlayer::decode_kind(Self::KIND, r.rest())?;
                Ok(Self(m))
            }
            fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
                if self.0.kind() != Self::KIND {
                    return Err(EncodeError::Invalid("movement fields for another kind"));
                }
                self.0.encode_body(out);
                Ok(())
            }
        }
    };
}

move_packet!(MovePlayerPos, MovePlayerPos);
move_packet!(MovePlayerPosRot, MovePlayerPosRot);
move_packet!(MovePlayerRot, MovePlayerRot);
move_packet!(MovePlayerStatusOnly, MovePlayerStatusOnly);

empty_packet!(
    /// `player_loaded` (769+): the client shows the world.
    PlayerLoaded,
    PlayerLoaded
);
empty_packet!(
    /// `client_tick_end` (768+).
    ClientTickEnd,
    ClientTickEnd
);

/// `chunk_batch_received`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkBatchReceived {
    pub chunks_per_tick: f32,
}

impl Packet for ChunkBatchReceived {
    const KIND: PacketKind = PacketKind::ChunkBatchReceived;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            chunks_per_tick: r.f32()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_f32(self.chunks_per_tick);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::round_trip;
    use super::*;

    #[test]
    fn round_trips() {
        for v in [767, 768, 770, 773, 775, 777] {
            round_trip(
                &PlayerPosition {
                    teleport_id: 7,
                    x: 0.5,
                    y: 65.0,
                    z: -0.5,
                    velocity: (0.0, 0.0, 0.0),
                    yaw: 90.0,
                    pitch: 0.0,
                    flags: 0,
                },
                v,
            );
            round_trip(
                &SetDefaultSpawnPosition {
                    dimension: if v >= 773 {
                        "pumbo:void".into()
                    } else {
                        String::new()
                    },
                    position: Position { x: 0, y: 64, z: 0 },
                    yaw: 0.0,
                    pitch: 0.0,
                },
                v,
            );
            round_trip(
                &SetTime {
                    world_age: 10,
                    time_of_day: if v < 775 { 6000 } else { 0 },
                    ticking: (768..775).contains(&v),
                    clocks: if v >= 775 {
                        vec![ClockUpdate {
                            clock: 0,
                            ticks: 6000,
                            partial_tick: 0.0,
                            rate: 0.0,
                        }]
                    } else {
                        Vec::new()
                    },
                },
                v,
            );
            round_trip(
                &LevelChunkWithLight {
                    x: 1,
                    z: -2,
                    heightmaps: if v >= 770 {
                        Heightmaps::List(vec![(4, vec![0; 37])])
                    } else {
                        Heightmaps::Nbt(pumbo_nbt::Tag::Compound(pumbo_nbt::Compound::new()))
                    },
                    sections: vec![1, 2, 3],
                    block_entities: Vec::new(),
                    light: LightData {
                        sky_mask: vec![0b110],
                        block_mask: Vec::new(),
                        empty_sky_mask: vec![1],
                        empty_block_mask: vec![0x0003_0000_0000],
                        sky: vec![vec![0xFF; 2048], vec![0; 2048]],
                        block: Vec::new(),
                    },
                },
                v,
            );
            round_trip(
                &PlayerInfoUpdate {
                    actions: PlayerInfoUpdate::ADD
                        | PlayerInfoUpdate::LISTED
                        | PlayerInfoUpdate::GAME_MODE,
                    entries: vec![InfoEntry {
                        id: Uuid::from_u128(5),
                        profile: Some(GameProfile {
                            id: Uuid::from_u128(5),
                            name: "Pumbo".into(),
                            properties: Vec::new(),
                        }),
                        game_mode: 2,
                        listed: true,
                        ..InfoEntry::default()
                    }],
                },
                v,
            );
        }
        round_trip(
            &MovePlayerPosRot(MovePlayer {
                position: Some((1.0, 2.0, 3.0)),
                rotation: Some((4.0, 5.0)),
                flags: MOVE_ON_GROUND,
            }),
            777,
        );
        round_trip(
            &MapItemData {
                map_id: 3,
                scale: 0,
                locked: true,
                icons: None,
                patch: Some(MapPatch {
                    columns: 128,
                    rows: 128,
                    x: 0,
                    z: 0,
                    colors: vec![34; 128 * 128],
                }),
            },
            767,
        );
    }

    #[test]
    fn bitset_bytes_trim_zeros() {
        use crate::Direction;
        use crate::packets::test_support::TestVersion;
        let v = TestVersion(777, Vec::new());
        let ctx = Ctx::new(&v, Direction::Clientbound);
        let mut out = Vec::new();
        put_bitset(&mut out, &[0x0601, 0], &ctx).unwrap();
        // Pumpkin's test vector for 26.3: length, then little-endian bytes.
        assert_eq!(out, vec![2, 0x01, 0x06]);
        assert_eq!(bitset(&mut Reader::new(&out), &ctx).unwrap(), vec![0x0601]);
    }
}
