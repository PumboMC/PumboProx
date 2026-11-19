//! What the tests of new and running servers share: the stand-in for
//! Pumpkin and a fresh folder of servers that has it.
// Each test file uses a part of it.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use pumbo_servers::{Config, Template};

pub const TAG: &str = "0.2.0+26.3-26.51";

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

/// A fresh folder with the stand-in as Pumpkin `TAG` and ten free ports.
pub fn config(tag: &str) -> Config {
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
        ports: ephemeral_ports(),
        stop_timeout_secs: 5,
        templates,
        ..Config::default()
    }
}

/// Ten ports from where the OS hands out ephemeral ones, `"p-p+9"`, so a test
/// never lands on the fixed ports of networks running on the same machine.
/// The manager still checks each one is free before it uses it.
fn ephemeral_ports() -> String {
    loop {
        let p = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if p <= u16::MAX - 9 {
            return format!("{p}-{}", p + 9);
        }
    }
}

pub fn nanos() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
}
