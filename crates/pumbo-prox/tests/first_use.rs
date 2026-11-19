//! How long the first use of a version takes in a fresh process: its
//! tables and packet indexes, then the translation tables of each older
//! client for a 26.3 server. Ignored by default; meaningful only in release:
//! `cargo test --release -p pumbo-prox --test first_use -- --ignored --nocapture`
#![cfg(feature = "translate")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Instant;

use pumbo_core::translate::TranslatorProvider;
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion, VersionRegistry};
use pumbo_translate::multiversion::Multiversion;

#[test]
#[ignore = "benchmark"]
fn first_use_of_each_version() {
    let started = Instant::now();
    let mut reg = VersionRegistry::new();
    pumbo_data::register_all(&mut reg).unwrap();
    println!(
        "register_all: {:.2} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    let protocols: Vec<ProtocolVersion> = pumbo_data::protocols().collect();
    for &v in &protocols {
        let m = reg.get(v).unwrap();
        let started = Instant::now();
        assert!(
            m.packet_id(Phase::Login, Direction::Serverbound, PacketKind::Hello)
                .is_some()
        );
        println!(
            "protocol {v}: tables and indexes {:.2} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
    let server = Some(ProtocolVersion(777));
    for &v in protocols.iter().filter(|v| v.0 < 777) {
        let provider = Multiversion::default();
        let started = Instant::now();
        assert!(provider.plan(v, server).is_some());
        println!(
            "protocol {v}: translation tables {:.2} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}
