//! Which client versions get in and where a joining player ends up when no
//! server takes them: the Pumbox, or a disconnect with the reason.
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

use common::{message, module};
use pumbo_protocol::frame::FrameConfig;
use pumbo_protocol::packets::common::GameProfile;
use pumbo_protocol::packets::login::{
    LoginAcknowledged, LoginDisconnect, LoginFinished, LoginStart,
};
use pumbo_protocol::packets::status::Intention;
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::{Direction, Phase, VersionModule};
use pumbo_prox::conn::Conn;
use pumbo_testclient::{JoinOptions, Player};
use tokio::net::TcpListener;
use uuid::Uuid;

/// A hang guard only: every wait below ends on an event.
const WAIT: Duration = Duration::from_secs(15);

const NO_SERVER: &str = "No server is available right now.";

/// A proxy config with these servers (`protocol` when given) and `try` order.
fn config(servers: &[(&str, SocketAddr, Option<i32>)], order: &[&str], extra: &str) -> String {
    let list: String = servers
        .iter()
        .map(|(name, addr, protocol)| match protocol {
            Some(p) => format!("  {name}: {{ address: \"{addr}\", protocol: {p} }}\n"),
            None => format!("  {name}: {{ address: \"{addr}\" }}\n"),
        })
        .collect();
    let servers = if list.is_empty() {
        "servers: {}\n".to_string()
    } else {
        format!("servers:\n{list}")
    };
    format!(
        "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
         {servers}routing:\n  try: [{}]\n\
         limits:\n  connections-per-ip-per-second: 100000\n  concurrent-per-ip: 100000\n\
         \x20 status-per-ip-per-second: 100000\n{extra}",
        order.join(", ")
    )
}

/// A server that is down: it takes the connection and closes it at once.
async fn down() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    addr
}

/// A server that refuses every login with `refusal`, or with `None` takes
/// the login and then closes the connection.
async fn scripted(refusal: Option<&'static str>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = answer(stream, refusal).await;
            });
        }
    });
    addr
}

fn send<P: Packet>(c: &mut Conn, m: &dyn VersionModule, p: &P) {
    let id = m
        .packet_id(Phase::Login, Direction::Clientbound, P::KIND)
        .unwrap();
    let payload = packets::encode(p, &Ctx::new(m, Direction::Clientbound)).unwrap();
    c.queue(id, &payload).unwrap();
}

fn sb(m: &dyn VersionModule) -> Ctx<'_> {
    Ctx::new(m, Direction::Serverbound)
}

async fn answer(stream: tokio::net::TcpStream, refusal: Option<&str>) -> Result<(), String> {
    let e = |e: pumbo_prox::conn::ConnError| e.to_string();
    let mut c = Conn::new(
        Box::pin(stream),
        FrameConfig::from_client(),
        FrameConfig::from_backend(),
    );
    let (f, _) = c.read_frame().await.map_err(e)?;
    let hs: Intention =
        packets::decode(&f.payload, &sb(&*module(777))).map_err(|e| e.to_string())?;
    let m = module(hs.protocol);
    let (f, _) = c.read_frame().await.map_err(e)?;
    let start: LoginStart = packets::decode(&f.payload, &sb(&*m)).map_err(|e| e.to_string())?;
    if let Some(text) = refusal {
        let reason_json = serde_json::json!({ "text": text }).to_string();
        send(&mut c, &*m, &LoginDisconnect { reason_json });
        return c.flush().await.map_err(e);
    }
    send(
        &mut c,
        &*m,
        &LoginFinished {
            profile: GameProfile {
                id: start.uuid,
                name: start.name,
                properties: Vec::new(),
            },
            strict_error_handling: true,
            session_id: Some(Uuid::nil()),
        },
    );
    c.flush().await.map_err(e)?;
    let (f, _) = c.read_frame().await.map_err(e)?;
    let _: LoginAcknowledged = packets::decode(&f.payload, &sb(&*m)).map_err(|e| e.to_string())?;
    Ok(())
}

/// The error of a join that did not reach play.
async fn refused(addr: SocketAddr, protocol: i32, name: &str) -> String {
    match Player::join(addr, module(protocol), JoinOptions::new(name)).await {
        Err(e) => e.to_string(),
        Ok(p) => panic!("{name} joined: {:?}", p.logins),
    }
}

// ------------------------------------------------- joins without a server

/// Only a server outside `routing.try` takes 26.3: the client still logs in
/// and waits in the Pumbox.
#[tokio::test(flavor = "multi_thread")]
async fn a_version_only_a_server_outside_try_takes_waits_in_the_pumbox() {
    let (old, _) = common::backend().await;
    let (new, _) = common::backend().await;
    let cfg = config(
        &[("old", old, Some(776)), ("new", new, Some(777))],
        &["old"],
        "",
    );
    let (proxy, addr) = common::start_proxy(&cfg).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Waiter"))
        .await
        .unwrap();
    assert_eq!(p.logins[0].dimension, "pumbo:virtual");
    assert!(message(&mut p, NO_SERVER).await, "{:?}", p.messages);
    p.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_refuses_at_join_disconnects_with_its_reason() {
    let lobby = scripted(Some("You are banned here.")).await;
    let (proxy, addr) =
        common::start_proxy(&config(&[("lobby", lobby, Some(777))], &["lobby"], "")).await;
    let err = refused(addr, 777, "Banned").await;
    assert!(err.contains("You are banned here."), "{err}");
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_pumbox_off_a_join_to_servers_that_are_down_is_a_kick() {
    let lobby = down().await;
    let cfg = config(
        &[("lobby", lobby, Some(777))],
        &["lobby"],
        "pumbox:\n  enabled: false\n",
    );
    let (proxy, addr) = common::start_proxy(&cfg).await;
    let err = refused(addr, 777, "Kicked").await;
    assert!(err.contains(NO_SERVER), "{err}");
    proxy.stop();
}

/// A server that took the login and then went away is not tried again from
/// the Pumbox: the player is disconnected.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_takes_the_login_and_closes_is_a_kick() {
    let lobby = scripted(None).await;
    let (proxy, addr) =
        common::start_proxy(&config(&[("lobby", lobby, Some(777))], &["lobby"], "")).await;
    // The client was already handed to it, so this is a lost connection.
    let err = refused(addr, 777, "Dropped").await;
    assert!(err.contains("Lost connection to the server."), "{err}");
    proxy.stop();
}

/// The same from a gate's world, where the proxy may already have sent the
/// client to configuration for the server.
#[tokio::test(flavor = "multi_thread")]
async fn through_the_test_gate_a_server_that_takes_the_login_and_closes_is_a_kick() {
    let lobby = scripted(None).await;
    let cfg = config(
        &[("lobby", lobby, Some(777))],
        &["lobby"],
        "virtual:\n  test-gate: true\n  test-gate-seconds: 0\n",
    );
    let (proxy, addr) = common::start_proxy(&cfg).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Gated"))
        .await
        .unwrap();
    p.command("gate release").await.unwrap();
    assert!(
        p.pump(WAIT, |p| p.disconnect.is_some()).await.unwrap(),
        "{:?} {:?}",
        p.logins.len(),
        p.messages
    );
    assert_eq!(p.disconnect.as_deref(), Some(NO_SERVER));
    proxy.stop();
}

// ------------------------------------------------------- the handshake

/// The only server speaks 26.2: a 26.3 client is refused before its login
/// with the text it got after the login before, and nothing counts it as
/// logged in.
#[tokio::test(flavor = "multi_thread")]
async fn a_version_no_server_takes_is_refused_before_the_login() {
    let (lobby, _) = common::backend().await;
    let (proxy, addr) =
        common::start_proxy(&config(&[("lobby", lobby, Some(776))], &["lobby"], "")).await;
    let err = refused(addr, 777, "Newer").await;
    // `refused:` is a `login_disconnect`; a kick after the login reads `no play login`.
    assert!(err.starts_with("refused:"), "{err}");
    assert!(
        err.contains("no server for Minecraft 26.3. Please join with 26.2."),
        "{err}"
    );
    let m = &proxy.metrics;
    assert_eq!(m.logins_offline.load(Ordering::Relaxed), 0);
    assert_eq!(m.logins_denied.load(Ordering::Relaxed), 1);
    let p = Player::join(addr, module(776), JoinOptions::new("Older"))
        .await
        .unwrap();
    assert_eq!(p.logins[0].dimension, "minecraft:overworld");
    p.close().await;
    proxy.stop();
}
