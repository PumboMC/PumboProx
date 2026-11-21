//! New servers and PumboBridge: the manager puts the bridge plugin from a
//! local stand-in for its GitHub releases into a new server, or says why not.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::sync::{Arc, Mutex, OnceLock};

use common::config;
use pumbo_engines::pumpkin::Pumpkin;
use pumbo_servers::{Manager, Network, ProcessRunner, Source};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The log of this thread while the guard lives.
fn capture_logs() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<u8>>>) {
    // With one live subscriber, tracing asks only the current thread whether
    // a log line met for the first time is wanted, so a line another test
    // reaches first would stay off here. A second one that lives as long as
    // the tests makes it ask every subscriber.
    static SECOND: OnceLock<tracing::Dispatch> = OnceLock::new();
    SECOND.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let log = Arc::new(Mutex::new(Vec::new()));
    let w = log.clone();
    let sub = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || Buf(w.clone()))
        .finish();
    (tracing::subscriber::set_default(sub), log)
}

/// GitHub's answer for the latest PumboBridge release: a file for each
/// Minecraft release in `mc` and `SHA256SUMS`, whose sum for the first is `sum`.
async fn fake_bridge_releases(file: &'static [u8], sum: String, mc: [&'static str; 2]) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let b = base.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let (base, sum) = (b.clone(), sum.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split(' ').nth(1).unwrap_or("").to_string();
                let names = [
                    format!("PumboBridge-Pumpkin-{}-0.1.2-beta.wasm", mc[0]),
                    format!("PumboBridge-Pumpkin-{}-0.1.2-beta.wasm", mc[1]),
                    "SHA256SUMS".into(),
                ];
                let body: Vec<u8> = if path == "/releases/latest" {
                    let assets: Vec<String> = names
                        .iter()
                        .map(|n| {
                            format!(r#"{{"name":"{n}","browser_download_url":"{base}/dl/{n}"}}"#)
                        })
                        .collect();
                    format!(
                        r#"{{"tag_name":"v0.1.2-beta","assets":[{}]}}"#,
                        assets.join(",")
                    )
                    .into_bytes()
                } else if path.ends_with("SHA256SUMS") {
                    format!("{sum}  {}\n{}  {}\n", names[0], "1".repeat(64), names[1]).into_bytes()
                } else if path.ends_with(&names[0]) {
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
                let _ = s.write_all(&body).await;
            });
        }
    });
    base
}

/// A manager in a fresh folder named after `tag` that takes PumboBridge
/// from the releases at `base`.
fn with(tag: &str, base: String) -> Arc<Manager> {
    let sources: Vec<Arc<dyn Source>> = vec![Arc::new(Pumpkin::default())];
    let network = Network {
        forwarding_secret: None,
        bridge: Some(("127.0.0.1:25578".into(), "ab".repeat(32))),
        bridge_releases: Some(base),
    };
    Manager::new(
        config(tag),
        network,
        sources,
        Arc::new(ProcessRunner::default()),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn new_server_gets_bridge_plugin_and_config() {
    const FILE: &[u8] = b"\0asm bridge";
    let good = pumbo_servers::versions::hex(
        aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, FILE).as_ref(),
    );
    let m = with(
        "bridge",
        fake_bridge_releases(FILE, good, ["26.3", "26.2"]).await,
    );
    m.create("arena", None, None, &[]).await.unwrap();
    let dir = m.config.dir.join("arena");
    assert_eq!(
        std::fs::read(dir.join("plugins/PumboBridge-26.3.wasm")).unwrap(),
        FILE
    );
    let cfg = std::fs::read_to_string(dir.join("plugins/data/pumbobridge/config.yml")).unwrap();
    assert!(
        cfg.contains("127.0.0.1:25578") && cfg.contains(&"ab".repeat(32)),
        "{cfg}"
    );

    // A file that does not match SHA256SUMS never reaches a server.
    let m = with(
        "bridge",
        fake_bridge_releases(FILE, "0".repeat(64), ["26.3", "26.2"]).await,
    );
    m.create("arena", None, None, &[]).await.unwrap();
    let plugins = m.config.dir.join("arena/plugins");
    assert!(!plugins.join("PumboBridge-26.3.wasm").exists());
    assert!(plugins.join("data/pumbobridge/config.yml").is_file());
}

/// A release without a file for the server's Minecraft release never gives
/// it another one's, and the log says why.
#[tokio::test(flavor = "multi_thread")]
async fn a_bridge_release_without_this_minecraft_is_not_installed() {
    const FILE: &[u8] = b"\0asm bridge";
    let good = pumbo_servers::versions::hex(
        aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, FILE).as_ref(),
    );
    let (_logs, log) = capture_logs();
    let m = with(
        "other-mc",
        fake_bridge_releases(FILE, good, ["26.2", "26.1"]).await,
    );
    m.create("arena", None, None, &[]).await.unwrap();
    let plugins = m.config.dir.join("arena/plugins");
    let bridge = std::fs::read_dir(&plugins)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("PumboBridge"));
    assert!(!bridge);
    assert!(plugins.join("data/pumbobridge/config.yml").is_file());
    let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
    assert!(log.contains("has no file for Minecraft 26.3"), "{log}");
}
