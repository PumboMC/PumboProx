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

/// A login dropped by its timeout while a plugin works on the profile
/// leaves no player behind in the plugin host.
#[tokio::test(flavor = "multi_thread")]
async fn cancelled_login_leaves_no_player() {
    let dir = plugin_dir("cancel", true);
    std::fs::write(
        dir.join("auth.yml"),
        "id: auth\nversion: 0.1.0\napi: \"0.1\"\nevents: [gate, profile]\ngate: { name: auth }\nprofile: { priority: 0 }\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("auth")).unwrap();
    std::fs::write(dir.join("auth/config.yml"), "tag: slow\n").unwrap();
    let text = config(&dir);
    let (proxy, _) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin directory");
    let profile = pumbo_core::profile::GameProfile {
        id: Uuid::from_u128(7),
        name: "steve".into(),
        properties: Vec::new(),
    };
    let conn = pumbo_prox::plugins::connection("127.0.0.1:5000".parse().unwrap(), "localhost", 0);
    let login = tokio::time::timeout(
        Duration::from_millis(300),
        plugins.joined(profile, false, conn),
    );
    assert!(login.await.is_err(), "the login timed out");
    for _ in 0..100 {
        if plugins.host.player(1).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        plugins.host.player(1).is_none(),
        "the host forgot the player"
    );
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

/// A reload reads the whole file, the host sections too, before it changes
/// anything: a broken host section keeps the old config. A good file applies
/// the log level at once and names the changes that wait for a restart.
#[tokio::test(flavor = "multi_thread")]
async fn reload_checks_the_whole_file_first() {
    let dir = plugin_dir("reload", false);
    let perms = dir.join("permissions.yml");
    let text = format!(
        "{}\npermissions:\n  file: {:?}\n",
        config(&dir),
        perms.display().to_string()
    );
    let file = dir.join("pumboprox.yml");
    std::fs::write(&file, &text).unwrap();
    let (proxy, _) = common::start_proxy_at(&text, Some(file.clone())).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin directory");
    proxy.plugins.set(plugins).unwrap();

    let more = text.replace(
        "servers:\n",
        "servers:\n  extra: { address: \"127.0.0.1:2\" }\n",
    );
    let broken = more.replace(
        "required-gates: [auth]",
        "required-gates: [auth]\n  memory-mb: lots",
    );
    std::fs::write(&file, broken).unwrap();
    let err = proxy.reload().unwrap_err();
    assert!(err.contains("invalid u32 at line"), "{err}");
    assert!(!proxy.runtime().config.servers.contains_key("extra"));
    // A permissions file that does not load: the old config stays too.
    std::fs::write(&file, &more).unwrap();
    std::fs::write(&perms, "groups: [").unwrap();
    assert!(proxy.reload().is_err());
    assert!(!proxy.runtime().config.servers.contains_key("extra"));
    std::fs::remove_file(&perms).unwrap();

    let good = more
        .replace("127.0.0.1:0", "127.0.0.1:25659")
        .replace("routing:", "logging:\n  level: error\n\nrouting:");
    std::fs::write(&file, good).unwrap();
    assert_eq!(proxy.reload().unwrap(), ["listener"]);
    assert!(proxy.runtime().config.servers.contains_key("extra"));
    let level_now = tracing::enabled!(tracing::Level::WARN);
    let test_level = std::env::var("PUMBO_TEST_LOG").unwrap_or_else(|_| "warn".into());
    pumbo_prox::logging::set_level(&test_level).unwrap();
    assert!(!level_now, "logging.level applies with the reload");
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
    assert!(
        Some(plugins.admin(
            None,
            &["plugin".into(), "unload".into(), "auth".into()],
            &|_| true
        ))
        .is_some()
    );
    p.pump(wait, |p| p.disconnect.is_some()).await.unwrap();
    let kick = p.disconnect.clone().unwrap_or_default();
    assert!(kick.contains("Could not verify the connection"), "{kick}");
    assert_eq!(log.joins.load(std::sync::atomic::Ordering::SeqCst), 0);
    let refused = login(addr, "steve").await.unwrap_or_default();
    assert!(refused.contains("temporarily unavailable"), "{refused}");

    assert!(
        Some(plugins.admin(
            None,
            &["plugin".into(), "load".into(), "auth".into()],
            &|_| true
        ))
        .is_some()
    );
    let slot = plugins.host.plugin("auth").unwrap();
    for _ in 0..300 {
        if slot.status() == pumbo_host::Status::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        slot.status(),
        pumbo_host::Status::Running,
        "auth did not start again"
    );
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

/// Every start makes a missing plugin directory: the host runs and
/// `prox plugins load` has a place.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_plugin_directory_is_created() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-e5-created-{}", std::process::id()))
        .join("plugins");
    assert!(!dir.exists());
    let text = format!(
        "listener:\n  - bind: \"127.0.0.1:0\"\nforwarding:\n  mode: none\nplugins:\n  dir: {:?}\n",
        dir.display().to_string()
    );
    let (proxy, _) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin host");
    assert!(dir.is_dir());
    assert!(plugins.host.plugins().is_empty());
    proxy.stop();
}

/// A proxy without servers (players wait in the Pumbox) and the test plugin
/// as `perms` with `short-name: perms`; `extra` goes into the config.
async fn pumbox_with_plugin(
    tag: &str,
    extra: &str,
) -> (
    std::sync::Arc<pumbo_prox::server::Proxy>,
    SocketAddr,
    std::sync::Arc<Plugins>,
) {
    let manifest = "id: perms\nversion: 0.1.0\napi: \"0.1\"\nshort-name: perms\n";
    pumbox_with(tag, extra, &test_plugin(), "perms", Some(manifest)).await
}

/// A proxy without servers and the plugin `wasm` as `id`, with `manifest`
/// next to it or the one built in.
async fn pumbox_with(
    tag: &str,
    extra: &str,
    wasm: &std::path::Path,
    id: &str,
    manifest: Option<&str>,
) -> (
    std::sync::Arc<pumbo_prox::server::Proxy>,
    SocketAddr,
    std::sync::Arc<Plugins>,
) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-e6-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(m) = manifest {
        std::fs::write(dir.join(format!("{id}.yml")), m).unwrap();
    }
    std::fs::copy(wasm, dir.join(format!("{id}.wasm"))).unwrap();
    let text = format!(
        "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
         limits:\n  connections-per-ip-per-second: 1000\n  concurrent-per-ip: 1000\n\
         plugins:\n  dir: {:?}\n{extra}",
        dir.display().to_string()
    );
    let (proxy, addr) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin host");
    proxy.plugins.set(plugins.clone()).unwrap();
    let slot = plugins.host.plugin(id).unwrap();
    for _ in 0..600 {
        if slot.status() == pumbo_host::Status::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(slot.status(), pumbo_host::Status::Running);
    (proxy, addr, plugins)
}

/// Tab completion of `text` as the client asks for it.
async fn suggest(p: &mut pumbo_testclient::Player, id: i32, text: &str) -> Vec<String> {
    let before = p.suggestions.len();
    p.c.send(&pumbo_protocol::packets::play::CommandSuggestion {
        id,
        text: text.into(),
    })
    .await
    .unwrap();
    assert!(
        p.pump(Duration::from_secs(15), |p| p.suggestions.len() > before)
            .await
            .unwrap(),
        "no answer to {text}"
    );
    let last = p.suggestions.last().unwrap();
    assert_eq!(last.id, id);
    last.matches.iter().map(|m| m.text.clone()).collect()
}

/// `/pp <TAB>` asks the plugin's `suggest` first, then offers the plugin's
/// subcommands, not player names; past them the plugin's `suggest` answers,
/// or player names when it has nothing.
#[tokio::test(flavor = "multi_thread")]
async fn pp_tab_lists_subcommands() {
    let (proxy, addr, plugins) = pumbox_with_plugin("pp-tab", "").await;
    for line in ["reg pp", "reg user umbrella", "reg group umbrella"] {
        plugins.console(line);
    }
    let uuid = pumbo_identity::offline_uuid("Steve");
    let mut p = pumbo_testclient::Player::join(
        addr,
        module(777),
        pumbo_testclient::JoinOptions::new("Steve"),
    )
    .await
    .unwrap();
    for _ in 0..600 {
        if plugins
            .visible_commands(uuid)
            .iter()
            .any(|(n, subs)| n == "pp" && subs.len() == 2)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(suggest(&mut p, 1, "/pp ").await, ["group", "user"]);
    assert_eq!(suggest(&mut p, 2, "/pp U").await, ["user"]);
    assert_eq!(suggest(&mut p, 3, "/pp user St").await, ["Steve"]);
    let echo = suggest(&mut p, 4, "/echo a  b").await;
    assert!(
        echo.len() == 1 && echo[0].starts_with("echo|a  b|"),
        "{echo:?}"
    );
    let first = suggest(&mut p, 5, "/echo ").await;
    assert!(
        first.len() == 1 && first[0].starts_with("echo||"),
        "{first:?}"
    );
    p.close().await;
    proxy.stop();
}

/// An operator from `commands.operators` uses `/prox plugins` and `/prox
/// debug`, which the `/prox` help shows them, without a permission entry;
/// another player is refused both.
#[tokio::test(flavor = "multi_thread")]
async fn operator_can_use_prox_plugins() {
    let uuid = pumbo_identity::offline_uuid("Owner");
    let (proxy, addr, _plugins) =
        pumbox_with_plugin("operator", &format!("commands:\n  operators: [{uuid}]\n")).await;
    let mut p = pumbo_testclient::Player::join(
        addr,
        module(777),
        pumbo_testclient::JoinOptions::new("Owner"),
    )
    .await
    .unwrap();
    p.command("prox plugins").await.unwrap();
    assert!(
        common::message(&mut p, "perms 0.1.0").await,
        "{:?}",
        p.messages
    );
    p.command("prox debug services").await.unwrap();
    assert!(
        common::message(&mut p, "Services:").await,
        "{:?}",
        p.messages
    );
    p.command("prox plugins reload perms").await.unwrap();
    assert!(
        common::message(&mut p, "perms reloaded").await,
        "{:?}",
        p.messages
    );
    assert!(
        !p.messages.iter().any(|m| m.contains("permission")),
        "{:?}",
        p.messages
    );
    let mut guest = pumbo_testclient::Player::join(
        addr,
        module(777),
        pumbo_testclient::JoinOptions::new("Guest"),
    )
    .await
    .unwrap();
    for line in ["prox plugins", "prox debug services"] {
        let before = guest.messages.len();
        guest.command(line).await.unwrap();
        assert!(
            guest
                .pump(Duration::from_secs(15), |g| g.messages[before..]
                    .iter()
                    .any(|m| m.contains("You do not have permission")))
                .await
                .unwrap(),
            "{line}: {:?}",
            guest.messages
        );
    }
    assert!(
        !guest
            .messages
            .iter()
            .any(|m| m.contains("perms 0.1.0") || m.contains("Services:")),
        "{:?}",
        guest.messages
    );
    guest.close().await;
    p.close().await;
    proxy.stop();
}

/// `/pp` arguments complete on the proxy as on Pumpkin: online players, then
/// groups and permission nodes; the first word only what the player may use
/// (`PUMBO_PERMS_WASM`: a built `pumbo-perms.wasm`; without it the test does
/// nothing).
#[tokio::test(flavor = "multi_thread")]
async fn pp_tab_completes_arguments() {
    let Ok(wasm) = std::env::var("PUMBO_PERMS_WASM") else {
        eprintln!("[skip] PUMBO_PERMS_WASM is not set");
        return;
    };
    let (proxy, addr, plugins) = pumbox_with(
        "pp-args",
        "",
        std::path::Path::new(&wasm),
        "pumbo-perms",
        None,
    )
    .await;
    let mut p = pumbo_testclient::Player::join(
        addr,
        module(777),
        pumbo_testclient::JoinOptions::new("Steve"),
    )
    .await
    .unwrap();
    let uuid = pumbo_identity::offline_uuid("Steve");
    for line in [
        "pp user Steve permission set * true",
        "pp creategroup vip",
        "pp group vip permission set minecraft:command.gamemode true",
    ] {
        plugins.console(line);
    }
    for _ in 0..600 {
        if plugins.permission(uuid, "pumbo.perms.user.info", None) == Some(true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        suggest(&mut p, 1, "/pp user ")
            .await
            .contains(&"Steve".to_string())
    );
    assert!(
        suggest(&mut p, 2, "/pp user Steve parent add ")
            .await
            .contains(&"vip".to_string())
    );
    assert!(
        suggest(
            &mut p,
            3,
            "/pp user Steve permission set minecraft:command.game"
        )
        .await
        .contains(&"minecraft:command.gamemode".to_string())
    );
    let mut alex = pumbo_testclient::Player::join(
        addr,
        module(777),
        pumbo_testclient::JoinOptions::new("Alex"),
    )
    .await
    .unwrap();
    plugins.console("pp user Alex permission set pumbo.perms.user.info true");
    let alex_uuid = pumbo_identity::offline_uuid("Alex");
    for _ in 0..600 {
        if plugins.permission(alex_uuid, "pumbo.perms.user.info", None) == Some(true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut first = suggest(&mut alex, 4, "/pp ").await;
    first.sort();
    assert_eq!(first, ["help", "user"]);
    alex.close().await;
    p.close().await;
    proxy.stop();
}

/// An offline account claims the proxy only while the required login gate
/// `auth` runs; without it the claim is refused and the code stays.
#[tokio::test(flavor = "multi_thread")]
async fn offline_claim_needs_a_running_login_gate() {
    use pumbo_prox::commands::{self, Source};
    for (tag, wasm) in [("claim-gate", true), ("claim-no-gate", false)] {
        let dir = plugin_dir(tag, wasm);
        let text = format!(
            "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\nplugins:\n  dir: {:?}\n  required-gates: [auth]\n",
            dir.display().to_string()
        );
        let path = dir.join("pumboprox.yml");
        std::fs::write(&path, &text).unwrap();
        let (proxy, _) = common::start_proxy_at(&text, Some(path)).await;
        let plugins = Plugins::start(&text, &proxy, Vec::new())
            .await
            .unwrap()
            .expect("plugin directory");
        proxy.plugins.set(plugins).unwrap();
        if wasm {
            let slot = proxy.plugins.get().unwrap().host.plugin("auth").unwrap();
            for _ in 0..600 {
                if slot.status() == pumbo_host::Status::Running {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let code = commands::claim_code(&proxy).unwrap();
        let id = pumbo_identity::offline_uuid("Owner");
        let rt = proxy.runtime();
        let out = commands::run(
            &proxy,
            &rt,
            &Source::Player { id, server: None },
            &format!("prox claim {code}"),
        )
        .unwrap();
        let text = out.lines[0].plain_text();
        if wasm {
            assert!(text.contains("now an operator"), "{text}");
            assert!(proxy.runtime().operators.contains(&id));
        } else {
            assert!(text.contains("not premium"), "{text}");
            assert!(commands::claim_open(&proxy, &proxy.runtime()));
        }
        proxy.stop();
    }
}
