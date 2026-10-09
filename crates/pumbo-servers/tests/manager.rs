//! The manager with a stand-in for Pumpkin (`tests/fake-pumpkin`): create,
//! start, ready, stop, a kill after the stop timeout, restarts after crashes,
//! taking over a server left by a crashed proxy, delete to the trash, and
//! downloads from a local HTTP server.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use pumbo_servers::{
    Config, Manager, Network, ProcessRunner, Progress, Pumpkin, Source, State, Template,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TAG: &str = "0.2.0+26.3-26.51";
/// Its first download drops half way.
const FLAKY: &str = "0.3.0+26.4-26.60";

/// The stand-in, built once in its own target directory.
fn fake_bin() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target/fake-pumpkin");
        let ok = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args(["build", "--locked", "-p", "fake-pumpkin", "--target-dir"])
            .arg(&target)
            .status()
            .unwrap()
            .success();
        assert!(ok, "cargo build -p fake-pumpkin");
        target.join("debug/fake-pumpkin")
    })
    .clone()
}

/// A fresh folder with the stand-in as Pumpkin `TAG`; ports `first..first+9`.
fn config(tag: &str, first: u16) -> Config {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "servers-{tag}-{}-{}",
        std::process::id(),
        nanos()
    ));
    let version = dir.join("versions/pumpkin").join(TAG);
    std::fs::create_dir_all(&version).unwrap();
    std::fs::copy(fake_bin(), version.join("pumpkin")).unwrap();
    let mut templates = std::collections::BTreeMap::new();
    templates.insert(
        "default".into(),
        Template {
            autostart: false,
            ..Template::default()
        },
    );
    Config {
        enabled: true,
        dir: dir.join("servers"),
        versions_dir: dir.join("versions"),
        ports: format!("{first}-{}", first + 9),
        stop_timeout_secs: 5,
        templates,
        ..Config::default()
    }
}

fn nanos() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
}

fn manager(cfg: Config) -> Arc<Manager> {
    let sources: Vec<Arc<dyn Source>> = vec![Arc::new(Pumpkin::default())];
    let network = Network {
        velocity_secret: Some("s3cret".into()),
        bridge: Some(("127.0.0.1:25578".into(), "ab".repeat(32))),
    };
    Manager::new(cfg, network, sources, Arc::new(ProcessRunner::default())).unwrap()
}

/// Telemetry off in every test config.
fn no_telemetry(m: &Manager, name: &str) {
    let path = m.config.dir.join(name).join("pumpkin.toml");
    let mut toml = std::fs::read_to_string(&path).unwrap();
    toml.push_str("\n[telemetry]\nenabled = false\n");
    std::fs::write(path, toml).unwrap();
}

async fn wait(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out: {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn pid_of(m: &Manager, name: &str) -> Option<u32> {
    m.list().into_iter().find(|s| s.name == name)?.pid
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Own test process only, by its PID.
fn kill(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
}

async fn create(m: &Arc<Manager>, name: &str) {
    m.create(name, None, None, &[]).await.unwrap();
    no_telemetry(m, name);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_start_stop() {
    let m = manager(config("life", 25750));
    m.create("lobby", None, None, &["lobby".into()])
        .await
        .unwrap_err();
    create(&m, "arena").await;
    create(&m, "arena-2").await;
    let e = m.entry("arena").unwrap();
    let e2 = m.entry("arena-2").unwrap();
    assert_eq!(e.version, TAG);
    assert_ne!(e.port, e2.port);
    let toml =
        |n: &str| std::fs::read_to_string(m.config.dir.join(n).join("pumpkin.toml")).unwrap();
    let seed = |t: String| t.lines().nth(1).unwrap().to_string();
    assert_ne!(seed(toml("arena")), seed(toml("arena-2")));
    assert!(toml("arena").contains(&format!("address = \"127.0.0.1:{}\"", e.port)));
    assert!(toml("arena").contains("secret = \"s3cret\""));
    let bridge = m
        .config
        .dir
        .join("arena/plugins/data/pumbobridge/config.yml");
    assert!(
        std::fs::read_to_string(bridge)
            .unwrap()
            .contains("proxy: 127.0.0.1:25578")
    );
    assert!(m.create("arena", None, None, &[]).await.is_err());

    m.start("arena").unwrap();
    assert!(m.start("arena").is_err(), "running already");
    wait("ready", 20, || m.state("arena") == State::Ready).await;
    let pid = pid_of(&m, "arena").unwrap();
    assert_eq!(m.entry("arena").unwrap().pid, Some(pid));
    wait("log", 5, || {
        m.logs("arena", 10)
            .unwrap()
            .iter()
            .any(|l| l.contains("listening"))
    })
    .await;
    m.console("arena", "say hi").unwrap();
    wait("console", 5, || {
        m.logs("arena", 10).unwrap().iter().any(|l| l == "> say hi")
    })
    .await;
    assert!(
        std::fs::read_to_string(m.config.dir.join("arena/console.log"))
            .unwrap()
            .contains("> say hi")
    );
    m.stop("arena").await.unwrap();
    assert_eq!(m.state("arena"), State::Stopped);
    assert!(!alive(pid));
    assert_eq!(m.entry("arena").unwrap().pid, None);

    let to = m.delete("arena-2").await.unwrap();
    assert!(to.starts_with(m.config.dir.join(".trash")) && to.join("pumpkin.toml").is_file());
    assert!(m.entry("arena-2").is_none());
    assert!(!m.config.dir.join("arena-2").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn restarts_after_a_crash_then_gives_up() {
    let mut cfg = config("crash", 25760);
    cfg.templates.get_mut("default").unwrap().restart_on_crash = 1;
    let m = manager(cfg);
    create(&m, "arena").await;
    m.start("arena").unwrap();
    wait("ready", 20, || m.state("arena") == State::Ready).await;
    let first = pid_of(&m, "arena").unwrap();
    kill(first);
    wait("restarted", 20, || {
        m.state("arena") == State::Ready && pid_of(&m, "arena").is_some_and(|p| p != first)
    })
    .await;
    let second = pid_of(&m, "arena").unwrap();
    kill(second);
    wait("crashed", 10, || m.state("arena") == State::Crashed).await;
    assert_eq!(m.entry("arena").unwrap().pid, None);
    // A crashed server starts again by hand.
    m.start("arena").unwrap();
    wait("ready again", 20, || m.state("arena") == State::Ready).await;
    m.shutdown().await;
    assert_eq!(m.state("arena"), State::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_ignoring_stop_is_killed() {
    let mut cfg = config("kill", 25770);
    cfg.stop_timeout_secs = 1;
    let m = manager(cfg);
    create(&m, "stubborn").await;
    std::fs::write(m.config.dir.join("stubborn/fake-ignore-stop"), "").unwrap();
    m.start("stubborn").unwrap();
    wait("ready", 20, || m.state("stubborn") == State::Ready).await;
    let pid = pid_of(&m, "stubborn").unwrap();
    let start = Instant::now();
    m.stop("stubborn").await.unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert!(!alive(pid));
    assert!(
        m.logs("stubborn", 10)
            .unwrap()
            .iter()
            .any(|l| l == "ignoring stop")
    );
}

#[test]
fn a_server_left_by_a_crashed_proxy_is_taken_over() {
    let cfg = config("orphan", 25780);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pid = rt.block_on(async {
        let m = manager(cfg.clone());
        create(&m, "arena").await;
        m.start("arena").unwrap();
        wait("ready", 20, || m.state("arena") == State::Ready).await;
        pid_of(&m, "arena").unwrap()
    });
    // The proxy dies: its tasks end, the server keeps running.
    rt.shutdown_timeout(Duration::from_secs(1));
    assert!(alive(pid));
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let m = manager(cfg);
        m.boot();
        wait("taken over", 5, || m.state("arena") == State::Ready).await;
        assert_eq!(pid_of(&m, "arena"), Some(pid));
        m.stop("arena").await.unwrap();
        wait("gone", 10, || !alive(pid)).await;
        assert_eq!(m.entry("arena").unwrap().pid, None);
    });
    if alive(pid) {
        kill(pid);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn autostart_and_a_stale_pid() {
    let mut cfg = config("boot", 25790);
    cfg.templates.get_mut("default").unwrap().autostart = true;
    let m = manager(cfg.clone());
    create(&m, "arena").await;
    wait("ready", 20, || m.state("arena") == State::Ready).await;
    m.shutdown().await;
    // A PID that is not a server (this test) is not taken over.
    let yml = m.config.dir.join("servers.yml");
    let text = std::fs::read_to_string(&yml).unwrap();
    std::fs::write(
        &yml,
        text.replace(
            "autostart: true",
            &format!("autostart: false\n    pid: {}", std::process::id()),
        ),
    )
    .unwrap();
    let m = manager(cfg);
    m.boot();
    assert_eq!(m.state("arena"), State::Stopped);
    assert_eq!(m.entry("arena").unwrap().pid, None);
}

// ------------------------------------------------------------------ downloads

/// GitHub's answers for three releases: a good one, one with a wrong
/// checksum, a development build without checksums.
async fn fake_github(file: &'static [u8], slow: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let asset = pumbo_servers::versions::pumpkin_asset(
        std::env::consts::ARCH,
        std::env::consts::OS,
        cfg!(target_env = "musl"),
    )
    .unwrap();
    let good = pumbo_servers::versions::hex(
        aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, file).as_ref(),
    );
    let b = base.clone();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let base = b.clone();
            let good = good.clone();
            let asked = asked.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                assert!(
                    head.contains("user-agent: PumboProx/")
                        || head.contains("User-Agent: PumboProx/"),
                    "{head}"
                );
                let path = head.split(' ').nth(1).unwrap_or("").to_string();
                let rel = |tag: &str, pre: bool, sums: bool| {
                    let mut assets = vec![format!(
                        r#"{{"name":"{asset}","browser_download_url":"{base}/dl/{tag}/{asset}"}}"#
                    )];
                    if sums {
                        assets.push(format!(
                            r#"{{"name":"checksums.sha256","browser_download_url":"{base}/dl/{tag}/checksums.sha256"}}"#
                        ));
                    }
                    format!(
                        r#"{{"tag_name":"{tag}","prerelease":{pre},"draft":false,"assets":[{}]}}"#,
                        assets.join(",")
                    )
                };
                let body: Vec<u8> = if path.starts_with("/releases") {
                    format!(
                        "[{},{},{},{}]",
                        rel("canary", true, false),
                        rel(TAG, false, true),
                        rel("0.1.0-dev+26.2-26.45", false, true),
                        rel(FLAKY, false, true)
                    )
                    .into_bytes()
                } else if path.ends_with("checksums.sha256") {
                    let sum = if !path.contains("0.1.0") {
                        good.clone()
                    } else {
                        "0".repeat(64)
                    };
                    format!("{sum}  {asset}\n{}  other\n", "1".repeat(64)).into_bytes()
                } else if path.ends_with(asset)
                    && path.contains("0.3.0")
                    && asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
                {
                    // The first try of FLAKY: the connection drops half way.
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        file.len()
                    );
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(&file[..file.len() / 2]).await;
                    return;
                } else if path.ends_with(asset) {
                    file.to_vec()
                } else {
                    Vec::new()
                };
                let status = if body.is_empty() {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let _ = s
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                if slow && path.ends_with(asset) {
                    // About 2 KiB a second: long enough to cancel.
                    for chunk in body.chunks(100) {
                        if s.write_all(chunk).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    return;
                }
                let _ = s.write_all(&body).await;
            });
        }
    });
    base
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_are_checked() {
    const FILE: &[u8] = b"#!/bin/sh\necho pumpkin\n";
    let base = fake_github(FILE, false).await;
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "versions-{}-{}",
        std::process::id(),
        nanos()
    ));
    let store = |allow: bool| {
        let sources: Vec<Arc<dyn Source>> = vec![Arc::new(Pumpkin::with_api(&base))];
        pumbo_servers::VersionStore::new(dir.clone(), sources, allow).unwrap()
    };
    let s = store(false);
    let list = s.releases("pumpkin").await.unwrap();
    assert_eq!(list.len(), 4);
    assert!(!list[0].official && list[1].official);
    assert!(s.cached("pumpkin").is_some());

    let seen = std::sync::Mutex::new(Vec::new());
    let on = |p: Progress| seen.lock().unwrap().push(p);
    assert_eq!(s.download("pumpkin", "#2", &on).await.unwrap(), TAG);
    let seen = seen.into_inner().unwrap();
    let len = Some(FILE.len() as u64);
    assert_eq!(seen.first(), Some(&Progress::Started { tag: TAG.into() }));
    assert_eq!(
        seen.get(1),
        Some(&Progress::Bytes {
            done: 0,
            total: len
        })
    );
    assert_eq!(
        seen.last(),
        Some(&Progress::Bytes {
            done: FILE.len() as u64,
            total: len
        })
    );
    let file = s.file("pumpkin", TAG).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), FILE);
    assert!(executable(&file));

    let bad = s
        .download("pumpkin", "0.1.0-dev+26.2-26.45", &|_| {})
        .await
        .unwrap_err();
    assert!(bad.contains("SHA256 mismatch"), "{bad}");
    assert!(s.file("pumpkin", "0.1.0-dev+26.2-26.45").is_none());
    assert!(
        !dir.join("pumpkin/0.1.0-dev+26.2-26.45").exists(),
        "no partial file"
    );

    let refused = s.download("pumpkin", "canary", &|_| {}).await.unwrap_err();
    assert!(refused.contains("no SHA256"), "{refused}");
    assert!(s.download("pumpkin", "#9", &|_| {}).await.is_err());
    assert!(s.download("paper", "#1", &|_| {}).await.is_err());

    // A dropped connection: a second attempt, reported.
    let seen = std::sync::Mutex::new(Vec::new());
    let on = |p: Progress| seen.lock().unwrap().push(p);
    s.download("pumpkin", FLAKY, &on).await.unwrap();
    assert!(
        seen.lock()
            .unwrap()
            .contains(&Progress::Retry { attempt: 2, of: 3 })
    );
    assert_eq!(
        std::fs::read(s.file("pumpkin", FLAKY).unwrap()).unwrap(),
        FILE
    );

    let s = store(true);
    s.download("pumpkin", "#1", &|_| {}).await.unwrap();
    assert_eq!(
        s.installed("pumpkin"),
        vec![FLAKY.to_string(), TAG.to_string(), "canary".into()]
    );
    assert_eq!(s.latest_installed("pumpkin").as_deref(), Some(FLAKY));
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o111 != 0
}

/// The whole cycle on a real Pumpkin: `PUMBO_PUMPKIN_BIN=/path/to/pumpkin
/// cargo test -p pumbo-servers --test manager -- --ignored real_pumpkin`
/// (`PUMBO_PUMPKIN_TAG` names its release, default `TAG`).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PUMBO_PUMPKIN_BIN"]
async fn real_pumpkin() {
    let bin = PathBuf::from(std::env::var("PUMBO_PUMPKIN_BIN").expect("PUMBO_PUMPKIN_BIN"));
    let tag = std::env::var("PUMBO_PUMPKIN_TAG").unwrap_or_else(|_| TAG.into());
    let mut cfg = config("real", 25795);
    cfg.ports = "25795-25799".into();
    cfg.stop_timeout_secs = 30;
    cfg.templates.get_mut("default").unwrap().version = tag.clone();
    let version = cfg.versions_dir.join("pumpkin").join(&tag);
    std::fs::create_dir_all(&version).unwrap();
    let file = version.join("pumpkin");
    let _ = std::fs::remove_file(&file);
    std::os::unix::fs::symlink(std::path::absolute(&bin).unwrap(), &file).unwrap();
    let m = manager(cfg);
    create(&m, "real").await;
    m.start("real").unwrap();
    wait("ready", 180, || m.state("real") == State::Ready).await;
    let first = pid_of(&m, "real").unwrap();
    kill(first);
    wait("restarted", 180, || {
        m.state("real") == State::Ready && pid_of(&m, "real").is_some_and(|p| p != first)
    })
    .await;
    let second = pid_of(&m, "real").unwrap();
    let start = Instant::now();
    m.stop("real").await.unwrap();
    eprintln!("[measure] stop took {:?}", start.elapsed());
    assert!(!alive(second));
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "stopped by stop, not by a kill"
    );
    eprintln!("{}", m.logs("real", 15).unwrap().join("\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_can_be_cancelled() {
    static FILE: [u8; 20_000] = [7; 20_000];
    let base = fake_github(&FILE, true).await;
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "versions-cancel-{}-{}",
        std::process::id(),
        nanos()
    ));
    let sources: Vec<Arc<dyn Source>> = vec![Arc::new(Pumpkin::with_api(&base))];
    let s = Arc::new(pumbo_servers::VersionStore::new(dir.clone(), sources, false).unwrap());
    let run = |tag: &'static str| {
        let s = s.clone();
        tokio::spawn(async move { s.download("pumpkin", tag, &|_| {}).await })
    };
    let (a, b) = (run(TAG), run(FLAKY));
    wait("both running", 10, || s.downloads().len() == 2).await;
    assert!(
        s.download("pumpkin", TAG, &|_| {})
            .await
            .unwrap_err()
            .contains("is being downloaded")
    );
    // Some bytes are on disk before the cancel.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(dir.join("pumpkin").join(TAG).join("pumpkin.part").exists());
    assert!(s.cancel("pumpkin", TAG));
    assert!(s.cancel("pumpkin", FLAKY));
    assert!(!s.cancel("pumpkin", "canary"));
    for t in [a, b] {
        assert_eq!(
            t.await.unwrap().unwrap_err(),
            pumbo_servers::versions::CANCELLED
        );
    }
    assert!(s.downloads().is_empty());
    assert!(!dir.join("pumpkin").join(TAG).exists());
    assert!(!dir.join("pumpkin").join(FLAKY).exists());
}
