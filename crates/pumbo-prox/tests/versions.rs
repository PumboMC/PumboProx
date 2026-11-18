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

// ------------------------------------------------------- the Pumbox at join

/// The config file in a fresh directory of this test.
fn file(tag: &str, text: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("prox-versions-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pumboprox.yml");
    std::fs::write(&path, text).unwrap();
    path
}

/// The 26.3 server is down and the other speaks 26.2: the player waits in
/// the Pumbox and joins the 26.3 server once a reload brings it back.
#[tokio::test(flavor = "multi_thread")]
async fn a_join_whose_servers_all_fail_waits_in_the_pumbox() {
    let (old, _) = common::backend().await;
    let dead = down().await;
    let servers = |new| {
        config(
            &[("old", old, Some(776)), ("new", new, Some(777))],
            &["old", "new"],
            "",
        )
    };
    let path = file("join", &servers(dead));
    let (proxy, addr) = common::start_proxy_at(&servers(dead), Some(path.clone())).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Patient"))
        .await
        .unwrap();
    assert_eq!(p.logins[0].dimension, "pumbo:virtual");
    assert!(message(&mut p, NO_SERVER).await, "{:?}", p.messages);
    let (live, log) = common::backend().await;
    std::fs::write(&path, servers(live)).unwrap();
    proxy.reload().unwrap();
    assert!(
        p.pump(WAIT, |p| p.logins.len() == 2).await.unwrap(),
        "{:?}",
        p.disconnect
    );
    assert_eq!(p.logins[1].dimension, "minecraft:overworld");
    p.command("server").await.unwrap();
    assert!(message(&mut p, "You are on new.").await, "{:?}", p.messages);
    assert_eq!(log.joins.load(Ordering::SeqCst), 1);
    p.close().await;
    proxy.stop();
}

/// The server was down when the player joined and refuses them once it is
/// back: the player leaves the Pumbox with the server's reason.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_refuses_a_waiting_player_disconnects_with_its_reason() {
    let dead = down().await;
    let servers = |lobby| config(&[("lobby", lobby, Some(777))], &["lobby"], "");
    let path = file("refused", &servers(dead));
    let (proxy, addr) = common::start_proxy_at(&servers(dead), Some(path.clone())).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Waiting"))
        .await
        .unwrap();
    assert_eq!(p.logins[0].dimension, "pumbo:virtual");
    assert!(message(&mut p, NO_SERVER).await, "{:?}", p.messages);
    let strict = scripted(Some("You are banned here.")).await;
    std::fs::write(&path, servers(strict)).unwrap();
    proxy.reload().unwrap();
    assert!(
        p.pump(WAIT, |p| p.disconnect.is_some()).await.unwrap(),
        "{:?}",
        p.messages
    );
    assert_eq!(p.disconnect.as_deref(), Some("You are banned here."));
    proxy.stop();
}

/// The same for a server that takes the login of a waiting player and then
/// closes: the player is disconnected, not sent back to wait.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_drops_a_waiting_player_after_the_login_is_a_kick() {
    let dead = down().await;
    let servers = |lobby| config(&[("lobby", lobby, Some(777))], &["lobby"], "");
    let path = file("dropped", &servers(dead));
    let (proxy, addr) = common::start_proxy_at(&servers(dead), Some(path.clone())).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Dropped"))
        .await
        .unwrap();
    assert!(message(&mut p, NO_SERVER).await, "{:?}", p.messages);
    let closing = scripted(None).await;
    std::fs::write(&path, servers(closing)).unwrap();
    proxy.reload().unwrap();
    assert!(
        p.pump(WAIT, |p| p.disconnect.is_some()).await.unwrap(),
        "{:?}",
        p.messages
    );
    assert_eq!(p.disconnect.as_deref(), Some(NO_SERVER));
    proxy.stop();
}

/// A server without modern forwarding set up refuses nobody, but the proxy
/// cannot hand it the player: the player waits while the proxy retries.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_forwarding_set_up_keeps_the_player_waiting() {
    let (lobby, log) = common::backend().await;
    let secret = common::secret_file("versions-forwarding", "a-forwarding-secret-of-32-chars!");
    let cfg = config(&[("lobby", lobby, Some(777))], &["lobby"], "").replace(
        "  mode: none\n",
        &format!(
            "  mode: modern\n  secret-file: \"{}\"\n",
            common::toml_path(&secret)
        ),
    );
    let (proxy, addr) = common::start_proxy(&cfg).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Unforwarded"))
        .await
        .unwrap();
    assert_eq!(p.logins[0].dimension, "pumbo:virtual");
    assert!(message(&mut p, NO_SERVER).await, "{:?}", p.messages);
    assert_eq!(log.joins.load(Ordering::SeqCst), 0);
    p.close().await;
    proxy.stop();
}

// ------------------------------------------------------- versions.clients

/// The status a client of `protocol` gets: shown protocol and version name.
async fn status(addr: SocketAddr, protocol: i32) -> (i64, String) {
    let (_, json) = pumbo_testclient::status(addr, module(protocol))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    (
        v["version"]["protocol"].as_i64().unwrap(),
        v["version"]["name"].as_str().unwrap().to_string(),
    )
}

/// Off the list: refused before the login with the listed versions, and
/// shown the newest listed one in the server list.
#[tokio::test(flavor = "multi_thread")]
async fn versions_off_the_list_are_refused_before_the_login() {
    let (lobby, _) = common::backend().await;
    let cfg = config(
        &[("lobby", lobby, None)],
        &["lobby"],
        "versions:\n  clients: [\"1.21.4\"]\n",
    );
    let (proxy, addr) = common::start_proxy(&cfg).await;
    let err = refused(addr, 777, "Newer").await;
    assert!(err.starts_with("refused:"), "{err}");
    assert!(
        err.contains("This server supports Minecraft up to 1.21.4."),
        "{err}"
    );
    let err = refused(addr, 767, "Older").await;
    assert!(
        err.contains("Unsupported Minecraft version. Use 1.21.4."),
        "{err}"
    );
    let p = Player::join(addr, module(769), JoinOptions::new("Listed"))
        .await
        .unwrap();
    assert_eq!(p.logins[0].dimension, "minecraft:overworld");
    p.close().await;
    assert_eq!(status(addr, 777).await, (769, "PumboProx 1.21.4".into()));
    assert_eq!(status(addr, 769).await.0, 769);
    proxy.stop();
    // The owner's text, with the listed versions.
    let cfg = cfg.replace(
        "versions:\n",
        "versions:\n  unsupported-message: \"<red>Only {versions} here\"\n",
    );
    let (proxy, addr) = common::start_proxy(&cfg).await;
    let err = refused(addr, 777, "Newer").await;
    assert!(err.contains("Only 1.21.4 here"), "{err}");
    proxy.stop();
}

/// A reload changes the list for new connections; players online stay.
#[tokio::test(flavor = "multi_thread")]
async fn the_list_follows_reload_and_spares_players_online() {
    let (lobby, _) = common::backend().await;
    let open = config(&[("lobby", lobby, None)], &["lobby"], "");
    let path = file("reload", &open);
    let (proxy, addr) = common::start_proxy_at(&open, Some(path.clone())).await;
    let mut p = Player::join(addr, module(769), JoinOptions::new("Early"))
        .await
        .unwrap();
    std::fs::write(&path, format!("{open}versions:\n  clients: [\"26.3\"]\n")).unwrap();
    proxy.reload().unwrap();
    let err = refused(addr, 769, "Late").await;
    assert!(
        err.contains("Unsupported Minecraft version. Use 26.3."),
        "{err}"
    );
    assert_eq!(status(addr, 769).await.0, 777);
    p.command("server").await.unwrap();
    assert!(
        message(&mut p, "You are on lobby.").await,
        "{:?}",
        p.messages
    );
    std::fs::write(&path, &open).unwrap();
    proxy.reload().unwrap();
    let again = Player::join(addr, module(769), JoinOptions::new("Again"))
        .await
        .unwrap();
    again.close().await;
    p.close().await;
    proxy.stop();
}

/// Without servers only the list counts: a listed version waits in the
/// Pumbox (where a new network is claimed), any other is refused with the
/// list, never for a missing server.
#[tokio::test(flavor = "multi_thread")]
async fn a_network_without_servers_checks_only_the_list() {
    let list = "versions:\n  clients: [\"26.3\"]\n";
    for operators in [
        "",
        "commands:\n  operators: [\"069a79f4-44e9-4726-a5be-fca90e38aaf5\"]\n",
    ] {
        let cfg = config(&[], &[], &format!("{list}{operators}"));
        let (proxy, addr) = common::start_proxy(&cfg).await;
        let code = pumbo_prox::commands::claim_code(&proxy);
        assert_eq!(code.is_some(), operators.is_empty());
        let p = Player::join(addr, module(777), JoinOptions::new("Owner"))
            .await
            .unwrap();
        assert_eq!(p.logins[0].dimension, "pumbo:virtual");
        assert_eq!(
            pumbo_prox::commands::claim_open(&proxy, &proxy.runtime()),
            operators.is_empty()
        );
        let err = refused(addr, 769, "Older").await;
        assert!(
            err.contains("Unsupported Minecraft version. Use 26.3."),
            "{err}"
        );
        p.close().await;
        proxy.stop();
    }
}

/// The versions a refused client is told to use are listed ones.
#[tokio::test(flavor = "multi_thread")]
async fn the_version_hint_names_only_listed_versions() {
    let (mid, _) = common::backend().await;
    let (old, _) = common::backend().await;
    let cfg = config(
        &[("mid", mid, Some(775)), ("old", old, Some(776))],
        &["mid", "old"],
        "versions:\n  clients: [\"26.2\", \"26.3\"]\n",
    );
    let (proxy, addr) = common::start_proxy(&cfg).await;
    let err = refused(addr, 777, "Newer").await;
    assert!(err.contains("Please join with 26.2."), "{err}");
    proxy.stop();
    // No listed version has a server: the plain text, not an empty list.
    let cfg = config(
        &[("old", old, Some(776))],
        &["old"],
        "versions:\n  clients: [\"26.3\"]\n",
    );
    let (proxy, addr) = common::start_proxy(&cfg).await;
    let err = refused(addr, 777, "Newest").await;
    assert!(err.starts_with("refused:"), "{err}");
    assert!(err.contains(NO_SERVER), "{err}");
    assert!(!err.contains("Please join"), "{err}");
    proxy.stop();
}
