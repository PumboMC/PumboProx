//! Shared helpers of the proxy's integration tests.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::Write as _;
use std::net::{SocketAddr, TcpStream as StdTcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pumbo_data::DataVersion;
use pumbo_prox::config::Config;
use pumbo_prox::server::Proxy;
use std::sync::Mutex;

use pumbo_protocol::frame::FrameConfig;
use pumbo_protocol::packets::common::{ClientboundCustomPayload, KeepAlive};
use pumbo_protocol::packets::configuration::FinishConfiguration;
use pumbo_protocol::packets::login::{LoginAcknowledged, LoginFinished, LoginStart};
use pumbo_protocol::packets::play::{ChatCommand, Login};
use pumbo_protocol::packets::status::Intention;
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::types::{Reader, WriteExt};
use pumbo_protocol::{Direction, PacketKind, Phase};
use pumbo_protocol::{ProtocolVersion, VersionModule};
use pumbo_prox::conn::Conn;
use pumbo_prox::world::{LANDING_Y, MAP_RED, MAP_WHITE};
use pumbo_testclient::Player;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use uuid::Uuid;

/// A `textures` property value as Mojang sends it (base64 JSON with a skin URL).
pub const TEXTURES: &str = "eyJ0aW1lc3RhbXAiOjE3MDAwMDAwMDAwMDAsInByb2ZpbGVJZCI6IjA2OWE3OWY0NDRlOTQ3MjZhNWJlZmNhOTBlMzhhYWY1IiwicHJvZmlsZU5hbWUiOiJOb3RjaCIsInRleHR1cmVzIjp7IlNLSU4iOnsidXJsIjoiaHR0cDovL3RleHR1cmVzLm1pbmVjcmFmdC5uZXQvdGV4dHVyZS8yOTIwMDlhNDkyNWI1OGYwMmM3N2RhZGMzZWNlZjA3ZWE0Yzc0NzJmNjRlMGZkYzMyY2U1NTIyNDg5MzYyNjgwIn19fQ==";

pub fn module(protocol: i32) -> Arc<dyn VersionModule> {
    Arc::new(DataVersion::new(
        pumbo_data::tables(ProtocolVersion(protocol)).unwrap(),
    ))
}

/// Starts a proxy from config text; returns it with the first listener's address.
pub async fn start_proxy(toml: &str) -> (Arc<Proxy>, SocketAddr) {
    start_proxy_at(toml, None).await
}

pub async fn start_proxy_at(toml: &str, path: Option<PathBuf>) -> (Arc<Proxy>, SocketAddr) {
    let cfg = Config::parse(toml).unwrap();
    let mut logging = cfg.logging.clone();
    logging.level = std::env::var("PUMBO_TEST_LOG").unwrap_or_else(|_| "warn".into());
    pumbo_prox::logging::init(&logging);
    let proxy = Proxy::new(cfg, path).unwrap();
    let listeners = proxy.bind().await.unwrap();
    let addr = listeners[0].0.local_addr();
    tokio::spawn(proxy.clone().serve(listeners));
    (proxy, addr)
}

/// A secret file with mode 0600 in a fresh temporary directory.
pub fn secret_file(tag: &str, secret: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pumbo-e3-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("forwarding.secret");
    std::fs::write(&path, secret).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

/// Path inside a double-quoted YAML (or TOML) string: the same escapes.
pub fn toml_path(p: &std::path::Path) -> String {
    p.to_str().unwrap().replace('\\', "\\\\")
}

#[derive(Debug, Default)]
pub struct MojangLog {
    pub has_joined: Vec<(String, String)>,
    pub lookups: Vec<String>,
}

pub fn premium_uuid(name: &str) -> Option<Uuid> {
    match name {
        "Notch" => Some(Uuid::parse_str("069a79f444e94726a5befca90e38aaf5").unwrap()),
        "jeb_" => Some(Uuid::parse_str("853c80ef3c3749fdaa49938b674adae6").unwrap()),
        _ => None,
    }
}

/// `GET /session/minecraft/hasJoined?...` and `GET /lookup/<name>`.
pub async fn fake_mojang() -> (String, Arc<Mutex<MojangLog>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(MojangLog::default()));
    let l = log.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let log = l.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split(' ').nth(1).unwrap_or("").to_string();
                let (status, body) = if let Some(q) =
                    path.strip_prefix("/session/minecraft/hasJoined?")
                {
                    let get = |k: &str| {
                        q.split('&')
                            .find_map(|p| p.strip_prefix(&format!("{k}=")))
                            .unwrap_or("")
                            .to_string()
                    };
                    let (user, hash) = (get("username"), get("serverId"));
                    log.lock().unwrap().has_joined.push((user.clone(), hash));
                    match premium_uuid(&user) {
                        Some(id) => (
                            "200 OK",
                            format!(
                                r#"{{"id":"{}","name":"{user}","properties":[{{"name":"textures","value":"{TEXTURES}","signature":"c2lnbmF0dXJl"}}]}}"#,
                                id.simple()
                            ),
                        ),
                        None => ("204 No Content", String::new()),
                    }
                } else if let Some(name) = path.strip_prefix("/lookup/") {
                    log.lock().unwrap().lookups.push(name.to_string());
                    match (name, premium_uuid(name)) {
                        ("Broken", _) => ("500 Internal Server Error", String::new()),
                        (_, Some(id)) => (
                            "200 OK",
                            format!(r#"{{"id":"{}","name":"{name}"}}"#, id.simple()),
                        ),
                        (_, None) => (
                            "404 Not Found",
                            r#"{"errorMessage":"not found"}"#.to_string(),
                        ),
                    }
                } else {
                    ("404 Not Found", String::new())
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
    (base, log)
}

// ---------------------------------------------------------------- Pumpkin

pub struct Pumpkin {
    child: Child,
    pub pid: u32,
    out: PathBuf,
}

/// A panicking test must not leave the server running (own PID only).
impl Drop for Pumpkin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Pumpkin {
    pub fn start(bin: &Path, dir: &Path) -> Self {
        let out = dir.join("server.out");
        let log = std::fs::File::create(&out).unwrap();
        let child = Command::new(bin)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let pid = child.id();
        eprintln!("pumpkin pid {pid}");
        Self { child, pid, out }
    }

    pub fn command(&mut self, cmd: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{cmd}").unwrap();
        stdin.flush().unwrap();
    }

    pub fn log(&self) -> String {
        String::from_utf8_lossy(&std::fs::read(&self.out).unwrap_or_default()).to_string()
    }

    /// `stop` on the console, then a kill of this PID only.
    pub fn stop(mut self) {
        let _ = self.child.stdin.as_mut().map(|s| writeln!(s, "stop"));
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("pumpkin pid {} did not stop, killing it", self.pid);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn wait_port(addr: SocketAddr) {
    let start = Instant::now();
    while StdTcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(90),
            "{addr} not ready"
        );
        std::thread::sleep(Duration::from_millis(300));
    }
}

pub fn pumpkin_config(template: &str, port: u16, secret: &str) -> String {
    let mut s = template.to_string();
    let mut set = |from: &str, to: &str| {
        assert!(s.contains(from), "template lacks {from:?}");
        s = s.replacen(from, to, 1);
    };
    set(
        "[networking.proxy]\nenabled = false",
        "[networking.proxy]\nenabled = true",
    );
    set(
        "[networking.proxy.velocity]\nenabled = false\nsecret = \"\"",
        &format!("[networking.proxy.velocity]\nenabled = true\nsecret = \"{secret}\""),
    );
    let java = s.find("[networking.java]").unwrap();
    let addr_at = java + s[java..].find("address = ").unwrap();
    let line_end = addr_at + s[addr_at..].find('\n').unwrap();
    s.replace_range(
        addr_at..line_end,
        &format!("address = \"127.0.0.1:{port}\""),
    );
    s = s.replace("use_tty = true", "use_tty = false");
    assert!(
        s.contains("online_mode = false"),
        "backend must be offline behind the proxy"
    );
    // Telemetry off (0.2.0 has the section; 0.1.0-dev has no telemetry).
    if s.contains("[telemetry]") {
        assert!(
            s.contains("[telemetry]\nenabled = false"),
            "telemetry must be off"
        );
    }
    s
}

// ---------------------------------------------------------------- virtual world

/// Palette of the section with the platform: contains `state`.
pub fn platform_in(
    chunk: &pumbo_protocol::packets::world::LevelChunkWithLight,
    m: &dyn VersionModule,
    state: u32,
) -> bool {
    let f = m.features();
    let mut r = Reader::new(&chunk.sections);
    for section in 0..16 {
        r.i16().unwrap();
        if f.chunk_section_fluid_count {
            r.i16().unwrap();
        }
        let bits = r.u8().unwrap();
        let palette: Vec<u32> = if bits == 0 {
            vec![r.varint().unwrap() as u32]
        } else {
            let n = r.varint().unwrap();
            (0..n).map(|_| r.varint().unwrap() as u32).collect()
        };
        let longs = if !f.chunk_data_unsized {
            r.varint().unwrap() as usize
        } else if bits == 0 {
            0
        } else {
            4096usize.div_ceil(64 / bits as usize)
        };
        r.take(longs * 8).unwrap();
        // Biomes: one value.
        assert_eq!(r.u8().unwrap(), 0);
        r.varint().unwrap();
        if !f.chunk_data_unsized {
            r.varint().unwrap();
        }
        if section == 4 {
            return palette.contains(&state);
        }
    }
    false
}

/// The test gate from the player's side (`[virtual] test-gate`), up to
/// before the release: the virtual login, chunks with the platform, the map
/// in hand, title and bossbar, a fall checked by the gate, a command, chat,
/// idling for `idle`, a world change. Returns a line for the report.
pub async fn through_gate(p: &mut Player, what: &str, idle: Duration) -> String {
    let m = p.module().clone();
    let login = &p.logins[0];
    assert_eq!(login.dimension, "pumbo:virtual", "{what}");
    assert!(
        login.dimension_type >= 4,
        "{what}: our dimension type follows vanilla's"
    );
    // Chunks, then the world shows and the map arrives.
    assert!(
        p.pump(GATE_WAIT, |p| !p.maps.is_empty() && !p.slots.is_empty())
            .await
            .unwrap(),
        "{what}: no map ({} chunks, {} shown)",
        p.chunks.len(),
        p.worlds_shown
    );
    assert_eq!(p.chunks.len(), 25, "{what}");
    let tables = pumbo_data::tables(m.protocol()).unwrap();
    let stone = tables.block_state("minecraft:smooth_stone", &[]).unwrap();
    let center = p.chunks.iter().find(|c| (c.x, c.z) == (0, 0)).unwrap();
    assert!(
        platform_in(center, &*m, stone),
        "{what}: platform in chunk 0 0"
    );
    let map = &p.maps[0];
    let patch = map.patch.as_ref().unwrap();
    assert_eq!(
        (patch.columns, patch.rows, patch.colors.len()),
        (128, 128, 16384)
    );
    assert_eq!(patch.colors[0], MAP_RED);
    assert_eq!(patch.colors[20 * 128 + 10], MAP_WHITE);
    let slot = &p.slots[0];
    let item = tables
        .registry("minecraft:item")
        .unwrap()
        .id("minecraft:filled_map")
        .unwrap();
    assert_eq!(
        (slot.slot, slot.item.item),
        (36, item as i32),
        "{what}: map in hand"
    );
    assert!(
        p.pump(GATE_WAIT, |p| !p.titles.is_empty()
            && !p.bossbars.is_empty())
            .await
            .unwrap()
    );
    assert_eq!(p.titles[0], "PumboProx");
    assert_eq!(p.bossbars[0], "Test gate");
    // Fall onto the platform: the proxy decodes the movement, the gate checks
    // it against the vanilla curve.
    let heights = p.fall(LANDING_Y, 200).await.unwrap();
    assert!(
        message(p, "fall check: Passed").await,
        "{what}: {:?}",
        p.messages
    );
    // A command (arguments never echoed) and chat (text stays text).
    p.command("gate secret").await.unwrap();
    assert!(message(p, "command /gate (11 characters)").await, "{what}");
    p.chat("<red>%player_name%", None).await.unwrap();
    assert!(message(p, "chat: <red>%player_name%").await, "{what}");
    // Idle: only keep-alives keep the connection.
    let before = p.c.keep_alives;
    p.pump(idle, |_| false).await.unwrap();
    assert!(p.disconnect.is_none(), "{what}: {:?}", p.disconnect);
    let answered = p.c.keep_alives - before;
    assert!(
        answered as u64 >= (idle.as_secs() / 10).saturating_sub(1),
        "{what}: {answered} keep-alives"
    );
    // Another world in the same connection.
    p.command("gate world").await.unwrap();
    assert!(
        p.pump(GATE_WAIT, |p| p.respawns == 1 && p.worlds_shown == 2)
            .await
            .unwrap(),
        "{what}: world change"
    );
    assert!(message(p, "world 2").await);
    format!(
        "{} chunks, fall {} ticks to y={:.2}, {answered} keep-alives in {} s",
        p.chunks.len(),
        heights.len(),
        heights.last().unwrap(),
        idle.as_secs()
    )
}

pub async fn message(p: &mut Player, text: &str) -> bool {
    p.pump(GATE_WAIT, |p| p.messages.iter().any(|m| m.contains(text)))
        .await
        .unwrap()
}

const GATE_WAIT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------- backend

/// A scripted backend for virtual world tests (any protocol, `forwarding = none`).
#[derive(Debug, Default)]
pub struct Log {
    pub joins: AtomicUsize,
    pub commands: Mutex<Vec<String>>,
}

/// A backend of any protocol (from the handshake): login, configuration,
/// play `login`, keep-alives every second; records commands.
pub async fn backend() -> (SocketAddr, Arc<Log>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(Log::default());
    let l = log.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let log = l.clone();
            tokio::spawn(async move {
                let _ = serve(stream, &log).await;
            });
        }
    });
    (addr, log)
}

fn cb_send<P: Packet>(c: &mut Conn, m: &dyn VersionModule, phase: Phase, p: &P) {
    let id = m.packet_id(phase, Direction::Clientbound, P::KIND).unwrap();
    let payload = packets::encode(p, &Ctx::new(m, Direction::Clientbound)).unwrap();
    c.queue(id, &payload).unwrap();
}

fn sb_decode<P: Packet>(m: &dyn VersionModule, payload: &[u8]) -> P {
    packets::decode(payload, &Ctx::new(m, Direction::Serverbound)).unwrap()
}

async fn serve(stream: TcpStream, log: &Log) -> Result<(), String> {
    let e = |e: pumbo_prox::conn::ConnError| e.to_string();
    let mut c = Conn::new(
        Box::pin(stream),
        FrameConfig::from_client(),
        FrameConfig::from_backend(),
    );
    let (f, _) = c.read_frame().await.map_err(e)?;
    let hs: Intention = sb_decode(&*module(777), &f.payload);
    let m = module(hs.protocol);
    let m = &*m;
    let (f, _) = c.read_frame().await.map_err(e)?;
    let start: LoginStart = sb_decode(m, &f.payload);
    cb_send(
        &mut c,
        m,
        Phase::Login,
        &LoginFinished {
            profile: pumbo_protocol::packets::common::GameProfile {
                id: start.uuid,
                name: start.name,
                properties: Vec::new(),
            },
            strict_error_handling: true,
            session_id: Some(Uuid::nil()),
        },
    );
    c.flush().await.map_err(e)?;
    let (f, _) = c.read_frame().await.map_err(e)?;
    let _: LoginAcknowledged = sb_decode(m, &f.payload);
    let cfg = Phase::Configuration;
    let mut brand = Vec::new();
    brand.put_string("backend", 64).unwrap();
    cb_send(
        &mut c,
        m,
        cfg,
        &ClientboundCustomPayload {
            channel: "minecraft:brand".into(),
            data: brand,
        },
    );
    cb_send(&mut c, m, cfg, &FinishConfiguration);
    c.flush().await.map_err(e)?;
    loop {
        let (f, _) = c.read_frame().await.map_err(e)?;
        if m.packet_kind(cfg, Direction::Serverbound, f.id) == Some(PacketKind::FinishConfiguration)
        {
            break;
        }
    }
    let p = Phase::Play;
    cb_send(
        &mut c,
        m,
        p,
        &Login {
            entity_id: 7,
            hardcore: false,
            dimensions: vec!["minecraft:overworld".into()],
            max_players: 20,
            view_distance: 2,
            simulation_distance: 2,
            reduced_debug_info: false,
            respawn_screen: true,
            limited_crafting: false,
            dimension_type: 0,
            dimension: "minecraft:overworld".into(),
            hashed_seed: 0,
            game_mode: 0,
            previous_game_mode: None,
            debug: false,
            flat: true,
            death_location: None,
            portal_cooldown: 0,
            sea_level: 63,
            online_mode: false,
            enforces_secure_chat: false,
        },
    );
    c.flush().await.map_err(e)?;
    log.joins.fetch_add(1, Ordering::SeqCst);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut ka = 1;
    loop {
        tokio::select! {
            r = c.fill() => {
                r.map_err(e)?;
                while let Some((f, _)) = c.next_frame().map_err(|e| e.to_string())? {
                    let kind = m.packet_kind(p, Direction::Serverbound, f.id);
                    if kind == Some(PacketKind::ChatCommand) {
                        let cmd: ChatCommand = sb_decode(m, &f.payload);
                        log.commands.lock().unwrap().push(cmd.command);
                    } else if kind == Some(PacketKind::ConfigurationAcknowledged) {
                        // The proxy takes the player elsewhere.
                        return Ok(());
                    }
                }
            }
            _ = tick.tick() => {
                ka += 1;
                cb_send(&mut c, m, p, &KeepAlive { id: ka });
                c.flush().await.map_err(e)?;
            }
        }
    }
}
