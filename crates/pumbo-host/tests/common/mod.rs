//! Test harness: builds the WASM plugins once, writes plugin directories and
//! fakes the proxy core behind the bridge.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::future::BoxFuture;
use pumbo_host::wit::events::ConnectEvent;
use pumbo_host::wit::players::ConnectError;
use pumbo_host::wit::types::{Connection, PlayerContext, PlayerId, PlayerInfo, Profile, Route};
use pumbo_host::{
    CommandOutcome, CommandSender, Host, HostConfig, PlayerCommand, ProxyBridge, Status,
};

/// Builds the plugins for `wasm32-wasip2` once per test binary.
pub fn wasm(name: &str) -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        if let Ok(p) = std::env::var("PUMBO_PLUGINS_WASM") {
            return PathBuf::from(p);
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target/wasm-plugins");
        let status = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args([
                "build",
                "--locked",
                "-p",
                "pumbo-host-test-plugin",
                "-p",
                "pumbo-example",
                "--target",
                "wasm32-wasip2",
                "--profile",
                "plugin",
                "--target-dir",
            ])
            .arg(&target)
            .status()
            .expect("cargo build of the plugins");
        assert!(status.success(), "building the plugins failed");
        target.join("wasm32-wasip2/plugin")
    });
    let file = match name {
        "test" => "pumbo_host_test_plugin.wasm",
        "example" => "pumbo_example.wasm",
        other => panic!("unknown plugin {other}"),
    };
    dir.join(file)
}

static RUN: AtomicU64 = AtomicU64::new(0);

/// A plugin directory of one test. Directories are unique per run and left
/// in place under the target directory.
pub struct Env {
    pub root: PathBuf,
}

impl Env {
    pub fn new(test: &str) -> Env {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let n = RUN.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("host-tests")
            .join(format!("{test}-{stamp}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        Env { root }
    }

    pub fn plugins(&self) -> PathBuf {
        self.root.join("plugins")
    }

    /// Writes `<id>.yml` and copies the module (`which`: "test", "example",
    /// or "" for none).
    pub fn plugin(&self, id: &str, which: &str, manifest: &str) {
        let text = format!("id: {id}\nversion: 0.1.0\napi: \"0.1\"\n{manifest}\n");
        std::fs::write(self.plugins().join(format!("{id}.yml")), text).unwrap();
        if !which.is_empty() {
            std::fs::copy(wasm(which), self.plugins().join(format!("{id}.wasm"))).unwrap();
        }
    }

    pub fn config_file(&self, id: &str, path: &str, text: &str) {
        let p = self.plugins().join(id).join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    pub fn file(&self, path: &str, text: &str) -> PathBuf {
        let p = self.root.join(path);
        std::fs::write(&p, text).unwrap();
        p
    }

    /// Host config for tests: short backoffs, this directory.
    pub fn host_config(&self, extra: &str) -> HostConfig {
        let dir = self.plugins().display().to_string();
        // Into the `plugins` mapping of `extra` when it has one.
        let ours = format!(
            "  dir: {dir:?}\n  restart-backoff-ms: [100, 200]\n  gate-reload-wait-ms: 5000\n"
        );
        let extra = format!("\n{extra}\n");
        let text = if extra.contains("\nplugins:\n") {
            extra.replacen("\nplugins:\n", &format!("\nplugins:\n{ours}"), 1)
        } else {
            format!("{extra}plugins:\n{ours}")
        };
        let mut cfg = HostConfig::parse(&text).unwrap();
        cfg.permissions.file = self.root.join("permissions.yml");
        cfg
    }
}

/// The proxy core as the host sees it: records commands, runs `connect`
/// like a session task would (`on-server-connect`, then the server change)
/// and feeds `echo:<n>` messages back as chat.
#[derive(Default)]
pub struct FakeBridge {
    pub host: OnceLock<Host>,
    pub sent: Mutex<Vec<(PlayerId, PlayerCommand)>>,
    pub connects: AtomicU64,
}

impl FakeBridge {
    pub fn messages(&self, id: PlayerId) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| *p == id)
            .filter_map(|(_, c)| match c {
                PlayerCommand::Message(m) => Some(m.plain_text()),
                _ => None,
            })
            .collect()
    }

    pub fn count(&self, f: impl Fn(&PlayerCommand) -> bool) -> usize {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, c)| f(c))
            .count()
    }

    pub fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }
}

impl ProxyBridge for FakeBridge {
    fn send(&self, player: PlayerId, cmd: PlayerCommand) {
        if let PlayerCommand::Message(m) = &cmd
            && m.plain_text().starts_with("echo:")
            && let Some(host) = self.host.get()
        {
            let host = host.clone();
            let text = m.plain_text();
            tokio::spawn(async move {
                let _ = host.on_chat(player, text).await;
            });
        }
        // Keep memory bounded in long tests.
        let mut sent = self.sent.lock().unwrap();
        if sent.len() > 200_000 {
            sent.clear();
        }
        sent.push((player, cmd));
    }

    fn connect(
        &self,
        player: PlayerId,
        server: String,
    ) -> BoxFuture<'static, Result<(), ConnectError>> {
        self.connects.fetch_add(1, Ordering::Relaxed);
        let host = self.host.get().cloned();
        Box::pin(async move {
            let Some(host) = host else {
                return Err(ConnectError::Cancelled);
            };
            let e = ConnectEvent {
                player,
                target: server.clone(),
                reason: "plugin".into(),
            };
            match host.on_server_connect(e).await {
                pumbo_host::ConnectDecision::Allow => {
                    host.player_server_changed(player, Some(server), false);
                    Ok(())
                }
                pumbo_host::ConnectDecision::Deny(t) => Err(ConnectError::Refused(
                    pumbo_host::wit::types::Text::Plain(t.plain_text()),
                )),
                pumbo_host::ConnectDecision::Redirect(_) => Ok(()),
            }
        })
    }

    fn reconnect(&self, _: PlayerId) -> BoxFuture<'static, Result<(), ConnectError>> {
        Box::pin(async { Ok(()) })
    }
}

pub async fn start(cfg: HostConfig) -> (Host, Arc<FakeBridge>) {
    let bridge = Arc::new(FakeBridge::default());
    let host = Host::start(cfg, bridge.clone()).await.expect("host start");
    let _ = bridge.host.set(host.clone());
    host.wait_started(Duration::from_secs(120))
        .await
        .expect("required plugins load");
    (host, bridge)
}

pub fn player(id: PlayerId, name: &str, server: Option<&str>) -> PlayerInfo {
    PlayerInfo {
        id,
        profile: Profile {
            id: pumbo_host::wit_uuid(uuid::Uuid::from_u64_pair(0xabcd, id)),
            name: name.into(),
            properties: Vec::new(),
        },
        online_mode: false,
        connection: connection(),
        brand: None,
        settings: None,
        server: server.map(str::to_string),
        context: PlayerContext {
            server: None,
            groups: Vec::new(),
        },
        in_virtual: false,
    }
}

pub fn connection() -> Connection {
    Connection {
        address: "127.0.0.1".into(),
        port: 50000,
        virtual_host: "localhost".into(),
        protocol: 777,
        original_protocol: None,
        route: Route::Direct,
    }
}

pub async fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub async fn wait_status(
    host: &Host,
    id: &str,
    timeout: Duration,
    f: impl Fn(&Status) -> bool,
) -> Status {
    let slot = host.plugin(id).expect("plugin");
    wait_for(&format!("status of {id}"), timeout, || f(&slot.status())).await;
    slot.status()
}

/// Sends a command that makes plugin `id` fail and waits until its instance is gone:
/// restarting, disabled or already replaced (a restart quicker than the status poll is not
/// missed).
pub async fn fail_by(host: &Host, id: &str, sender: CommandSender, line: &str) -> CommandOutcome {
    let slot = host.plugin(id).expect("plugin");
    let instance = slot.generation.load(Ordering::SeqCst);
    let out = host.dispatch_command(sender, line);
    wait_for(&format!("end of {id}"), Duration::from_secs(120), || {
        slot.status() != Status::Running || slot.generation.load(Ordering::SeqCst) != instance
    })
    .await;
    out
}

pub fn exists(p: &Path) -> bool {
    p.exists()
}
