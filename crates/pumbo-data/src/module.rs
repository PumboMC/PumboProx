//! [`VersionModule`] backed by generated tables.

use std::collections::HashMap;

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
#[derive(Debug)]
pub struct DataVersion {
    tables: &'static Tables,
    releases: Vec<String>,
    by_id: [Vec<Option<PacketKind>>; PHASES * DIRECTIONS],
    by_kind: HashMap<(Phase, Direction, PacketKind), i32>,
    parsers: Option<&'static Registry>,
}

impl DataVersion {
    pub fn new(tables: &'static Tables) -> Self {
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
            tables,
            releases: tables.release_names(),
            by_id,
            by_kind,
            parsers: tables.registry("command_argument_type"),
        }
    }

    pub fn tables(&self) -> &'static Tables {
        self.tables
    }
}

impl VersionModule for DataVersion {
    fn protocol(&self) -> ProtocolVersion {
        self.tables.protocol
    }

    fn release_names(&self) -> &[String] {
        &self.releases
    }

    fn packet_kind(&self, phase: Phase, direction: Direction, id: i32) -> Option<PacketKind> {
        let idx = usize::try_from(id).ok()?;
        self.by_id
            .get(slot(phase, direction))?
            .get(idx)
            .copied()
            .flatten()
    }

    fn packet_id(&self, phase: Phase, direction: Direction, kind: PacketKind) -> Option<i32> {
        self.by_kind.get(&(phase, direction, kind)).copied()
    }

    fn command_argument_type(&self, id: i32) -> Option<&str> {
        self.parsers?.name(u32::try_from(id).ok()?)
    }
}
