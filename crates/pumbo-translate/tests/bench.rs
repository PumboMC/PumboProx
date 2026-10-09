//! Cost of the version translator on the recorded vanilla 26.3 session (see
//! `golden.rs`): microseconds per frame and per chunk, per client protocol.
//! Ignored by default; meaningful only in release (`PUMBO_MV_BENCH_REC`
//! points at another recorded 26.3 session):
//! `cargo test --release -p pumbo-translate --test bench -- --ignored --nocapture`
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pumbo_protocol::{Direction, Phase, ProtocolVersion};
use pumbo_testclient::recording;
use pumbo_translate::multiversion::version_data;
use pumbo_translate_mv as mv;

const ROUNDS: usize = 50;

#[test]
#[ignore = "benchmark"]
fn translation_cost() {
    let root = std::env::var_os("PUMBO_DATA_FULL").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/pumbo-data-full"),
        PathBuf::from,
    );
    // `PUMBO_MV_BENCH_REC`: another 26.3 session, e.g. Pumpkin's normal world
    // (`PUMBO_MV_RECORD` in pumbo-prox's `translate_mv` test).
    let server = recording::read(
        &std::env::var_os("PUMBO_MV_BENCH_REC")
            .map_or_else(|| root.join("777/session-known.rec"), PathBuf::from),
    )
    .unwrap();
    let server_tables = pumbo_data::tables(ProtocolVersion(777)).unwrap();
    let chunk_id = server_tables
        .packet_id(
            Phase::Play,
            Direction::Clientbound,
            "level_chunk_with_light",
        )
        .unwrap();
    let server_packs_id = server_tables
        .packet_id(
            Phase::Configuration,
            Direction::Serverbound,
            "select_known_packs",
        )
        .unwrap();
    for client in [767, 770, 773, 776] {
        let started = Instant::now();
        let tables = Arc::new(
            mv::Tables::new(
                version_data(ProtocolVersion(client)).unwrap(),
                version_data(ProtocolVersion(777)).unwrap(),
            )
            .unwrap(),
        );
        let build = started.elapsed();
        let own = recording::read(&root.join(format!("{client}/session-known.rec"))).unwrap();
        let client_tables = pumbo_data::tables(ProtocolVersion(client)).unwrap();
        let packs_id = client_tables
            .packet_id(
                Phase::Configuration,
                Direction::Serverbound,
                "select_known_packs",
            )
            .unwrap();
        let packs = own
            .iter()
            .find(|f| {
                f.phase == Phase::Configuration
                    && f.direction == Direction::Serverbound
                    && f.id == packs_id
            })
            .unwrap();
        let (mut frames, mut bytes, mut chunks) = (0usize, 0usize, 0usize);
        let (mut chunk_bytes, mut chunks_out) = (0usize, 0usize);
        // What the proxy does next with a translated frame: compress it again (threshold 256).
        let frame_cfg = pumbo_protocol::frame::FrameConfig::from_client().with_threshold(256);
        let (mut encode_all, mut encode_chunks) = (Duration::ZERO, Duration::ZERO);
        let mut buf = bytes::BytesMut::new();
        let (mut all, mut chunk_time) = (Duration::ZERO, Duration::ZERO);
        let mut out = mv::Output::default();
        for _ in 0..ROUNDS {
            let mut t = mv::Translator::new(tables.clone());
            for f in &server {
                if !matches!(f.phase, Phase::Configuration | Phase::Play) {
                    continue;
                }
                out.clear();
                if f.direction == Direction::Serverbound {
                    if f.phase == Phase::Configuration && f.id == server_packs_id {
                        t.to_server(packs.id, &packs.payload, &mut out);
                    }
                    continue;
                }
                let at = Instant::now();
                t.to_client(f.id, &f.payload, &mut out);
                let took = at.elapsed();
                all += took;
                frames += 1;
                bytes += f.payload.len();
                let at = Instant::now();
                for (id, payload) in &out.to_client {
                    buf.clear();
                    pumbo_protocol::frame::encode(&mut buf, *id, payload, &frame_cfg).unwrap();
                }
                let encoded = at.elapsed();
                encode_all += encoded;
                if f.phase == Phase::Play && f.id == chunk_id {
                    encode_chunks += encoded;
                    chunk_time += took;
                    chunks += 1;
                    chunk_bytes += f.payload.len();
                    chunks_out += out.to_client.len();
                }
            }
        }
        eprintln!(
            "{client}: tables {:.1} ms; {:.2} us/frame ({} frames, {:.0} MB/s), {:.1} us/chunk ({} chunks, {} KB each, {} out); compression after it {:.2} us/frame, {:.0} us/chunk",
            build.as_secs_f64() * 1000.0,
            all.as_secs_f64() * 1e6 / frames as f64,
            frames / ROUNDS,
            bytes as f64 / all.as_secs_f64() / 1e6,
            chunk_time.as_secs_f64() * 1e6 / chunks as f64,
            chunks / ROUNDS,
            chunk_bytes / chunks.max(1) / 1024,
            chunks_out / ROUNDS,
            encode_all.as_secs_f64() * 1e6 / frames as f64,
            encode_chunks.as_secs_f64() * 1e6 / chunks as f64
        );
    }
}
