//! Seeds `fuzz/corpus/<target>/` from the vanilla recordings in
//! `target/pumbo-data-full` (local only, like the recordings themselves):
//! `cargo +nightly run --release --bin seed`.

use std::path::{Path, PathBuf};

use bytes::BytesMut;
use pumbo_data::DataVersion;
use pumbo_protocol::frame::{FrameConfig, encode};
use pumbo_protocol::packets::DECODED;
use pumbo_protocol::{Direction, Phase, VersionModule};

fn write(dir: &Path, n: &mut usize, data: &[u8]) {
    let _ = std::fs::create_dir_all(dir);
    *n += 1;
    let _ = std::fs::write(dir.join(format!("seed-{n:06}")), data);
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let full = std::env::var_os("PUMBO_DATA_FULL")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../target/pumbo-data-full"));
    let corpus = root.join("corpus");
    let versions: Vec<_> = pumbo_data::protocols().collect();
    let mut n = 0usize;
    for (vi, v) in versions.iter().enumerate() {
        let Ok(tables) = pumbo_data::tables(*v) else {
            continue;
        };
        let module = DataVersion::new(tables);
        for name in ["session-known.rec", "session-none.rec"] {
            let Ok(frames) = pumbo_testclient::recording::read(&full.join(v.0.to_string()).join(name))
            else {
                continue;
            };
            for f in &frames {
                let Some(kind) = module.packet_kind(f.phase, f.direction, f.id) else {
                    continue;
                };
                let (target, phases): (&str, &[Phase]) = match f.phase {
                    Phase::Handshake | Phase::Status => {
                        ("handshake_status", &[Phase::Handshake, Phase::Status])
                    }
                    Phase::Login => ("login", &[Phase::Login]),
                    Phase::Configuration => ("configuration", &[Phase::Configuration]),
                    Phase::Play => ("play", &[Phase::Play]),
                };
                let candidates: Vec<_> = DECODED
                    .iter()
                    .filter(|(p, d, _)| phases.contains(p) && *d == f.direction)
                    .collect();
                let Some(k) = candidates
                    .iter()
                    .position(|(p, _, kk)| *p == f.phase && *kk == kind)
                else {
                    continue;
                };
                if f.payload.len() > 64 * 1024 {
                    continue;
                }
                let mut data = vec![
                    vi as u8,
                    u8::from(f.direction == Direction::Serverbound),
                    k as u8,
                ];
                data.extend_from_slice(&f.payload);
                write(&corpus.join(target), &mut n, &data);
                if f.payload.len() < 4096 {
                    let mut framed = BytesMut::new();
                    let mode = 4u8; // from_client, threshold 64
                    if encode(
                        &mut framed,
                        f.id,
                        &f.payload,
                        &FrameConfig::from_client().with_threshold(64),
                    )
                    .is_ok()
                    {
                        let mut data = vec![mode];
                        data.extend_from_slice(&framed);
                        write(&corpus.join("frame"), &mut n, &data);
                    }
                }
            }
        }
    }
    eprintln!("wrote {n} seeds to {}", corpus.display());
}
