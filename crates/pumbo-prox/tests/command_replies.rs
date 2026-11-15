//! What the built-in commands answer, for the console and in game, kept in
//! `tests/snapshots/command_replies.txt`: every reply line (plain text for
//! the console, the chat JSON for a player), what the command logs, the
//! messages that come later and what other players' sessions get. `PUMBO_SNAPSHOT_WRITE=1` writes the
//! file anew.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pumbo_core::profile::GameProfile;
use pumbo_prox::commands::{self, Source};
use pumbo_prox::plugins::Plugins;
use pumbo_prox::server::{Proxy, SessionCmd};
use pumbo_servers::State;
use pumbo_text::{Component, TextFormat};
use tokio::sync::mpsc::Receiver;
use uuid::Uuid;

const TAG: &str = "0.2.0+26.3-26.51";
const WAIT: Duration = Duration::from_secs(30);

fn cargo_build(args: &[&str], target: &Path) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ok = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["build", "--locked"])
        .args(args)
        .arg("--target-dir")
        .arg(target)
        .status()
        .unwrap()
        .success();
    assert!(ok, "cargo build {args:?}");
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fake_pumpkin() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let target = root().join("target/fake-pumpkin");
        cargo_build(&["-p", "fake-pumpkin"], &target);
        target.join("debug/fake-pumpkin")
    })
    .clone()
}

fn test_plugin() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let target = root().join("target/wasm-plugins");
        cargo_build(
            &[
                "-p",
                "pumbo-host-test-plugin",
                "--target",
                "wasm32-wasip2",
                "--profile",
                "plugin",
            ],
            &target,
        );
        target.join("wasm32-wasip2/plugin/pumbo_host_test_plugin.wasm")
    })
    .clone()
}

/// A failing test must not leave its servers running (own PIDs only).
struct Cleanup(Arc<pumbo_servers::Manager>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for pid in self.0.list().into_iter().filter_map(|s| s.pid) {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
}

/// A player with a session queue that the test reads.
struct Online {
    name: &'static str,
    id: Uuid,
    rx: Receiver<SessionCmd>,
    _guard: pumbo_prox::server::PlayerGuard,
}

async fn join(proxy: &Arc<Proxy>, name: &'static str, id: Uuid) -> Online {
    let profile = GameProfile {
        id,
        name: name.into(),
        properties: Vec::new(),
    };
    let addr = "127.0.0.1:40000".parse().unwrap();
    let (guard, rx) = proxy.register_player(&profile, addr).unwrap();
    if let Some(p) = proxy.plugins.get() {
        p.joined(
            profile,
            false,
            pumbo_prox::plugins::connection(addr, "localhost", 777),
        )
        .await
        .unwrap();
    }
    proxy.set_server(id, Some("lobby"));
    Online {
        name,
        id,
        rx,
        _guard: guard,
    }
}

/// What the calling thread logs at info and above while the guard lives,
/// the way the proxy writes it: no time, no target.
fn capture_logs() -> (
    tracing::subscriber::DefaultGuard,
    Arc<std::sync::Mutex<Vec<u8>>>,
) {
    struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let w = Arc::clone(&log);
    let sub = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || Buf(Arc::clone(&w)))
        .finish();
    (tracing::subscriber::set_default(sub), log)
}

/// Who runs a line in [`Snap::run`].
#[derive(Clone, Copy)]
enum By {
    Console,
    Player(usize),
}

struct Snap {
    out: String,
    /// Replaced by `<name>` in the file: temporary paths, ports, versions.
    vary: Vec<(String, &'static str)>,
}

impl Snap {
    fn component(&mut self, prefix: &str, c: &Component, player: bool) {
        let _ = writeln!(self.out, "{prefix}{}", c.plain_text());
        if player {
            let _ = writeln!(self.out, "{prefix}  {}", c.to_json(TextFormat::V770));
        }
    }

    fn cmd(&mut self, who: &str, cmd: &SessionCmd) {
        match cmd {
            SessionCmd::Message(c) => self.component(&format!("  -> {who}: "), c, true),
            SessionCmd::Connect { server, quiet } => {
                let _ = writeln!(self.out, "  -> {who}: connect {server} quiet={quiet}");
            }
            other => {
                let _ = writeln!(self.out, "  -> {who}: {other:?}");
            }
        }
    }

    /// Runs `line` as `by` the way the console and a session do: the proxy's
    /// own commands first, then the plugin host. What the command logs at
    /// info and above follows its reply. `late` messages are waited for (the
    /// player's, the console's go to the log).
    async fn run(
        &mut self,
        proxy: &Arc<Proxy>,
        players: &mut [Online],
        by: By,
        line: &str,
        late: usize,
    ) {
        let rt = proxy.runtime();
        let (label, source, player) = match by {
            By::Console => ("console".to_string(), Source::Console, false),
            By::Player(i) => (
                players[i].name.to_string(),
                Source::Player {
                    id: players[i].id,
                    server: Some("lobby"),
                },
                true,
            ),
        };
        let _ = writeln!(self.out, "\n== {label}: {line}");
        let (logs, log) = capture_logs();
        let run = commands::run(proxy, &rt, &source, line);
        let host = match (&run, proxy.plugins.get()) {
            (None, Some(p)) => match by {
                By::Console => p.console(line),
                By::Player(i) => p.command(players[i].id, line),
            },
            _ => None,
        };
        drop(logs);
        match run {
            Some(reply) => {
                for l in &reply.lines {
                    self.component("  ", l, player);
                }
                if let Some(to) = reply.connect {
                    let _ = writeln!(self.out, "  connect: {to}");
                }
            }
            None => match host {
                Some(lines) => {
                    let _ = writeln!(self.out, "  (plugin host)");
                    for l in &lines {
                        self.component("  ", l, player);
                    }
                }
                None => {
                    let _ = writeln!(self.out, "  (not a proxy command)");
                }
            },
        }
        let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
        for l in log.lines() {
            let _ = writeln!(self.out, "  log: {}", l.trim_start());
        }
        if let By::Player(i) = by {
            for _ in 0..late {
                let cmd = tokio::time::timeout(WAIT, players[i].rx.recv())
                    .await
                    .unwrap_or_else(|_| panic!("{line}: no late answer"))
                    .unwrap();
                let name = players[i].name;
                self.cmd(name, &cmd);
            }
        }
        for p in players.iter_mut() {
            while let Ok(cmd) = p.rx.try_recv() {
                let name = p.name;
                self.cmd(name, &cmd);
            }
        }
    }

    fn check(self, file: &str) {
        let mut text = self.out;
        for (from, to) in &self.vary {
            text = text.replace(from.as_str(), to);
        }
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/snapshots")
            .join(file);
        if std::env::var_os("PUMBO_SNAPSHOT_WRITE").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &text).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_default();
        if want != text {
            let new = path.with_extension("txt.new");
            std::fs::write(&new, &text).unwrap();
            let at = want
                .lines()
                .zip(text.lines())
                .position(|(a, b)| a != b)
                .unwrap_or(want.lines().count().min(text.lines().count()));
            panic!(
                "command replies changed at line {} (new file: {}):\n- {}\n+ {}",
                at + 1,
                new.display(),
                want.lines().nth(at).unwrap_or_default(),
                text.lines().nth(at).unwrap_or_default()
            );
        }
    }
}

async fn wait(what: &str, mut f: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !f() {
        assert!(start.elapsed() < WAIT, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Every command of the proxy, its servers, its route and the plugin host's
/// admin commands, with each answer they give.
#[tokio::test(flavor = "multi_thread")]
async fn command_replies_stay_the_same() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("prox-replies-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let version = dir.join("versions/pumpkin").join(TAG);
    std::fs::create_dir_all(&version).unwrap();
    std::fs::copy(fake_pumpkin(), version.join("pumpkin")).unwrap();
    let plugins_dir = dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    // One plugin: the test plugin twice would register its commands twice.
    std::fs::write(
        plugins_dir.join("perms.yml"),
        "id: perms\nversion: 0.1.0\napi: \"0.1\"\nshort-name: perms\nevents: [gate]\ngate: { name: auth }\n",
    )
    .unwrap();
    std::fs::copy(test_plugin(), plugins_dir.join("perms.wasm")).unwrap();
    let key = dir.join("bridge.key");
    std::fs::write(&key, "ab".repeat(32)).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let (upstream, _) = common::backend().await;
    let owner = pumbo_identity::offline_uuid("Owner");
    let ports = common::ephemeral_ports();
    let d = common::toml_path(&dir);
    let config = format!(
        "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
         servers:\n  lobby: {{ address: \"127.0.0.1:1\" }}\n  survival: {{ address: \"127.0.0.1:2\" }}\n\
         routing:\n  try: [lobby]\ncommands:\n  operators: [\"{owner}\"]\n\
         limits:\n  connections-per-ip-per-second: 1000\n  concurrent-per-ip: 1000\n\
         managed-servers:\n  enabled: true\n  dir: \"{d}/servers\"\n  versions-dir: \"{d}/versions\"\n  \
         ports: {ports}\n  stop-timeout-secs: 5\n  download-from-game: false\n  \
         templates:\n    default: {{ autostart: false, restart-on-crash: 0 }}\n    \
         auto: {{ autostart: true, restart-on-crash: 0 }}\n\
         bridge:\n  enabled: true\n  listen: \"127.0.0.1:0\"\n  key-file: \"{d}/bridge.key\"\n\
         plugins:\n  dir: \"{d}/plugins\"\n",
    );
    let file = dir.join("pumboprox.yml");
    std::fs::write(&file, format!("# Test network\n{config}")).unwrap();
    let (proxy, _) = common::start_proxy_at(&config, Some(file.clone())).await;
    let mut snap = Snap {
        out: String::new(),
        vary: vec![
            (dir.display().to_string(), "<dir>"),
            (env!("CARGO_PKG_VERSION").to_string(), "<version>"),
        ],
    };
    let mut players: Vec<Online> = Vec::new();
    use By::{Console as C, Player as P};
    const OWNER: By = P(0);
    const GUEST: By = P(1);

    // Off: plugins, bridge, servers.
    for line in [
        "prox plugins",
        "prox debug services",
        "prox bridge",
        "prox servers",
        "prox route gates",
        "prox route gates require auth",
    ] {
        snap.run(&proxy, &mut players, C, line, 0).await;
    }

    pumbo_prox::servers::start(&proxy).unwrap();
    let m = proxy.servers.get().unwrap().clone();
    let _cleanup = Cleanup(m.clone());
    // After the servers: new servers get no bridge files (no download).
    let bridge =
        pumbo_prox::bridge::Bridge::new(&proxy, proxy.runtime().config.bridge.clone()).unwrap();
    proxy.bridge.set(bridge.clone()).unwrap();
    let plugins = Plugins::start(&config, &proxy, vec![bridge.native()])
        .await
        .unwrap()
        .unwrap();
    proxy.plugins.set(plugins.clone()).unwrap();
    for id in ["perms"] {
        let slot = plugins.host.plugin(id).unwrap();
        wait("plugin running", || {
            slot.status() == pumbo_host::Status::Running
        })
        .await;
    }
    players.push(join(&proxy, "Owner", owner).await);
    players.push(join(&proxy, "Guest", pumbo_identity::offline_uuid("Guest")).await);

    let cases: &[(By, &str, usize)] = &[
        // /server
        (OWNER, "server", 0),
        (OWNER, "server survival", 0),
        (OWNER, "server SURVIVAL", 0),
        (OWNER, "server ghost", 0),
        (OWNER, "server stop arena", 0),
        (GUEST, "server", 0),
        (C, "server", 0),
        (C, "server lobby", 0),
        // /glist, /find
        (C, "glist", 0),
        (OWNER, "glist", 0),
        (GUEST, "glist", 0),
        (C, "find", 0),
        (C, "find Steve", 0),
        (OWNER, "find guest", 0),
        (OWNER, "pumbo:find Guest", 0),
        // /send
        (C, "send", 0),
        (C, "send Owner", 0),
        (C, "send Owner nowhere", 0),
        (C, "send Steve lobby", 0),
        (C, "send current lobby", 0),
        (OWNER, "send current survival", 0),
        (OWNER, "send lobby survival", 0),
        (OWNER, "send Guest Survival", 0),
        (C, "send all lobby", 0),
        (GUEST, "send all lobby", 0),
        // /alert
        (C, "alert", 0),
        (C, "alert   ", 0),
        (C, "alert &cHello <bold>all</bold> & co", 0),
        (OWNER, "alert hi", 0),
        (GUEST, "alert hi", 0),
        // /prox, help, version, reload
        (C, "prox", 0),
        (C, "prox help", 0),
        (OWNER, "prox", 0),
        (OWNER, "prox help 2", 0),
        (OWNER, "prox help 9", 0),
        (OWNER, "prox bogus", 0),
        (GUEST, "prox", 0),
        (C, "prox version", 0),
        (GUEST, "prox version", 0),
        (C, "prox reload", 0),
        (OWNER, "prox reload", 0),
        (GUEST, "prox reload", 0),
        // plugins and debug (the plugin host)
        (C, "prox plugins", 0),
        (OWNER, "prox plugins", 0),
        (GUEST, "prox plugins", 0),
        (C, "prox plugins reload", 0),
        (C, "prox plugins frob perms", 0),
        (C, "prox plugins reload ghost", 0),
        (C, "prox plugins load perms", 0),
        (OWNER, "prox plugins reload perms", 1),
        (OWNER, "prox plugins unload perms", 1),
        (C, "prox plugins unload ghost", 0),
        (OWNER, "prox plugins load perms", 1),
        (GUEST, "prox plugins reload perms", 0),
        (C, "prox debug", 0),
        (C, "prox debug bogus", 0),
        (C, "prox debug services", 0),
        (C, "prox debug perms list", 0),
        (C, "prox debug perms Owner", 0),
        (C, "prox debug perms Owner pumbo.proxy.op", 0),
        (C, "prox debug perms Owner pumbo.proxy.reload lobby", 0),
        (C, "prox debug perms Guest pumbo.proxy.bridge.key", 0),
        (C, "prox debug perms Guest minecraft:command.gamemode", 0),
        (C, "prox debug perms Ghost pumbo.proxy.reload", 0),
        (OWNER, "prox debug services", 0),
        (GUEST, "prox debug services", 0),
        // the plugin host's own commands
        (C, "pumbo", 0),
        (OWNER, "pumbo", 0),
        (GUEST, "pumbo", 0),
        (C, "pumboperms", 0),
        (OWNER, "pumboperms version", 0),
        (OWNER, "pumboperms bogus", 0),
        (GUEST, "pumboperms version", 0),
        // bridge
        (C, "prox bridge", 0),
        (C, "prox bridge key", 0),
        (OWNER, "prox bridge", 0),
        (OWNER, "prox bridge key", 0),
        (OWNER, "prox bridge bogus", 0),
        (GUEST, "prox bridge", 0),
        // op and deop
        (C, "prox op", 0),
        (C, "prox deop", 0),
        (C, "prox op Steve", 0),
        (C, "prox op Owner", 0),
        (C, "prox deop Guest", 0),
        (GUEST, "prox op Guest", 0),
        (OWNER, "prox op Guest", 0),
        (OWNER, "prox op 069a79f4-44e9-4726-a5be-fca90e38aaf5", 0),
        (OWNER, "prox deop 069a79f4-44e9-4726-a5be-fca90e38aaf5", 0),
        (C, "prox op Guest", 0),
        (GUEST, "prox reload", 0),
        (OWNER, "prox deop guest", 0),
        (GUEST, "prox reload", 0),
        // claim on a network that has an operator
        (C, "prox claim", 0),
        (OWNER, "prox claim 123456", 0),
        (GUEST, "prox claim", 0),
        // route
        (C, "prox route", 0),
        (OWNER, "prox route", 0),
        (GUEST, "prox route", 0),
        (C, "prox route help", 0),
        (OWNER, "prox route help", 0),
        (C, "prox route gates", 0),
        (OWNER, "prox route gates", 0),
        (C, "prox route bogus", 0),
        (C, "prox route servers", 0),
        (C, "prox route servers add", 0),
        (C, "prox route servers add ghost", 0),
        (C, "prox route servers add lobby", 0),
        (C, "prox route servers add survival 5", 0),
        (C, "prox route servers add survival x", 0),
        (C, "prox route servers add survival 1", 0),
        (OWNER, "prox route servers move lobby 1", 0),
        (C, "prox route servers move ghost 1", 0),
        (C, "prox route servers move lobby 3", 0),
        (C, "prox route servers remove ghost", 0),
        (C, "prox route servers remove survival", 0),
        (C, "prox route servers remove lobby", 0),
        (C, "prox route host set bad_domain! lobby", 0),
        (C, "prox route host set play.example.org", 0),
        (C, "prox route host set play.example.org ghost", 0),
        (C, "prox route host set Play.Example.org lobby", 0),
        (OWNER, "prox route", 0),
        (C, "prox route host remove play.example.org", 0),
        (C, "prox route host remove play.example.org", 0),
        (C, "prox route gates require ghost", 0),
        (C, "prox route gates require auth", 0),
        (C, "prox route gates require auth", 0),
        (OWNER, "prox route gates", 0),
        (C, "prox route gates optional auth", 0),
        (C, "prox route gates optional auth", 0),
        (GUEST, "prox route servers add survival", 0),
    ];
    for (by, line, late) in cases {
        snap.run(&proxy, &mut players, *by, line, *late).await;
    }
    // Changes that cannot be saved: the config file is gone.
    let moved = dir.join("pumboprox.yml.away");
    std::fs::rename(&file, &moved).unwrap();
    let cases: &[(By, &str, usize)] = &[
        (C, "prox route servers add survival", 0),
        (C, "prox route host set play.example.org lobby", 0),
        (C, "prox route gates require auth", 0),
        (C, "prox op Guest", 0),
        (C, "prox deop Owner", 0),
    ];
    for (by, line, late) in cases {
        snap.run(&proxy, &mut players, *by, line, *late).await;
    }
    std::fs::rename(&moved, &file).unwrap();
    // Config reloads: a file that does not read, a change that waits for a
    // restart, a required gate no plugin has, and back.
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, "listener: [\n").unwrap();
    snap.run(&proxy, &mut players, C, "prox reload", 0).await;
    snap.run(&proxy, &mut players, OWNER, "prox reload", 0)
        .await;
    let bind = "bind: \"127.0.0.1:0\"";
    assert!(text.contains(bind));
    std::fs::write(&file, text.replace(bind, "bind: \"127.0.0.1:1\"")).unwrap();
    snap.run(&proxy, &mut players, C, "prox reload", 0).await;
    std::fs::write(&file, &text).unwrap();
    snap.run(&proxy, &mut players, OWNER, "prox reload", 0)
        .await;
    let gates = "required-gates: []";
    assert!(text.contains(gates));
    std::fs::write(&file, text.replace(gates, "required-gates: [ghost]")).unwrap();
    snap.run(&proxy, &mut players, C, "prox reload", 0).await;
    snap.run(&proxy, &mut players, C, "prox route gates", 0)
        .await;
    snap.run(&proxy, &mut players, OWNER, "prox route gates", 0)
        .await;
    std::fs::write(&file, &text).unwrap();
    snap.run(&proxy, &mut players, C, "prox reload", 0).await;
    // Plugin reloads: code that does not load, a manifest that waits for a
    // restart, and back.
    let wasm = plugins_dir.join("perms.wasm");
    std::fs::write(&wasm, b"not wasm").unwrap();
    snap.run(&proxy, &mut players, OWNER, "prox plugins reload perms", 1)
        .await;
    std::fs::copy(test_plugin(), &wasm).unwrap();
    let manifest = plugins_dir.join("perms.yml");
    let yml = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, yml.replace("0.1.0", "0.2.0")).unwrap();
    snap.run(&proxy, &mut players, OWNER, "prox plugins reload perms", 1)
        .await;
    std::fs::write(&manifest, &yml).unwrap();
    snap.run(&proxy, &mut players, OWNER, "prox plugins reload perms", 1)
        .await;
    let cases: &[(By, &str, usize)] = &[
        // downloads
        (C, "prox download", 0),
        (OWNER, "prox download", 0),
        (GUEST, "prox download", 0),
        (C, "prox download help", 0),
        (OWNER, "prox download help", 0),
        (C, "prox download paper", 0),
        (C, "prox download nope", 0),
        (C, "prox download stop", 0),
        (OWNER, "prox download pumpkin 0.1.0", 0),
        // servers from the proxy
        (C, "prox servers", 0),
        (OWNER, "prox servers", 0),
        (OWNER, "prox servers list", 0),
        (GUEST, "prox servers", 0),
        (C, "prox servers help", 0),
        (OWNER, "prox servers help", 0),
        (C, "prox servers bogus", 0),
        (OWNER, "prox servers bogus", 0),
        (C, "prox servers new", 0),
        (C, "prox servers start", 0),
        (C, "prox servers logs", 0),
        (C, "prox servers delete", 0),
        (C, "prox servers start ghost", 0),
        (OWNER, "prox servers start ghost", 0),
        (C, "prox servers start lobby", 0),
        (C, "prox servers stop ghost", 0),
        (C, "prox servers stop lobby", 0),
        (C, "prox servers restart ghost", 0),
        (C, "prox servers restart lobby", 0),
        (GUEST, "prox servers start ghost", 0),
        (OWNER, &format!("prox servers new arena {TAG}"), 1),
    ];
    for (by, line, late) in cases {
        snap.run(&proxy, &mut players, *by, line, *late).await;
    }
    wait("arena listed", || {
        proxy.runtime().backends.iter().any(|b| b.name == "arena")
    })
    .await;
    let arena = dir.join("servers/arena");
    std::fs::write(arena.join("fake-upstream"), upstream.to_string()).unwrap();
    let toml = arena.join("pumpkin.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(toml, format!("{text}\n[telemetry]\nenabled = false\n")).unwrap();
    let port = m.entry("arena").unwrap().port;
    snap.vary.push((format!("port {port}"), "port <port>"));
    snap.vary.push((format!("\"{port}\""), "\"<port>\""));

    let cases: &[(By, &str, usize)] = &[
        (OWNER, &format!("prox servers new arena {TAG}"), 1),
        (C, "prox servers", 0),
        (OWNER, "prox servers", 0),
        (C, "prox servers logs arena", 0),
        (OWNER, "prox servers logs arena 5", 0),
        (OWNER, "server", 0),
        (OWNER, "prox servers stop arena", 1),
        (OWNER, "prox servers start arena", 1),
    ];
    for (by, line, late) in cases {
        snap.run(&proxy, &mut players, *by, line, *late).await;
    }
    let cases: &[(By, &str, usize)] = &[
        (OWNER, "prox servers start arena", 0),
        (OWNER, "prox servers restart arena", 1),
        (OWNER, "prox servers stop arena", 1),
        (OWNER, "prox servers stop arena", 1),
        (C, "prox servers delete arena", 0),
        (OWNER, "prox servers delete arena", 0),
        (OWNER, "prox servers delete arena nope", 0),
    ];
    for (by, line, late) in cases {
        snap.run(&proxy, &mut players, *by, line, *late).await;
    }
    wait("arena stopped", || m.state("arena") == State::Stopped).await;
    snap.run(
        &proxy,
        &mut players,
        OWNER,
        "prox servers delete arena confirm",
        1,
    )
    .await;
    snap.run(&proxy, &mut players, OWNER, "server arena", 0)
        .await;
    // A template that starts the new server, and a delete that cannot move
    // the folder.
    snap.run(
        &proxy,
        &mut players,
        OWNER,
        &format!("prox servers new auto {TAG} auto"),
        2,
    )
    .await;
    let port = m.entry("auto").unwrap().port;
    snap.vary.push((format!("port {port}"), "port <port>"));
    snap.vary.push((format!("\"{port}\""), "\"<port>\""));
    let trash = dir.join("servers/.trash");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(&trash, mode(0o555)).unwrap();
        snap.run(
            &proxy,
            &mut players,
            OWNER,
            "prox servers delete auto confirm",
            1,
        )
        .await;
        std::fs::set_permissions(&trash, mode(0o755)).unwrap();
    }
    for e in std::fs::read_dir(&trash).unwrap() {
        let name = e.unwrap().file_name().to_string_lossy().into_owned();
        snap.vary.push((name, "<trashed>"));
    }
    snap.check("command_replies.txt");
    drop(players);
    m.shutdown().await;
    proxy.stop();
}

/// `/prox claim` on a fresh network: no operator, no server.
#[tokio::test(flavor = "multi_thread")]
async fn claim_replies_stay_the_same() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("prox-claim-replies-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n";
    let file = dir.join("pumboprox.yml");
    std::fs::write(&file, config).unwrap();
    let (proxy, _) = common::start_proxy_at(config, Some(file.clone())).await;
    let mut snap = Snap {
        out: String::new(),
        vary: vec![
            (dir.display().to_string(), "<dir>"),
            (env!("CARGO_PKG_VERSION").to_string(), "<version>"),
        ],
    };
    let premium = Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap();
    let mut players = vec![
        join(&proxy, "Offline", pumbo_identity::offline_uuid("Offline")).await,
        join(&proxy, "Notch", premium).await,
        join(
            &proxy,
            "Late",
            Uuid::from_u128(0x1234_5678_9abc_4def_8123_4567_89ab_cdef),
        )
        .await,
    ];
    use By::{Console as C, Player as P};
    snap.run(&proxy, &mut players, C, "prox claim", 0).await;
    snap.run(&proxy, &mut players, P(1), "prox claim", 0).await;
    let code = commands::claim_code(&proxy).unwrap();
    let wrong = if code == "000000" { "000001" } else { "000000" };
    snap.vary.push((code.clone(), "<code>"));
    snap.vary.push((wrong.to_string(), "<wrong>"));
    let cases: Vec<(By, String)> = vec![
        (P(0), "prox".into()),
        (P(0), "prox claim".into()),
        (P(0), format!("prox claim {code}")),
        (P(1), "prox".into()),
        (P(1), "prox help".into()),
        (P(1), "prox claim".into()),
        (P(1), format!("prox claim {wrong}")),
        (C, "prox claim".into()),
    ];
    for (by, line) in &cases {
        snap.run(&proxy, &mut players, *by, line, 0).await;
    }
    // A claim that cannot be saved keeps the code.
    let moved = dir.join("pumboprox.yml.away");
    std::fs::rename(&file, &moved).unwrap();
    snap.run(&proxy, &mut players, P(1), &format!("prox claim {code}"), 0)
        .await;
    std::fs::rename(&moved, &file).unwrap();
    let cases: Vec<(By, String)> = vec![
        (P(1), format!("prox claim {code}")),
        (P(1), "prox claim".into()),
        (P(2), format!("prox claim {code}")),
    ];
    for (by, line) in &cases {
        snap.run(&proxy, &mut players, *by, line, 0).await;
    }
    // Too many wrong codes end the code.
    let _ = std::fs::write(dir.join("pumboprox.yml"), config);
    proxy.reload().unwrap();
    let code = commands::claim_code(&proxy).unwrap();
    let wrong = if code == "000000" { "000001" } else { "000000" };
    snap.vary.push((code.clone(), "<code>"));
    snap.vary.push((wrong.to_string(), "<wrong>"));
    for _ in 0..6 {
        snap.run(
            &proxy,
            &mut players,
            P(2),
            &format!("prox claim {wrong}"),
            0,
        )
        .await;
    }
    snap.run(&proxy, &mut players, P(2), "prox", 0).await;
    snap.check("claim_replies.txt");
    drop(players);
    proxy.stop();
}
