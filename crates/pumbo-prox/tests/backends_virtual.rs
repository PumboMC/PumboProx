//! E6 against real servers (ignored by default: they start Java servers and
//! a Pumpkin binary).
//!
//! - `vanilla_release_matrix`: the vanilla server of every protocol 767–777
//!   behind the proxy (`forwarding = none`) with the test gate: the player
//!   walks through the virtual world (`common::through_gate`: chunks,
//!   platform, map, title, bossbar, fall, command, chat, idling for
//!   `PUMBO_VIRTUAL_IDLE_SECS`, default 60, world change), is released to
//!   the vanilla server without a disconnect, moves there and stays. A second
//!   player without known packs enters and is released too. The server runs
//!   a data pack with our `pumbo:void` dimension type (and biome before 774)
//!   as JSON: vanilla must load it with its own codec and send it back equal
//!   to what the proxy sends. Servers on 25640–25643, two at a time, in
//!   `PUMBO_E6_WORK/vanilla` (default `target/pumbo-e6`); jars from
//!   `PUMBO_JARS` (default `~/.cache/pumbo-datagen`). `PUMBO_ONLY=767,777`
//!   picks protocols.
//! - `pumpkin_release`: official Pumpkin 0.2.0 (26.3, `PUMBO_PUMPKIN_BIN`,
//!   base config `PUMBO_PUMPKIN_BASE` with telemetry off) with Velocity modern
//!   forwarding on 25646; clients 777, 775 and 767 (767 and 775 through the
//!   built-in translator) are released there from the virtual world.
//!
//! `cargo test -p pumbo-prox --test backends_virtual -- --ignored --nocapture --test-threads=1`
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

use common::{Pumpkin, module, secret_file, start_proxy, through_gate, toml_path, wait_port};
use pumbo_datagen::record::{PLAYER, Server, prepare_dir};
use pumbo_nbt::Tag;
use pumbo_protocol::packets::configuration::RegistryData;
use pumbo_protocol::packets::{self, Ctx};
use pumbo_protocol::{Direction, PacketKind, Phase, VersionModule};
use pumbo_testclient::{JoinOptions, Player};

const WAIT: Duration = Duration::from_secs(20);

fn env_path(key: &str, default: &str) -> PathBuf {
    std::env::var_os(key).map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn work() -> PathBuf {
    env_path("PUMBO_E6_WORK", "target/pumbo-e6")
}

fn idle() -> Duration {
    Duration::from_secs(
        std::env::var("PUMBO_VIRTUAL_IDLE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60),
    )
}

/// NBT as data pack JSON (our bytes are all flags, so booleans).
fn json(t: &Tag) -> serde_json::Value {
    use serde_json::Value as V;
    match t {
        Tag::Byte(b) => V::Bool(*b != 0),
        Tag::Short(v) => V::from(*v),
        Tag::Int(v) => V::from(*v),
        Tag::Long(v) => V::from(*v),
        Tag::Float(v) => V::from(f64::from(*v)),
        Tag::Double(v) => V::from(*v),
        Tag::String(s) => V::from(s.clone()),
        Tag::List(l) => V::Array(l.items.iter().map(json).collect()),
        Tag::Compound(c) => V::Object(c.iter().map(|(k, v)| (k.to_string(), json(v))).collect()),
        other => panic!("no JSON for {other:?}"),
    }
}

/// `world/datapacks/pumbo` with our void entries, in the pack format of the jar.
fn void_datapack(dir: &Path, jar: &Path, m: &dyn VersionModule) {
    let out = std::process::Command::new("unzip")
        .arg("-p")
        .arg(jar)
        .arg("version.json")
        .output()
        .unwrap();
    let version: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pv = &version["pack_version"];
    let pack = match pv.get("data_major") {
        Some(major) => serde_json::json!({
            "description": "pumbo void",
            "min_format": [major, pv["data_minor"]],
            "max_format": [major, pv["data_minor"]],
        }),
        None => serde_json::json!({ "description": "pumbo void", "pack_format": pv["data"] }),
    };
    let root = dir.join("world/datapacks/pumbo");
    std::fs::create_dir_all(root.join("data/pumbo/dimension_type")).unwrap();
    std::fs::write(
        root.join("pack.mcmeta"),
        serde_json::json!({ "pack": pack }).to_string(),
    )
    .unwrap();
    let f = m.features();
    std::fs::write(
        root.join("data/pumbo/dimension_type/void.json"),
        json(&pumbo_virtual::registry::dimension_type_nbt(f)).to_string(),
    )
    .unwrap();
    if let Some(ours) = pumbo_virtual::registry::biome_nbt(f) {
        // A data pack biome also has generation settings (not sent to
        // clients): vanilla's `the_void` with our climate and colours.
        let release = jar.parent().and_then(|d| d.file_name()).unwrap();
        let release = release.to_str().unwrap();
        let inner = dir.join("inner.jar");
        let unzip = |file: &Path, entry: &str| {
            std::process::Command::new("unzip")
                .arg("-p")
                .arg(file)
                .arg(entry)
                .output()
                .unwrap()
                .stdout
        };
        let bytes = unzip(
            jar,
            &format!("META-INF/versions/{release}/server-{release}.jar"),
        );
        std::fs::write(&inner, bytes).unwrap();
        let template = unzip(&inner, "data/minecraft/worldgen/biome/the_void.json");
        let mut b: serde_json::Value = serde_json::from_slice(&template).unwrap();
        for (k, v) in json(&ours).as_object().unwrap() {
            b[k] = v.clone();
        }
        std::fs::create_dir_all(root.join("data/pumbo/worldgen/biome")).unwrap();
        std::fs::write(
            root.join("data/pumbo/worldgen/biome/void.json"),
            b.to_string(),
        )
        .unwrap();
    }
}

/// `pumbo:void` as the backend sent it in its configuration, per registry.
fn backend_void(p: &Player, m: &dyn VersionModule) -> Vec<(String, Tag)> {
    let ctx = Ctx::new(m, Direction::Clientbound);
    let mut out: Vec<(String, Tag)> = Vec::new();
    for f in &p.c.log {
        if f.phase != Phase::Configuration
            || f.direction != Direction::Clientbound
            || m.packet_kind(f.phase, f.direction, f.id) != Some(PacketKind::RegistryData)
        {
            continue;
        }
        let r: RegistryData = packets::decode(&f.payload, &ctx).unwrap();
        if let Some(data) = r
            .entries
            .iter()
            .find(|e| e.id == "pumbo:void")
            .and_then(|e| e.data.clone())
        {
            // The later one is the backend's (the first is ours).
            out.retain(|(reg, _)| *reg != r.registry);
            out.push((r.registry, data));
        }
    }
    out
}

/// The client after release: confirms the position, moves a little, chats.
async fn play_on_backend(p: &mut Player, what: &str) {
    let ok = p
        .pump(WAIT, |p| {
            p.logins.len() == 2 && p.position.is_some() && p.shown()
        })
        .await
        .unwrap();
    if !ok || std::env::var_os("PUMBO_DUMP").is_some() {
        // What crossed the wire around the release, for the failure message.
        let m = p.module().clone();
        for f in p.c.log.iter().rev().take(40).rev() {
            let kind = m.packet_kind(f.phase, f.direction, f.id);
            eprintln!(
                "{what}: {:?} {:?} {:?} {} bytes {:02x?}",
                f.phase,
                f.direction,
                kind,
                f.payload.len(),
                &f.payload[..f.payload.len().min(48)]
            );
        }
    }
    assert!(
        ok,
        "{what}: no backend login after release: {:?}",
        p.disconnect
    );
    let (x, y, z) = p.position.unwrap();
    for i in 0..10 {
        p.c.send(&pumbo_protocol::packets::world::MovePlayerPos(
            pumbo_protocol::packets::world::MovePlayer {
                position: Some((x, y, z + f64::from(i) * 0.01)),
                rotation: None,
                flags: pumbo_protocol::packets::world::MOVE_ON_GROUND,
            },
        ))
        .await
        .unwrap();
        p.tick_end().await.unwrap();
        let _ = p.pump(Duration::from_millis(50), |_| false).await;
    }
    p.pump(Duration::from_secs(3), |_| false).await.unwrap();
    assert!(p.disconnect.is_none(), "{what}: {:?}", p.disconnect);
    assert_eq!(p.reconfigurations, 1, "{what}");
}

fn vanilla_one(protocol: i32, slot: u16) -> Result<String, String> {
    let m = module(protocol);
    let tables = pumbo_data::tables(m.protocol()).map_err(|e| e.to_string())?;
    let release = tables.releases.last().ok_or("no release")?.name.clone();
    let home = std::env::var("HOME").unwrap_or_default();
    let jar = env_path("PUMBO_JARS", &format!("{home}/.cache/pumbo-datagen"))
        .join(&release)
        .join("server.jar");
    let dir = work().join("vanilla").join(&release);
    let port = 25640 + slot;
    prepare_dir(&dir, port, protocol).map_err(|e| e.to_string())?;
    void_datapack(&dir, &jar, &*m);
    let started = Instant::now();
    let mut server = Server::start(&dir, &jar, "java").map_err(|e| e.to_string())?;
    eprintln!("{release}: vanilla pid {} on {port}", server.pid);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    // A failed assertion must not leave the server running (own PID only).
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        server
        .wait_ready(addr, Duration::from_secs(240))
        .map_err(|e| e.to_string())
        .and_then(|()| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(async {
                let cfg = format!(
                    "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
                     servers:\n  lobby: {{ address: \"{addr}\", protocol: {protocol} }}\nrouting:\n  try: [lobby]\n\
                     virtual:\n  test-gate: true\n  test-gate-seconds: 0\n"
                );
                let (proxy, paddr) = start_proxy(&cfg).await;
                let what = format!("{release} ({protocol})");
                let mut p = Player::join(paddr, m.clone(), JoinOptions::new(PLAYER)).await.map_err(|e| e.to_string())?;
                let report = through_gate(&mut p, &what, idle()).await;
                p.command("gate release").await.unwrap();
                play_on_backend(&mut p, &what).await;
                // Vanilla read our entries from the data pack and sends them back.
                let theirs = backend_void(&p, &*m);
                let f = m.features();
                let mut ours = vec![(
                    "minecraft:dimension_type".to_string(),
                    pumbo_virtual::registry::dimension_type_nbt(f),
                )];
                if let Some(b) = pumbo_virtual::registry::biome_nbt(f) {
                    ours.push(("minecraft:worldgen/biome".to_string(), b));
                }
                for (reg, tag) in &ours {
                    let back = theirs.iter().find(|(r, _)| r == reg).map(|(_, t)| t);
                    assert!(
                        back.is_some_and(|b| b.equivalent(tag)),
                        "{what}: {reg}: vanilla sent {back:?}, we send {tag:?}"
                    );
                }
                p.close().await;
                tokio::time::sleep(Duration::from_secs(1)).await;
                // Without known packs: full data, then the same release.
                let mut extra = String::new();
                if pumbo_data::full_registry_data(m.protocol()).is_some() {
                    let mut opts = JoinOptions::new(PLAYER);
                    opts.known_packs = false;
                    let mut q = Player::join(paddr, m.clone(), opts).await.map_err(|e| e.to_string())?;
                    assert!(q.pump(WAIT, |q| !q.maps.is_empty()).await.unwrap(), "{what}: no map without packs");
                    q.command("gate release").await.unwrap();
                    play_on_backend(&mut q, &format!("{what} without packs")).await;
                    q.close().await;
                    extra = ", without known packs too".into();
                }
                proxy.stop();
                Ok(format!(
                    "{what}: {report}; released to vanilla, vanilla loads and sends pumbo:void equal{extra}"
                ))
            })
        })
    }));
    server.stop();
    let result = result.unwrap_or_else(|p| std::panic::resume_unwind(p));
    result.map(|r| format!("{r} ({} s)", started.elapsed().as_secs()))
}

#[test]
#[ignore = "starts vanilla servers of every protocol"]
fn vanilla_release_matrix() {
    let only: Vec<i32> = std::env::var("PUMBO_ONLY")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let protocols: Vec<i32> = pumbo_data::protocols()
        .map(|p| p.0)
        .filter(|p| only.is_empty() || only.contains(p))
        .collect();
    let mut failures = Vec::new();
    for pair in protocols.chunks(2) {
        let handles: Vec<_> = pair
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let p = *p;
                (p, std::thread::spawn(move || vanilla_one(p, i as u16)))
            })
            .collect();
        for (p, h) in handles {
            match h.join() {
                Ok(Ok(line)) => eprintln!("OK {line}"),
                Ok(Err(e)) => failures.push(format!("{p}: {e}")),
                Err(_) => failures.push(format!("{p}: panicked")),
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
#[ignore = "starts a Pumpkin binary"]
fn pumpkin_release() {
    let dir = work().join("pumpkin");
    std::fs::create_dir_all(&dir).unwrap();
    let bin = env_path("PUMBO_PUMPKIN_BIN", "target/pumbo-e6/pumpkin-bin/pumpkin");
    let base = std::fs::read_to_string(env_path(
        "PUMBO_PUMPKIN_BASE",
        "target/pumbo-e6/pumpkin-bin/pumpkin.toml.base",
    ))
    .unwrap();
    assert!(
        base.contains("[telemetry]\nenabled = false"),
        "telemetry must be off"
    );
    let secret = "e6-virtual-world-test-secret-0123456789";
    let port = 25646;
    // The Java listener and the Velocity secret of the base config.
    let java = base.find("[networking.java]").unwrap();
    let at = java + base[java..].find("address = ").unwrap();
    let end = at + base[at..].find('\n').unwrap();
    let mut cfg = base.clone();
    cfg.replace_range(at..end, &format!("address = \"127.0.0.1:{port}\""));
    let velocity = cfg
        .find("[networking.proxy.velocity]\nenabled = true\nsecret = \"")
        .unwrap();
    let at = velocity + cfg[velocity..].find("secret = ").unwrap();
    let end = at + cfg[at..].find('\n').unwrap();
    cfg.replace_range(at..end, &format!("secret = \"{secret}\""));
    assert!(cfg.contains("[networking.proxy]\nenabled = true"));
    std::fs::write(dir.join("pumpkin.toml"), cfg).unwrap();
    let server = Pumpkin::start(&bin.canonicalize().unwrap(), &dir);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    wait_port(addr);
    let secret_path = secret_file("e6-pumpkin", secret);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let cfg = format!(
            "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\n\
             forwarding:\n  mode: modern\n  secret-file: \"{}\"\n\
             servers:\n  pumpkin: {{ address: \"{addr}\", protocol: 777 }}\nrouting:\n  try: [pumpkin]\n\
             virtual:\n  test-gate: true\n  test-gate-seconds: 0\n",
            toml_path(&secret_path)
        );
        let (_proxy, paddr) = start_proxy(&cfg).await;
        for protocol in [777, 775, 767] {
            let what = format!("Pumpkin 0.2.0, client {protocol}");
            let mut p = Player::join(paddr, module(protocol), JoinOptions::new(&format!("Gate{protocol}")))
                .await
                .unwrap();
            assert!(p.pump(WAIT, |p| !p.maps.is_empty()).await.unwrap(), "{what}: no map");
            let heights = p.fall(pumbo_prox::world::LANDING_Y, 200).await.unwrap();
            p.command("gate release").await.unwrap();
            play_on_backend(&mut p, &what).await;
            eprintln!(
                "OK {what}: fell {} ticks, released, login on Pumpkin ({}), moved, stayed",
                heights.len(),
                p.logins[1].dimension
            );
            p.close().await;
        }
    });
    server.stop();
}
