//! E6 end-to-end tests without external servers: the virtual world through
//! the test gate (`[virtual] test-gate`) for every protocol 767–777, with and
//! without known packs: join, chunks with the platform, a fall checked
//! against the vanilla curve, map in hand decoded, title and bossbar, a
//! command and chat, idling with keep-alives, a world change, and release to
//! a scripted backend without a disconnect. Also entering from a server and
//! going back, the release timer and the gate time limit.
//!
//! `PUMBO_VIRTUAL_IDLE_SECS` (default 60) sets the idle part (the plan's
//! 5 minutes run by hand).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{module, start_proxy, through_gate};
use pumbo_testclient::{JoinOptions, Player};

const WAIT: Duration = Duration::from_secs(15);

fn config(backend: SocketAddr, extra: &str) -> String {
    format!(
        "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
         servers:\n  lobby: {{ address: \"{backend}\" }}\nrouting:\n  try: [lobby]\n\
         limits:\n  connections-per-ip-per-second: 1000\n  concurrent-per-ip: 1000\n\
         virtual:\n  test-gate: true\n  test-gate-seconds: 0\n{extra}"
    )
}

/// One player through the whole gate; returns a line for the report.
async fn gate_run(addr: SocketAddr, protocol: i32, known: bool, idle: Duration) -> String {
    let m = module(protocol);
    let name = format!("V{protocol}{}", if known { "k" } else { "n" });
    let mut opts = JoinOptions::new(&name);
    opts.known_packs = known;
    let mut p = Player::join(addr, m.clone(), opts).await.unwrap();
    let what = format!("{protocol} known={known}");
    let report = through_gate(&mut p, &what, idle).await;
    // Release: the backend's login follows, no disconnect.
    p.command("gate release").await.unwrap();
    assert!(
        p.pump(WAIT, |p| p.logins.len() == 2).await.unwrap(),
        "{what}: {:?}",
        p.disconnect
    );
    assert_eq!(p.reconfigurations, 1);
    assert_eq!(p.logins[1].dimension, "minecraft:overworld");
    p.pump(Duration::from_millis(300), |_| false).await.unwrap();
    assert!(p.disconnect.is_none());
    p.close().await;
    format!("{what}: {report}, released")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_protocol_through_the_gate() {
    let idle = Duration::from_secs(
        std::env::var("PUMBO_VIRTUAL_IDLE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60),
    );
    let (backend_addr, log) = common::backend().await;
    let (_proxy, addr) = start_proxy(&config(backend_addr, "")).await;
    let mut runs = Vec::new();
    for v in pumbo_data::protocols() {
        for known in [true, false] {
            if !known && pumbo_data::full_registry_data(v).is_none() {
                eprintln!("{v}: no full registry data in this build, skipped without known packs");
                continue;
            }
            runs.push(tokio::spawn(gate_run(addr, v.0, known, idle)));
        }
    }
    let n = runs.len();
    for r in runs {
        eprintln!("{}", r.await.unwrap());
    }
    assert_eq!(log.joins.load(Ordering::SeqCst), n);
    // Nothing of the gate reached the backend.
    assert!(log.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn enter_from_a_server_and_back() {
    let (backend_addr, log) = common::backend().await;
    let (proxy, addr) = start_proxy(&config(backend_addr, "")).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Returner"))
        .await
        .unwrap();
    p.pump(WAIT, |p| !p.maps.is_empty()).await.unwrap();
    p.command("gate release").await.unwrap();
    assert!(p.pump(WAIT, |p| p.logins.len() == 2).await.unwrap());
    let id = proxy.find_player("Returner").unwrap().id;
    assert_eq!(
        proxy.find_player("Returner").unwrap().server.as_deref(),
        Some("lobby")
    );
    // From the server into the gate: start_configuration, then our login.
    pumbo_prox::world::enter_test_gate(&proxy, id);
    assert!(
        p.pump(WAIT, |p| p.logins.len() == 3).await.unwrap(),
        "{:?}",
        p.disconnect
    );
    assert_eq!(p.logins[2].dimension, "pumbo:virtual");
    assert_eq!(p.reconfigurations, 2);
    assert_eq!(proxy.find_player("Returner").unwrap().server, None);
    p.pump(WAIT, |p| p.maps.len() == 2).await.unwrap();
    p.command("gate release").await.unwrap();
    assert!(p.pump(WAIT, |p| p.logins.len() == 4).await.unwrap());
    assert_eq!(p.logins[3].dimension, "minecraft:overworld");
    assert_eq!(log.joins.load(Ordering::SeqCst), 2);
    assert!(p.disconnect.is_none());
}

#[tokio::test]
async fn release_timer_and_gate_limit() {
    let (backend_addr, _) = common::backend().await;
    let (_proxy, addr) = start_proxy(
        &config(backend_addr, "  test-gate-seconds: 2\n").replace("  test-gate-seconds: 0\n", ""),
    )
    .await;
    let mut p = Player::join(addr, module(767), JoinOptions::new("Timed"))
        .await
        .unwrap();
    assert!(
        p.pump(Duration::from_secs(10), |p| p.logins.len() == 2)
            .await
            .unwrap()
    );
    assert!(p.disconnect.is_none());

    let (backend_addr, _) = common::backend().await;
    let (_proxy, addr) = start_proxy(&config(backend_addr, "  gate-timeout-ms: 1500\n")).await;
    let mut p = Player::join(addr, module(775), JoinOptions::new("Slow"))
        .await
        .unwrap();
    p.pump(Duration::from_secs(10), |p| p.disconnect.is_some())
        .await
        .unwrap();
    assert!(
        p.disconnect
            .as_deref()
            .is_some_and(|d| d.contains("too long")),
        "{:?}",
        p.disconnect
    );
}

/// `[tab]` header and footer of the proxy: in the virtual world and again
/// after the release, replacing the backend's (without plugins: `&` codes).
#[tokio::test]
async fn proxy_tab_header() {
    let (backend_addr, _) = common::backend().await;
    let extra = "tab:\n  header: \"&eWelcome\"\n  footer: \"&7pumbo\"\n  refresh-ms: 200\n";
    let cfg = config(backend_addr, "").replace("virtual:", &format!("{extra}virtual:"));
    let (_proxy, addr) = start_proxy(&cfg).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Tabby"))
        .await
        .unwrap();
    assert!(p.pump(WAIT, |p| !p.tab_headers.is_empty()).await.unwrap());
    assert_eq!(p.tab_headers[0], "Welcome");
    p.command("gate release").await.unwrap();
    assert!(
        p.pump(WAIT, |p| p.logins.len() == 2 && p.tab_headers.len() == 2)
            .await
            .unwrap()
    );
    assert_eq!(p.tab_headers[1], "Welcome");
    // The refresh sends nothing while the text stays the same.
    p.pump(Duration::from_millis(800), |_| false).await.unwrap();
    assert_eq!(p.tab_headers.len(), 2);
}
