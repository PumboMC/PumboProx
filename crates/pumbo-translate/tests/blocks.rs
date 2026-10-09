//! Block entities for older clients: NBT in the client's layout (pot sherds
//! before 26.3, text as JSON before 1.21.5) and the bed block entities that
//! clients before 26.2 need to draw beds (ViaBackwards `BlockPacketRewriter26_3`,
//! `BlockItemPacketRewriter26_2`, `BlockPacketRewriter1_21_5`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;

use pumbo_nbt::{Compound, Limits, List, Tag};
use pumbo_protocol::ProtocolVersion;
use pumbo_translate::multiversion::version_data;
use pumbo_translate_mv as mv;

fn data(protocol: i32) -> mv::VersionData {
    version_data(ProtocolVersion(protocol)).unwrap()
}

fn packet(v: &mv::VersionData, phase: usize, name: &str) -> i32 {
    v.packets[phase][0].iter().position(|n| n == name).unwrap() as i32
}

fn registry_id(v: &mv::VersionData, registry: &str, name: &str) -> i32 {
    v.registries[registry]
        .iter()
        .position(|n| n == name)
        .unwrap() as i32
}

fn var_int(out: &mut Vec<u8>, v: i32) {
    let mut v = v as u32;
    loop {
        if v < 0x80 {
            out.push(v as u8);
            return;
        }
        out.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
}

fn read_var_int(r: &mut &[u8]) -> i32 {
    let mut v = 0u32;
    for shift in 0..5 {
        let b = r[0];
        *r = &r[1..];
        v |= u32::from(b & 0x7F) << (7 * shift);
        if b & 0x80 == 0 {
            break;
        }
    }
    v as i32
}

/// Every play packet the server sends through a fresh translator.
fn translate(client: i32, packets: &[(&str, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
    let (c, s) = (data(client), data(777));
    let tables = Arc::new(mv::Tables::new(c.clone(), s.clone()).unwrap());
    let mut t = mv::Translator::new(tables);
    let mut out = mv::Output::default();
    t.to_client(packet(&s, 0, "finish_configuration"), &[], &mut out);
    out.clear();
    for (name, payload) in packets {
        t.to_client(packet(&s, 1, name), payload, &mut out);
    }
    out.to_client
        .drain(..)
        .map(|(id, p)| (c.packets[1][0][id as usize].clone(), p))
        .collect()
}

fn compound(entries: Vec<(&str, Tag)>) -> Tag {
    let mut c = Compound::new();
    for (k, v) in entries {
        c.insert(k, v);
    }
    Tag::Compound(c)
}

fn nbt(tag: &Tag) -> Vec<u8> {
    let mut out = Vec::new();
    pumbo_nbt::write_network(&mut out, Some(tag)).unwrap();
    out
}

/// `block_entity_data` at 1, 2, 3 of a server block entity type.
fn block_entity(kind: &str, tag: &Tag) -> Vec<u8> {
    let s = data(777);
    let mut p = ((1i64 << 38) | (3 << 12) | 2).to_be_bytes().to_vec();
    var_int(&mut p, registry_id(&s, "block_entity_type", kind));
    p.extend_from_slice(&nbt(tag));
    p
}

/// The NBT of a translated `block_entity_data`.
fn nbt_of(payload: &[u8]) -> Tag {
    let mut r = &payload[8..];
    read_var_int(&mut r);
    pumbo_nbt::read_network(&mut r, Limits::BACKEND)
        .unwrap()
        .unwrap()
}

#[test]
fn pot_sherds_become_a_list_before_26_3() {
    // The pot from the user's 26.1.2 log (Pumpkin 0.2.0 sends 26.3 NBT).
    let item = |id: &str| compound(vec![("id", Tag::String(id.into()))]);
    let pot = compound(vec![(
        "sherds",
        compound(vec![
            ("back", item("minecraft:brick")),
            ("front", item("minecraft:flow_pottery_sherd")),
            ("left", item("minecraft:brick")),
            ("right", item("minecraft:brick")),
        ]),
    )]);
    for client in 767..=776 {
        let out = translate(
            client,
            &[("block_entity_data", block_entity("decorated_pot", &pot))],
        );
        assert_eq!(out.len(), 1, "{client}");
        let Tag::Compound(c) = nbt_of(&out[0].1) else {
            panic!("{client}: not a compound")
        };
        let sherds: Vec<&str> = match c.get("sherds") {
            Some(Tag::List(l)) => l.items.iter().filter_map(Tag::as_str).collect(),
            other => panic!("{client}: sherds {other:?}"),
        };
        assert_eq!(
            sherds,
            [
                "minecraft:brick",
                "minecraft:brick",
                "minecraft:brick",
                "minecraft:flow_pottery_sherd"
            ],
            "{client}: back, left, right, front"
        );
    }
}

#[test]
fn sign_text_is_json_before_1_21_5() {
    let line = |s: &str| compound(vec![("text", Tag::String(s.into()))]);
    let side = compound(vec![
        (
            "messages",
            Tag::List(List::of(vec![line("Hi"), line(""), line(""), line("")])),
        ),
        ("color", Tag::String("black".into())),
        ("has_glowing_text", Tag::Byte(0)),
    ]);
    let sign = compound(vec![
        ("front_text", side.clone()),
        ("back_text", side),
        ("is_waxed", Tag::Byte(0)),
    ]);
    for client in 767..=776 {
        let out = translate(
            client,
            &[("block_entity_data", block_entity("sign", &sign))],
        );
        assert_eq!(out.len(), 1, "{client}");
        let got = nbt_of(&out[0].1);
        if client >= 770 {
            assert_eq!(got, sign, "{client}: unchanged");
            continue;
        }
        let Tag::Compound(c) = got else { panic!() };
        let Some(Tag::Compound(front)) = c.get("front_text") else {
            panic!()
        };
        let Some(Tag::List(lines)) = front.get("messages") else {
            panic!()
        };
        // JSON text: a plain string is a whole component.
        assert_eq!(lines.items[0], Tag::String(r#""Hi""#.into()), "{client}");
    }
}

/// A 26.3 overworld chunk at 0, 0: air everywhere but one bed in section 4.
fn chunk_with_bed(bed_state: i32) -> Vec<u8> {
    let mut sections = Vec::new();
    for i in 0..24 {
        sections.extend_from_slice(&[0, 0, 0, 0]); // block and fluid counts
        if i == 4 {
            // Four bits, palette [air, bed], the bed at x 3, y 5, z 7.
            sections.push(4);
            var_int(&mut sections, 2);
            var_int(&mut sections, 0);
            var_int(&mut sections, bed_state);
            let mut longs = vec![0u64; 256];
            let index = (5 << 8) | (7 << 4) | 3;
            longs[index / 16] |= 1 << ((index % 16) * 4);
            longs
                .iter()
                .for_each(|l| sections.extend_from_slice(&l.to_be_bytes()));
        } else {
            sections.extend_from_slice(&[0, 0]);
        }
        sections.extend_from_slice(&[0, 0]); // biomes: single, 0
    }
    let mut p = vec![0; 8];
    var_int(&mut p, 0); // no heightmaps
    var_int(&mut p, sections.len() as i32);
    p.extend_from_slice(&sections);
    var_int(&mut p, 0); // no block entities
    p.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // light masks and arrays
    p
}

#[test]
fn beds_get_their_block_entity_before_26_2() {
    let s = data(777);
    let red_bed = s.blocks.iter().find(|b| b.name == "red_bed").unwrap();
    let state = (red_bed.first_state + red_bed.default_offset) as i32;
    for client in 767..=776 {
        let c = data(client);
        let out = translate(client, &[("level_chunk_with_light", chunk_with_bed(state))]);
        let (_, chunk) = &out[0];
        // Skip position, heightmaps (NBT before 1.21.5) and the section data.
        let mut r = &chunk[8..];
        if client < 770 {
            pumbo_nbt::read_network(&mut r, Limits::BACKEND).unwrap();
        } else {
            read_var_int(&mut r);
        }
        let n = read_var_int(&mut r) as usize;
        r = &r[n..];
        let entities = read_var_int(&mut r);
        if client >= 776 {
            assert_eq!(entities, 0, "{client}: 26.2 has no bed block entity");
            continue;
        }
        assert_eq!(entities, 1, "{client}");
        assert_eq!(r[0], (3 << 4) | 7, "{client}: x 3, z 7");
        assert_eq!(
            i16::from_be_bytes([r[1], r[2]]),
            5, // -64 + 4 * 16 + 5
            "{client}: y"
        );
        let mut rest = &r[3..];
        assert_eq!(
            read_var_int(&mut rest),
            registry_id(&c, "block_entity_type", "bed"),
            "{client}"
        );

        // A bed placed later: the block, then its block entity.
        let mut update = ((10i64 << 38) | (20 << 12) | 70).to_be_bytes().to_vec();
        var_int(&mut update, state);
        let names: Vec<String> = translate(client, &[("block_update", update)])
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["block_update", "block_entity_data"], "{client}");
    }
}
