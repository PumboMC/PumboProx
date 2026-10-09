//! Chunks, light and block changes: `level_chunk_with_light`, `light_update`,
//! `chunks_biomes`, `block_update`, `section_blocks_update`,
//! `block_entity_data`, `block_event`, `level_event`.

use pumbo_nbt::{Compound, Limits, List, Tag};
use pumbo_text::{Component, TextFormat};

use crate::remap::ceil_log2;
use crate::version::{V1_21_5, V26_3, eras};
use crate::wire::{Get, Put, copy, var_int_len};
use crate::{Ctx, Tables, nbt};

eras! {
    enum ChunkFormat {
        /// NBT heightmaps.
        V1_21 = V1_21,
        /// Heightmap list; section buffer padded as if long arrays were length-prefixed.
        V1_21_5 = V1_21_5,
        /// Padding dropped.
        V1_21_6 = V1_21_6,
        /// Fluid count per section; same as the server.
        V26_1 = V26_1,
    }
}

eras! {
    enum PaletteFormat {
        /// Long arrays are length-prefixed.
        V1_21 = V1_21,
        /// No prefix; the client derives the length from the bits.
        V1_21_5 = V1_21_5,
    }
}

eras! {
    enum LightFormat {
        /// Masks as long arrays.
        V1_21 = V1_21,
        /// Masks as byte arrays; same as the server.
        V26_3 = V26_3,
    }
}

/// What a paletted container holds, and the client's global palette.
struct Container<'a> {
    /// 4096 block states or 64 biomes.
    entries: usize,
    /// Above this the palette is the global registry.
    max_indirect_bits: u8,
    /// Server's global palette width.
    server_bits: u8,
    /// Client's global palette width.
    client_bits: u8,
    remap: &'a dyn Fn(i32) -> i32,
}

const fn long_count(entries: usize, bits: u8) -> usize {
    if bits == 0 {
        return 0;
    }
    entries.div_ceil(64 / bits as usize)
}

fn write_longs(out: &mut Vec<u8>, longs: &[i64], format: PaletteFormat) {
    if format == PaletteFormat::V1_21 {
        out.put_len(longs.len());
    }
    for &l in longs {
        out.put_i64(l);
    }
}

/// One server container in the client's format. Returns the long count written.
fn container(
    r: &mut &[u8],
    out: &mut Vec<u8>,
    c: &Container<'_>,
    format: PaletteFormat,
) -> Option<usize> {
    let bits = r.get_u8()?;
    if bits == 0 {
        let id = r.get_var_int()?;
        out.put_u8(0);
        out.put_var_int((c.remap)(id));
        write_longs(out, &[], format);
        return Some(0);
    }
    let indirect = bits <= c.max_indirect_bits;
    let palette = if indirect {
        let n = r.get_len()?;
        let mut p = Vec::with_capacity(n.min(256));
        for _ in 0..n {
            p.push(r.get_var_int()?);
        }
        Some(p)
    } else {
        None
    };
    let n = long_count(c.entries, bits);
    let mut longs = Vec::with_capacity(n);
    for _ in 0..n {
        longs.push(r.get_i64()?);
    }
    if let Some(palette) = palette {
        // Indices into the palette stay as they are.
        out.put_u8(bits);
        out.put_len(palette.len());
        for id in palette {
            out.put_var_int((c.remap)(id));
        }
        write_longs(out, &longs, format);
        return Some(longs.len());
    }
    if bits != c.server_bits {
        return None;
    }
    let repacked = repack(&longs, c.entries, bits, c.client_bits, c.remap);
    out.put_u8(c.client_bits);
    write_longs(out, &repacked, format);
    Some(repacked.len())
}

/// Unpacks `entries` values of `from` bits, maps them and packs them with `to` bits.
fn repack(longs: &[i64], entries: usize, from: u8, to: u8, map: &dyn Fn(i32) -> i32) -> Vec<i64> {
    let from_per_long = 64 / usize::from(from.max(1));
    let to_per_long = 64 / usize::from(to.max(1));
    let from_mask = (1u64 << from) - 1;
    let to_mask = (1u64 << to) - 1;
    let mut out = vec![0i64; long_count(entries, to)];
    for index in 0..entries {
        let word = longs.get(index / from_per_long).copied().unwrap_or(0) as u64;
        let value = (word >> ((index % from_per_long) * usize::from(from))) & from_mask;
        let mapped = (map(value as i32) as u64) & to_mask;
        if let Some(slot) = out.get_mut(index / to_per_long) {
            *slot |= (mapped << ((index % to_per_long) * usize::from(to))) as i64;
        }
    }
    out
}

/// Server heightmap types the older NBT form names, in vanilla order.
const NBT_HEIGHTMAPS: &[(i32, &str)] = &[(4, "MOTION_BLOCKING"), (1, "WORLD_SURFACE")];

fn heightmaps(r: &mut &[u8], format: ChunkFormat, out: &mut Vec<u8>) -> Option<()> {
    let n = r.get_len()?;
    let mut maps = Vec::with_capacity(n.min(8));
    for _ in 0..n {
        let kind = r.get_var_int()?;
        let len = r.get_len()?;
        let longs = r.bytes(len.checked_mul(8)?)?;
        maps.push((kind, len, longs));
    }
    if format != ChunkFormat::V1_21 {
        out.put_len(maps.len());
        for (kind, len, longs) in maps {
            out.put_var_int(kind);
            out.put_len(len);
            out.put_slice(longs);
        }
        return Some(());
    }
    // Nameless compound of long arrays.
    out.put_u8(10);
    for (kind, name) in NBT_HEIGHTMAPS {
        let Some((_, len, longs)) = maps.iter().find(|(k, _, _)| k == kind) else {
            continue;
        };
        out.put_u8(12);
        out.put_i16(i16::try_from(name.len()).ok()?);
        out.put_slice(name.as_bytes());
        out.put_i32(i32::try_from(*len).ok()?);
        out.put_slice(longs);
    }
    out.put_u8(0);
    Some(())
}

fn sections(ctx: &mut Ctx<'_>, mut r: &[u8], format: ChunkFormat) -> Option<Vec<u8>> {
    let client = ctx.client();
    let palette = PaletteFormat::of(client);
    let biome_count = ctx
        .t
        .client
        .synced
        .registries
        .iter()
        .find(|(n, _)| n == "worldgen/biome")
        .map_or(64, |(_, e)| e.len());
    let server_biomes = ctx
        .s
        .server_registries
        .get("worldgen/biome")
        .map_or(64, Vec::len);
    let biome_map = ctx.dynamic("worldgen/biome").cloned();
    let t = ctx.t;
    let block_remap = |id: i32| t.block_state(id);
    let biome_remap = |id: i32| biome_map.as_ref().map_or(id, |m| m.or(id, 0));
    let blocks = Container {
        entries: 4096,
        max_indirect_bits: 8,
        server_bits: ceil_log2(u32::try_from(t.block_states.len()).ok()?),
        client_bits: t.block_bits,
        remap: &block_remap,
    };
    let biomes = Container {
        entries: 64,
        max_indirect_bits: 3,
        server_bits: ceil_log2(u32::try_from(server_biomes).ok()?),
        client_bits: ceil_log2(u32::try_from(biome_count).ok()?),
        remap: &biome_remap,
    };
    let mut out = Vec::with_capacity(r.len() + 64);
    let mut prefix_bytes = 0;
    while !r.is_empty() {
        out.put_i16(r.get_i16()?);
        let fluids = r.get_i16()?;
        if format == ChunkFormat::V26_1 {
            out.put_i16(fluids);
        }
        let b = container(&mut r, &mut out, &blocks, palette)?;
        let m = container(&mut r, &mut out, &biomes, palette)?;
        prefix_bytes += var_int_len(i32::try_from(b).ok()?) + var_int_len(i32::try_from(m).ok()?);
    }
    if format == ChunkFormat::V1_21_5 {
        out.resize(out.len() + prefix_bytes, 0);
    }
    Some(out)
}

/// Light masks: a server bit set (bytes) as the client's (longs before 26.3).
fn light_data(client: i32, mut r: &[u8], out: &mut Vec<u8>) -> Option<()> {
    if LightFormat::of(client) == LightFormat::V26_3 {
        out.put_slice(r);
        return Some(());
    }
    for _ in 0..4 {
        let n = r.get_len()?;
        let bytes = r.bytes(n)?;
        out.put_len(n.div_ceil(8));
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            for (w, b) in word.iter_mut().zip(chunk) {
                *w = *b;
            }
            out.put_i64(i64::from_le_bytes(word));
        }
    }
    out.put_slice(r);
    Some(())
}

pub(crate) fn level_chunk(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let format = ChunkFormat::of(ctx.client());
    let mut out = Vec::with_capacity(r.len() + 128);
    copy(&mut r, 8, &mut out)?;
    heightmaps(&mut r, format, &mut out)?;
    let len = r.get_len()?;
    let data = r.bytes(len)?;
    let sections = sections(ctx, data, format)?;
    out.put_len(sections.len());
    out.put_slice(&sections);
    let n = r.get_len()?;
    let mut entities = Vec::new();
    let mut kept = 0usize;
    let types = ctx.t.registry("block_entity_type");
    for _ in 0..n {
        let xz = r.get_u8()?;
        let y = r.get_i16()?;
        let kind = r.get_var_int()?;
        let data = nbt::split(&mut r)?;
        let Some(client_kind) = types.and_then(|m| m.get(kind)) else {
            continue;
        };
        entities.put_u8(xz);
        entities.put_i16(y);
        entities.put_var_int(client_kind);
        block_entity_nbt(ctx, kind, data, &mut entities);
        kept += 1;
    }
    if let Some(bed) = ctx.t.bed_entity {
        for (xz, y) in beds(ctx.t, data) {
            entities.put_u8(xz);
            entities.put_i16(y);
            entities.put_var_int(bed);
            entities.put_u8(10);
            entities.put_u8(0);
            kept += 1;
        }
    }
    out.put_len(kept);
    out.put_slice(&entities);
    light_data(ctx.client(), r, &mut out)?;
    Some(out)
}

pub(crate) fn light_update(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.put_var_int(r.get_var_int()?);
    out.put_var_int(r.get_var_int()?);
    light_data(ctx.client(), r, &mut out)?;
    Some(out)
}

/// `chunks_biomes`: per chunk, the biome containers of every section.
pub(crate) fn chunks_biomes(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let palette = PaletteFormat::of(ctx.client());
    let client_count = ctx
        .t
        .client
        .synced
        .registries
        .iter()
        .find(|(n, _)| n == "worldgen/biome")
        .map_or(64, |(_, e)| e.len());
    let server_count = ctx
        .s
        .server_registries
        .get("worldgen/biome")
        .map_or(64, Vec::len);
    let map = ctx.dynamic("worldgen/biome").cloned();
    let remap = |id: i32| map.as_ref().map_or(id, |m| m.or(id, 0));
    let biomes = Container {
        entries: 64,
        max_indirect_bits: 3,
        server_bits: ceil_log2(u32::try_from(server_count).ok()?),
        client_bits: ceil_log2(u32::try_from(client_count).ok()?),
        remap: &remap,
    };
    let n = r.get_len()?;
    let mut out = Vec::new();
    out.put_len(n);
    for _ in 0..n {
        copy(&mut r, 8, &mut out)?;
        let len = r.get_len()?;
        let mut data = r.bytes(len)?;
        let mut sections = Vec::new();
        while !data.is_empty() {
            container(&mut data, &mut sections, &biomes, palette)?;
        }
        out.put_len(sections.len());
        out.put_slice(&sections);
    }
    Some(out)
}

/// A bed placed at a packed block position: its block entity for clients before 26.2.
fn bed_at(ctx: &mut Ctx<'_>, pos: i64) {
    let Some(bed) = ctx.t.bed_entity else { return };
    let mut p = Vec::with_capacity(11);
    p.put_i64(pos);
    p.put_var_int(bed);
    p.put_u8(10);
    p.put_u8(0);
    ctx.after.push(("block_entity_data", p));
}

pub(crate) fn block_update(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(12);
    let pos = r.get_i64()?;
    out.put_i64(pos);
    let state = r.get_var_int()?;
    out.put_var_int(ctx.t.block_state(state));
    if ctx.t.is_bed(state) {
        bed_at(ctx, pos);
    }
    Some(out)
}

pub(crate) fn section_blocks_update(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 8);
    let section = r.get_i64()?;
    out.put_i64(section);
    let n = r.get_len()?;
    out.put_len(n);
    for _ in 0..n {
        let entry = r.get_var_long()? as u64;
        let server_state = (entry >> 12) as i32;
        let state = ctx.t.block_state(server_state) as u64;
        out.put_var_long(((state << 12) | (entry & 0xFFF)) as i64);
        if ctx.t.is_bed(server_state) {
            // Section X 22 bits, Z 22 bits, Y 20 bits; entry: x << 8 | z << 4 | y.
            let (sx, sy, sz) = (section >> 42, section << 44 >> 44, section << 22 >> 42);
            let (x, z, y) = (
                (entry >> 8 & 15) as i64,
                (entry >> 4 & 15) as i64,
                (entry & 15) as i64,
            );
            let (x, y, z) = (sx * 16 + x, sy * 16 + y, sz * 16 + z);
            bed_at(
                ctx,
                ((x & 0x3FF_FFFF) << 38) | ((z & 0x3FF_FFFF) << 12) | (y & 0xFFF),
            );
        }
    }
    Some(out)
}

/// `block_entity_data`: types the client lacks are dropped.
pub(crate) fn block_entity_data(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len());
    copy(&mut r, 8, &mut out)?;
    let kind = r.get_var_int()?;
    out.put_var_int(ctx.t.registry("block_entity_type")?.get(kind)?);
    block_entity_nbt(ctx, kind, nbt::split(&mut r)?, &mut out);
    Some(out)
}

/// Block entity NBT in the client's version. NBT that does not convert goes
/// out empty (the client keeps its defaults) rather than in the server's layout.
fn block_entity_nbt(ctx: &Ctx<'_>, kind: i32, data: &[u8], out: &mut Vec<u8>) {
    let client = ctx.client();
    let name = usize::try_from(kind)
        .ok()
        .and_then(|i| ctx.t.server.registry("block_entity_type").get(i))
        .map_or("", String::as_str);
    let pot = name == "decorated_pot" && client < V26_3;
    if !pot && client >= V1_21_5 {
        out.put_slice(data);
        return;
    }
    let converted = pumbo_nbt::read_network(&mut &data[..], Limits::BACKEND)
        .ok()
        .flatten()
        .and_then(|tag| match tag {
            Tag::Compound(mut c) => {
                if pot {
                    pot_sherds(&mut c);
                }
                if client < V1_21_5 {
                    text_as_json(&mut c, TextFormat::for_protocol(client));
                }
                let mut bytes = Vec::with_capacity(data.len());
                pumbo_nbt::write_network(&mut bytes, Some(&Tag::Compound(c))).ok()?;
                Some(bytes)
            }
            _ => None,
        });
    match converted {
        Some(bytes) => out.put_slice(&bytes),
        None => {
            out.put_u8(10);
            out.put_u8(0);
        }
    }
}

/// 26.3→26.2 (ViaBackwards `BlockPacketRewriter26_3`): pot sherds by side
/// became item templates; older clients read a list of item names in the
/// order back, left, right, front.
fn pot_sherds(c: &mut Compound) {
    let Some(Tag::Compound(sides)) = c.get("sherds") else {
        return;
    };
    let items = ["back", "left", "right", "front"]
        .iter()
        .map(|side| {
            let id = match sides.get(side) {
                Some(Tag::Compound(item)) => item.get("id").and_then(Tag::as_str),
                _ => None,
            };
            Tag::String(id.unwrap_or("minecraft:brick").to_string())
        })
        .collect();
    c.insert("sherds", Tag::List(List::of(items)));
}

/// 1.21.5→1.21.4 (ViaBackwards `BlockPacketRewriter1_21_5`): text in block
/// entities was JSON strings: sign lines and custom names.
fn text_as_json(c: &mut Compound, format: TextFormat) {
    let json = |tag: &Tag| {
        let text = Component::from_nbt(tag).unwrap_or_else(|_| Component::text(""));
        Tag::String(text.to_json(format))
    };
    if let Some(name) = c.get("CustomName").filter(|t| t.as_str().is_none()) {
        let name = json(name);
        c.insert("CustomName", name);
    }
    for side in ["front_text", "back_text"] {
        let Some(Tag::Compound(text)) = c.get(side) else {
            continue;
        };
        let mut text = text.clone();
        for key in ["messages", "filtered_messages"] {
            if let Some(Tag::List(lines)) = text.get(key) {
                let lines = List::of(lines.items.iter().map(json).collect());
                text.insert(key, Tag::List(lines));
            }
        }
        c.insert(side, Tag::Compound(text));
    }
}

/// 26.2→26.1 (ViaBackwards `BlockItemPacketRewriter26_2`): beds lost their
/// block entity in 26.2, and older clients draw a bed only with one. Positions
/// of the beds in the server's chunk sections, as packed XZ and Y.
fn beds(t: &Tables, mut data: &[u8]) -> Vec<(u8, i16)> {
    // (section, packed XZ, Y in the section)
    let mut found = Vec::new();
    let mut sections = 0i16;
    let r = &mut data;
    while !r.is_empty() {
        let Some(blocks) = r
            .bytes(4)
            .and_then(|_| read_container(r, 4096, 8))
            .filter(|_| read_container(r, 64, 3).is_some())
        else {
            break;
        };
        let palette_has_bed = match &blocks.palette {
            Some(p) => p.iter().any(|s| t.is_bed(*s)),
            None => true,
        };
        if palette_has_bed {
            for index in 0..4096 {
                if t.is_bed(blocks.get(index)) {
                    let (x, z, y) = (index & 15, (index >> 4) & 15, index >> 8);
                    found.push((sections, ((x << 4) | z) as u8, y as i16));
                }
            }
        }
        sections += 1;
    }
    // ponytail: the bottom of the world from the section count (24: overworld at -64, else 0);
    // read it from the dimension type when custom heights matter.
    let min_y = if sections == 24 { -64 } else { 0 };
    found
        .into_iter()
        .map(|(section, xz, y)| (xz, min_y + section * 16 + y))
        .collect()
}

/// A server paletted container, read for lookups.
struct Read {
    bits: u8,
    single: i32,
    palette: Option<Vec<i32>>,
    longs: Vec<u64>,
}

impl Read {
    fn get(&self, index: usize) -> i32 {
        if self.bits == 0 {
            return self.single;
        }
        let per_long = 64 / usize::from(self.bits);
        let word = self.longs.get(index / per_long).copied().unwrap_or(0);
        let v = (word >> ((index % per_long) * usize::from(self.bits))) & ((1 << self.bits) - 1);
        match &self.palette {
            Some(p) => p.get(v as usize).copied().unwrap_or(0),
            None => v as i32,
        }
    }
}

fn read_container(r: &mut &[u8], entries: usize, max_indirect: u8) -> Option<Read> {
    let bits = r.get_u8()?;
    if bits == 0 {
        let single = r.get_var_int()?;
        return Some(Read {
            bits,
            single,
            palette: Some(vec![single]),
            longs: Vec::new(),
        });
    }
    let palette = if bits <= max_indirect {
        let n = r.get_len()?;
        let mut p = Vec::with_capacity(n.min(256));
        for _ in 0..n {
            p.push(r.get_var_int()?);
        }
        Some(p)
    } else {
        None
    };
    let n = long_count(entries, bits);
    let mut longs = Vec::with_capacity(n);
    for _ in 0..n {
        longs.push(r.get_i64()? as u64);
    }
    Some(Read {
        bits,
        single: 0,
        palette,
        longs,
    })
}

pub(crate) fn block_event(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len());
    copy(&mut r, 10, &mut out)?;
    out.put_var_int(ctx.t.blocks.get(r.get_var_int()?)?);
    Some(out)
}

/// Level events whose data is a block state.
const BLOCK_STATE_EVENTS: &[i32] = &[2001, 3008];

pub(crate) fn level_event(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let event = r.get_i32()?;
    let mut out = Vec::with_capacity(17);
    out.put_i32(event);
    copy(&mut r, 8, &mut out)?;
    let data = r.get_i32()?;
    out.put_i32(if BLOCK_STATE_EVENTS.contains(&event) {
        ctx.t.block_state(data)
    } else {
        data
    });
    out.put_slice(r);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(values: &[u32], bits: u8) -> Vec<i64> {
        let per_long = 64 / bits as usize;
        let mut out = vec![0i64; values.len().div_ceil(per_long)];
        for (i, &v) in values.iter().enumerate() {
            out[i / per_long] |= (u64::from(v) << ((i % per_long) * bits as usize)) as i64;
        }
        out
    }

    #[test]
    fn repack_widens_and_maps() {
        let values: Vec<u32> = (0..64).collect();
        let packed = repack(&pack(&values, 6), 64, 6, 7, &|v| v + 1);
        let expected = pack(&values.iter().map(|v| v + 1).collect::<Vec<_>>(), 7);
        assert_eq!(packed, expected);
    }

    #[test]
    fn light_masks_become_longs() {
        // Sky mask bits 1, 2; empty block mask; empty sky bit 0; empty block bits 0..9; no arrays.
        let server = [1u8, 0x06, 0, 1, 0x01, 2, 0xFF, 0x03, 0, 0];
        let mut out = Vec::new();
        light_data(776, &server, &mut out).unwrap();
        let mut expected = vec![1u8];
        expected.extend_from_slice(&6i64.to_be_bytes());
        expected.push(0);
        expected.push(1);
        expected.extend_from_slice(&1i64.to_be_bytes());
        expected.push(1);
        expected.extend_from_slice(&0x3FFi64.to_be_bytes());
        expected.extend_from_slice(&[0, 0]);
        assert_eq!(out, expected);
        let mut same = Vec::new();
        light_data(777, &server, &mut same).unwrap();
        assert_eq!(same, server);
    }
}
