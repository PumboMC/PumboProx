//! The plugin host in the proxy (E5): gates and closed logins seen by a real
//! client through the login of the proxy. Gates run after the login, with
//! the client in configuration (E6, so a gate can hold it in a virtual
//! world); a refusal is a configuration `disconnect`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use common::{module, start_proxy};
use pumbo_protocol::packets::login::{LoginCompression, LoginDisconnect, LoginStart};
use pumbo_protocol::packets::status::Intention;
use pumbo_protocol::{PacketKind, Phase};
use pumbo_prox::plugins::Plugins;
use pumbo_testclient::client::Client;
use uuid::Uuid;

/// The test plugin of pumbo-host, built once for wasm32-wasip2.
fn test_plugin() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target/wasm-plugins");
        let status = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args([
                "build",
                "--locked",
                "-p",
                "pumbo-host-test-plugin",
                "--target",
                "wasm32-wasip2",
            ])
            .args(["--profile", "plugin", "--target-dir"])
            .arg(&target)
            .status()
            .expect("cargo build of the test plugin");
        assert!(status.success());
        target.join("wasm32-wasip2/plugin/pumbo_host_test_plugin.wasm")
    })
    .clone()
}

fn plugin_dir(tag: &str, with_wasm: bool) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-e5-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("auth.yml"),
        "id: auth\nversion: 0.1.0\napi: \"0.1\"\nevents: [gate, virtual-input]\ngate: { name: auth }\n",
    )
    .unwrap();
    if with_wasm {
        std::fs::copy(test_plugin(), dir.join("auth.wasm")).unwrap();
    }
    dir
}

fn config(dir: &std::path::Path) -> String {
    config_with(dir, "127.0.0.1:1")
}

fn config_with(dir: &std::path::Path, lobby: &str) -> String {
    format!(
        r#"
listener:
  - bind: "127.0.0.1:0"

login:
  online-mode: false

limits:
  connections-per-ip-per-second: 1000
  concurrent-per-ip: 1000

forwarding:
  mode: none

servers:
  lobby: {{ address: "{}" }}

routing:
  try: [lobby]

server-group:
  - name: all
    servers: [lobby]

plugins:
  dir: {:?}
  required-gates: [auth]
"#,
        lobby,
        dir.display().to_string()
    )
}

fn client_information(locale: &str) -> pumbo_protocol::packets::common::ClientInformation {
    pumbo_protocol::packets::common::ClientInformation {
        locale: locale.into(),
        view_distance: 2,
        chat_mode: 0,
        chat_colors: true,
        skin_parts: 0x7F,
        main_hand: 1,
        text_filtering: false,
        server_listing: true,
        particle_status: 0,
    }
}

/// Logs in and returns the first disconnect text (login or configuration).
async fn login(addr: SocketAddr, name: &str) -> Option<String> {
    login_with(addr, name, None).await
}

/// `settings_gap`: send `client_information` in its own write this long after the
/// acknowledgement (a client whose settings arrive in a later TCP segment).
async fn login_with(
    addr: SocketAddr,
    name: &str,
    settings_gap: Option<Duration>,
) -> Option<String> {
    let mut c = Client::connect(addr, module(777)).await.unwrap();
    c.send(&Intention {
        protocol: 777,
        address: "localhost".into(),
        port: 25565,
        intent: 2,
    })
    .await
    .unwrap();
    c.phase = Phase::Login;
    c.send(&LoginStart {
        name: name.into(),
        uuid: Uuid::nil(),
    })
    .await
    .unwrap();
    loop {
        let (k, f) = c.recv_within(Duration::from_secs(20)).await.unwrap();
        match k {
            Some(PacketKind::LoginCompression) => {
                let p: LoginCompression = c.decode(&f).unwrap();
                c.set_compression(p.threshold);
            }
            Some(PacketKind::LoginFinished) => break,
            Some(PacketKind::LoginDisconnect) => {
                let d: LoginDisconnect = c.decode(&f).unwrap();
                return Some(
                    pumbo_text::Component::from_json(&d.reason_json)
                        .unwrap()
                        .plain_text(),
                );
            }
            other => panic!("login: {other:?}"),
        }
    }
    // Settings in the same write as the acknowledgement, like the vanilla client.
    let mut out = c
        .frame(&pumbo_protocol::packets::login::LoginAcknowledged)
        .unwrap();
    c.phase = Phase::Configuration;
    if let Some(gap) = settings_gap {
        c.write_bytes(&out).await.unwrap();
        tokio::time::sleep(gap).await;
        out = Vec::new();
    }
    out.extend(c.frame(&client_information("pl_pl")).unwrap());
    c.write_bytes(&out).await.unwrap();
    loop {
        let Ok((k, f)) = c.recv_within(Duration::from_secs(20)).await else {
            return None;
        };
        if k == Some(PacketKind::Disconnect) {
            let d: pumbo_protocol::packets::common::Disconnect = c.decode(&f).unwrap();
            return Some(
                pumbo_text::Component::from_nbt(&d.reason)
                    .unwrap()
                    .plain_text(),
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_plugin_decides_at_login() {
    let dir = plugin_dir("gate", true);
    let text = config(&dir);
    let (proxy, addr) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin directory");
    proxy.plugins.set(plugins).unwrap();

    assert_eq!(
        login(addr, "deny_steve").await.as_deref(),
        Some("denied by test gate")
    );
    // The settings sent with `login_acknowledged` are read before the gates.
    assert_eq!(
        login(addr, "locale_steve").await.as_deref(),
        Some("locale pl_pl")
    );
    // ...and settings that arrive a moment later in their own segment.
    assert_eq!(
        login_with(addr, "locale_steve", Some(Duration::from_millis(100)))
            .await
            .as_deref(),
        Some("locale pl_pl")
    );
    // Passes the gate (held, then released by the plugin); then the proxy
    // tries the (unreachable) server.
    for name in ["hold_alex", "notch"] {
        let after = login(addr, name).await.unwrap_or_default();
        assert!(!after.contains("denied"), "{name}: {after}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_required_gate_closes_login() {
    let dir = plugin_dir("missing", false);
    let text = config(&dir);
    let (proxy, addr) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin directory");
    proxy.plugins.set(plugins).unwrap();
    let kick = login(addr, "steve").await.unwrap();
    assert!(kick.contains("temporarily unavailable"), "{kick}");
}

/// E6: a gate plugin holds the player in a virtual world through WIT
/// `virtual` (world, map, experience, title), the player's command comes back
/// as an input, `virtual.release` lets the gate pass and the player reaches
/// the server without a disconnect. For 767 and 777.
#[tokio::test(flavor = "multi_thread")]
async fn gate_plugin_holds_in_a_virtual_world() {
    let (backend, log) = common::backend().await;
    let dir = plugin_dir("virtual", true);
    let text = config_with(&dir, &backend.to_string());
    let (proxy, addr) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin directory");
    proxy.plugins.set(plugins).unwrap();
    for (i, protocol) in [767, 777].into_iter().enumerate() {
        let name = format!("virtual_{protocol}");
        let mut p = pumbo_testclient::Player::join(
            addr,
            module(protocol),
            pumbo_testclient::JoinOptions::new(&name),
        )
        .await
        .unwrap();
        assert_eq!(p.logins[0].dimension, "pumbo:virtual", "{protocol}");
        let wait = Duration::from_secs(15);
        assert!(
            p.pump(wait, |p| !p.maps.is_empty() && !p.titles.is_empty())
                .await
                .unwrap(),
            "{protocol}: map and title from the plugin: {:?}",
            p.disconnect
        );
        assert_eq!(p.titles[0], "Plugin world");
        assert_eq!(p.maps[0].patch.as_ref().unwrap().colors[0], 34);
        p.command("release").await.unwrap();
        assert!(
            p.pump(wait, |p| p.logins.len() == 2).await.unwrap(),
            "{protocol}: released: {:?}",
            p.disconnect
        );
        assert_eq!(p.logins[1].dimension, "minecraft:overworld");
        assert_eq!(log.joins.load(std::sync::atomic::Ordering::SeqCst), i + 1);
        p.pump(Duration::from_millis(300), |_| false).await.unwrap();
        assert!(p.disconnect.is_none());
        p.close().await;
    }
}

/// Placeholders in the MOTD come from the host's push values and caches
/// (§5.8.3): `%proxy_online%` and `%proxy_max%` without asking a plugin.
#[tokio::test(flavor = "multi_thread")]
async fn motd_placeholders() {
    let dir = plugin_dir("motd", true);
    let text = config(&dir).replace(
        "login:",
        "status:\n  motd: <gold>online %proxy_online% of %proxy_max%\n  max-players: 42\n\nlogin:",
    );
    let (proxy, addr) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin directory");
    proxy.plugins.set(plugins).unwrap();
    let (_, json) = pumbo_testclient::status(addr, module(777)).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let motd = pumbo_text::Component::from_json_value(&v["description"])
        .unwrap()
        .plain_text();
    assert_eq!(motd, "online 0 of 42", "{json}");
}

/// `plugin unload` on the console while the gate holds a player in its
/// virtual world: the player is kicked with the gate failure text, never let
/// through; logins stay closed until `plugin load`.
#[tokio::test(flavor = "multi_thread")]
async fn unloading_the_holding_gate_kicks() {
    let (backend, log) = common::backend().await;
    let dir = plugin_dir("unload", true);
    let text = config_with(&dir, &backend.to_string());
    let (proxy, addr) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin directory");
    proxy.plugins.set(plugins.clone()).unwrap();
    let mut p = pumbo_testclient::Player::join(
        addr,
        module(777),
        pumbo_testclient::JoinOptions::new("virtual_unload"),
    )
    .await
    .unwrap();
    let wait = Duration::from_secs(15);
    assert!(p.pump(wait, |p| !p.titles.is_empty()).await.unwrap());
    assert!(plugins.console("pumbo proxy plugin unload auth").is_some());
    p.pump(wait, |p| p.disconnect.is_some()).await.unwrap();
    let kick = p.disconnect.clone().unwrap_or_default();
    assert!(kick.contains("Could not verify the connection"), "{kick}");
    assert_eq!(log.joins.load(std::sync::atomic::Ordering::SeqCst), 0);
    let refused = login(addr, "steve").await.unwrap_or_default();
    assert!(refused.contains("temporarily unavailable"), "{refused}");

    assert!(plugins.console("pumbo proxy plugin load auth").is_some());
    let slot = plugins.host.plugin("auth").unwrap();
    for _ in 0..300 {
        if slot.status() == pumbo_host::Status::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let after = login(addr, "deny_steve").await.unwrap_or_default();
    assert_eq!(after, "denied by test gate");
}

/// A title, the action bar and a boss bar a gate sends before the client is
/// in play (still in configuration) reach it after it joins the world.
#[tokio::test(flavor = "multi_thread")]
async fn gate_messages_before_play_are_kept() {
    let dir = plugin_dir("early", true);
    let text = config(&dir);
    let (proxy, addr) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin directory");
    proxy.plugins.set(plugins).unwrap();
    for protocol in [767, 777] {
        let mut p = pumbo_testclient::Player::join(
            addr,
            module(protocol),
            pumbo_testclient::JoinOptions::new(&format!("early_{protocol}")),
        )
        .await
        .unwrap();
        let shown = |p: &pumbo_testclient::Player| {
            !p.titles.is_empty()
                && !p.bossbars.is_empty()
                && p.kinds
                    .contains(&(Phase::Play, PacketKind::SetActionBarText))
        };
        assert!(
            p.pump(Duration::from_secs(15), shown).await.unwrap(),
            "{protocol}: titles {:?}, bars {:?}, {:?}",
            p.titles,
            p.bossbars,
            p.disconnect
        );
        assert_eq!(p.titles[0], "Early title");
        assert_eq!(p.bossbars, ["early bar"]);
        p.close().await;
    }
}
