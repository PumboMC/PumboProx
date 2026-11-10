//! Released proxy plugins on this host (contract tests). Ignored by default:
//! `PUMBO_CONTRACT_PLUGINS` names a directory with the `.wasm` files pinned in
//! `tools/backends.sha256` (downloaded by `.github/workflows/backends.yml`).
//! Every pinned plugin must load and run; one the host refuses fails the test
//! with its id and the host's reason. Then a player goes through the login
//! gate to a server, and permission, ban and filter commands take effect.
//!
//! `PUMBO_CONTRACT_PLUGINS=dir cargo test -p pumbo-prox --test contracts -- --ignored`
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{module, start_proxy};
use pumbo_prox::plugins::Plugins;
use pumbo_testclient::{JoinOptions, Player};

const WAIT: Duration = Duration::from_secs(30);

/// The pinned plugin files: (name, sha256).
fn pinned() -> Vec<(String, String)> {
    let pins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/backends.sha256");
    std::fs::read_to_string(pins)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let (sum, path) = l.split_once("  ")?;
            let name = path.strip_prefix("plugins/")?;
            Some((name.to_string(), sum.to_string()))
        })
        .collect()
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PUMBO_CONTRACT_PLUGINS"]
async fn released_plugins_load_and_work() {
    let Some(from) = std::env::var_os("PUMBO_CONTRACT_PLUGINS").map(PathBuf::from) else {
        panic!("set PUMBO_CONTRACT_PLUGINS to the directory of the released plugins");
    };
    let pins = pinned();
    assert!(!pins.is_empty(), "no plugins in tools/backends.sha256");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-contracts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, sum) in &pins {
        let file = from.join(name);
        assert_eq!(&sha256(&file), sum, "{name} is not the pinned release");
        std::fs::copy(&file, dir.join(name)).unwrap();
    }
    let (backend, log) = common::backend().await;
    let text = format!(
        "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
         limits:\n  connections-per-ip-per-second: 100\n  concurrent-per-ip: 100\n\
         servers:\n  lobby: {{ address: \"{backend}\", protocol: 777 }}\nrouting:\n  try: [lobby]\n\
         plugins:\n  dir: {:?}\n",
        dir.display().to_string()
    );
    let (proxy, addr) = start_proxy(&text).await;
    let plugins = Plugins::start(&text, &proxy, Vec::new())
        .await
        .unwrap()
        .expect("plugin host");
    proxy.plugins.set(plugins.clone()).unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let slots = plugins.host.plugins();
        let state: Vec<String> = slots
            .iter()
            .map(|s| format!("{}: {:?}", s.id, s.status()))
            .collect();
        if slots.len() == pins.len()
            && slots
                .iter()
                .all(|s| s.status() == pumbo_host::Status::Running)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the released plugins do not all run: {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // The connection check is a gravity test with a captcha after it; the
    // reloaded config turns it off, so the next gate is the login.
    let filter = dir.join("pumbo-filter/config.yml");
    let cfg = std::fs::read_to_string(&filter).unwrap();
    assert!(cfg.contains("check-mode: GRAVITY_THEN_CAPTCHA"), "{cfg}");
    std::fs::write(
        &filter,
        cfg.replacen("check-mode: GRAVITY_THEN_CAPTCHA", "check-mode: NEVER", 1),
    )
    .unwrap();
    plugins.host.reload_plugin("pumbo-filter").await.unwrap();
    let slot = plugins.host.plugin("pumbo-filter").unwrap();
    assert_eq!(
        slot.status(),
        pumbo_host::Status::Running,
        "pumbo-filter after its reload"
    );

    // The login gate holds the player until the account exists.
    let mut p = Player::join(addr, module(777), JoinOptions::new("Steve"))
        .await
        .unwrap();
    let said = |p: &Player, text: &str| p.messages.iter().any(|m| m.contains(text));
    assert!(
        p.pump(WAIT, |p| said(p, "/register")).await.unwrap(),
        "no register prompt: {:?} {:?}",
        p.messages,
        p.disconnect
    );
    p.command("register Contract-Pass-7 Contract-Pass-7")
        .await
        .unwrap();
    assert!(
        p.pump(WAIT, |p| p.logins.len() == 2).await.unwrap(),
        "not on the server after /register: {:?} {:?}",
        p.messages,
        p.disconnect
    );
    assert!(said(&p, "logged in"), "{:?}", p.messages);
    assert_eq!(log.joins.load(std::sync::atomic::Ordering::SeqCst), 1);

    // A permission from the provider plugin.
    let steve = pumbo_identity::offline_uuid("Steve");
    plugins.console("pp user Steve permission set contract.node true");
    let deadline = tokio::time::Instant::now() + WAIT;
    while plugins.permission(steve, "contract.node", None) != Some(true) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "pumbo-perms: contract.node is {:?}",
            plugins.permission(steve, "contract.node", None)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A ban kicks the player and refuses the next login.
    plugins.console("ban Steve contract test");
    assert!(p.pump(WAIT, |p| p.disconnect.is_some()).await.unwrap());
    let reason = p.disconnect.clone().unwrap();
    assert!(reason.contains("banned"), "{reason}");
    let again = Player::join(addr, module(777), JoinOptions::new("Steve")).await;
    let refused = match again {
        Err(e) => e.to_string(),
        Ok(mut p) => {
            p.pump(WAIT, |p| p.disconnect.is_some()).await.unwrap();
            p.disconnect.clone().unwrap_or_default()
        }
    };
    assert!(refused.contains("banned"), "{refused}");
    proxy.stop();
}
