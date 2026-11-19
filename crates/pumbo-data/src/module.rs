//! [`VersionModule`] backed by generated tables.

use std::collections::HashMap;
use std::sync::OnceLock;

use pumbo_protocol::{Direction, KNOWN_PACKETS, PacketKind, Phase, ProtocolVersion, VersionModule};

use crate::{Registry, Tables};

const PHASES: usize = Phase::ALL.len();
const DIRECTIONS: usize = Direction::ALL.len();

fn slot(phase: Phase, direction: Direction) -> usize {
    let p = Phase::ALL.iter().position(|x| *x == phase).unwrap_or(0);
    let d = Direction::ALL
        .iter()
        .position(|x| *x == direction)
        .unwrap_or(0);
    p * DIRECTIONS + d
}

/// A protocol version described by generated tables. The ID → kind lookup is a
/// dense array per phase and direction (one bounds check per frame, §2.2).
/// An embedded version builds its tables and lookups the first time a packet
/// is looked up, not when it is registered.
#[derive(Debug)]
pub struct DataVersion {
    protocol: ProtocolVersion,
    releases: Vec<String>,
    index: OnceLock<Index>,
}

/// The lookups of one version; empty when its tables do not parse.
#[derive(Debug, Default)]
struct Index {
    by_id: [Vec<Option<PacketKind>>; PHASES * DIRECTIONS],
    by_kind: HashMap<(Phase, Direction, PacketKind), i32>,
    parsers: Option<&'static Registry>,
}

impl Index {
    fn new(tables: &'static Tables) -> Self {
        let mut by_id: [Vec<Option<PacketKind>>; PHASES * DIRECTIONS] = Default::default();
        let mut by_kind = HashMap::new();
        for &(phase, direction, kind) in KNOWN_PACKETS {
            let Some(id) = tables.packet_id(phase, direction, kind.name()) else {
                continue;
            };
            let (Ok(idx), Some(list)) =
                (usize::try_from(id), by_id.get_mut(slot(phase, direction)))
            else {
                continue;
            };
            if list.len() <= idx {
                list.resize(idx + 1, None);
            }
            if let Some(cell) = list.get_mut(idx) {
                *cell = Some(kind);
            }
            by_kind.insert((phase, direction, kind), id);
        }
        Self {
            by_id,
            by_kind,
            parsers: tables.registry("command_argument_type"),
        }
    }
}

impl DataVersion {
    /// A version of these tables, its lookups built now.
    pub fn new(tables: &'static Tables) -> Self {
        Self {
            protocol: tables.protocol,
            releases: tables.release_names(),
            index: OnceLock::from(Index::new(tables)),
        }
    }

    /// An embedded protocol whose tables are parsed on first use.
    pub(crate) fn embedded(protocol: ProtocolVersion, releases: &[&str]) -> Self {
        Self {
            protocol,
            releases: releases.iter().map(|r| (*r).to_string()).collect(),
            index: OnceLock::new(),
        }
    }

    fn index(&self) -> &Index {
        self.index.get_or_init(|| {
            crate::tables(self.protocol)
                .map(Index::new)
                .unwrap_or_default()
        })
    }
}

impl VersionModule for DataVersion {
    fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    fn release_names(&self) -> &[String] {
        &self.releases
    }

    fn packet_kind(&self, phase: Phase, direction: Direction, id: i32) -> Option<PacketKind> {
        let idx = usize::try_from(id).ok()?;
        self.index()
            .by_id
            .get(slot(phase, direction))?
            .get(idx)
            .copied()
            .flatten()
    }

    fn packet_id(&self, phase: Phase, direction: Direction, kind: PacketKind) -> Option<i32> {
        self.index().by_kind.get(&(phase, direction, kind)).copied()
    }

    fn command_argument_type(&self, id: i32) -> Option<&str> {
        self.index().parsers?.name(u32::try_from(id).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_embedded_version_builds_its_lookups_on_first_use() {
        let m = DataVersion::embedded(ProtocolVersion(777), &["26.3"]);
        assert_eq!(m.release_names(), ["26.3"]);
        assert!(m.index.get().is_none());
        assert!(
            m.packet_id(Phase::Login, Direction::Serverbound, PacketKind::Hello)
                .is_some()
        );
        assert!(m.index.get().is_some());
    }

    #[test]
    fn a_version_without_tables_knows_no_packet() {
        let m = DataVersion::embedded(ProtocolVersion(1), &[]);
        assert!(m.release_names().is_empty());
        assert_eq!(m.packet_kind(Phase::Login, Direction::Serverbound, 0), None);
        assert_eq!(
            m.packet_id(Phase::Login, Direction::Serverbound, PacketKind::Hello),
            None
        );
        assert_eq!(m.command_argument_type(0), None);
    }
}
