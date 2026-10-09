//! Consistency of the generated tables (plan §7, E1).

use std::collections::{BTreeMap, BTreeSet};

use pumbo_data::{protocols, register_all, tables};
use pumbo_protocol::{
    Direction, KNOWN_PACKETS, PacketKind, Phase, ProtocolVersion, VersionRegistry,
};

/// Releases per protocol (plan §2.1), checked against the manifest-driven run.
const RELEASES: &[(i32, &[&str])] = &[
    (767, &["1.21", "1.21.1"]),
    (768, &["1.21.2", "1.21.3"]),
    (769, &["1.21.4"]),
    (770, &["1.21.5"]),
    (771, &["1.21.6"]),
    (772, &["1.21.7", "1.21.8"]),
    (773, &["1.21.9", "1.21.10"]),
    (774, &["1.21.11"]),
    (775, &["26.1", "26.1.1", "26.1.2"]),
    (776, &["26.2"]),
    (777, &["26.3"]),
];

/// Known packets that appeared after 767 (plan §2.4). Everything else in
/// `KNOWN_PACKETS` must exist from 767 on.
const ADDED_LATER: &[(Phase, Direction, PacketKind, i32)] = &[
    (
        Phase::Play,
        Direction::Clientbound,
        PacketKind::SetPlayerInventory,
        768,
    ),
    (
        Phase::Play,
        Direction::Serverbound,
        PacketKind::ClientTickEnd,
        768,
    ),
    (
        Phase::Play,
        Direction::Serverbound,
        PacketKind::PlayerLoaded,
        769,
    ),
    (
        Phase::Configuration,
        Direction::Clientbound,
        PacketKind::ShowDialog,
        771,
    ),
    (
        Phase::Configuration,
        Direction::Clientbound,
        PacketKind::ClearDialog,
        771,
    ),
    (
        Phase::Configuration,
        Direction::Serverbound,
        PacketKind::CustomClickAction,
        771,
    ),
    (
        Phase::Play,
        Direction::Clientbound,
        PacketKind::ShowDialog,
        771,
    ),
    (
        Phase::Play,
        Direction::Clientbound,
        PacketKind::ClearDialog,
        771,
    ),
    (
        Phase::Configuration,
        Direction::Clientbound,
        PacketKind::CodeOfConduct,
        773,
    ),
    (
        Phase::Configuration,
        Direction::Serverbound,
        PacketKind::AcceptCodeOfConduct,
        773,
    ),
    (
        Phase::Configuration,
        Direction::Clientbound,
        PacketKind::PostEffects,
        777,
    ),
];

fn since(phase: Phase, dir: Direction, kind: PacketKind) -> i32 {
    ADDED_LATER
        .iter()
        .find(|(p, d, k, _)| *p == phase && *d == dir && *k == kind)
        .map_or(767, |(_, _, _, v)| *v)
}

#[test]
fn every_protocol_of_the_range_is_embedded_with_its_releases() {
    let got: Vec<i32> = protocols().map(|p| p.0).collect();
    let want: Vec<i32> = RELEASES.iter().map(|(p, _)| *p).collect();
    assert_eq!(got, want);
    for (p, releases) in RELEASES {
        let t = tables(ProtocolVersion(*p)).unwrap();
        assert_eq!(t.release_names(), *releases, "protocol {p}");
        for r in &t.releases {
            assert_eq!(r.server_sha1.len(), 40);
        }
    }
}

#[test]
fn known_packets_present_from_their_first_protocol() {
    for v in protocols() {
        let t = tables(v).unwrap();
        for &(phase, dir, kind) in KNOWN_PACKETS {
            let id = t.packet_id(phase, dir, kind.name());
            if v.0 >= since(phase, dir, kind) {
                assert!(
                    id.is_some(),
                    "{v}: {phase:?}/{dir:?}/{} missing",
                    kind.name()
                );
            } else {
                assert!(
                    id.is_none(),
                    "{v}: {} exists earlier than expected",
                    kind.name()
                );
            }
        }
    }
}

#[test]
fn early_phases_are_fully_known() {
    // Handshake, status, login and configuration are decoded in full (E2), so
    // every packet there must be on the known list.
    let known: BTreeSet<(Phase, Direction, &str)> = KNOWN_PACKETS
        .iter()
        .map(|(p, d, k)| (*p, *d, k.name()))
        .collect();
    for v in protocols() {
        for p in &tables(v).unwrap().packets {
            if p.phase != Phase::Play {
                assert!(
                    known.contains(&(p.phase, p.direction, p.name.as_str())),
                    "{v}: {:?}/{:?}/{} is not on the known list",
                    p.phase,
                    p.direction,
                    p.name
                );
            }
        }
    }
}

#[test]
fn packet_ids_are_dense_and_unique() {
    for v in protocols() {
        let mut groups: BTreeMap<(Phase, Direction), Vec<i32>> = BTreeMap::new();
        for p in &tables(v).unwrap().packets {
            groups.entry((p.phase, p.direction)).or_default().push(p.id);
        }
        for ((phase, dir), mut ids) in groups {
            ids.sort();
            let want: Vec<i32> = (0..i32::try_from(ids.len()).unwrap()).collect();
            assert_eq!(ids, want, "{v}: {phase:?}/{dir:?}");
        }
    }
}

#[test]
fn version_modules_map_both_ways() {
    let mut reg = VersionRegistry::new();
    register_all(&mut reg).unwrap();
    assert_eq!(reg.oldest(), Some(ProtocolVersion::V767));
    assert_eq!(reg.newest(), Some(ProtocolVersion::V777));
    for v in reg.versions() {
        let m = reg.get(v).unwrap();
        for &(phase, dir, kind) in KNOWN_PACKETS {
            if let Some(id) = m.packet_id(phase, dir, kind) {
                assert_eq!(m.packet_kind(phase, dir, id), Some(kind));
            }
        }
        assert_eq!(m.packet_kind(Phase::Play, Direction::Clientbound, -1), None);
        assert_eq!(
            m.packet_kind(Phase::Play, Direction::Clientbound, 100_000),
            None
        );
        assert!(!m.release_names().is_empty());
    }
    // Same name, other ids: the table really is per version.
    let id = |p| {
        reg.get(ProtocolVersion(p)).unwrap().packet_id(
            Phase::Login,
            Direction::Clientbound,
            PacketKind::LoginFinished,
        )
    };
    assert_eq!(id(767), Some(2));
    assert_eq!(
        reg.get(ProtocolVersion::V767).unwrap().packet_kind(
            Phase::Login,
            Direction::Serverbound,
            0
        ),
        Some(PacketKind::Hello)
    );
}

#[test]
fn numbers_from_the_plan() {
    // Plan §2.4: map item, map component and barrier states.
    let t = tables(ProtocolVersion::V767).unwrap();
    assert_eq!(t.registry("item").unwrap().id("filled_map"), Some(982));
    assert_eq!(
        t.registry("data_component_type").unwrap().id("map_id"),
        Some(26)
    );
    let barrier = t.block("barrier").unwrap();
    assert_eq!((barrier.first_state, barrier.state_count()), (10365, 2));
    let t = tables(ProtocolVersion::V777).unwrap();
    assert_eq!(
        t.registry("item").unwrap().id("minecraft:filled_map"),
        Some(1238)
    );
    assert_eq!(
        t.registry("data_component_type").unwrap().id("map_id"),
        Some(48)
    );
    assert_eq!(
        t.block_state("barrier", &[("waterlogged", "true")]),
        Some(14380)
    );
    assert_eq!(t.block_state("barrier", &[]), Some(14381));
    assert_eq!(t.block_state("air", &[]), Some(0));
    // Command parsers start with brigadier's.
    assert_eq!(
        t.registry("command_argument_type").unwrap().name(0),
        Some("brigadier:bool")
    );
}

#[test]
fn block_states_cover_the_id_space_without_gaps() {
    for v in protocols() {
        let t = tables(v).unwrap();
        let mut next = 0u32;
        for b in &t.blocks {
            assert_eq!(b.first_state, next, "{v}: {}", b.name);
            assert!(b.default_offset < b.state_count());
            next += b.state_count();
        }
        assert_eq!(next, t.block_state_count());
    }
}

#[test]
fn static_registries_present_everywhere() {
    for v in protocols() {
        let t = tables(v).unwrap();
        for name in pumbo_data::STATIC_REGISTRIES {
            let r = t.registry(name).unwrap();
            assert!(!r.is_empty(), "{v}: {name}");
            let unique: BTreeSet<&String> = r.entries.iter().collect();
            assert_eq!(unique.len(), r.len(), "{v}: {name} has duplicates");
        }
    }
}
