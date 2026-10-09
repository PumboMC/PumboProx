//! The `pumboprox` process ends after SIGTERM even when its console reads
//! from a stream that never closes (a pipe held by another process, a TTY in
//! `screen`).

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
             servers:\n  lobby: {{ address: \"127.0.0.1:1\" }}\nrouting:\n  try: [lobby]\n"
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
