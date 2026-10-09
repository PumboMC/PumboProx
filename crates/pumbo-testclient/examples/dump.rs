//! Prints recorded frames of one packet kind as hex:
//! `cargo run -p pumbo-testclient --example dump -- <recording> <protocol> <kind> [all]`.

use pumbo_data::DataVersion;
use pumbo_protocol::{Direction, ProtocolVersion, VersionModule};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(path), Some(protocol), Some(kind)) = (args.first(), args.get(1), args.get(2)) else {
        eprintln!("usage: dump <recording> <protocol> <kind> [all]");
        return;
    };
    let Ok(protocol) = protocol.parse::<i32>() else {
        return;
    };
    let Ok(tables) = pumbo_data::tables(ProtocolVersion(protocol)) else {
        return;
    };
    let module = DataVersion::new(tables);
    let Ok(frames) = pumbo_testclient::recording::read(std::path::Path::new(path)) else {
        eprintln!("cannot read {path}");
        return;
    };
    for (i, f) in frames.iter().enumerate() {
        let k = module.packet_kind(f.phase, f.direction, f.id);
        if k.map(|k| k.name()) == Some(kind.as_str())
            && (f.direction == Direction::Clientbound || args.get(3).is_some())
        {
            let hex: Vec<String> = f.payload.iter().map(|b| format!("{b:02x}")).collect();
            println!("{i} {:?} {:?} {}", f.phase, f.direction, hex.join(" "));
        }
    }
}
