//! Registering the embedded versions parses none of their tables; one
//! version's tables are parsed when its module is first asked for a packet.
//! One test in its own process, so nothing else parses tables meanwhile.
#![allow(clippy::unwrap_used)]

use pumbo_data::{is_parsed, protocols, register_all};
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion, VersionRegistry};

#[test]
fn tables_are_parsed_on_first_use() {
    let mut reg = VersionRegistry::new();
    register_all(&mut reg).unwrap();
    assert!(protocols().all(|v| !is_parsed(v)));
    let m = reg.get(ProtocolVersion::V769).unwrap();
    assert_eq!(m.release_names(), ["1.21.4"]);
    assert!(!is_parsed(ProtocolVersion::V769));
    assert!(
        m.packet_id(Phase::Login, Direction::Serverbound, PacketKind::Hello)
            .is_some()
    );
    assert!(is_parsed(ProtocolVersion::V769));
    assert!(!is_parsed(ProtocolVersion::V770));
}
