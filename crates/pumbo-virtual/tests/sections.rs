//! Golden test of the chunk section format (plan §5.7): every chunk a vanilla
//! server of each protocol sent in the recordings (`pumbo-datagen record`,
//! a flat world) is read back block by block and written again with
//! `write_section`; the bytes must match. Vanilla 770 pads the section data
//! with zeros (its size estimate still counts the removed data lengths), so
//! there the padding is ignored.
//!
//! Without recordings the test says what it skipped;
//! `PUMBO_REQUIRE_RECORDINGS=1` makes that a failure.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;

use pumbo_data::DataVersion;
use pumbo_protocol::packets::world::LevelChunkWithLight;
use pumbo_protocol::packets::{self, Ctx};
use pumbo_protocol::types::Reader;
use pumbo_protocol::{Direction, PacketKind, Phase, VersionFeatures, VersionModule};
use pumbo_testclient::recording;

fn full_root() -> PathBuf {
    std::env::var_os("PUMBO_DATA_FULL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/pumbo-data-full")
        })
}

/// One paletted container: (states or biomes, single value).
fn container(r: &mut Reader<'_>, f: VersionFeatures, entries: usize, max_indirect: u8) -> Vec<u32> {
    let bits = r.u8().unwrap();
    let palette: Option<Vec<u32>> = if bits == 0 {
        Some(vec![r.varint().unwrap() as u32])
    } else if bits <= max_indirect {
        let n = r.varint().unwrap();
        Some((0..n).map(|_| r.varint().unwrap() as u32).collect())
    } else {
        None
    };
    let per = if bits == 0 { 0 } else { 64 / bits as usize };
    let longs = if !f.chunk_data_unsized {
        r.varint().unwrap() as usize
    } else if bits == 0 {
        0
    } else {
        entries.div_ceil(per)
    };
    let data: Vec<u64> = (0..longs).map(|_| r.i64().unwrap() as u64).collect();
    let palette = palette.expect("direct palettes do not occur in a flat world");
    if bits == 0 {
        return vec![palette[0]; entries];
    }
    (0..entries)
        .map(|i| {
            let v = (data[i / per] >> ((i % per) * bits as usize)) & ((1 << bits) - 1);
            palette[v as usize]
        })
        .collect()
}

#[test]
fn recorded_sections_encode_back() {
    let require = std::env::var("PUMBO_REQUIRE_RECORDINGS").is_ok_and(|v| v == "1");
    let mut checked = 0;
    for v in pumbo_data::protocols() {
        let path = full_root().join(v.0.to_string()).join("session-known.rec");
        let Ok(frames) = recording::read(&path) else {
            assert!(!require, "no recording for {v}");
            eprintln!("{v}: no recording, skipped");
            continue;
        };
        let m = DataVersion::new(pumbo_data::tables(v).unwrap());
        let f = m.features();
        let tables = pumbo_data::tables(v).unwrap();
        let fluids: Vec<u32> = ["minecraft:water", "minecraft:lava"]
            .iter()
            .filter_map(|n| tables.block(n))
            .flat_map(|b| b.first_state..b.first_state + b.state_count())
            .collect();
        let mut chunks = 0;
        for fr in &frames {
            if fr.phase != Phase::Play
                || m.packet_kind(fr.phase, fr.direction, fr.id)
                    != Some(PacketKind::LevelChunkWithLight)
            {
                continue;
            }
            let c: LevelChunkWithLight =
                packets::decode(&fr.payload, &Ctx::new(&m, Direction::Clientbound)).unwrap();
            let mut r = Reader::new(&c.sections);
            let mut ours = Vec::new();
            // A flat overworld: 24 sections.
            for _ in 0..24 {
                r.i16().unwrap();
                if f.chunk_section_fluid_count {
                    r.i16().unwrap();
                }
                let blocks = container(&mut r, f, 4096, 8);
                let biomes = container(&mut r, f, 64, 3);
                assert!(
                    biomes.iter().all(|b| *b == biomes[0]),
                    "{v}: one biome per section"
                );
                pumbo_virtual::world::write_section(
                    &mut ours,
                    f,
                    &blocks,
                    0,
                    biomes[0] as i32,
                    &fluids,
                )
                .unwrap();
            }
            let rest = r.rest();
            assert!(
                rest.iter().all(|b| *b == 0),
                "{v}: only zero padding after the sections"
            );
            assert!(
                rest.is_empty() || v.0 == 770,
                "{v}: {} bytes after the sections",
                rest.len()
            );
            assert_eq!(
                ours,
                c.sections[..c.sections.len() - rest.len()],
                "{v}: chunk {} {}",
                c.x,
                c.z
            );
            chunks += 1;
        }
        assert!(chunks > 0, "{v}: no chunks recorded");
        eprintln!("{v}: {chunks} chunks re-encoded byte for byte");
        checked += 1;
    }
    eprintln!("{checked} protocols checked");
}
