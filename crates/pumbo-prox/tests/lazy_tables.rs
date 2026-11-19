//! A version's tables load when a player of a listed version needs them:
//! a ping from a version off the list loads none of its own, and neither does
//! a ping over the status limit. In its own process, so nothing else parses
//! tables meanwhile; each test asks about versions of its own.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::net::SocketAddr;
use std::sync::atomic::Ordering;

use common::module;
use pumbo_protocol::packets::status::{Intention, StatusRequest, StatusResponse};
use pumbo_protocol::{Phase, ProtocolVersion};
use pumbo_testclient::Client;

/// The status a client of `protocol` gets, sent in the frames of 26.3 (the
/// status layout is the same in every version), so the test's own client
/// loads no tables of `protocol`.
async fn ping(addr: SocketAddr, protocol: i32) -> String {
    let mut c = Client::connect(addr, module(777)).await.unwrap();
    c.send(&Intention {
        protocol,
        address: "localhost".into(),
        port: addr.port(),
        intent: Intention::STATUS,
    })
    .await
    .unwrap();
    c.phase = Phase::Status;
    c.send(&StatusRequest).await.unwrap();
    let (_, f) = c.recv().await.unwrap();
    let r: StatusResponse = c.decode(&f).unwrap();
    r.json
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ping_loads_only_the_tables_of_a_listed_version() {
    let cfg = "listener:\n  - bind: \"127.0.0.1:0\"\nforwarding:\n  mode: none\n\
               versions:\n  clients: [\"26.2\"]\n";
    let (proxy, addr) = common::start_proxy(cfg).await;
    assert!(!pumbo_data::is_parsed(ProtocolVersion(769)));
    assert!(!pumbo_data::is_parsed(ProtocolVersion(776)));
    assert!(ping(addr, 769).await.contains("\"protocol\":776"));
    assert!(!pumbo_data::is_parsed(ProtocolVersion(769)));
    assert!(ping(addr, 776).await.contains("\"protocol\":776"));
    assert!(pumbo_data::is_parsed(ProtocolVersion(776)));
    proxy.stop();
}

/// A ping over `limits.status-per-ip-per-second` is dropped before the
/// tables of its version are read.
#[tokio::test(flavor = "multi_thread")]
async fn a_ping_over_the_limit_loads_no_tables() {
    let cfg = "listener:\n  - bind: \"127.0.0.1:0\"\nforwarding:\n  mode: none\n\
               limits:\n  status-per-ip-per-second: 0\n";
    let (proxy, addr) = common::start_proxy(cfg).await;
    let mut c = Client::connect(addr, module(777)).await.unwrap();
    c.send(&Intention {
        protocol: 775,
        address: "localhost".into(),
        port: addr.port(),
        intent: Intention::STATUS,
    })
    .await
    .unwrap();
    // The proxy closes the connection without an answer.
    assert!(c.recv().await.is_err());
    let dropped = proxy.metrics.rejected_status_rate.load(Ordering::Relaxed);
    assert_eq!(dropped, 1);
    assert!(!pumbo_data::is_parsed(ProtocolVersion(775)));
    proxy.stop();
}
