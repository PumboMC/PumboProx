//! Host tests, part A of E5: actors, deadlines, failures, restarts, limits,
//! reload, required gates, HTTP rules and profile patches (plan §4, §7 E5).
//!
//! Load test length: `PUMBO_LOAD_PLAYERS` (default 100) and `PUMBO_LOAD_SECS`
//! (default 5); the plan's run is 500 players for 600 s.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::*;
use pumbo_host::manifest::EventKind;
use pumbo_host::wit::events::{ChatReply, ConnectEvent, PreLoginEvent};
use pumbo_host::{CommandOutcome, CommandSender, ConnectDecision, GateOutcome, PreLogin, Status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

const LONG: Duration = Duration::from_secs(120);

/// Plan §4.4 item 9: `on-server-connect` calls `connect` (leading to another
/// `on-server-connect` of the same plugin), `on-command` registers commands,
/// `on-chat` sends chat back to the player (fed in as chat again), for many
/// players at once. Zero hangs, no leaked tasks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reentrancy_under_load() {
    let env = Env::new("reentrancy");
    env.plugin("loop", "test", r#"events: [server-connect, chat]"#);
    let mut cfg = env.host_config("");
    // A loaded CI machine can miss the default deadlines (chat 200 ms, connect 2 s); a
    // missed deadline lets the event through, which is not what this test is about.
    cfg.plugins.timeouts.insert(EventKind::ServerConnect, 8_000);
    cfg.plugins.timeouts.insert(EventKind::Chat, 8_000);
    let (host, bridge) = start(cfg).await;
    assert_eq!(host.plugin("loop").unwrap().status(), Status::Running);
    let players = env_num("PUMBO_LOAD_PLAYERS", 100);
    let secs = env_num("PUMBO_LOAD_SECS", 5);
    for i in 1..=players {
        host.player_joined(player(i, &format!("p{i}"), Some("lobby")));
    }
    let baseline = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let hangs = Arc::new(AtomicU64::new(0));
    let rounds = Arc::new(AtomicU64::new(0));
    let until = Instant::now() + Duration::from_secs(secs);
    let mut tasks = Vec::new();
    for i in 1..=players {
        let (host, hangs, rounds) = (host.clone(), hangs.clone(), rounds.clone());
        tasks.push(tokio::spawn(async move {
            while Instant::now() < until {
                let e = ConnectEvent {
                    player: i,
                    target: "loop".into(),
                    reason: "test".into(),
                };
                match tokio::time::timeout(Duration::from_secs(10), host.on_server_connect(e)).await
                {
                    Ok(ConnectDecision::Deny(t)) => assert_eq!(t.plain_text(), "moved to lobby"),
                    Ok(other) => panic!("unexpected {other:?}"),
                    Err(_) => {
                        hangs.fetch_add(1, Ordering::Relaxed);
                    }
                }
                let out =
                    host.dispatch_command(CommandSender::Player(i), &format!("/reg c{}", i % 20));
                assert_eq!(out, CommandOutcome::Handled);
                match tokio::time::timeout(
                    Duration::from_secs(10),
                    host.on_chat(i, "echo:3".into()),
                )
                .await
                {
                    Ok(r) => assert_eq!(r, ChatReply::Cancel),
                    Err(_) => {
                        hangs.fetch_add(1, Ordering::Relaxed);
                    }
                }
                rounds.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let rounds = rounds.load(Ordering::Relaxed);
    eprintln!(
        "[measure] {players} players, {secs} s: {rounds} rounds ({} connects through the plugin)",
        bridge.connects.load(Ordering::Relaxed)
    );
    assert_eq!(hangs.load(Ordering::Relaxed), 0, "hangs");
    assert!(rounds > players);
    // Echo chains end by themselves; then the task count is back to the baseline.
    let mut alive = 0;
    for _ in 0..200 {
        alive = tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks();
        if alive <= baseline + 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        alive <= baseline + 2,
        "tasks leaked: {alive} alive, baseline {baseline}"
    );
    assert_eq!(host.on_chat(1, "hello".into()).await, ChatReply::Pass);
    assert_eq!(host.plugin("loop").unwrap().status(), Status::Running);
}

/// An endless loop is stopped by the time budget, the plugin restarts and
/// the host and other plugins keep answering.
#[tokio::test(flavor = "multi_thread")]
async fn endless_loop_is_interrupted() {
    let env = Env::new("spin");
    env.plugin("spin", "test", r#"events: [chat]"#);
    env.plugin("other", "test", r#"events: [chat]"#);
    env.config_file("other", "config.yml", "prefix: o\n");
    let (host, bridge) = start(env.host_config("")).await;
    assert_eq!(host.plugin("other").unwrap().status(), Status::Running);
    host.player_joined(player(1, "a", Some("lobby")));
    let started = Instant::now();
    assert_eq!(
        fail_by(&host, "spin", CommandSender::Player(1), "/spin").await,
        CommandOutcome::Handled
    );
    let trapped = started.elapsed();
    eprintln!("[measure] endless loop stopped after {trapped:?}");
    assert!(trapped < Duration::from_secs(2), "{trapped:?}");
    // The other plugin works during the restart.
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), "/oecho hi"),
        CommandOutcome::Handled
    );
    wait_for("echo of the other plugin", LONG, || {
        bridge.messages(1).contains(&"echo hi".to_string())
    })
    .await;
    wait_status(&host, "spin", LONG, |s| *s == Status::Running).await;
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), "/echo back"),
        CommandOutcome::Handled
    );
    wait_for("echo after restart", LONG, || {
        bridge.messages(1).contains(&"echo back".to_string())
    })
    .await;
}

/// A panic during a gate denies the player (never passes), the plugin
/// restarts; repeated failures disable it and close logins until a reload.
#[tokio::test(flavor = "multi_thread")]
async fn panic_denies_gate_and_restarts() {
    let env = Env::new("panic");
    env.plugin(
        "auth",
        "test",
        "events: [gate]\ngate: { name: auth, priority: 100 }",
    );
    let mut cfg = env.host_config("");
    cfg.plugins.required_gates = vec!["auth".into()];
    cfg.plugins.max_failures = 3;
    let (host, _) = start(cfg).await;
    assert!(host.login_open().is_ok());
    host.player_joined(player(1, "slow_a", None));
    let h = host.clone();
    let gate = tokio::spawn(async move { h.run_gates(1).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        host.dispatch_command(CommandSender::Console, "/panic"),
        CommandOutcome::Handled
    );
    let r = tokio::time::timeout(LONG, gate).await.unwrap().unwrap();
    assert!(matches!(r, GateOutcome::Deny(_)), "{r:?}");

    wait_status(&host, "auth", LONG, |s| *s == Status::Running).await;
    host.player_joined(player(2, "b", None));
    assert_eq!(host.run_gates(2).await, GateOutcome::Pass);
    host.player_joined(player(3, "deny_c", None));
    assert!(matches!(host.run_gates(3).await, GateOutcome::Deny(_)));

    for _ in 0..2 {
        wait_status(&host, "auth", LONG, |s| *s == Status::Running).await;
        assert_eq!(
            fail_by(&host, "auth", CommandSender::Console, "/panic").await,
            CommandOutcome::Handled
        );
    }
    let s = wait_status(&host, "auth", LONG, |s| matches!(s, Status::Disabled(_))).await;
    eprintln!("[result] after 3 failures: {s:?}");
    let closed = host.login_open().unwrap_err();
    assert!(closed.plain_text().contains("temporarily unavailable"));
    assert!(matches!(host.run_gates(2).await, GateOutcome::Deny(_)));
    let pre = host
        .on_pre_login(PreLoginEvent {
            connection: connection(),
            name: "x".into(),
            claimed_uuid: None,
        })
        .await;
    assert!(matches!(pre, PreLogin::Deny(_)));

    host.reload_plugin("auth").await.unwrap();
    assert!(host.login_open().is_ok());
    assert_eq!(host.run_gates(2).await, GateOutcome::Pass);
}

/// `/prox plugins unload|load|reload <id>`: unloading the gate that holds a
/// player denies it (never a pass) and closes logins until `load`; players
/// need `pumbo.proxy.plugins.manage`.
#[tokio::test(flavor = "multi_thread")]
async fn plugin_command_unloads_and_loads() {
    let env = Env::new("plugin-cmd");
    env.plugin("auth", "test", "events: [gate]\ngate: { name: auth }");
    let mut cfg = env.host_config("");
    cfg.plugins.required_gates = vec!["auth".into()];
    let (host, _) = start(cfg).await;
    let words = |l: &str| l.split(' ').map(str::to_string).collect::<Vec<_>>();
    let admin = |line: &str| host.proxy_admin(CommandSender::Console, &words(line));
    host.player_joined(player(1, "never_a", None));
    let h = host.clone();
    let gate = tokio::spawn(async move { h.run_gates(1).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(admin("plugin unload auth"), CommandOutcome::Handled);
    let r = tokio::time::timeout(LONG, gate).await.unwrap().unwrap();
    assert!(matches!(r, GateOutcome::Deny(_)), "{r:?}");
    wait_status(&host, "auth", LONG, |s| matches!(s, Status::Disabled(_))).await;
    assert!(host.login_open().is_err());
    // A later unload changes nothing, a player without the node is refused.
    admin("plugin unload auth");
    assert!(matches!(
        host.proxy_admin(CommandSender::Player(1), &words("plugin load auth")),
        CommandOutcome::Refused(_)
    ));
    assert!(matches!(
        admin("plugin load nope"),
        CommandOutcome::Reply(_)
    ));
    assert_eq!(admin("plugin load auth"), CommandOutcome::Handled);
    wait_status(&host, "auth", LONG, |s| *s == Status::Running).await;
    assert!(host.login_open().is_ok());
    host.player_joined(player(2, "b", None));
    assert_eq!(host.run_gates(2).await, GateOutcome::Pass);
}

/// Gate timeout from the proxy config is a hard limit: a slow gate denies.
#[tokio::test(flavor = "multi_thread")]
async fn gate_deadline_and_hold() {
    let env = Env::new("gate-deadline");
    env.plugin("auth", "test", "events: [gate]\ngate: { name: auth }");
    let mut cfg = env.host_config("");
    cfg.plugins.gate_timeouts.insert("auth".into(), 300);
    let (host, _) = start(cfg).await;
    host.player_joined(player(1, "slow_a", None));
    let t = Instant::now();
    assert!(matches!(host.run_gates(1).await, GateOutcome::Deny(_)));
    assert!(t.elapsed() < Duration::from_secs(1));
    // hold + release after 50 ms passes; hold without release times out.
    host.player_joined(player(2, "hold_b", None));
    assert_eq!(host.run_gates(2).await, GateOutcome::Pass);
    host.player_joined(player(3, "never_c", None));
    let t = Instant::now();
    assert!(matches!(host.run_gates(3).await, GateOutcome::Deny(_)));
    assert!(t.elapsed() >= Duration::from_millis(250));
}

/// A required gate whose module is missing closes logins, and its sensitive
/// commands are swallowed instead of going to a backend.
#[tokio::test(flavor = "multi_thread")]
async fn missing_required_gate_closes_logins() {
    let env = Env::new("missing");
    env.plugin(
        "auth",
        "",
        "events: [gate]\ngate: { name: auth }\nsensitive-commands: [login, l]",
    );
    let mut cfg = env.host_config("");
    cfg.plugins.required_gates = vec!["auth".into()];
    let (host, _) = start(cfg).await;
    assert_eq!(host.plugin("auth").unwrap().status(), Status::Missing);
    assert!(host.login_open().is_err());
    let pre = host
        .on_pre_login(PreLoginEvent {
            connection: connection(),
            name: "x".into(),
            claimed_uuid: None,
        })
        .await;
    assert!(matches!(pre, PreLogin::Deny(_)));
    host.player_joined(player(1, "x", None));
    assert!(matches!(host.run_gates(1).await, GateOutcome::Deny(_)));
    for line in ["/login secret", "/L secret", "login"] {
        assert!(
            matches!(
                host.dispatch_command(CommandSender::Player(1), line),
                CommandOutcome::Refused(_)
            ),
            "{line}"
        );
    }
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), "/spawn"),
        CommandOutcome::NotOurs
    );

    // A required gate nobody declares closes logins too.
    let env = Env::new("missing2");
    let mut cfg = env.host_config("");
    cfg.plugins.required_gates = vec!["filter".into()];
    cfg.plugins.required_plugins = vec!["pumbo-bans".into()];
    let (host, _) = start(cfg).await;
    assert!(host.login_open().is_err());
}

/// Memory limit: an allocation above the limit traps, the plugin restarts.
#[tokio::test(flavor = "multi_thread")]
async fn memory_limit() {
    let env = Env::new("memory");
    env.plugin("mem", "test", "");
    let mut cfg = env.host_config("");
    cfg.plugins.memory_mb = 32;
    let (host, bridge) = start(cfg).await;
    host.player_joined(player(1, "a", None));
    assert_eq!(
        fail_by(&host, "mem", CommandSender::Player(1), "/alloc 64").await,
        CommandOutcome::Handled
    );
    wait_status(&host, "mem", LONG, |s| *s == Status::Running).await;
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), "/alloc 4"),
        CommandOutcome::Handled
    );
    wait_for("small allocation", LONG, || {
        bridge.messages(1).contains(&"alloc 4194304".to_string())
    })
    .await;
}

/// HTTP: no hosts in the manifest means no HTTP; a listed host that resolves
/// to 127.0.0.1 is refused; with private addresses allowed (tests use plain
/// http) the request works, with a neutral User-Agent and a size limit.
#[tokio::test(flavor = "multi_thread")]
async fn http_rules() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let heads = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let h2 = heads.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let heads = h2.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut n = 0;
                while !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf[n..]).await {
                        Ok(0) | Err(_) => return,
                        Ok(k) => n += k,
                    }
                }
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                heads.lock().unwrap().push(head.clone());
                let resp = if head.starts_with("GET /big") {
                    let body = vec![b'x'; 2 * 1024 * 1024];
                    let mut r =
                        format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len())
                            .into_bytes();
                    r.extend(body);
                    r
                } else if head.starts_with("GET /redirect") {
                    b"HTTP/1.1 302 Found\r\nlocation: http://example.com/\r\ncontent-length: 0\r\n\r\n".to_vec()
                } else {
                    b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello".to_vec()
                };
                let _ = s.write_all(&resp).await;
            });
        }
    });

    let env = Env::new("http");
    env.plugin("nohttp", "test", "");
    env.plugin("web", "test", "http: [localhost, \"127.0.0.1\"]");
    env.config_file("web", "config.yml", "prefix: w\n");
    let (host, bridge) = start(env.host_config("")).await;
    host.player_joined(player(1, "a", None));
    let ask = |line: &str| {
        assert_eq!(
            host.dispatch_command(CommandSender::Player(1), line),
            CommandOutcome::Handled
        )
    };
    ask("/http https://example.com/");
    ask(&format!("/whttp https://localhost:{port}/ok"));
    ask(&format!("/whttp https://127.0.0.1:{port}/ok"));
    ask(&format!("/whttp http://localhost:{port}/ok"));
    wait_for("4 answers", LONG, || bridge.messages(1).len() >= 4).await;
    assert!(
        bridge
            .messages(1)
            .iter()
            .all(|m| m == "http err HttpError::NotAllowed"),
        "{:?}",
        bridge.messages(1)
    );

    let env = Env::new("http-private");
    env.plugin("web", "test", "http: [localhost, \"127.0.0.1\"]");
    let mut cfg = env.host_config("");
    cfg.plugins.http_allow_private = true;
    cfg.plugins.http_allow_plain_for_tests = true;
    let (host, bridge) = start(cfg).await;
    host.player_joined(player(1, "a", None));
    for path in ["ok", "big", "redirect"] {
        let line = format!("/http http://127.0.0.1:{port}/{path}");
        bridge.clear();
        assert_eq!(
            host.dispatch_command(CommandSender::Player(1), &line),
            CommandOutcome::Handled
        );
        wait_for("answer", LONG, || !bridge.messages(1).is_empty()).await;
        let m = bridge.messages(1).remove(0);
        eprintln!("[result] {line}: {m}");
        match line.as_str() {
            l if l.ends_with("/ok") => assert_eq!(m, "http ok 200 hello"),
            l if l.ends_with("/big") => assert_eq!(m, "http err HttpError::TooLarge"),
            _ => assert_eq!(m, "http err HttpError::NotAllowed"),
        }
    }
    let heads = heads.lock().unwrap();
    let ok = heads
        .iter()
        .find(|h| h.starts_with("GET /ok"))
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        ok.contains(&format!(
            "user-agent: pumboprox/{}",
            env!("CARGO_PKG_VERSION")
        )),
        "{ok}"
    );
}

/// Two plugins patch the profile in priority order and see earlier patches;
/// a plugin without `profile-properties` cannot change properties; premium
/// profiles stay untouched.
#[tokio::test(flavor = "multi_thread")]
async fn profile_patches_in_order() {
    let env = Env::new("profile");
    let m = |prio: i32, props: &str| {
        format!("events: [profile]\nprofile: {{ priority: {prio} }}\nprofile-properties: [{props}]")
    };
    env.plugin("skins", "test", &m(200, "\"trail\""));
    env.plugin("auth", "test", &m(100, "\"trail\""));
    env.plugin("rogue", "test", &m(300, ""));
    for (id, tag) in [("skins", "A"), ("auth", "B"), ("rogue", "C")] {
        env.config_file(id, "config.yml", &format!("tag: {tag}\nprefix: {id}\n"));
    }
    let (host, _) = start(env.host_config("")).await;
    host.player_joined(player(1, "a", None));
    let p = host.on_profile(1).await.unwrap();
    let trail: Vec<_> = p
        .properties
        .iter()
        .filter(|q| q.name == "trail")
        .map(|q| q.value.clone())
        .collect();
    assert_eq!(trail, ["BA"]);
    assert_eq!(host.player(1).unwrap().profile, p);

    let mut premium = player(2, "b", None);
    premium.online_mode = true;
    host.player_joined(premium);
    let p = host.on_profile(2).await.unwrap();
    assert!(p.properties.is_empty());
}

/// `on-pre-login` reaches every listener even after a `force-online`; the
/// first `force-*` counts, a `deny` anywhere wins (D-E7-2).
#[tokio::test(flavor = "multi_thread")]
async fn pre_login_reaches_every_plugin() {
    let env = Env::new("pre-login");
    for (id, answer) in [("a", "force-online"), ("b", "force-offline"), ("c", "deny")] {
        env.plugin(id, "test", "events: [pre-login]");
        env.config_file(
            id,
            "config.yml",
            &format!("prefix: {id}\npre_login: {answer}\nreport: 9\n"),
        );
    }
    let (host, bridge) = start(env.host_config("")).await;
    let pre = |name: &str| {
        host.on_pre_login(PreLoginEvent {
            connection: connection(),
            name: name.into(),
            claimed_uuid: None,
        })
    };
    assert_eq!(pre("steve").await, PreLogin::ForceOnline);
    assert_eq!(
        bridge.messages(9),
        ["pre a steve", "pre b steve", "pre c steve"]
    );
    bridge.clear();
    assert!(matches!(pre("deny_x").await, PreLogin::Deny(_)));
    assert_eq!(bridge.messages(9).len(), 3);
}

/// Reloading a gate under 200 bots: bots the gate denies are never let in,
/// the others wait in the gate's queue and pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_under_load_has_no_window() {
    let env = Env::new("reload");
    env.plugin("auth", "test", "events: [gate]\ngate: { name: auth }");
    let mut cfg = env.host_config("");
    cfg.plugins.required_gates = vec!["auth".into()];
    let (host, _) = start(cfg).await;
    for i in 1..=200u64 {
        let name = if i % 2 == 0 {
            format!("deny_{i}")
        } else {
            format!("ok_{i}")
        };
        host.player_joined(player(i, &name, None));
    }
    let until = Instant::now() + Duration::from_secs(3);
    let leaked = Arc::new(AtomicU64::new(0));
    let refused = Arc::new(AtomicU64::new(0));
    let passed = Arc::new(AtomicU64::new(0));
    let mut tasks = Vec::new();
    for i in 1..=200u64 {
        let (host, leaked, refused, passed) = (
            host.clone(),
            leaked.clone(),
            refused.clone(),
            passed.clone(),
        );
        tasks.push(tokio::spawn(async move {
            while Instant::now() < until {
                let r = host.run_gates(i).await;
                match (i % 2 == 0, r) {
                    (true, GateOutcome::Pass) => leaked.fetch_add(1, Ordering::Relaxed),
                    (false, GateOutcome::Deny(_)) => refused.fetch_add(1, Ordering::Relaxed),
                    (false, GateOutcome::Pass) => passed.fetch_add(1, Ordering::Relaxed),
                    _ => 0,
                };
            }
        }));
    }
    for _ in 0..2 {
        tokio::time::sleep(Duration::from_millis(800)).await;
        let t = Instant::now();
        host.reload_plugin("auth").await.unwrap();
        eprintln!("[measure] gate reload under load: {:?}", t.elapsed());
    }
    for t in tasks {
        t.await.unwrap();
    }
    eprintln!(
        "[result] passed {}, refused while reloading {}, leaked {}",
        passed.load(Ordering::Relaxed),
        refused.load(Ordering::Relaxed),
        leaked.load(Ordering::Relaxed)
    );
    assert_eq!(leaked.load(Ordering::Relaxed), 0);
    assert!(passed.load(Ordering::Relaxed) > 0);
}

/// A synchronous export (`describe`) waiting on an async import traps the
/// instance in wasmtime 49 ("cannot block a synchronous task"); the host
/// reports it as a failed start and does not hang.
#[tokio::test(flavor = "multi_thread")]
async fn sync_export_waiting_on_async_import() {
    let env = Env::new("sync-async");
    env.plugin("bad", "test", "");
    env.config_file("bad", "config.yml", "bad_describe: true\n");
    let (host, _) = start(env.host_config("")).await;
    let s = wait_status(&host, "bad", LONG, |s| {
        matches!(s, Status::Restarting(_) | Status::Disabled(_))
    })
    .await;
    eprintln!("[result] describe blocking on an async import: {s:?}");
    let (Status::Restarting(msg) | Status::Disabled(msg)) = s else {
        unreachable!()
    };
    assert!(msg.contains("cannot block a synchronous task"), "{msg}");
    env.config_file("bad", "config.yml", "bad_describe: false\n");
    host.reload_plugin("bad").await.unwrap();
    assert_eq!(host.plugin("bad").unwrap().status(), Status::Running);
}

/// Commands: reserved names, conflicts between plugins, sensitive commands,
/// re-registration after a restart.
#[tokio::test(flavor = "multi_thread")]
async fn commands_registration() {
    let env = Env::new("commands");
    env.plugin("a", "test", "");
    env.plugin("b", "test", "");
    env.config_file("b", "config.yml", "prefix: b\n");
    let (host, bridge) = start(env.host_config("")).await;
    host.player_joined(player(1, "x", None));
    // One at a time: the two plugins answer concurrently.
    for (i, line) in [
        "/reg pumbo",
        "/reg server",
        "/reg hello",
        "/breg hello",
        "/breg Bad!",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            host.dispatch_command(CommandSender::Player(1), line),
            CommandOutcome::Handled
        );
        wait_for("answer", LONG, || bridge.messages(1).len() > i).await;
    }
    let m = bridge.messages(1);
    assert!(m[0].contains("reserved by the proxy"), "{m:?}");
    assert!(m[1].contains("reserved by the proxy"), "{m:?}");
    assert_eq!(m[2], "reg Ok(())");
    assert!(m[3].contains("registered by a"), "{m:?}");
    assert!(m[4].contains("invalid command name"), "{m:?}");
    let visible: Vec<String> = host
        .visible_commands(1)
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert!(visible.contains(&"hello".to_string()) && visible.contains(&"bsecret".to_string()));
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), "/secret pw"),
        CommandOutcome::Handled
    );
    // After a restart only the commands of `init` exist again.
    assert_eq!(
        fail_by(&host, "a", CommandSender::Console, "/panic").await,
        CommandOutcome::Handled
    );
    wait_status(&host, "a", LONG, |s| *s == Status::Running).await;
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), "/hello"),
        CommandOutcome::NotOurs
    );
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), "/echo z"),
        CommandOutcome::Handled
    );
}

/// Imports with host state: boss bars (hidden when the instance ends),
/// timers, password hashing on the host, `/data` read-write and `/config`
/// read-only, nothing else of the host's file system.
#[tokio::test(flavor = "multi_thread")]
async fn imports_bars_timers_crypto_files() {
    let env = Env::new("imports");
    env.plugin("imp", "test", "");
    env.config_file("imp", "config.yml", "report: 1\n");
    let (host, bridge) = start(env.host_config("")).await;
    host.player_joined(player(1, "a", None));
    let ask = |line: &'static str| {
        let before = bridge.messages(1).len();
        assert_eq!(
            host.dispatch_command(CommandSender::Player(1), line),
            CommandOutcome::Handled
        );
        let b = bridge.clone();
        async move {
            wait_for(line, LONG, || b.messages(1).len() > before).await;
            b.messages(1)[before].clone()
        }
    };
    use pumbo_host::{BossbarCommand, PlayerCommand};
    assert_eq!(ask("/bar").await, "bar ok");
    let bars: Vec<BossbarCommand> = bridge
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, c)| match c {
            PlayerCommand::Bossbar(b) => Some(b.clone()),
            _ => None,
        })
        .collect();
    assert!(
        matches!(bars.as_slice(), [BossbarCommand::Show { .. }, BossbarCommand::Progress { progress, .. }, BossbarCommand::Hide { .. }] if *progress == 0.5),
        "{bars:?}"
    );

    let t = ask("/timer 50").await;
    let id = t.trim_start_matches("timer ").to_string();
    wait_for("timer fired", LONG, || {
        bridge.messages(1).contains(&format!("fired {id}"))
    })
    .await;
    let every = ask("/every 50")
        .await
        .trim_start_matches("timer ")
        .to_string();
    wait_for("three ticks", LONG, || {
        bridge
            .messages(1)
            .iter()
            .filter(|m| **m == format!("fired {every}"))
            .count()
            >= 3
    })
    .await;
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), &format!("/cancel {every}")),
        CommandOutcome::Handled
    );
    wait_for("cancelled", LONG, || {
        bridge.messages(1).contains(&"cancelled".to_string())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let n = bridge
        .messages(1)
        .iter()
        .filter(|m| **m == format!("fired {every}"))
        .count();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        bridge
            .messages(1)
            .iter()
            .filter(|m| **m == format!("fired {every}"))
            .count(),
        n
    );

    assert_eq!(ask("/hash s3cret").await, "hash true false");
    assert_eq!(
        ask("/file a.txt hello").await,
        "file true hello ro=true outside=true"
    );
    assert!(env.plugins().join("data/imp/a.txt").exists());

    // A bar left shown is hidden when the instance ends.
    assert_eq!(ask("/bar keep").await, "bar ok");
    let hides =
        || bridge.count(|c| matches!(c, PlayerCommand::Bossbar(BossbarCommand::Hide { .. })));
    let before = hides();
    assert_eq!(
        host.dispatch_command(CommandSender::Console, "/panic"),
        CommandOutcome::Handled
    );
    wait_for("bar hidden", LONG, || hides() > before).await;
}
