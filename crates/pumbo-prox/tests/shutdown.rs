//! The `pumboprox` process ends after SIGTERM even when its console reads
//! from a stream that never closes (a pipe held by another process, a TTY in
//! `screen`), and also while it is still starting; a second signal ends it
//! at once.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn sigterm_ends_the_process_with_the_console_open() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-sigterm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A port free a moment ago (a fixed one may be taken by a server).
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    std::fs::write(
        dir.join("pumboprox.yml"),
        format!(
            "listener:\n  - bind: \"{addr}\"\nforwarding:\n  mode: none\n\
             servers:\n  lobby: {{ address: \"127.0.0.1:1\" }}\nrouting:\n  try: [lobby]\n\
             bridge:\n  enabled: false\n"
        ),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pumboprox"))
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    // Held open until the end: the console never sees the end of its input.
    let stdin = child.stdin.take();
    let start = Instant::now();
    while std::net::TcpStream::connect(addr).is_err() {
        if let Some(s) = child.try_wait().unwrap() {
            panic!("exited before listening: {s}");
        }
        assert!(start.elapsed() < Duration::from_secs(30), "not listening");
        std::thread::sleep(Duration::from_millis(100));
    }
    let kill = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break Some(s);
        }
        if start.elapsed() > Duration::from_secs(10) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if status.is_none() {
        // Own child only.
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(stdin);
    let status = status.expect("still running 10 s after SIGTERM");
    eprintln!("[measure] exit {:?} after SIGTERM", start.elapsed());
    assert!(status.success(), "{status}");
}

/// The test plugin of pumbo-host, built once for wasm32-wasip2.
fn test_plugin() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = root.join("target/wasm-plugins");
    let ok = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["build", "--locked", "-p", "pumbo-host-test-plugin"])
        .args([
            "--target",
            "wasm32-wasip2",
            "--profile",
            "plugin",
            "--target-dir",
        ])
        .arg(&target)
        .status()
        .unwrap()
        .success();
    assert!(ok, "cargo build of the test plugin");
    target.join("wasm32-wasip2/plugin/pumbo_host_test_plugin.wasm")
}

/// Starts the proxy with a plugin whose `init` works for 6 s, sends SIGTERM
/// while it does (once, or with `again` every 50 ms until the process ends:
/// stopping that plugin takes a moment) and returns the exit status and the log.
fn stop_during_start(tag: &str, again: bool) -> (std::process::ExitStatus, String) {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-sigterm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("plugins/slow")).unwrap();
    std::fs::write(
        dir.join("plugins/slow.yml"),
        "id: slow\nversion: 0.1.0\napi: \"0.1\"\nevents: [chat]\n",
    )
    .unwrap();
    std::fs::copy(test_plugin(), dir.join("plugins/slow.wasm")).unwrap();
    std::fs::write(dir.join("plugins/slow/config.yml"), "busy_init_ms: 6000\n").unwrap();
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    std::fs::write(
        dir.join("pumboprox.yml"),
        format!(
            "listener:\n  - bind: \"{addr}\"\nforwarding:\n  mode: none\n\
             servers:\n  lobby: {{ address: \"127.0.0.1:1\" }}\nrouting:\n  try: [lobby]\n\
             bridge:\n  enabled: false\nplugins:\n  load-budget-ms: 10000\n"
        ),
    )
    .unwrap();
    let log = dir.join("out.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_pumboprox"))
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let pid = child.id().to_string();
    let stdin = child.stdin.take();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        std::net::TcpStream::connect(addr).is_err(),
        "still starting (the plugin's init runs)"
    );
    let kill = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(kill.success());
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break Some(s);
        }
        if start.elapsed() > Duration::from_secs(15) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
        if again {
            // Not reaped yet, so the PID is still this child's.
            let _ = Command::new("kill").args(["-TERM", &pid]).status();
        }
    };
    if status.is_none() {
        // Own child only.
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(stdin);
    let log = std::fs::read_to_string(&log).unwrap_or_default();
    (status.expect("still running 15 s after SIGTERM"), log)
}

/// SIGTERM while a plugin is still loading stops the start, stops the plugin
/// and ends the process as a normal stop does (exit 0).
#[test]
fn sigterm_during_the_start_stops_cleanly() {
    let (status, log) = stop_during_start("start", false);
    assert!(status.success(), "{status}\n{log}");
    assert!(log.contains("stopping (signal during the start)"), "{log}");
    assert!(log.contains("stopped"), "{log}");
}

/// A second signal while stopping ends the process at once (exit 130).
#[test]
fn second_signal_forces_the_stop() {
    let (status, log) = stop_during_start("forced", true);
    assert_eq!(status.code(), Some(130), "{status}\n{log}");
    assert!(
        log.contains("forced stop: plugins and managed servers were not stopped"),
        "{log}"
    );
}
