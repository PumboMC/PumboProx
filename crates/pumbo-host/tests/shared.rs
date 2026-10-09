//! Host tests, part B of E5: service registry, placeholders, permission
//! provider with contexts, bus, descriptions and `/pumbo`, plugins enabled
//! per server and config overlays (plan §5.8, §6.6, §11 E5).

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
use pumbo_host::wit::events::{ChatReply, ConnectEvent};
use pumbo_host::{CommandOutcome, CommandSender, GateOutcome, Host, HostConfig, Status};

const LONG: Duration = Duration::from_secs(120);

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Sends a command and waits for the first new message starting with `prefix`.
async fn ask(host: &Host, bridge: &FakeBridge, player: u64, line: &str, prefix: &str) -> String {
    let before = bridge.messages(player).len();
    assert_eq!(
        host.dispatch_command(CommandSender::Player(player), line),
        CommandOutcome::Handled,
        "{line}"
    );
    let mut out = None;
    wait_for(line, LONG, || {
        out = bridge
            .messages(player)
            .into_iter()
            .skip(before)
            .find(|m| m.starts_with(prefix));
        out.is_some()
    })
    .await;
    out.unwrap()
}

fn replies(o: CommandOutcome) -> Vec<String> {
    match o {
        CommandOutcome::Reply(lines) => lines
            .iter()
            .map(pumbo_text::Component::plain_text)
            .collect(),
        other => panic!("not a reply: {other:?}"),
    }
}

/// Two plugins calling each other's services: a cycle A→B→A within the
/// depth, then `too-deep`; errors as values; the caller keeps working while
/// a provider is slow, loops or switches its service off.
#[tokio::test(flavor = "multi_thread")]
async fn services_cycle_and_errors() {
    let env = Env::new("services");
    env.plugin(
        "svc-a",
        "test",
        "provides:\n  - { service: \"test:a\", version: \"1.0\" }\nuses:\n  - { service: \"test:b\", version: \"1.0\" }",
    );
    env.plugin(
        "svc-b",
        "test",
        "provides:\n  - { service: \"test:b\", version: \"1.0\" }\nuses:\n  - { service: \"test:a\", version: \"1.0\" }",
    );
    env.config_file(
        "svc-a",
        "config.yml",
        "prefix: a\npeer: test:b\nreport: 1\n",
    );
    env.config_file(
        "svc-b",
        "config.yml",
        "prefix: b\npeer: test:a\nreport: 1\n",
    );
    let (host, bridge) = start(env.host_config("")).await;
    host.player_joined(player(1, "a", None));
    let b = &*bridge;

    assert_eq!(
        ask(&host, b, 1, "/acall test:b ping 7", "call").await,
        "call ok 7"
    );
    let deep = ask(&host, b, 1, "/acall test:b ping 20", "call").await;
    assert!(deep.contains("toodeep"), "{deep}");
    assert!(
        ask(&host, b, 1, "/acall test:x ping 1", "call")
            .await
            .contains("notdeclared")
    );
    assert!(
        ask(&host, b, 1, "/acall test:b nosuch 0", "call")
            .await
            .contains("unknownmethod")
    );
    assert!(
        ask(&host, b, 1, "/acall test:b size 300000", "call")
            .await
            .contains("toolarge")
    );

    // A slow provider: the caller gets `timeout` and answers meanwhile.
    let h = host.clone();
    let br = bridge.clone();
    let slow = tokio::spawn(async move { ask(&h, &br, 1, "/acall test:b slow 0", "call").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let t = Instant::now();
    assert_eq!(ask(&host, b, 1, "/aecho alive", "echo").await, "echo alive");
    assert!(t.elapsed() < Duration::from_millis(500));
    assert!(slow.await.unwrap().contains("timeout"));

    // A provider in an endless loop traps: `provider-failed`, it restarts,
    // consumers get `on-service-changed` both ways.
    bridge.clear();
    assert!(
        ask(&host, b, 1, "/acall test:b spin 0", "call")
            .await
            .contains("providerfailed")
    );
    wait_for("changed none", LONG, || {
        bridge
            .messages(1)
            .iter()
            .any(|m| m == "changed test:b none")
    })
    .await;
    wait_status(&host, "svc-b", LONG, |s| *s == Status::Running).await;
    wait_for("changed 1.0", LONG, || {
        bridge.messages(1).iter().any(|m| m == "changed test:b 1.0")
    })
    .await;
    assert_eq!(
        ask(&host, b, 1, "/acall test:b ping 1", "call").await,
        "call ok 1"
    );

    // The provider switches its service off: `unavailable` at once.
    assert_eq!(
        ask(&host, b, 1, "/bavail test:b off", "avail").await,
        "avail Ok(())"
    );
    assert_eq!(
        ask(&host, b, 1, "/alookup test:b", "lookup").await,
        "lookup None"
    );
    let t = Instant::now();
    assert!(
        ask(&host, b, 1, "/acall test:b ping 1", "call")
            .await
            .contains("unavailable")
    );
    assert!(t.elapsed() < Duration::from_millis(500));
    assert!(
        ask(&host, b, 1, "/bavail test:a off", "avail")
            .await
            .contains("not in provides")
    );
    ask(&host, b, 1, "/bavail test:b on", "avail").await;
    assert_eq!(
        ask(&host, b, 1, "/alookup test:b", "lookup").await,
        "lookup Some(\"1.0\")"
    );
}

/// Missing providers, versions, provider choice and the reserved namespace.
#[tokio::test(flavor = "multi_thread")]
async fn services_registry_rules() {
    let env = Env::new("registry");
    env.plugin(
        "prov",
        "test",
        "provides:\n  - { service: \"test:a\", version: \"1.2\" }",
    );
    env.plugin(
        "opt",
        "test",
        "uses:\n  - { service: \"test:missing\", version: \"1.0\" }\n  - { service: \"test:a\", version: \"1.3\" }",
    );
    env.plugin(
        "strict",
        "test",
        "uses:\n  - { service: \"test:a\", version: \"1.3\", required: true }",
    );
    env.config_file("opt", "config.yml", "prefix: o\n");
    env.config_file("strict", "config.yml", "prefix: s\n");
    let (host, bridge) = start(env.host_config("")).await;
    host.player_joined(player(1, "a", None));
    let t = Instant::now();
    assert!(
        ask(&host, &bridge, 1, "/ocall test:missing ping 1", "call")
            .await
            .contains("unavailable")
    );
    assert!(t.elapsed() < Duration::from_millis(500));
    assert!(
        ask(&host, &bridge, 1, "/ocall test:a ping 1", "call")
            .await
            .contains("incompatible")
    );
    let s = host.plugin("strict").unwrap().status();
    let Status::Disabled(why) = s else {
        panic!("{s:?}")
    };
    assert!(why.contains("1.3 needed"), "{why}");

    // Two providers need a choice.
    let env = Env::new("registry-choice");
    env.plugin(
        "p1",
        "test",
        "provides:\n  - { service: \"test:a\", version: \"1.0\" }",
    );
    env.plugin(
        "p2",
        "test",
        "provides:\n  - { service: \"test:a\", version: \"1.0\" }",
    );
    env.config_file("p2", "config.yml", "prefix: x\n");
    let err = Host::start(env.host_config(""), Arc::new(FakeBridge::default()))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("p1, p2"), "{err}");
    let cfg = env.host_config("services:\n  providers: { \"test:a\": p2 }\n");
    assert!(
        Host::start(cfg, Arc::new(FakeBridge::default()))
            .await
            .is_ok()
    );

    // `pumbo:` is reserved.
    let env = Env::new("registry-pumbo");
    env.plugin(
        "evil",
        "test",
        "provides:\n  - { service: \"pumbo:accounts\", version: \"1.0\" }",
    );
    let err = Host::start(env.host_config(""), Arc::new(FakeBridge::default()))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("reserved"), "{err}");
}

fn rss_kb() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// 10 000 calls at once: the limits answer `overloaded` at once and memory
/// stays bounded; the provider and the caller keep working.
#[tokio::test(flavor = "multi_thread")]
async fn flood_of_calls_is_overloaded() {
    let env = Env::new("flood");
    env.plugin(
        "prov",
        "test",
        "provides:\n  - { service: \"test:a\", version: \"1.0\" }",
    );
    env.plugin(
        "caller",
        "test",
        "uses:\n  - { service: \"test:a\", version: \"1.0\" }",
    );
    env.config_file("caller", "config.yml", "prefix: c\n");
    let (host, bridge) = start(env.host_config("")).await;
    host.player_joined(player(1, "a", None));
    let before = rss_kb();
    let t = Instant::now();
    let r = ask(&host, &bridge, 1, "/cflood 10000 test:a", "flood").await;
    eprintln!(
        "[measure] flood of 10000 calls: {r} in {:?}, RSS +{} KB",
        t.elapsed(),
        rss_kb().saturating_sub(before)
    );
    let overloaded: usize = r
        .split_whitespace()
        .find_map(|w| w.strip_prefix("overloaded="))
        .unwrap()
        .parse()
        .unwrap();
    assert!(overloaded >= 10_000 - 64, "{r}");
    assert!(rss_kb().saturating_sub(before) < 200 * 1024);
    assert_eq!(ask(&host, &bridge, 1, "/echo ok", "echo").await, "echo ok");
}

const PH: &str = r#"
placeholders:
  namespace: test
  keys:
    - { name: secret, scope: player, mode: push, public: false }
    - { name: rank, scope: player, mode: push, public: true, fallback: "?" }
    - { name: g_motd, scope: global, mode: push, public: true }
    - { name: pull, scope: player, mode: pull, public: true, ttl-ms: 60000 }
    - { name: slow, scope: player, mode: pull, public: true }
    - { name: inject, scope: player, mode: pull, public: true }
"#;

/// Push values with contexts, pull values with a deadline and a cache that
/// a server change clears, private keys, aliases, one pass, literal text.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placeholders() {
    let env = Env::new("placeholders");
    env.plugin("ph", "test", PH);
    env.plugin("other", "test", "");
    env.config_file("other", "config.yml", "prefix: o\n");
    let mut cfg = env.host_config("server-group:\n  - { name: survivals, servers: [survival] }\n");
    cfg.placeholders
        .aliases
        .insert("rank".into(), "test_rank".into());
    let (host, bridge) = start(cfg).await;
    host.player_joined(player(1, "Steve", None));
    host.player_server_changed(1, Some("lobby".into()), false);
    let b = &*bridge;

    assert_eq!(
        ask(&host, b, 1, "/push rank <gray>Player global", "push").await,
        "push Ok(())"
    );
    ask(&host, b, 1, "/push rank <gold>VIP group=survivals", "push").await;
    ask(&host, b, 1, "/push secret s3cret global", "push").await;
    ask(&host, b, 1, "/push g_motd <aqua>Welcome global", "push").await;
    assert!(
        ask(&host, b, 1, "/push nope x global", "push")
            .await
            .contains("not in the manifest")
    );

    let r = |s: &str| host.render_now(s, &[], Some(1)).plain_text();
    assert_eq!(
        r("%test_rank% %rank% %test_rank@survival% %test_g_motd%"),
        "Player Player VIP Welcome"
    );
    host.player_server_changed(1, Some("survival".into()), false);
    assert_eq!(
        r("%rank% %test_rank@lobby% %test_rank@global%"),
        "VIP Player Player"
    );
    assert_eq!(
        r("%player_name% %player_server% %proxy_online% %nope_x% %test_secret%"),
        "Steve survival 1 %nope_x% s3cret"
    );

    // A private key resolves for its owner and the proxy, not for others.
    assert_eq!(
        ask(&host, b, 1, "/resolve %test_secret%", "resolved").await,
        "resolved s3cret"
    );
    assert_eq!(
        ask(
            &host,
            b,
            1,
            "/oresolve %test_secret% %test_rank%",
            "resolved"
        )
        .await,
        "resolved %test_secret% <gold>VIP"
    );

    // Arguments are literal, values are not scanned again, events are cut.
    let lit = ask(&host, b, 1, "/resolve {arg}", "resolved").await;
    assert_eq!(
        pumbo_text::parse_mini(lit.trim_start_matches("resolved ")).plain_text(),
        "%test_secret% <click:run_command:/op me>y"
    );
    let inj = ask(&host, b, 1, "/resolve %test_inject%", "resolved").await;
    assert_eq!(inj, "resolved %test_secret%x");

    // Command arguments from a player reach the plugin as written.
    assert_eq!(
        ask(&host, b, 1, "/echo %test_secret% <red>x", "echo").await,
        "echo %test_secret% <red>x"
    );

    // Pull: deadline then fallback; the cache follows the server change.
    let t = Instant::now();
    assert_eq!(
        ask(&host, b, 1, "/resolve %test_slow%", "resolved").await,
        "resolved %test_slow%"
    );
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(
        host.render("%test_pull%", &[], Some(1)).await.plain_text(),
        "pull:pull::survival"
    );
    host.player_server_changed(1, Some("lobby".into()), false);
    // The old context's entry is gone: no value until the pull answers.
    assert_eq!(r("%test_pull%"), "%test_pull%");
    assert_eq!(
        host.render("%test_pull%", &[], Some(1)).await.plain_text(),
        "pull:pull::lobby"
    );
    assert_eq!(
        host.render("%test_pull@survival%", &[], Some(1))
            .await
            .plain_text(),
        "pull:pull::survival"
    );

    // 500 players resolve pull values at once, within the deadline.
    for i in 2..502u64 {
        host.player_joined(player(i, &format!("p{i}"), None));
        host.player_server_changed(i, Some("lobby".into()), false);
    }
    let t = Instant::now();
    let renders = (2..502u64).map(|i| {
        let h = host.clone();
        async move { h.render("%test_pull%", &[], Some(i)).await.plain_text() }
    });
    let out = futures::future::join_all(renders).await;
    let ok = out.iter().filter(|s| *s == "pull:pull::lobby").count();
    eprintln!(
        "[measure] 500 players resolving a pull placeholder: {ok} resolved in {:?}",
        t.elapsed()
    );
    assert!(ok >= 450, "{ok}");
    assert!(t.elapsed() < Duration::from_secs(5));
}

/// The permission provider loads sets with contexts after the gates; bots
/// refused by a gate never reach it; failures deny or fall back to the file.
#[tokio::test(flavor = "multi_thread")]
async fn permission_provider_with_contexts() {
    let env = Env::new("perm-provider");
    env.plugin(
        "perm",
        "test",
        "events: [gate]\ngate: { name: auth }\npermission-provider: true\nprovides:\n  - { service: \"pumbo:permissions\", version: \"1.0\" }",
    );
    env.plugin("asker", "test", "");
    env.config_file("asker", "config.yml", "prefix: q\n");
    env.config_file(
        "perm",
        "config.yml",
        "report: 99\nperms:\n  - { node: pumbo.test.fly, value: true, ctx: global }\n  - { node: pumbo.test.fly, value: false, ctx: server=survival }\n  - { node: pumbo.test.kit, value: true, ctx: group=minigames }\n",
    );
    env.file(
        "permissions.yml",
        "groups:\n  default:\n    permissions: [pumbo.test.base, \"-pumbo.test.kit\"]\n",
    );
    let cfg = env.host_config("permissions: { provider: perm }\nserver-group:\n  - { name: minigames, servers: [bedwars] }\n");
    let (host, bridge) = start(cfg).await;
    host.player_joined(player(99, "reporter", None));
    host.player_joined(player(1, "steve", None));
    assert!(!host.has_permission(1, "pumbo.test.fly", None));
    assert!(host.has_permission(1, "pumbo.test.base", None));
    assert_eq!(host.run_gates(1).await, GateOutcome::Pass);
    assert!(host.has_permission(1, "pumbo.test.fly", Some("lobby")));
    assert!(!host.has_permission(1, "pumbo.test.fly", Some("survival")));
    // Group of the provider beats the global entry of the file.
    assert!(host.has_permission(1, "pumbo.test.kit", Some("bedwars")));
    assert!(!host.has_permission(1, "pumbo.test.kit", Some("lobby")));
    host.player_server_changed(1, Some("survival".into()), false);
    assert!(!host.has_permission(1, "pumbo.test.fly", None));

    // Bots refused by the gate never reach the provider.
    for i in 10..30u64 {
        host.player_joined(player(i, &format!("deny_{i}"), None));
        assert!(matches!(host.run_gates(i).await, GateOutcome::Deny(_)));
    }
    // Steve only: once at his gate, and once more when the provider's start
    // (handing over permissions.yml, then reloading the players loaded so
    // far) ran after that gate, which a test alone is fast enough for.
    let loads: Vec<String> = bridge
        .messages(99)
        .into_iter()
        .filter(|m| m.starts_with("load "))
        .collect();
    assert!(
        (1..=2).contains(&loads.len()) && loads.iter().all(|m| m == "load steve"),
        "{:?}",
        bridge.messages(99)
    );

    // Offline check through the provider's service.
    let hasoff = ask(&host, &bridge, 1, "/qhasoff 5 pumbo.test.fly", "hasoff").await;
    assert_eq!(hasoff, "hasoff Ok(true)");

    // `set` needs permissions-write, `replace` the configured provider.
    let set = ask(&host, &bridge, 1, "/qpset pumbo.test.x true global", "pset").await;
    assert!(set.contains("permissions-write"), "{set}");
    let repl = ask(&host, &bridge, 1, "/qprepl", "prepl").await;
    assert!(
        repl.contains("only the configured permission provider"),
        "{repl}"
    );
    let repl = ask(&host, &bridge, 1, "/prepl", "prepl").await;
    assert_eq!(repl, "prepl Some(Ok(()))");
    assert!(!host.has_permission(1, "pumbo.test.fly", Some("lobby")));

    // The provider fails: deny, or the file layer only.
    env.config_file("perm", "config.yml", "report: 99\nfail_load: true\n");
    host.reload_plugin("perm").await.unwrap();
    host.player_joined(player(2, "alex", None));
    let r = host.run_gates(2).await;
    let GateOutcome::Deny(t) = r else {
        panic!("{r:?}")
    };
    assert!(
        t.plain_text()
            .contains("Permissions are temporarily unavailable")
    );
    let cfg = env.host_config("permissions: { provider: perm, on-load-failure: file-only }\n");
    let (host2, _) = start(cfg).await;
    host2.player_joined(player(3, "joe", None));
    assert_eq!(host2.run_gates(3).await, GateOutcome::Pass);
    assert!(host2.has_permission(3, "pumbo.test.base", None));
}

/// PumboPerms spec §16: without config the installed provider is taken
/// (`auto`) and gets permissions.yml at its start; once it loaded a player
/// its set replaces the file; when it is unloaded the file decides again for
/// everyone, and when it comes back the players are loaded again. A named
/// provider that is not installed leaves the file in charge.
#[tokio::test(flavor = "multi_thread")]
async fn provider_replaces_the_file_and_falls_back() {
    let env = Env::new("perm-fallback");
    env.plugin(
        "perm",
        "test",
        "permission-provider: true\nprovides:\n  - { service: \"pumbo:permissions\", version: \"1.0\" }",
    );
    env.config_file(
        "perm",
        "config.yml",
        "report: 99\nperms:\n  - { node: pumbo.test.fly, value: true, ctx: global }\n",
    );
    env.file(
        "permissions.yml",
        "groups:\n  default:\n    permissions: [pumbo.test.base]\nplayers:\n  Jeb_:\n    groups: [default]\n",
    );
    let (host, bridge) = start(env.host_config("")).await;
    host.player_joined(player(99, "reporter", None));
    let file_sent = |b: &FakeBridge| b.messages(99).iter().any(|m| m.starts_with("file "));
    for _ in 0..100 {
        if file_sent(&bridge) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        file_sent(&bridge),
        "the provider got permissions.yml: {:?}",
        bridge.messages(99)
    );

    host.player_joined(player(1, "steve", None));
    assert!(
        host.has_permission(1, "pumbo.test.base", None),
        "before the load: the file"
    );
    assert_eq!(host.run_gates(1).await, GateOutcome::Pass);
    assert!(host.has_permission(1, "pumbo.test.fly", None));
    assert!(
        !host.has_permission(1, "pumbo.test.base", None),
        "the provider replaces the file"
    );

    // Unloaded: the file decides again at once, also for new players.
    host.plugin("perm").unwrap().unload().await.unwrap();
    assert!(host.has_permission(1, "pumbo.test.base", None));
    assert!(!host.has_permission(1, "pumbo.test.fly", None));
    host.player_joined(player(2, "alex", None));
    assert_eq!(host.run_gates(2).await, GateOutcome::Pass);
    assert!(host.has_permission(2, "pumbo.test.base", None));

    // Back: everyone past the gates is loaded from it again.
    host.reload_plugin("perm").await.unwrap();
    for _ in 0..100 {
        if host.has_permission(2, "pumbo.test.fly", None)
            && host.has_permission(1, "pumbo.test.fly", None)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(host.has_permission(1, "pumbo.test.fly", None));
    assert!(host.has_permission(2, "pumbo.test.fly", None));
    assert!(!host.has_permission(2, "pumbo.test.base", None));
    host.shutdown().await;

    // A named provider that is not installed: the file, no start error.
    let (host2, _) = start(env.host_config("permissions: { provider: nothere }\n")).await;
    host2.player_joined(player(3, "joe", None));
    assert_eq!(host2.run_gates(3).await, GateOutcome::Pass);
    assert!(host2.has_permission(3, "pumbo.test.base", None));
    assert!(!host2.has_permission(3, "pumbo.test.fly", None));
    host2.shutdown().await;
    // `file`: the plugin is installed but not asked.
    let (host3, _) = start(env.host_config("permissions: { provider: file }\n")).await;
    host3.player_joined(player(4, "ann", None));
    assert_eq!(host3.run_gates(4).await, GateOutcome::Pass);
    assert!(host3.has_permission(4, "pumbo.test.base", None));
    assert!(!host3.has_permission(4, "pumbo.test.fly", None));
}

/// Bus: order per publisher, overflow of a slow subscriber, topics outside
/// `publishes` and `pumbo:` topics of other plugins refused.
#[tokio::test(flavor = "multi_thread")]
async fn bus_events() {
    let env = Env::new("bus");
    env.plugin(
        "pub",
        "test",
        "publishes: [\"test:tick@1.0\", \"pumbo:skin-changed@1.0\"]",
    );
    env.plugin("sub", "test", "subscribes: [\"test:tick@1.0\"]");
    env.plugin("slow", "test", "subscribes: [\"test:tick@1.0\"]");
    env.config_file("sub", "config.yml", "prefix: s\nreport: 1\n");
    env.config_file(
        "slow",
        "config.yml",
        "prefix: w\nreport: 2\nbus_sleep_ms: 50\n",
    );
    let mut cfg = env.host_config("");
    cfg.services.bus_queue = 200;
    let (host, bridge) = start(cfg).await;
    host.player_joined(player(1, "a", None));
    host.player_joined(player(2, "b", None));
    assert_eq!(
        ask(&host, &bridge, 1, "/pub test:tick@1.0 100", "pub").await,
        "pub 100 None"
    );
    wait_for("100 events", LONG, || {
        bridge
            .messages(1)
            .iter()
            .filter(|m| m.starts_with("bus "))
            .count()
            >= 100
    })
    .await;
    let got: Vec<String> = bridge
        .messages(1)
        .into_iter()
        .filter(|m| m.starts_with("bus "))
        .collect();
    let want: Vec<String> = (0..100)
        .map(|i| format!("bus test:tick {i} from pub"))
        .collect();
    assert_eq!(got, want);
    // 400 more: the slow subscriber (50 ms per event, queue 200) drops some.
    ask(&host, &bridge, 1, "/pub test:tick@1.0 400", "pub").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let dropped: u64 = host
        .render_metrics()
        .lines()
        .find_map(|l| l.strip_prefix("pumbo_bus_dropped_total "))
        .and_then(|v| v.parse().ok())
        .unwrap();
    eprintln!("[result] bus: {dropped} events dropped for the slow subscriber (queue 200)");
    assert!(dropped >= 100, "{dropped}");

    let other = ask(&host, &bridge, 1, "/pub test:other@1.0 1", "pub").await;
    assert!(other.contains("not in publishes"), "{other}");
    let pumbo = ask(&host, &bridge, 1, "/pub pumbo:skin-changed@1.0 1", "pub").await;
    assert!(pumbo.contains("only pumbo-skins"), "{pumbo}");
}

/// Description: actions with permissions checked by the host, config checked
/// before a reload (the plugin keeps the old one), secrets hidden, declared
/// metrics only, `/pumbo` and aliases, offline description.
#[tokio::test(flavor = "multi_thread")]
async fn descriptions_and_umbrella() {
    let env = Env::new("describe");
    env.plugin(
        "adm",
        "test",
        "short-name: test\nshort-alias: tt\npermissions:\n  - { node: pumbo.test.kick, description: Kick }",
    );
    env.config_file("adm", "config.yml", "api_key: s3cr3t\nlevel: 2\n");
    env.file(
        "permissions.yml",
        "players:\n  boss:\n    permissions: [pumbo.test.*, pumbo.proxy.plugins]\n",
    );
    let (host, bridge) = start(env.host_config("")).await;
    host.player_joined(player(1, "boss", None));
    host.player_joined(player(2, "nobody", None));

    let d = host.describe_json("adm").unwrap();
    assert_eq!(d["actions"][0]["name"], "kick");
    assert_eq!(d["actions"][0]["params"][0]["kind"], "player");
    assert_eq!(d["metrics"][0]["name"], "hits");
    assert_eq!(
        d["config-schema"]["properties"]["api_key"]["x-pumbo-secret"],
        true
    );
    assert!(!d.to_string().contains("s3cr3t"));
    let view = host.config_view("adm").unwrap();
    assert_eq!(view["api_key"], "***");
    assert_eq!(view["level"], 2);

    let kick =
        |p: u64| host.dispatch_command(CommandSender::Player(p), "/pumbo test kick Steve bye now");
    assert!(matches!(kick(2), CommandOutcome::Refused(_)));
    assert_eq!(kick(1), CommandOutcome::Handled);
    wait_for("action", LONG, || {
        bridge.messages(1).iter().any(|m| m.starts_with("action"))
    })
    .await;
    assert_eq!(
        bridge.messages(1)[0],
        "action kick player=Steve,reason=bye now by Actor::Player(1) level 2"
    );

    // A broken config is refused with the field; the plugin keeps the old one.
    env.config_file("adm", "config.yml", "level: high\n");
    let r = host.dispatch_command(CommandSender::Player(1), "/pumbo test reload");
    let CommandOutcome::Refused(t) = r else {
        panic!("{r:?}")
    };
    assert!(
        t.plain_text().contains("config.yml: level: expected"),
        "{}",
        t.plain_text()
    );
    env.config_file("adm", "config.yml", "level: 5\n");
    env.config_file("adm", "servers/survival.yml", "levle: 3\n");
    let r = host.dispatch_command(CommandSender::Player(1), "/pumbotest reload");
    let CommandOutcome::Refused(t) = r else {
        panic!("{r:?}")
    };
    assert!(
        t.plain_text()
            .contains("servers/survival.yml: levle: unknown option"),
        "{}",
        t.plain_text()
    );
    bridge.clear();
    assert_eq!(kick(1), CommandOutcome::Handled);
    wait_for("old level", LONG, || {
        bridge.messages(1).iter().any(|m| m.ends_with("level 2"))
    })
    .await;
    env.config_file("adm", "servers/survival.yml", "level: 3\n");
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), "/pumbo test reload"),
        CommandOutcome::Handled
    );
    wait_for("reloaded", LONG, || {
        bridge.messages(1).iter().any(|m| m == "Reloaded.")
    })
    .await;
    bridge.clear();
    assert_eq!(kick(1), CommandOutcome::Handled);
    wait_for("new level", LONG, || {
        bridge.messages(1).iter().any(|m| m.ends_with("level 5"))
    })
    .await;

    // Declared metrics only.
    assert_eq!(
        ask(&host, &bridge, 1, "/metric hits 3 x", "metric").await,
        "metric Ok(())"
    );
    assert!(
        ask(&host, &bridge, 1, "/metric nope 1", "metric")
            .await
            .contains("not declared")
    );
    assert!(
        host.render_metrics()
            .contains("pumbo_plugin_adm_hits{kind=\"x\"} 3"),
        "{}",
        host.render_metrics()
    );

    let list = replies(host.dispatch_command(CommandSender::Player(1), "/pumbo"));
    assert!(list.iter().any(|l| l.starts_with("adm 0.1.0")), "{list:?}");
    assert!(matches!(
        host.dispatch_command(CommandSender::Player(2), "/pumbo"),
        CommandOutcome::Refused(_)
    ));
    // `/pumbotest` and its short alias are in the command tree of who may use them.
    let tree = host.visible_commands(1);
    let own = tree.iter().find(|c| c.name == "pumbotest").unwrap();
    assert_eq!(own.aliases, ["tt"]);
    assert!(own.subcommands.contains(&"reload".to_string()), "{own:?}");
    assert!(own.subcommands.contains(&"kick".to_string()), "{own:?}");
    assert!(
        !host
            .visible_commands(2)
            .iter()
            .any(|c| c.name == "pumbotest")
    );
    let v = replies(host.dispatch_command(CommandSender::Console, "/pumbotest version"));
    assert_eq!(v, ["adm 0.1.0 Running"]);
    // The short alias is `/pumbotest`; without a subcommand: the commands.
    let v = replies(host.dispatch_command(CommandSender::Console, "/tt version"));
    assert_eq!(v, ["adm 0.1.0 Running"]);
    let help = replies(host.dispatch_command(CommandSender::Console, "/tt"));
    assert_eq!(help[0], "adm 0.1.0   /pumbo test", "{help:?}");
    assert!(help.contains(&"kick".to_string()), "{help:?}");
    let perms = replies(host.dispatch_command(CommandSender::Console, "/pumbo proxy perms list"));
    assert!(perms.iter().any(|l| l.starts_with("pumbo.test.kick")));
    let check = replies(host.dispatch_command(
        CommandSender::Console,
        "/pumbo proxy perms check boss pumbo.test.kick",
    ));
    assert!(check[0].starts_with("pumbo.test.kick = true"), "{check:?}");
    // A server node without an entry is not denied by the proxy: the server decides.
    let check = replies(host.dispatch_command(
        CommandSender::Console,
        "/pumbo proxy perms check boss minecraft:command.gamemode survival",
    ));
    assert!(
        check[0].contains("not set") && check[0].contains("the server decides"),
        "{check:?}"
    );

    // The same description without starting the plugin.
    let (json, check) = pumbo_host::describe_offline(env.host_config(""), "adm")
        .await
        .unwrap();
    assert_eq!(json["actions"][1]["dangerous"], true);
    assert_eq!(check, Ok(()));
}

/// A plugin enabled on one server only (plan §11, E5): players elsewhere do
/// not see its commands or events; events without a server reach it.
#[tokio::test(flavor = "multi_thread")]
async fn plugin_enabled_per_server() {
    let env = Env::new("scope");
    env.plugin(
        "surv",
        "test",
        "events: [chat, gate, context-changed]\ngate: { name: g }",
    );
    env.config_file("surv", "config.yml", "prefix: s\nreport: 1\n");
    let cfg = env.host_config(
        "server-group:\n  - { name: minigames, servers: [bedwars, skywars] }\nplugins:\n  surv:\n    servers: [survival]\n    groups: [minigames]\n    except: [skywars]\n",
    );
    let (host, bridge) = start(cfg).await;
    host.player_joined(player(1, "a", None));
    // No server yet (gates): the plugin gets the event.
    host.player_joined(player(9, "deny_bot", None));
    assert!(matches!(host.run_gates(9).await, GateOutcome::Deny(_)));

    host.player_server_changed(1, Some("lobby".into()), false);
    let names = |id| {
        host.visible_commands(id)
            .into_iter()
            .map(|c| c.name)
            .collect::<Vec<_>>()
    };
    assert!(!names(1).contains(&"secho".to_string()));
    assert_eq!(
        host.dispatch_command(CommandSender::Player(1), "/secho x"),
        CommandOutcome::NotOurs
    );
    assert_eq!(host.on_chat(1, "echo:0".into()).await, ChatReply::Pass);
    // Sensitive commands are never forwarded, even where the plugin is off.
    assert!(matches!(
        host.dispatch_command(CommandSender::Player(1), "/ssecret pw"),
        CommandOutcome::Refused(_)
    ));

    bridge.clear();
    host.player_server_changed(1, Some("survival".into()), false);
    assert!(bridge.count(|c| matches!(c, pumbo_host::PlayerCommand::CommandsChanged)) >= 1);
    assert!(names(1).contains(&"secho".to_string()));
    assert_eq!(ask(&host, &bridge, 1, "/secho x", "echo").await, "echo x");
    assert_eq!(host.on_chat(1, "echo:0".into()).await, ChatReply::Cancel);

    host.player_server_changed(1, Some("bedwars".into()), false);
    assert!(names(1).contains(&"secho".to_string()));
    host.player_server_changed(1, Some("skywars".into()), false);
    assert!(!names(1).contains(&"secho".to_string()));
    // Console commands always reach the plugin.
    assert_eq!(
        host.dispatch_command(CommandSender::Console, "/secho y"),
        CommandOutcome::Handled
    );
    // `context-changed` when the old or the new server is in the scope.
    wait_for("context events", LONG, || {
        bridge
            .messages(1)
            .contains(&"ctx 1 skywars from bedwars".to_string())
    })
    .await;
    let ctx: Vec<String> = bridge
        .messages(1)
        .into_iter()
        .filter(|m| m.starts_with("ctx "))
        .collect();
    assert!(
        ctx.contains(&"ctx 1 bedwars from survival".to_string()),
        "{ctx:?}"
    );
    assert!(
        ctx.contains(&"ctx 1 skywars from bedwars".to_string()),
        "{ctx:?}"
    );
}

/// Config overlays per server and group with the example plugin: the value
/// changes with the player's server without a restart; of two groups the
/// first in the config wins.
#[tokio::test(flavor = "multi_thread")]
async fn config_overlays_follow_the_player() {
    let env = Env::new("overlays");
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/example/pumbo-example.yml"
    ))
    .unwrap();
    let body: String = manifest
        .lines()
        .filter(|l| !l.starts_with("id:") && !l.starts_with("version:") && !l.starts_with("api:"))
        .collect::<Vec<_>>()
        .join("\n");
    env.plugin("pumbo-example", "example", &body);
    env.config_file("pumbo-example", "config.yml", "greeting: Hello\n");
    env.config_file(
        "pumbo-example",
        "servers/survival.yml",
        "greeting: Welcome to survival\n",
    );
    env.config_file("pumbo-example", "groups/arenas.yml", "greeting: Fight\n");
    env.config_file("pumbo-example", "groups/events.yml", "greeting: Party\n");
    let cfg = env.host_config(
        "server-group:\n  - { name: arenas, servers: [arena] }\n  - { name: events, servers: [arena, party] }\n",
    );
    let (host, bridge) = start(cfg).await;
    assert_eq!(
        host.plugin("pumbo-example").unwrap().status(),
        Status::Running
    );
    host.player_joined(player(1, "Steve", None));
    host.player_server_changed(1, Some("lobby".into()), false);
    let hello = ask(&host, &bridge, 1, "/hello", "[Example] Hello").await;
    assert_eq!(hello, "[Example] Hello, Steve! (hello #1)");
    host.player_server_changed(1, Some("survival".into()), false);
    assert_eq!(
        ask(&host, &bridge, 1, "/hello", "[Example]").await,
        "[Example] Welcome to survival, Steve! (hello #2)"
    );
    host.player_server_changed(1, Some("arena".into()), false);
    assert!(
        ask(&host, &bridge, 1, "/hello", "[Example]")
            .await
            .starts_with("[Example] Fight")
    );
    host.player_server_changed(1, Some("party".into()), false);
    assert!(
        ask(&host, &bridge, 1, "/hello", "[Example]")
            .await
            .starts_with("[Example] Party")
    );
    // The counter service and its push placeholder; the pull placeholder per context.
    assert_eq!(host.render_now("%hellos%", &[], Some(1)).plain_text(), "4");
    assert_eq!(
        host.render("%example_greeting@survival%", &[], Some(1))
            .await
            .plain_text(),
        "Welcome to survival"
    );
    assert_eq!(host.validate_config("pumbo-example"), Ok(()));
}

/// Plan §4.4 item 9 with services: `connect` from `on-server-connect`,
/// command registration, chat to itself, service cycles A→B→A beyond the
/// depth, a provider that loops and one that never answers — all at once.
/// `PUMBO_LOAD_PLAYERS` and `PUMBO_LOAD_SECS` set the size (plan: 500, 600).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn load_with_services() {
    let env = Env::new("load");
    env.plugin(
        "svc-a",
        "test",
        "events: [server-connect, chat]\nprovides:\n  - { service: \"test:a\", version: \"1.0\" }\nuses:\n  - { service: \"test:b\", version: \"1.0\" }\n  - { service: \"test:bad\", version: \"1.0\" }",
    );
    env.plugin(
        "svc-b",
        "test",
        "provides:\n  - { service: \"test:b\", version: \"1.0\" }\nuses:\n  - { service: \"test:a\", version: \"1.0\" }",
    );
    env.plugin(
        "bad",
        "test",
        "provides:\n  - { service: \"test:bad\", version: \"1.0\" }",
    );
    env.config_file("svc-a", "config.yml", "peer: test:b\n");
    env.config_file("svc-b", "config.yml", "prefix: b\npeer: test:a\n");
    env.config_file("bad", "config.yml", "prefix: x\n");
    let mut cfg: HostConfig = env.host_config("");
    cfg.plugins.max_failures = 1_000_000;
    let (host, bridge) = start(cfg).await;
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
            let mut n = 0u64;
            while Instant::now() < until {
                let e = ConnectEvent {
                    player: i,
                    target: "loop".into(),
                    reason: "test".into(),
                };
                if tokio::time::timeout(Duration::from_secs(10), host.on_server_connect(e))
                    .await
                    .is_err()
                {
                    hangs.fetch_add(1, Ordering::Relaxed);
                }
                let line = match n % 4 {
                    0 => format!("/call test:b ping {}", 1 + n % 12),
                    1 => format!("/reg c{}", i % 10),
                    2 if i % 50 == 0 => "/call test:bad hang 0".to_string(),
                    3 if i % 97 == 0 && n % 40 == 3 => "/call test:bad spin 0".to_string(),
                    _ => "/echo x".to_string(),
                };
                let _ = host.dispatch_command(CommandSender::Player(i), &line);
                if tokio::time::timeout(Duration::from_secs(10), host.on_chat(i, "echo:2".into()))
                    .await
                    .is_err()
                {
                    hangs.fetch_add(1, Ordering::Relaxed);
                }
                n += 1;
                rounds.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let calls = bridge.count(|c| matches!(c, pumbo_host::PlayerCommand::Message(m) if m.plain_text().starts_with("call ")));
    eprintln!(
        "[measure] load: {players} players, {secs} s, {} rounds, {calls} service answers, hangs {}",
        rounds.load(Ordering::Relaxed),
        hangs.load(Ordering::Relaxed)
    );
    assert_eq!(hangs.load(Ordering::Relaxed), 0);
    // Calls still in flight (hang, slow) end within the service deadline.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut alive = 0;
    for _ in 0..300 {
        alive = tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks();
        if alive <= baseline + 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eprintln!("[measure] tasks after the load: {alive} (baseline {baseline})");
    // The hanging provider keeps its own tasks (they cannot be cancelled in
    // wasmtime 49); host tasks must not leak beyond them.
    assert!(
        alive <= baseline + 8,
        "tasks leaked: {alive}, baseline {baseline}"
    );
    assert_eq!(host.plugin("svc-a").unwrap().status(), Status::Running);
    assert_eq!(
        ask(&host, &bridge, 1, "/call test:b ping 3", "call").await,
        "call ok 3"
    );
}
