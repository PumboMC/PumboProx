//! What the proxy finds out about its servers and what it does with it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::module;
use pumbo_testclient::{JoinOptions, Player};

/// A server without `protocol` takes every listed client version, each in
/// its own version: nothing translates.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_a_protocol_takes_every_listed_version() {
    let (lobby, log) = common::backend().await;
    let cfg = format!(
        "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
         servers:\n  lobby: {{ address: \"{lobby}\" }}\nrouting:\n  try: [lobby]\n"
    );
    let (proxy, addr) = common::start_proxy(&cfg).await;
    for (protocol, name) in [(776, "Older"), (777, "Newer")] {
        let p = Player::join(addr, module(protocol), JoinOptions::new(name))
            .await
            .unwrap();
        assert_eq!(p.logins[0].dimension, "minecraft:overworld", "{name}");
        p.close().await;
    }
    assert_eq!(*log.logins.lock().unwrap(), [776, 777]);
    proxy.stop();
}
