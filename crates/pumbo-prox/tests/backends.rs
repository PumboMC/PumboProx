//! The proxy in front of real servers (plan E3, §7.2). Ignored by default:
//! they start Java servers and Pumpkin binaries and take minutes.
//!
//! - `vanilla_matrix`: the vanilla server of every protocol 767–777 behind the
//!   proxy (`forwarding = none`, 127.0.0.1): status, offline login, the play
//!   script, keep-alives for `PUMBO_KEEPALIVE_SECS` (default 120), the final kick.
//!   Jars from `PUMBO_JARS` (default `~/.cache/pumbo-datagen`, filled by
//!   `pumbo-datagen tables`), servers in `PUMBO_E3_WORK/vanilla`, ports 25600–25603.
//! - `pumpkin_forwarding`: a Pumpkin binary (`PUMBO_PUMPKIN_BIN`, its config
//!   template `PUMBO_PUMPKIN_TEMPLATE`, protocol `PUMBO_PUMPKIN_PROTOCOL`)
//!   with Velocity modern forwarding, in `PUMBO_E3_WORK/pumpkin-<protocol>`,
//!   ports 25604–25605.
//!
//! `cargo test -p pumbo-prox --test backends -- --ignored --nocapture --test-threads=1`
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{
    Pumpkin, TEXTURES, fake_mojang, module, premium_uuid, pumpkin_config, secret_file, start_proxy,
    toml_path, wait_port,
};
use pumbo_core::profile::{ForwardedPlayer, GameProfile};
use pumbo_datagen::record::{PLAYER, Server, prepare_dir};
use pumbo_forwarding::modern::VelocityModern;
use pumbo_protocol::packets::login::{CustomQuery, CustomQueryAnswer, LoginDisconnect, LoginStart};
use pumbo_protocol::packets::status::Intention;
use pumbo_protocol::{Direction, PacketKind, Phase};
use pumbo_testclient::session::FINAL_KICK;
use pumbo_testclient::{Client, Ending, SessionOptions};

fn env_path(key: &str, default: &str) -> PathBuf {
    std::env::var_os(key).map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn work() -> PathBuf {
    env_path("PUMBO_E3_WORK", "target/pumbo-e3")
}

// ---------------------------------------------------------------- vanilla

#[derive(Debug)]
struct MatrixRow {
    protocol: i32,
    release: String,
    status: bool,
    ending: Ending,
    keep_alives: u32,
    frames: usize,
    seconds: u64,
}

fn vanilla_one(protocol: i32, slot: u16, keep_alive: Duration) -> Result<MatrixRow, String> {
    let tables =
        pumbo_data::tables(pumbo_protocol::ProtocolVersion(protocol)).map_err(|e| e.to_string())?;
    let release = tables.releases.last().ok_or("no release")?.name.clone();
    let home = std::env::var("HOME").unwrap_or_default();
    let jar = env_path("PUMBO_JARS", &format!("{home}/.cache/pumbo-datagen"))
        .join(&release)
        .join("server.jar");
    let dir = work().join("vanilla").join(&release);
    let server_port = 25600 + slot * 2;
    let proxy_port = server_port + 1;
    prepare_dir(&dir, server_port, protocol).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let mut server = Server::start(&dir, &jar, "java").map_err(|e| e.to_string())?;
    eprintln!("{release}: vanilla pid {} on {server_port}", server.pid);
    let server_addr = SocketAddr::from(([127, 0, 0, 1], server_port));
    let result = server
        .wait_ready(server_addr, Duration::from_secs(240))
        .map_err(|e| e.to_string())
        .and_then(|()| {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().map_err(|e| e.to_string())?;
            rt.block_on(async {
                let cfg = format!(
                    "listener:\n  - bind: \"127.0.0.1:{proxy_port}\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
                     servers:\n  lobby: {{ address: \"{server_addr}\", protocol: {protocol} }}\nrouting:\n  try: [lobby]\n"
                );
                let (proxy, addr) = start_proxy(&cfg).await;
                let (_, json) = pumbo_testclient::status(addr, module(protocol)).await.map_err(|e| e.to_string())?;
                let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
                let status = v["version"]["protocol"] == protocol;
                let out = pumbo_testclient::run(
                    addr,
                    module(protocol),
                    &SessionOptions { name: PLAYER.into(), idle: keep_alive, ..SessionOptions::default() },
                )
                .await
                .map_err(|e| e.to_string())?;
                for n in &out.notes {
                    eprintln!("{release}: {n}");
                }
                proxy.stop();
                Ok(MatrixRow {
                    protocol,
                    release: release.clone(),
                    status,
                    ending: out.ending,
                    keep_alives: out.keep_alives,
                    frames: out.frames.len(),
                    seconds: started.elapsed().as_secs(),
                })
            })
        });
    server.stop();
    result
}

#[test]
#[ignore = "starts vanilla servers of every protocol"]
fn vanilla_matrix() {
    let keep_alive = Duration::from_secs(
        std::env::var("PUMBO_KEEPALIVE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120),
    );
    let only: Vec<i32> = std::env::var("PUMBO_ONLY")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let protocols: Vec<i32> = pumbo_data::protocols()
        .map(|p| p.0)
        .filter(|p| only.is_empty() || only.contains(p))
        .collect();
    let mut rows = Vec::new();
    for pair in protocols.chunks(2) {
        let handles: Vec<_> = pair
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                (
                    p,
                    std::thread::spawn(move || {
                        vanilla_one(p, u16::try_from(i).unwrap(), keep_alive)
                    }),
                )
            })
            .collect();
        for (p, h) in handles {
            rows.push((p, h.join().unwrap()));
        }
    }
    let mut failed = Vec::new();
    for (p, row) in &rows {
        match row {
            Ok(r) => {
                // Vanilla sends a keep-alive every 15 s.
                let ok = r.status
                    && r.ending == Ending::Disconnected(FINAL_KICK.into())
                    && u64::from(r.keep_alives) >= keep_alive.as_secs() / 15;
                eprintln!(
                    "{} {} ({}): status {}, ending {:?}, keep-alives {}, frames {}, {} s",
                    if ok { "OK  " } else { "FAIL" },
                    r.protocol,
                    r.release,
                    r.status,
                    r.ending,
                    r.keep_alives,
                    r.frames,
                    r.seconds
                );
                if !ok {
                    failed.push(*p);
                }
            }
            Err(e) => {
                eprintln!("FAIL {p}: {e}");
                failed.push(*p);
            }
        }
    }
    assert!(failed.is_empty(), "failed: {failed:?}");
}

// ---------------------------------------------------------------- Pumpkin

/// Answers the forwarding query directly (no proxy) with `answer` built
/// from the query, and returns how the backend reacted.
async fn direct_login(
    addr: SocketAddr,
    protocol: i32,
    name: &str,
    answer: impl Fn(&CustomQuery) -> Option<Vec<u8>>,
) -> Result<String, String> {
    let mut c = Client::connect(addr, module(protocol))
        .await
        .map_err(|e| e.to_string())?;
    c.send(&Intention {
        protocol,
        address: "127.0.0.1".into(),
        port: addr.port(),
        intent: 2,
    })
    .await
    .map_err(|e| e.to_string())?;
    c.phase = Phase::Login;
    c.send(&LoginStart {
        name: name.into(),
        uuid: pumbo_identity::offline_uuid(name),
    })
    .await
    .map_err(|e| e.to_string())?;
    loop {
        let (k, f) = c.recv().await.map_err(|e| e.to_string())?;
        match k {
            Some(PacketKind::CustomQuery) => {
                let q: CustomQuery = c.decode(&f).map_err(|e| e.to_string())?;
                let data = answer(&q);
                c.send(&CustomQueryAnswer {
                    message_id: q.message_id,
                    data,
                })
                .await
                .map_err(|e| e.to_string())?;
            }
            Some(PacketKind::LoginDisconnect) => {
                let d: LoginDisconnect = c.decode(&f).map_err(|e| e.to_string())?;
                return Ok(format!("refused: {}", d.reason_json));
            }
            Some(PacketKind::LoginFinished) => return Ok("accepted".into()),
            Some(PacketKind::LoginCompression) => {
                let p: pumbo_protocol::packets::login::LoginCompression =
                    c.decode(&f).map_err(|e| e.to_string())?;
                c.set_compression(p.threshold);
            }
            _ => {}
        }
    }
}

/// Names of `*.dat` files under `dir/world/players` (or `playerdata`).
fn player_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.join("world")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_string_lossy().contains("player")
                && p.extension().is_some_and(|x| x == "dat")
            {
                out.push(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    out
}

fn player(name: &str, ip: &str) -> ForwardedPlayer {
    ForwardedPlayer {
        profile: GameProfile {
            id: pumbo_identity::offline_uuid(name),
            name: name.into(),
            properties: Vec::new(),
        },
        address: ip.parse().unwrap(),
    }
}

#[test]
#[ignore = "starts a Pumpkin binary"]
fn pumpkin_forwarding() {
    let bin = env_path("PUMBO_PUMPKIN_BIN", "pumpkin");
    let template =
        std::fs::read_to_string(env_path("PUMBO_PUMPKIN_TEMPLATE", "pumpkin.toml.template"))
            .unwrap();
    let protocol: i32 = std::env::var("PUMBO_PUMPKIN_PROTOCOL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(777);
    let secret = "pumbo-e3-pumpkin-secret-0123456789";
    let (pumpkin_port, proxy_port) = (25604u16, 25605u16);
    // A fresh directory per run: the player data files below must come from this run.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let dir = work().join(format!("pumpkin-{protocol}-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("pumpkin.toml"),
        pumpkin_config(&template, pumpkin_port, secret),
    )
    .unwrap();
    let mut pumpkin = Pumpkin::start(&bin, &dir);
    let backend = SocketAddr::from(([127, 0, 0, 1], pumpkin_port));
    wait_port(backend);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut report = Vec::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(async {
            // Direct connections (§3.4): no answer, a wrong secret, a replayed answer.
            let none = direct_login(backend, protocol, "PumboDirect", |_| None).await.unwrap();
            report.push(format!("direct, no forwarding answer: {none}"));
            assert!(none.starts_with("refused"), "{none}");
            let wrong = VelocityModern::new(b"wrong-secret-wrong-secret").unwrap();
            let bad = direct_login(backend, protocol, "PumboDirect", |q| Some(wrong.answer(q.data.first().copied(), &player("PumboDirect", "198.51.100.1")))).await.unwrap();
            report.push(format!("direct, wrong secret: {bad}"));
            assert!(bad.starts_with("refused"), "{bad}");
            let right = VelocityModern::new(secret.as_bytes()).unwrap();
            let captured = right.answer(Some(4), &player("PumboReplay", "198.51.100.9"));
            for i in 1..=2 {
                let r = direct_login(backend, protocol, "PumboReplay", |_| Some(captured.clone())).await.unwrap();
                report.push(format!("direct, replayed answer #{i}: {r}"));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;

            // Through the proxy: PROXY header for the client address, online
            // logins against the fake Mojang API.
            let (mojang, _mlog) = fake_mojang().await;
            let secret_path = secret_file("pumpkin", secret);
            let cfg = format!(
                "listener:\n  - {{ bind: \"127.0.0.1:{proxy_port}\", proxy-protocol: true, trusted-proxies: [127.0.0.1/32] }}\n\
                 login:\n  online-mode: per-player\nauthentication:\n  session-server: \"{mojang}\"\n  profile-lookup: \"{mojang}/lookup\"\n\
                 forwarding:\n  mode: modern\n  secret-file: \"{}\"\nservers:\n  lobby: {{ address: \"{backend}\", protocol: {protocol} }}\n\
                 routing:\n  try: [lobby]\n",
                toml_path(&secret_path)
            );
            let (proxy, addr) = start_proxy(&cfg).await;
            let spawn = |name: &str, idle: u64, source: &str| {
                let o = SessionOptions {
                    name: name.into(),
                    script: false,
                    idle: Duration::from_secs(idle),
                    proxy_source: Some(source.parse().unwrap()),
                    ..SessionOptions::default()
                };
                tokio::spawn(async move { pumbo_testclient::run(addr, module(protocol), &o).await })
            };
            let offline = spawn("PumboOff", 40, "203.0.113.7:50000");
            let premium = spawn("Notch", 35, "203.0.113.8:50001");
            tokio::time::sleep(Duration::from_secs(4)).await;
            let watcher = pumbo_testclient::run(
                addr,
                module(protocol),
                &SessionOptions {
                    name: "PumboTwo".into(),
                    script: false,
                    idle: Duration::from_secs(4),
                    proxy_source: Some("203.0.113.9:50002".parse().unwrap()),
                    ..SessionOptions::default()
                },
            )
            .await
            .unwrap();
            pumpkin.command("ban-ip PumboOff");
            tokio::time::sleep(Duration::from_secs(2)).await;
            pumpkin.command("pardon-ip 203.0.113.7");
            let premium = premium.await.unwrap().unwrap();
            let offline = offline.await.unwrap().unwrap();
            report.push(format!("proxy, offline PumboOff: {:?}", offline.ending));
            report.push(format!(
                "proxy, premium Notch: {:?}, should_authenticate {:?}, keep-alives in 35 s: {}",
                premium.ending,
                premium.encryption.as_ref().map(|e| e.should_authenticate),
                premium.keep_alives
            ));
            report.push(format!("proxy, PumboTwo: {:?}", watcher.ending));
            // The second player sees the premium player's skin in player_info_update.
            let m = module(protocol);
            let seen = watcher.frames.iter().any(|f| {
                f.direction == Direction::Clientbound
                    && f.phase == Phase::Play
                    && m.packet_kind(f.phase, f.direction, f.id) == Some(PacketKind::PlayerInfoUpdate)
                    && f.payload.windows(TEXTURES.len()).any(|w| w == TEXTURES.as_bytes())
            });
            report.push(format!("textures of Notch in PumboTwo's player_info_update: {seen}"));
            // Pumpkin saves a player's data right after they leave.
            tokio::time::sleep(Duration::from_secs(2)).await;
            let log = pumpkin.log();
            // Pumpkin saves player data under the UUID it got from forwarding.
            let off_uuid = pumbo_identity::offline_uuid("PumboOff").to_string();
            let notch_uuid = premium_uuid("Notch").unwrap().to_string();
            let saved = player_files(&dir);
            report.push(format!("Pumpkin player data files: {saved:?}"));
            report.push(format!("Pumpkin log has IP 203.0.113.7 (ban-ip): {}", log.contains("Banned IP address 203.0.113.7")));
            proxy.stop();
            assert_eq!(premium.ending, Ending::Finished, "{:?}", premium.notes);
            assert_eq!(watcher.ending, Ending::Finished, "{:?}", watcher.notes);
            assert!(premium.encryption.unwrap().should_authenticate);
            assert!(matches!(offline.ending, Ending::Disconnected(_)), "kicked by ban-ip");
            assert!(seen, "textures not visible to the second player");
            assert!(saved.contains(&format!("{off_uuid}.dat")) && saved.contains(&format!("{notch_uuid}.dat")), "player data under forwarded UUIDs");
            assert!(premium.keep_alives >= 2, "Pumpkin keep-alives through the proxy");
            assert!(log.contains("Banned IP address 203.0.113.7"), "forwarded IP in the ban-ip answer");
        });
    }));
    pumpkin.stop();
    for line in &report {
        eprintln!("{line}");
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Diagnostic: `pumbo-testclient` straight to Pumpkin (no proxy, no
/// forwarding), with and without the vanilla play script.
#[test]
#[ignore = "starts a Pumpkin binary"]
fn pumpkin_testclient_direct() {
    let bin = env_path("PUMBO_PUMPKIN_BIN", "pumpkin");
    let template =
        std::fs::read_to_string(env_path("PUMBO_PUMPKIN_TEMPLATE", "pumpkin.toml.template"))
            .unwrap();
    let protocol: i32 = std::env::var("PUMBO_PUMPKIN_PROTOCOL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(777);
    let port = 25606u16;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let dir = work().join(format!("pumpkin-direct-{protocol}-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    // Forwarding config with the proxy section switched back off.
    let cfg = pumpkin_config(&template, port, "unused-secret-unused-secret").replacen(
        "[networking.proxy]\nenabled = true",
        "[networking.proxy]\nenabled = false",
        1,
    );
    std::fs::write(dir.join("pumpkin.toml"), cfg).unwrap();
    let pumpkin = Pumpkin::start(&bin, &dir);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    wait_port(addr);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let outcomes = rt.block_on(async {
        let mut v = Vec::new();
        for (name, script) in [("PumboIdle", false), ("PumboScript", true)] {
            let o = SessionOptions {
                name: name.into(),
                script,
                idle: Duration::from_secs(20),
                ..SessionOptions::default()
            };
            let out = pumbo_testclient::run(addr, module(protocol), &o)
                .await
                .unwrap();
            v.push((name, out.ending, out.keep_alives, out.notes));
        }
        v
    });
    pumpkin.stop();
    for (name, ending, keep_alives, notes) in &outcomes {
        eprintln!("{name}: {ending:?}, keep-alives {keep_alives}, notes {notes:?}");
    }
    assert_eq!(outcomes[0].1, Ending::Finished);
}

/// `online-mode = "per-player"` against the real Mojang profile lookup (two
/// HTTPS requests with the neutral User-Agent): a premium name gets an
/// encryption request with `should_authenticate`, an unused name goes offline.
/// Finishing the premium login needs a real client (docs/do-sprawdzenia-w-grze.md).
#[test]
#[ignore = "talks to api.minecraftservices.com"]
fn real_mojang_per_player() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let cfg = "listener:\n  - bind: \"127.0.0.1:25607\"\nlogin:\n  online-mode: per-player\nforwarding:\n  mode: none\n\
                   servers:\n  lobby: { address: \"127.0.0.1:25609\", protocol: 777 }\nrouting:\n  try: [lobby]\n";
        let (proxy, addr) = start_proxy(cfg).await;
        let premium = pumbo_testclient::run(
            addr,
            module(777),
            &SessionOptions { name: "Notch".into(), stop_at_encryption: true, ..SessionOptions::default() },
        )
        .await
        .unwrap();
        let enc = premium.encryption.expect("encryption request for a premium name");
        eprintln!("Notch: should_authenticate {}, public key {} bytes", enc.should_authenticate, enc.public_key_len);
        assert!(enc.should_authenticate);
        // No backend on 25609: the offline player is refused after login, without encryption.
        let cracked = pumbo_testclient::run(addr, module(777), &SessionOptions { name: "PumboE3q7x".into(), ..SessionOptions::default() })
            .await
            .unwrap();
        eprintln!("PumboE3q7x: encryption {:?}, ending {:?}", cracked.encryption, cracked.ending);
        assert!(cracked.encryption.is_none());
        proxy.stop();
    });
}

/// One idle session against an already running proxy (`PUMBO_PROXY_ADDR`,
/// `PUMBO_PROXY_PROTOCOL`, default 777), e.g. the release binary in front of
/// Pumpkin; checks the binary's wiring end to end.
#[test]
#[ignore = "needs a running proxy"]
fn session_against_running_proxy() {
    let addr: SocketAddr = std::env::var("PUMBO_PROXY_ADDR").unwrap().parse().unwrap();
    let protocol: i32 = std::env::var("PUMBO_PROXY_PROTOCOL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(777);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (_, json) = pumbo_testclient::status(addr, module(protocol))
            .await
            .unwrap();
        eprintln!("status: {json}");
        let o = SessionOptions {
            name: "PumboBinary".into(),
            script: false,
            idle: Duration::from_secs(20),
            ..SessionOptions::default()
        };
        let out = pumbo_testclient::run(addr, module(protocol), &o)
            .await
            .unwrap();
        eprintln!(
            "session: {:?}, keep-alives {}, notes {:?}",
            out.ending, out.keep_alives, out.notes
        );
        assert_eq!(out.ending, Ending::Finished);
    });
}
