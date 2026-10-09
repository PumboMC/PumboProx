//! Blocks of a virtual world and their chunks per protocol (plan §5.3).
//!
//! Blocks are kept by state name (`minecraft:stone_slab[type=top]`), since
//! state IDs differ per version; a chunk becomes a packet for a version on
//! first use and stays cached as bytes (1000 players in one world share one
//! copy). Empty chunks are one template per version with the coordinates
//! patched in.
//!
//! Section formats (checked against the vanilla recordings by
//! `tests/sections.rs`): up to 769 the paletted containers carry their data
//! length; from 770 they do not; from 775 a section also counts its fluid
//! blocks. From 770 heightmaps are a list instead of NBT.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use pumbo_nbt::{Compound, Tag};
use pumbo_protocol::packets::world::{Heightmaps, LevelChunkWithLight, LightData};
use pumbo_protocol::packets::{self, Ctx};
use pumbo_protocol::types::WriteExt;
use pumbo_protocol::{Direction, VersionFeatures, VersionModule};

use crate::Error;

/// Height of the void dimension (blocks 0..256, 16 sections).
pub const HEIGHT: i32 = 256;
const SECTIONS: usize = (HEIGHT / 16) as usize;
const AIR: &str = "minecraft:air";
/// Distinct block states in one section (indirect palette up to 8 bits).
const MAX_PALETTE: usize = 256;
/// Blocks in a world; plenty for platforms and small structures.
const MAX_BLOCKS: usize = 1 << 20;

/// Game mode in the virtual world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GameMode {
    Survival,
    Creative,
    #[default]
    Adventure,
    Spectator,
}

impl GameMode {
    pub fn id(self) -> i32 {
        match self {
            GameMode::Survival => 0,
            GameMode::Creative => 1,
            GameMode::Adventure => 2,
            GameMode::Spectator => 3,
        }
    }
}

/// How a world looks (WIT `world-options`). The dimension is always the
/// void; `time` is the fixed time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldOptions {
    pub time: i64,
    /// Sky light 0–15; 15 needs no light data (the client assumes full sky
    /// light where none is sent).
    pub light: u8,
    pub game_mode: GameMode,
    /// Chunk radius sent around the player (and the view distance in `login`).
    pub view_distance: u8,
}

impl Default for WorldOptions {
    fn default() -> Self {
        Self {
            time: 6000,
            light: 15,
            game_mode: GameMode::Adventure,
            view_distance: 2,
        }
    }
}

/// A block position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// `name` and `[key=value,...]` of a state string.
type State<'a> = (&'a str, Vec<(&'a str, &'a str)>);

fn parse_state(s: &str) -> Result<State<'_>, Error> {
    let (name, props) = match s.split_once('[') {
        Some((n, rest)) => (
            n,
            rest.strip_suffix(']')
                .ok_or_else(|| Error::Block(s.to_string()))?,
        ),
        None => (s, ""),
    };
    let props = props
        .split(',')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').ok_or_else(|| Error::Block(s.to_string())))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((name, props))
}

/// State ID of a state string in this version.
pub fn state_id(module: &dyn VersionModule, state: &str) -> Result<u32, Error> {
    let tables = pumbo_data::tables(module.protocol())?;
    let (name, props) = parse_state(state)?;
    tables
        .block_state(name, &props)
        .ok_or_else(|| Error::Block(state.to_string()))
}

#[derive(Debug, Default)]
struct Inner {
    /// (chunk x, chunk z) → position → state.
    chunks: BTreeMap<(i32, i32), BTreeMap<BlockPos, Arc<str>>>,
    count: usize,
    /// (protocol, chunk x, chunk z) → `level_chunk_with_light` payload.
    encoded: HashMap<(i32, i32, i32), Bytes>,
    /// Protocol → payload of an empty chunk at 0, 0.
    empty: HashMap<i32, Bytes>,
}

/// A virtual world; shared by every player in it.
#[derive(Debug)]
pub struct World {
    pub options: WorldOptions,
    inner: Mutex<Inner>,
}

impl World {
    pub fn new(options: WorldOptions) -> Self {
        Self {
            options,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sets a block by state name (`minecraft:air` removes it). The name is
    /// checked against the newest protocol; a version without the block shows
    /// air there. Players already in the world do not see the change
    /// (ponytail: live block updates when a plugin needs them).
    pub fn set_block(&self, pos: BlockPos, state: &str) -> Result<(), Error> {
        if !(0..HEIGHT).contains(&pos.y) {
            return Err(Error::OutOfWorld(pos.y));
        }
        let newest = pumbo_data::protocols()
            .last()
            .ok_or(Error::NoData("protocol tables"))?;
        let tables = pumbo_data::tables(newest)?;
        let (name, props) = parse_state(state)?;
        tables
            .block_state(name, &props)
            .ok_or_else(|| Error::Block(state.to_string()))?;
        let mut w = self.lock();
        let key = (pos.x >> 4, pos.z >> 4);
        if name == AIR || name == "air" {
            if let Some(c) = w.chunks.get_mut(&key)
                && c.remove(&pos).is_some()
            {
                w.count -= 1;
            }
        } else {
            if w.count >= MAX_BLOCKS {
                return Err(Error::TooManyBlocks);
            }
            if w.chunks
                .entry(key)
                .or_default()
                .insert(pos, state.into())
                .is_none()
            {
                w.count += 1;
            }
        }
        w.encoded.retain(|(_, x, z), _| (*x, *z) != key);
        Ok(())
    }

    /// Places a structure file (`.nbt` as saved by structure blocks, gzip or
    /// plain) with its origin at `at`.
    pub fn load_structure(&self, data: &[u8], at: BlockPos) -> Result<usize, Error> {
        let plain = if data.starts_with(&[0x1F, 0x8B]) {
            use std::io::Read as _;
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(data)
                .take(16 << 20)
                .read_to_end(&mut out)
                .map_err(|e| Error::Structure(e.to_string()))?;
            out
        } else {
            data.to_vec()
        };
        let (_, root) = pumbo_nbt::read_named(&mut plain.as_slice(), pumbo_nbt::Limits::BACKEND)
            .map_err(|e| Error::Structure(e.to_string()))?;
        let root = root.as_compound().ok_or(Error::Structure("root".into()))?;
        let list = |key: &str| match root.get(key) {
            Some(Tag::List(l)) => Ok(&l.items),
            _ => Err(Error::Structure(format!("no {key}"))),
        };
        let palette: Vec<String> = list("palette")?
            .iter()
            .map(|p| {
                let c = p.as_compound().ok_or(Error::Structure("palette".into()))?;
                let name = c
                    .get("Name")
                    .and_then(Tag::as_str)
                    .ok_or(Error::Structure("palette name".into()))?;
                let props: Vec<String> = match c.get("Properties").and_then(Tag::as_compound) {
                    Some(p) => p
                        .iter()
                        .filter_map(|(k, v)| Some(format!("{k}={}", v.as_str()?)))
                        .collect(),
                    None => Vec::new(),
                };
                Ok(if props.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}[{}]", props.join(","))
                })
            })
            .collect::<Result<_, Error>>()?;
        let mut placed = 0;
        for b in list("blocks")? {
            let c = b.as_compound().ok_or(Error::Structure("block".into()))?;
            let (Some(Tag::List(pos)), Some(Tag::Int(state))) = (c.get("pos"), c.get("state"))
            else {
                return Err(Error::Structure("block fields".into()));
            };
            let coord = |i: usize| match pos.items.get(i) {
                Some(Tag::Int(v)) => Ok(*v),
                _ => Err(Error::Structure("block position".into())),
            };
            let state = palette
                .get(usize::try_from(*state).map_err(|_| Error::Structure("state".into()))?)
                .ok_or(Error::Structure("state index".into()))?;
            let p = BlockPos {
                x: at.x.saturating_add(coord(0)?),
                y: at.y.saturating_add(coord(1)?),
                z: at.z.saturating_add(coord(2)?),
            };
            self.set_block(p, state)?;
            placed += 1;
        }
        Ok(placed)
    }

    /// A square of `state` with corners `a`, `b` at height `y`.
    pub fn fill_layer(
        &self,
        a: (i32, i32),
        b: (i32, i32),
        y: i32,
        state: &str,
    ) -> Result<(), Error> {
        for x in a.0.min(b.0)..=a.0.max(b.0) {
            for z in a.1.min(b.1)..=a.1.max(b.1) {
                self.set_block(BlockPos { x, y, z }, state)?;
            }
        }
        Ok(())
    }

    /// `level_chunk_with_light` payload of one chunk for this version.
    pub fn chunk(
        &self,
        module: &dyn VersionModule,
        biome: i32,
        cx: i32,
        cz: i32,
    ) -> Result<Bytes, Error> {
        let p = module.protocol().0;
        let mut w = self.lock();
        if let Some(b) = w.encoded.get(&(p, cx, cz)) {
            return Ok(b.clone());
        }
        let Some(blocks) = w.chunks.get(&(cx, cz)).filter(|b| !b.is_empty()) else {
            let empty = match w.empty.get(&p) {
                Some(e) => e.clone(),
                None => {
                    let e = Bytes::from(self.encode(module, biome, 0, 0, &BTreeMap::new())?);
                    w.empty.insert(p, e.clone());
                    e
                }
            };
            // The payload starts with the chunk x and z (two big-endian ints).
            let mut out = empty.to_vec();
            let xz = [cx.to_be_bytes(), cz.to_be_bytes()].concat();
            for (b, v) in out.iter_mut().zip(xz) {
                *b = v;
            }
            return Ok(Bytes::from(out));
        };
        let encoded = Bytes::from(self.encode(module, biome, cx, cz, blocks)?);
        w.encoded.insert((p, cx, cz), encoded.clone());
        Ok(encoded)
    }

    fn encode(
        &self,
        module: &dyn VersionModule,
        biome: i32,
        cx: i32,
        cz: i32,
        blocks: &BTreeMap<BlockPos, Arc<str>>,
    ) -> Result<Vec<u8>, Error> {
        let f = module.features();
        // States of this version, by section.
        let mut ids: HashMap<&str, u32> = HashMap::new();
        let mut grid: Vec<Vec<u32>> = vec![Vec::new(); SECTIONS];
        let mut tops = [0i32; 256];
        let air = state_id(module, AIR)?;
        for (pos, state) in blocks {
            let id = match ids.get(&**state) {
                Some(id) => *id,
                // A block this version lacks stays air.
                None => {
                    let id = state_id(module, state).unwrap_or(air);
                    ids.insert(state, id);
                    id
                }
            };
            let (Ok(s), Ok(lx), Ok(lz)) = (
                usize::try_from(pos.y >> 4),
                usize::try_from(pos.x & 15),
                usize::try_from(pos.z & 15),
            ) else {
                continue;
            };
            let Some(section) = grid.get_mut(s) else {
                continue;
            };
            if section.is_empty() {
                section.resize(4096, air);
            }
            let ly = usize::try_from(pos.y & 15).unwrap_or(0);
            if let Some(cell) = section.get_mut(ly * 256 + lz * 16 + lx) {
                *cell = id;
            }
            if id != air
                && let Some(t) = tops.get_mut(lz * 16 + lx)
            {
                *t = (*t).max(pos.y + 1);
            }
        }
        let fluids: Vec<u32> = ["minecraft:water", "minecraft:lava"]
            .iter()
            .filter_map(|n| pumbo_data::tables(module.protocol()).ok()?.block(n))
            .flat_map(|b| b.first_state..b.first_state + b.state_count())
            .collect();
        let mut sections = Vec::new();
        for section in &grid {
            write_section(&mut sections, f, section, air, biome, &fluids)?;
        }
        let heights = pack_heightmap(&tops);
        let heightmaps = if f.chunk_heightmaps_list {
            // WORLD_SURFACE, MOTION_BLOCKING, MOTION_BLOCKING_NO_LEAVES (as vanilla).
            Heightmaps::List(vec![
                (1, heights.clone()),
                (4, heights.clone()),
                (5, heights),
            ])
        } else {
            let mut c = Compound::new();
            c.insert("MOTION_BLOCKING", Tag::LongArray(heights.clone()));
            c.insert("WORLD_SURFACE", Tag::LongArray(heights));
            Heightmaps::Nbt(Tag::Compound(c))
        };
        let light = light(self.options.light);
        let packet = LevelChunkWithLight {
            x: cx,
            z: cz,
            heightmaps,
            sections,
            block_entities: Vec::new(),
            light,
        };
        Ok(packets::encode(
            &packet,
            &Ctx::new(module, Direction::Clientbound),
        )?)
    }
}

/// Sky light: none sent for 15 (the client assumes full sky light where no
/// section has data), otherwise every section at `level`.
fn light(level: u8) -> LightData {
    if level >= 15 {
        return LightData::default();
    }
    let n = level & 15;
    let all = ((1i64 << SECTIONS) - 1) << 1;
    LightData {
        sky_mask: vec![all],
        block_mask: Vec::new(),
        // The sections below and above the world.
        empty_sky_mask: vec![1 | (1i64 << (SECTIONS + 1))],
        empty_block_mask: Vec::new(),
        sky: vec![vec![n << 4 | n; 2048]; SECTIONS],
        block: Vec::new(),
    }
}

/// 256 heights (one per column) packed without spanning longs.
fn pack_heightmap(tops: &[i32; 256]) -> Vec<i64> {
    // ceil(log2(HEIGHT + 1)) bits per column.
    let bits = (u32::BITS - (HEIGHT as u32).leading_zeros()) as usize;
    pack(
        tops.iter().map(|t| u64::try_from(*t).unwrap_or(0)),
        bits,
        256,
    )
}

fn pack(values: impl Iterator<Item = u64>, bits: usize, count: usize) -> Vec<i64> {
    let per = 64 / bits.max(1);
    let mut out = vec![0i64; count.div_ceil(per)];
    for (i, v) in values.enumerate() {
        if let Some(l) = out.get_mut(i / per) {
            *l |= ((v & ((1 << bits) - 1)) << ((i % per) * bits)) as i64;
        }
    }
    out
}

/// One chunk section: counts, blocks (4096 state IDs, or none for air) and
/// the biome (one per section).
pub fn write_section(
    out: &mut Vec<u8>,
    f: VersionFeatures,
    blocks: &[u32],
    air: u32,
    biome: i32,
    fluids: &[u32],
) -> Result<(), Error> {
    let solid = blocks.iter().filter(|b| **b != air).count();
    out.put_i16(i16::try_from(solid).unwrap_or(i16::MAX));
    if f.chunk_section_fluid_count {
        let fluid = blocks.iter().filter(|b| fluids.contains(b)).count();
        out.put_i16(i16::try_from(fluid).unwrap_or(i16::MAX));
    }
    let mut palette: Vec<u32> = Vec::new();
    for b in blocks {
        if !palette.contains(b) {
            palette.push(*b);
        }
    }
    if palette.len() <= 1 {
        single(out, f, palette.first().copied().unwrap_or(air))?;
    } else {
        if palette.len() > MAX_PALETTE {
            return Err(Error::TooManyStates);
        }
        // At least 4 bits for blocks, as vanilla.
        let bits = (usize::BITS - (palette.len() - 1).leading_zeros()).max(4) as usize;
        out.put_u8(bits as u8);
        out.put_len(palette.len(), MAX_PALETTE, "palette")?;
        for p in &palette {
            out.put_varint(i32::try_from(*p).unwrap_or(0));
        }
        let data = pack(
            blocks
                .iter()
                .map(|b| palette.iter().position(|p| p == b).unwrap_or(0) as u64),
            bits,
            4096,
        );
        if !f.chunk_data_unsized {
            out.put_len(data.len(), 4096, "section data")?;
        }
        for l in data {
            out.put_i64(l);
        }
    }
    single(out, f, u32::try_from(biome).unwrap_or(0))
}

fn single(out: &mut Vec<u8>, f: VersionFeatures, value: u32) -> Result<(), Error> {
    out.put_u8(0);
    out.put_varint(i32::try_from(value).unwrap_or(0));
    if !f.chunk_data_unsized {
        out.put_varint(0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_and_layers() {
        let w = World::new(WorldOptions::default());
        w.fill_layer((-2, -2), (2, 2), 64, "minecraft:smooth_stone")
            .unwrap();
        w.set_block(
            BlockPos { x: 0, y: 65, z: 0 },
            "minecraft:oak_slab[type=top]",
        )
        .unwrap();
        assert!(
            w.set_block(BlockPos { x: 0, y: 65, z: 0 }, "minecraft:nope")
                .is_err()
        );
        assert!(
            w.set_block(BlockPos { x: 0, y: 300, z: 0 }, "minecraft:stone")
                .is_err()
        );
        for v in pumbo_data::protocols() {
            let m = pumbo_data::DataVersion::new(pumbo_data::tables(v).unwrap());
            for (cx, cz) in [(0, 0), (-1, -1), (5, 5)] {
                let payload = w.chunk(&m, 0, cx, cz).unwrap();
                let c: LevelChunkWithLight =
                    packets::decode(&payload, &Ctx::new(&m, Direction::Clientbound)).unwrap();
                assert_eq!((c.x, c.z), (cx, cz));
            }
        }
        assert_eq!(pack_heightmap(&[0; 256]).len(), 37);
    }
}
