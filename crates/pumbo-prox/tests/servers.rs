//! Servers from the proxy with a stand-in for Pumpkin (`tests/fake-pumpkin`,
//! forwarding to a scripted backend): `prox server new` puts the server in
//! the server list, a player switches to it with `/server`, a killed server
//! comes back, `delete` takes it off the list, and the servers stop with the
//! proxy.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common::{backend, module};
use pumbo_prox::commands::{self, Source};
use pumbo_prox::server::Proxy;
use pumbo_servers::State;
use pumbo_testclient::{JoinOptions, Player};

const TAG: &str = "0.2.0+26.3-26.51";
const WAIT: Duration = Duration::from_secs(15);

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

/// A proxy with `lobby` (a scripted backend) and servers from the proxy on
/// ports `first..first+9`; the stand-in is Pumpkin `TAG`.
async fn proxy(
    tag: &str,
    first: u16,
    operator: &str,
) -> (Arc<Proxy>, std::net::SocketAddr, PathBuf, String) {
    proxy_with(tag, first, operator, None).await
}

/// [`proxy`] with Pumpkin releases from `releases` and downloads from the game.
async fn proxy_with(
    tag: &str,
    first: u16,
    operator: &str,
    releases: Option<&str>,
) -> (Arc<Proxy>, std::net::SocketAddr, PathBuf, String) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("prox-servers-{tag}-{}", std::process::id()));
    let version = dir.join("versions/pumpkin").join(TAG);
    std::fs::create_dir_all(&version).unwrap();
    std::fs::copy(fake_bin(), version.join("pumpkin")).unwrap();
    let (lobby, _) = backend().await;
    let op = pumbo_identity::offline_uuid(operator);
    let config = format!(
        "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
         servers:\n  lobby: {{ address: \"{lobby}\" }}\nrouting:\n  try: [lobby]\n\
         commands:\n  operators: [\"{op}\"]\n\
         limits:\n  connections-per-ip-per-second: 1000\n  concurrent-per-ip: 1000\n\
         managed-servers:\n  enabled: true\n  dir: \"{d}/servers\"\n  versions-dir: \"{d}/versions\"\n  \
         ports: {first}-{}\n  stop-timeout-secs: 5\n  download-from-game: {}\n  \
         templates:\n    default: {{ autostart: false, restart-on-crash: 2 }}\n",
        first + 9,
        releases.is_some(),
        d = common::toml_path(&dir),
    );
    // On disk: `/prox route` writes it and reloads.
    let file = dir.join("pumboprox.yml");
    std::fs::write(&file, format!("# Test network\n{config}")).unwrap();
    let (proxy, addr) = common::start_proxy_at(&config, Some(file)).await;
    match releases {
        Some(api) => {
            let sources: Vec<Arc<dyn pumbo_servers::Source>> =
                vec![Arc::new(pumbo_servers::Pumpkin::with_api(api))];
            pumbo_prox::servers::start_with(&proxy, sources).unwrap();
        }
        None => pumbo_prox::servers::start(&proxy).unwrap(),
    }
    (proxy, addr, dir.join("servers"), lobby.to_string())
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

fn console(proxy: &Arc<Proxy>, line: &str) -> Vec<String> {
    let rt = proxy.runtime();
    commands::run(proxy, &rt, &Source::Console, line)
        .unwrap()
        .lines
        .iter()
        .map(|l| l.plain_text())
        .collect()
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

fn pid(proxy: &Proxy, name: &str) -> Option<u32> {
    proxy
        .servers
        .get()?
        .list()
        .into_iter()
        .find(|s| s.name == name)?
        .pid
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A new server with the stand-in forwarding to `upstream`, telemetry off.
async fn new_server(proxy: &Arc<Proxy>, dir: &std::path::Path, name: &str, upstream: &str) {
    console(proxy, &format!("prox servers new {name} {TAG}"));
    wait("created", 10, || {
        proxy.runtime().backends.iter().any(|b| b.name == name)
    })
    .await;
    std::fs::write(dir.join(name).join("fake-upstream"), upstream).unwrap();
    let toml = dir.join(name).join("pumpkin.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(toml, format!("{text}\n[telemetry]\nenabled = false\n")).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_server_joins_the_network() {
    let (proxy, addr, dir, lobby) = proxy("join", 25750, "Admin").await;
    let m = proxy.servers.get().unwrap().clone();
    let _cleanup = Cleanup(m.clone());
    // Online before the server exists: its session must see it at once.
    let mut p = Player::join(addr, module(777), JoinOptions::new("Admin"))
        .await
        .unwrap();
    assert!(p.pump(WAIT, |p| !p.logins.is_empty()).await.unwrap());
    assert!(suggest(&mut p, 1, "/server ar").await.is_empty());
    new_server(&proxy, &dir, "arena", &lobby).await;
    let rt = proxy.runtime();
    let b = rt.backends.iter().find(|b| b.name == "arena").unwrap();
    let port = m.entry("arena").unwrap().port;
    assert_eq!(b.address, format!("127.0.0.1:{port}"));
    assert_eq!(b.protocol.map(|p| p.0), Some(777), "26.3");
    assert!(rt.config.servers.contains_key("lobby"));

    let started = console(&proxy, "prox servers start arena");
    assert!(started[0].contains("Starting arena"), "{started:?}");
    wait("ready", 20, || m.state("arena") == State::Ready).await;
    let list = console(&proxy, "prox servers").join("\n");
    assert!(
        list.contains("arena · ready · pumpkin 0.2.0+26.3-26.51"),
        "{list}"
    );

    // The player goes there with /server.
    assert_eq!(suggest(&mut p, 2, "/server ar").await, ["arena"]);
    p.command("server arena").await.unwrap();
    assert!(
        p.pump(WAIT, |p| p.logins.len() == 2).await.unwrap(),
        "{:?}",
        p.disconnect
    );
    let id = pumbo_identity::offline_uuid("Admin");
    wait("on arena", 5, || {
        proxy.server_of(id).as_deref() == Some("arena")
    })
    .await;
    p.command("prox servers logs arena 5").await.unwrap();
    assert!(common::message(&mut p, "last").await);
    p.command("server lobby").await.unwrap();
    assert!(p.pump(WAIT, |p| p.logins.len() == 3).await.unwrap());

    // A killed server comes back with a new process.
    let first = pid(&proxy, "arena").unwrap();
    let _ = Command::new("kill")
        .args(["-KILL", &first.to_string()])
        .status();
    wait("restarted", 20, || {
        m.state("arena") == State::Ready && pid(&proxy, "arena").is_some_and(|p| p != first)
    })
    .await;

    // Delete: off the list, the folder in the trash.
    let second = pid(&proxy, "arena").unwrap();
    console(&proxy, "prox servers delete arena");
    assert!(m.entry("arena").is_some(), "not without confirm");
    console(&proxy, "prox servers delete arena confirm");
    wait("deleted", 15, || {
        !proxy.runtime().backends.iter().any(|b| b.name == "arena")
    })
    .await;
    assert!(!alive(second));
    assert!(dir.join(".trash").read_dir().unwrap().count() == 1);
    assert!(suggest(&mut p, 3, "/server ar").await.is_empty());
    p.command("server arena").await.unwrap();
    assert!(common::message(&mut p, "There is no server named arena").await);
    p.close().await;
}

/// Tab completion of `text` as the client asks for it.
async fn suggest(p: &mut Player, id: i32, text: &str) -> Vec<String> {
    let before = p.suggestions.len();
    p.c.send(&pumbo_protocol::packets::play::CommandSuggestion {
        id,
        text: text.into(),
    })
    .await
    .unwrap();
    assert!(
        p.pump(WAIT, |p| p.suggestions.len() > before)
            .await
            .unwrap()
    );
    let last = p.suggestions.last().unwrap();
    assert_eq!(last.id, id);
    last.matches.iter().map(|m| m.text.clone()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn servers_stop_with_the_proxy_and_players_need_permission() {
    let (proxy, addr, dir, lobby) = proxy("stop", 25760, "Admin").await;
    let m = proxy.servers.get().unwrap().clone();
    let _cleanup = Cleanup(m.clone());
    new_server(&proxy, &dir, "hub", &lobby).await;
    new_server(&proxy, &dir, "games", &lobby).await;
    console(&proxy, "prox servers start hub");
    console(&proxy, "prox servers start games");
    wait("ready", 20, || {
        m.state("hub") == State::Ready && m.state("games") == State::Ready
    })
    .await;
    let pids = [pid(&proxy, "hub").unwrap(), pid(&proxy, "games").unwrap()];

    // Not an operator: no server commands, no download from the game.
    let mut p = Player::join(addr, module(777), JoinOptions::new("Guest"))
        .await
        .unwrap();
    assert!(p.pump(WAIT, |p| !p.logins.is_empty()).await.unwrap());
    p.command("prox servers stop hub").await.unwrap();
    assert!(common::message(&mut p, "permission").await);
    assert_eq!(m.state("hub"), State::Ready);
    p.close().await;

    let start = Instant::now();
    m.shutdown().await;
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    for pid in pids {
        assert!(!alive(pid), "{pid}");
    }
    assert_eq!(m.state("hub"), State::Stopped);
    assert!(m.start("hub").is_err(), "no starts while stopping");
    let yml = std::fs::read_to_string(dir.join("servers.yml")).unwrap();
    assert!(!yml.contains("pid:"), "{yml}");
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_from_the_game_are_off_by_default() {
    let (_proxy, addr, _, _) = proxy("dl", 25770, "Admin").await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Admin"))
        .await
        .unwrap();
    assert!(p.pump(WAIT, |p| !p.logins.is_empty()).await.unwrap());
    p.command("prox download pumpkin #1").await.unwrap();
    assert!(common::message(&mut p, "Downloads from the game are off").await);
    p.command("prox servers new bad_name").await.unwrap();
    assert!(common::message(&mut p, "invalid server name").await);
    p.close().await;
}

// ------------------------------------------------------------------ downloads

const SLOW_A: &str = "0.4.0+26.3-26.70";
const SLOW_B: &str = "0.5.0+26.3-26.80";

/// Releases `SLOW_A` and `SLOW_B` with checksums; the file comes in 2 KiB
/// pieces every `ms` milliseconds.
async fn fake_github(file: &'static [u8], ms: u64) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let asset = pumbo_servers::versions::pumpkin_asset(
        std::env::consts::ARCH,
        std::env::consts::OS,
        cfg!(target_env = "musl"),
    )
    .unwrap();
    let sum = pumbo_servers::versions::hex(
        aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, file).as_ref(),
    );
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
                let rel = |tag: &str| {
                    format!(
                        r#"{{"tag_name":"{tag}","prerelease":false,"assets":[{{"name":"{asset}","browser_download_url":"{base}/dl/{tag}/{asset}"}},{{"name":"checksums.sha256","browser_download_url":"{base}/dl/{tag}/checksums.sha256"}}]}}"#
                    )
                };
                let body: Vec<u8> = if path.starts_with("/releases") {
                    format!("[{},{}]", rel(SLOW_A), rel(SLOW_B)).into_bytes()
                } else if path.ends_with("checksums.sha256") {
                    format!("{sum}  {asset}\n").into_bytes()
                } else {
                    file.to_vec()
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                if path.ends_with(asset) {
                    for piece in body.chunks(2048) {
                        if s.write_all(piece).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }
                } else {
                    let _ = s.write_all(&body).await;
                }
            });
        }
    });
    base
}

/// No colour codes or style tags left in what people read.
fn clean(text: &str) {
    for bad in [
        "&a", "&c", "&e", "&7", "&f", "<ok>", "<err>", "<v>", "<s>", "<c>", "<warn>",
    ] {
        assert!(!text.contains(bad), "{bad} in {text:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_shows_its_progress_on_a_boss_bar() {
    static FILE: [u8; 60_000] = [3; 60_000];
    let api = fake_github(&FILE, 40).await;
    let (proxy, addr, dir, _) = proxy_with("bar", 25780, "Admin", Some(&api)).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Admin"))
        .await
        .unwrap();
    assert!(p.pump(WAIT, |p| !p.logins.is_empty()).await.unwrap());
    p.command(&format!("prox download pumpkin {SLOW_A}"))
        .await
        .unwrap();
    let ready = format!("Pumpkin {SLOW_A} ready · SHA256 OK");
    assert!(
        p.pump(WAIT, |p| p.bossbars.iter().any(|b| b == &ready))
            .await
            .unwrap(),
        "{:?}",
        p.bossbars
    );
    let first = &p.bossbars[0];
    assert!(
        first.starts_with(&format!("Downloading Pumpkin {SLOW_A} · ")) && first.contains(" MB"),
        "{first}"
    );
    assert!(common::message(&mut p, "Downloaded pumpkin").await);
    let file = dir.join("../versions/pumpkin").join(SLOW_A).join("pumpkin");
    assert_eq!(std::fs::read(file).unwrap().len(), FILE.len());
    for m in &p.messages {
        clean(m);
    }
    p.close().await;
    drop(proxy);
}

#[tokio::test(flavor = "multi_thread")]
async fn running_downloads_can_be_stopped() {
    static FILE: [u8; 400_000] = [5; 400_000];
    let api = fake_github(&FILE, 50).await;
    let (proxy, _, dir, _) = proxy_with("stopdl", 25790, "Admin", Some(&api)).await;
    let m = proxy.servers.get().unwrap().clone();
    let _cleanup = Cleanup(m.clone());
    let mut said = console(&proxy, "prox download stop");
    assert!(said[0].contains("No downloads are running"), "{said:?}");
    said.extend(console(&proxy, &format!("prox download pumpkin {SLOW_A}")));
    said.extend(console(&proxy, &format!("prox download pumpkin {SLOW_B}")));
    wait("two downloads", 10, || m.versions.downloads().len() == 2).await;
    let shown = console(&proxy, "prox download");
    assert!(
        shown
            .iter()
            .any(|l| l.contains(&format!("Downloading now: pumpkin {SLOW_A}"))),
        "{shown:?}"
    );
    let one = console(&proxy, &format!("prox download stop {SLOW_B}"));
    assert_eq!(one.len(), 1, "{one:?}");
    assert!(one[0].contains(&format!("Stopped the download of pumpkin {SLOW_B}")));
    wait("one left", 10, || m.versions.downloads().len() == 1).await;
    // Without a name: every running download.
    console(&proxy, &format!("prox download pumpkin {SLOW_B}"));
    wait("two again", 10, || m.versions.downloads().len() == 2).await;
    let all = console(&proxy, "prox download stop");
    assert_eq!(all.len(), 2, "{all:?}");
    wait("none left", 10, || m.versions.downloads().is_empty()).await;
    let versions = dir.join("../versions/pumpkin");
    assert!(!versions.join(SLOW_A).exists() && !versions.join(SLOW_B).exists());
    // Every console text of the feature is plain.
    for line in [
        "prox download",
        "prox download nope",
        "prox servers",
        "prox servers",
        "prox servers new",
        "prox servers new BAD_name",
        "prox servers start ghost",
        "prox servers logs",
        "prox servers delete x",
    ] {
        said.extend(console(&proxy, line));
    }
    said.extend(shown);
    said.extend(one);
    said.extend(all);
    for l in &said {
        clean(l);
        assert!(
            [
                "PumboProx » ",
                "Downloading now",
                "paper: coming soon",
                "Commands:"
            ]
            .iter()
            .any(|p| l.starts_with(p)),
            "{l}"
        );
    }
}

// ------------------------------------------------------------------ route

#[tokio::test(flavor = "multi_thread")]
async fn the_route_is_shown_and_changed_in_the_config_file() {
    let (proxy, addr, dir, lobby) = proxy("route", 25790, "Admin").await;
    let file = dir.join("../pumboprox.yml");
    let m = proxy.servers.get().unwrap().clone();
    let _cleanup = Cleanup(m.clone());
    new_server(&proxy, &dir, "arena", &lobby).await;
    console(&proxy, "prox servers start arena");
    wait("ready", 20, || m.state("arena") == State::Ready).await;

    let shown = console(&proxy, "prox route").join("\n");
    for part in ["1 Domain", "Gates", "3 Servers", "1. lobby", "4 Fallback"] {
        assert!(shown.contains(part), "{part}: {shown}");
    }
    // A server run by the proxy goes first in routing.try.
    let said = console(&proxy, "prox route servers add arena 1");
    assert!(
        said[0].contains("Players now try arena as server 1"),
        "{said:?}"
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# Test network\n"), "{text}");
    assert!(text.contains("  try: [arena, lobby]\n"), "{text}");
    assert_eq!(proxy.runtime().config.try_order(), ["arena", "lobby"]);
    // A domain of its own, also for a server of the proxy.
    console(&proxy, "prox route servers move arena 2");
    assert_eq!(proxy.runtime().config.try_order(), ["lobby", "arena"]);
    console(&proxy, "prox route host set Play.Example.org arena");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("  play.example.org: [arena]\n")
    );
    let mut opts = JoinOptions::new("Admin");
    opts.host = "play.example.org".into();
    let mut p = Player::join(addr, module(777), opts).await.unwrap();
    assert!(p.pump(WAIT, |p| !p.logins.is_empty()).await.unwrap());
    let id = pumbo_identity::offline_uuid("Admin");
    wait("on arena", 5, || {
        proxy.server_of(id).as_deref() == Some("arena")
    })
    .await;
    p.close().await;
    console(&proxy, "prox route host remove play.example.org");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("forced-hosts: {}\n")
    );
    assert!(proxy.runtime().config.forced_hosts.is_empty());

    // Refused changes leave the file as it is.
    let before = std::fs::read_to_string(&file).unwrap();
    for (line, why) in [
        ("prox route servers add ghost", "no server named ghost"),
        ("prox route servers add lobby", "in the list already"),
        ("prox route servers move lobby 9", "position 9"),
        ("prox route host set bad_domain! lobby", "not a domain"),
        ("prox route gates require auth", "plugin host is off"),
    ] {
        let said = console(&proxy, line).join("\n");
        assert!(said.contains(why), "{line}: {said}");
        clean(&said);
    }
    console(&proxy, "prox route servers remove arena");
    let last = console(&proxy, "prox route servers remove lobby").join("\n");
    assert!(last.contains("the last server stays"), "{last}");
    assert_eq!(proxy.runtime().config.try_order(), ["lobby"]);
    assert_ne!(std::fs::read_to_string(&file).unwrap(), before);
    m.shutdown().await;
}

// ------------------------------------------------------------------ arguments

/// Every command with no, too few, wrong and right arguments, and Tab at every
/// position.
#[tokio::test(flavor = "multi_thread")]
async fn every_command_answers_its_arguments() {
    let (proxy, _, dir, lobby) = proxy("args", 25760, "Admin").await;
    let m = proxy.servers.get().unwrap().clone();
    let _cleanup = Cleanup(m.clone());
    new_server(&proxy, &dir, "arena", &lobby).await;
    let rt = proxy.runtime();
    let id = pumbo_identity::offline_uuid("Admin");
    let player = Source::Player {
        id,
        server: Some("lobby"),
    };
    let run = |source: &Source<'_>, line: &str| {
        let r = commands::run(&proxy, &rt, source, line).unwrap();
        let text = r
            .lines
            .iter()
            .map(|l| l.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        (text, r.connect)
    };
    let (_, to) = run(&player, "server lobby");
    assert_eq!(to.as_deref(), Some("lobby"));
    for (line, want) in [
        ("server", "You are on lobby"),
        ("server ghost", "There is no server named ghost"),
        (
            "server stop arena",
            "Did you mean /prox servers stop arena?",
        ),
    ] {
        let (text, _) = run(&player, line);
        assert!(text.contains(want), "{line}: {text}");
    }
    for (line, want) in [
        ("send", "Usage: /send <player|all|current> <server>"),
        ("send Steve", "Usage: /send"),
        ("send Steve nowhere", "There is no server named nowhere"),
        ("send Steve lobby", "Steve is not online"),
        ("send all lobby", "Sending 0 player(s) to lobby"),
        ("find", "Usage: /find <player>"),
        ("find Steve", "Steve is not online"),
        ("alert", "Usage: /alert <message>"),
        ("alert hi", "Alert sent to 0 player(s)"),
        ("prox", "PumboProx"),
        ("prox version", "protocols"),
        ("prox reload", "Config reloaded"),
        ("prox plugins", "plugin host is off"),
        ("prox bridge", "bridge is off"),
        ("prox download", "Server software: pumpkin"),
        ("prox download nope", "Unknown server software nope"),
        ("prox download paper", "coming soon"),
        ("prox download stop", "No downloads are running"),
        ("prox download help", "· Server software"),
        ("prox servers", "Servers run by the proxy (1)"),
        ("prox servers bogus", "Unknown command bogus"),
        ("prox servers start", "Usage: /prox servers start <name>"),
        ("prox servers start ghost", "No server named ghost"),
        ("prox servers start lobby", "lobby is not run by the proxy"),
        ("prox servers logs arena", "arena: last"),
        ("prox servers delete arena", "This stops arena"),
        ("prox servers new", "Usage: /prox servers new <name>"),
        ("prox servers new BAD", "Creating bad"),
        ("prox servers help", "· Servers"),
        ("prox route help", "· Way into the network"),
        ("prox route", "3 Servers"),
        (
            "prox route servers add",
            "Usage: /prox route servers add <name>",
        ),
        ("prox route bogus", "Usage: /prox route"),
    ] {
        let (text, _) = run(&Source::Console, line);
        assert!(text.contains(want), "{line}: {text}");
        clean(&text);
    }
    let tab = |line: &str| {
        commands::suggest(&proxy, &rt, &Source::Console, line)
            .map(|(_, m)| m)
            .unwrap_or_default()
    };
    for (line, want) in [
        ("server ", &["arena", "lobby"][..]),
        ("send ", &["all", "current", "lobby"]),
        ("send all ", &["arena", "lobby"]),
        (
            "prox ",
            &[
                "servers", "download", "route", "plugins", "bridge", "debug", "reload",
            ],
        ),
        ("prox plugins ", &["reload", "load", "unload"]),
        ("prox bridge ", &["key"]),
        ("prox debug ", &["perms", "services"]),
        ("prox debug perms ", &["list"]),
        ("prox download ", &["pumpkin", "paper", "stop", "help"]),
        ("prox download pumpkin ", &["list"]),
        (
            "prox servers ",
            &["new", "start", "stop", "logs", "delete", "help"],
        ),
        ("prox servers start ", &["arena"]),
        ("prox servers logs arena ", &["20"]),
        ("prox servers delete arena ", &["confirm"]),
        ("prox servers new x ", &["latest", TAG]),
        ("prox servers new x latest ", &["default"]),
        ("prox route ", &["servers", "host", "gates", "help"]),
        ("prox route servers ", &["add", "remove", "move"]),
        ("prox route servers add ", &["arena", "lobby"]),
        ("prox route servers move lobby ", &["1"]),
        ("prox route host ", &["set", "remove"]),
        ("prox route host set play.example.org ", &["arena", "lobby"]),
        ("prox route gates ", &["require", "optional"]),
    ] {
        let got = tab(line);
        for w in want {
            assert!(got.iter().any(|g| g == w), "{line:?}: {w} not in {got:?}");
        }
    }
    // A managed server is not offered where only file servers fit, and back.
    assert!(!tab("prox servers start ").contains(&"lobby".to_string()));
}
