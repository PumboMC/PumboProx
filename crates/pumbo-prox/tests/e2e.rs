//! End-to-end tests of the proxy without external servers: `pumbo-testclient`
//! as the player, a scripted backend that checks Velocity modern forwarding,
//! and a fake Mojang API (test builds only, feature `test-endpoints`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_lc_rs::hmac;
use common::{
    TEXTURES, fake_mojang, module, premium_uuid, secret_file, start_proxy, start_proxy_at,
    toml_path,
};
use pumbo_protocol::frame::FrameConfig;
use pumbo_protocol::packets::common::{ClientInformation, GameProfile, KeepAlive, Pong};
use pumbo_protocol::packets::configuration::FinishConfiguration;
use pumbo_protocol::packets::login::{
    CustomQuery, CustomQueryAnswer, LoginAcknowledged, LoginCompression, LoginDisconnect,
    LoginFinished, LoginStart,
};
use pumbo_protocol::packets::play::{CommandSuggestion, Login};
use pumbo_protocol::packets::status::Intention;
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::types::{Reader, WriteExt};
use pumbo_protocol::{Direction, PacketKind, Phase, VersionModule};
use pumbo_prox::conn::Conn;
use pumbo_testclient::{Client, ClientError, Ending, SessionOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

const SECRET: &str = "e3-test-secret-0123456789abcdef";

// ---------------------------------------------------------------- backend

#[derive(Debug, Clone)]
struct Forwarded {
    version: i32,
    ip: String,
    uuid: Uuid,
    name: String,
    properties: Vec<(String, String, Option<String>)>,
}

#[derive(Debug, Default)]
struct BackendLog {
    forwarded: Vec<Forwarded>,
    rejected: Vec<String>,
    keep_alive_answers: Vec<i64>,
    unknown_keep_alives: Vec<i64>,
    suggestions: usize,
    in_play: usize,
}

#[derive(Debug, Clone)]
struct BackendOpts {
    secret: String,
    /// Ask for forwarding (`velocity:player_info`).
    ask: bool,
    compression: Option<i32>,
    keep_alive_every: Duration,
    /// Play frames `(id, payload)` sent right after the play `login`.
    play: Vec<(i32, Vec<u8>)>,
}

impl Default for BackendOpts {
    fn default() -> Self {
        Self {
            secret: SECRET.into(),
            ask: true,
            compression: Some(256),
            keep_alive_every: Duration::from_millis(300),
            play: Vec::new(),
        }
    }
}

fn verify_forwarding(secret: &str, data: &[u8]) -> Result<Forwarded, String> {
    let (sig, rest) = data.split_at_checked(32).ok_or("short answer")?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    hmac::verify(&key, rest, sig).map_err(|_| "bad signature".to_string())?;
    let mut r = Reader::new(rest);
    let e = |e: pumbo_protocol::types::DecodeError| e.to_string();
    let version = r.varint().map_err(e)?;
    let ip = r.string(255).map_err(e)?;
    let uuid = r.uuid().map_err(e)?;
    let name = r.string(16).map_err(e)?;
    let n = r.varint().map_err(e)?;
    let mut properties = Vec::new();
    for _ in 0..n {
        let k = r.string(64).map_err(e)?;
        let v = r.string(32767).map_err(e)?;
        let s = r.option(|r| r.string(1024)).map_err(e)?;
        properties.push((k, v, s));
    }
    Ok(Forwarded {
        version,
        ip,
        uuid,
        name,
        properties,
    })
}

fn send<P: Packet>(c: &mut Conn, m: &dyn VersionModule, phase: Phase, p: &P) {
    let id = m.packet_id(phase, Direction::Clientbound, P::KIND).unwrap();
    let payload = packets::encode(p, &Ctx::new(m, Direction::Clientbound)).unwrap();
    c.queue(id, &payload).unwrap();
}

fn decode<P: Packet>(m: &dyn VersionModule, payload: &[u8]) -> P {
    packets::decode(payload, &Ctx::new(m, Direction::Serverbound)).unwrap()
}

async fn fake_backend(opts: BackendOpts) -> (SocketAddr, Arc<Mutex<BackendLog>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(BackendLog::default()));
    let l = log.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (opts, log) = (opts.clone(), l.clone());
            tokio::spawn(async move {
                let _ = serve_backend(stream, opts, log).await;
            });
        }
    });
    (addr, log)
}

async fn serve_backend(
    stream: TcpStream,
    opts: BackendOpts,
    log: Arc<Mutex<BackendLog>>,
) -> Result<(), String> {
    let e = |e: pumbo_prox::conn::ConnError| e.to_string();
    let mut c = Conn::new(
        Box::pin(stream),
        FrameConfig::from_client(),
        FrameConfig::from_backend(),
    );
    let (f, _) = c.read_frame().await.map_err(e)?;
    let hs: Intention = decode(&*module(777), &f.payload);
    let m = module(hs.protocol);
    let m = &*m;
    let (f, _) = c.read_frame().await.map_err(e)?;
    let start: LoginStart = decode(m, &f.payload);
    let mut profile = GameProfile {
        id: start.uuid,
        name: start.name,
        properties: Vec::new(),
    };
    if opts.ask {
        send(
            &mut c,
            m,
            Phase::Login,
            &CustomQuery {
                message_id: 7,
                channel: "velocity:player_info".into(),
                data: vec![4],
            },
        );
        c.flush().await.map_err(e)?;
        let (f, _) = c.read_frame().await.map_err(e)?;
        let answer: CustomQueryAnswer = decode(m, &f.payload);
        let checked = answer
            .data
            .ok_or_else(|| "not understood".to_string())
            .and_then(|d| verify_forwarding(&opts.secret, &d));
        let fwd = match checked {
            Ok(fwd) => fwd,
            Err(why) => {
                log.lock().unwrap().rejected.push(why.clone());
                send(
                    &mut c,
                    m,
                    Phase::Login,
                    &LoginDisconnect {
                        reason_json: format!("{{\"text\":\"{why}\"}}"),
                    },
                );
                c.flush().await.map_err(e)?;
                return Ok(());
            }
        };
        profile.id = fwd.uuid;
        profile.name = fwd.name.clone();
        log.lock().unwrap().forwarded.push(fwd);
    }
    if let Some(t) = opts.compression {
        send(&mut c, m, Phase::Login, &LoginCompression { threshold: t });
        c.flush().await.map_err(e)?;
        c.set_compression(t);
    }
    send(
        &mut c,
        m,
        Phase::Login,
        &LoginFinished {
            profile,
            strict_error_handling: true,
            session_id: Some(Uuid::nil()),
        },
    );
    c.flush().await.map_err(e)?;
    let (f, _) = c.read_frame().await.map_err(e)?;
    let _: LoginAcknowledged = decode(m, &f.payload);
    send(&mut c, m, Phase::Configuration, &KeepAlive { id: 1 });
    send(&mut c, m, Phase::Configuration, &FinishConfiguration);
    c.flush().await.map_err(e)?;
    loop {
        let (f, _) = c.read_frame().await.map_err(e)?;
        match m.packet_kind(Phase::Configuration, Direction::Serverbound, f.id) {
            Some(PacketKind::KeepAlive) => {
                let k: KeepAlive = decode(m, &f.payload);
                log.lock().unwrap().keep_alive_answers.push(k.id);
            }
            Some(PacketKind::FinishConfiguration) => break,
            _ => {}
        }
    }
    send(&mut c, m, Phase::Play, &play_login());
    for (id, payload) in &opts.play {
        c.queue(*id, payload).unwrap();
    }
    c.flush().await.map_err(e)?;
    log.lock().unwrap().in_play += 1;
    let mut sent: Vec<i64> = Vec::new();
    let mut tick = tokio::time::interval(opts.keep_alive_every);
    let mut next = 100;
    loop {
        tokio::select! {
            r = c.fill() => {
                r.map_err(e)?;
                while let Some((f, _)) = c.next_frame().map_err(|e| e.to_string())? {
                    match m.packet_kind(Phase::Play, Direction::Serverbound, f.id) {
                        Some(PacketKind::KeepAlive) => {
                            let k: KeepAlive = decode(m, &f.payload);
                            let mut l = log.lock().unwrap();
                            if sent.contains(&k.id) { l.keep_alive_answers.push(k.id) } else { l.unknown_keep_alives.push(k.id) }
                        }
                        Some(PacketKind::CommandSuggestion) => log.lock().unwrap().suggestions += 1,
                        _ => {}
                    }
                }
            }
            _ = tick.tick() => {
                next += 1;
                sent.push(next);
                send(&mut c, m, Phase::Play, &KeepAlive { id: next });
                c.flush().await.map_err(e)?;
            }
        }
    }
}

fn play_login() -> Login {
    Login {
        entity_id: 1,
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
    }
}

// ---------------------------------------------------------------- helpers

fn config(backend: SocketAddr, extra: &str) -> String {
    let secret = secret_file(&format!("{}", backend.port()), SECRET);
    format!(
        r#"
listener:
  - bind: "127.0.0.1:0"

login:
  online-mode: false

forwarding:
  mode: modern
  secret-file: "{}"

servers:
  lobby: {{ address: "{backend}", protocol: 777 }}

routing:
  try: [lobby]

limits:
  connections-per-ip-per-second: 100000
  concurrent-per-ip: 100000
  status-per-ip-per-second: 100000
{extra}
"#,
        toml_path(&secret)
    )
}

/// The listener takes PROXY headers from `cidr` (and requires them there).
fn with_proxy_protocol(cfg: &str, cidr: &str) -> String {
    cfg.replacen(
        "bind: \"127.0.0.1:0\"\n",
        &format!(
            "bind: \"127.0.0.1:0\"\n    proxy-protocol: true\n    trusted-proxies: [\"{cidr}\"]\n"
        ),
        1,
    )
}

fn opts(name: &str) -> SessionOptions {
    SessionOptions {
        name: name.into(),
        script: false,
        idle: Duration::from_secs(1),
        ..SessionOptions::default()
    }
}

fn client_information() -> ClientInformation {
    ClientInformation {
        locale: "en_us".into(),
        view_distance: 2,
        chat_mode: 0,
        chat_colors: true,
        skin_parts: 0x7F,
        main_hand: 1,
        text_filtering: false,
        server_listing: true,
        particle_status: 0,
    }
}

/// Logs in through the proxy and returns the client in play.
async fn login_to_play(addr: SocketAddr, name: &str) -> Client {
    let mut c = Client::connect(addr, module(777)).await.unwrap();
    c.send(&Intention {
        protocol: 777,
        address: "localhost".into(),
        port: 25565,
        intent: 2,
    })
    .await
    .unwrap();
    c.phase = Phase::Login;
    c.send(&LoginStart {
        name: name.into(),
        uuid: Uuid::nil(),
    })
    .await
    .unwrap();
    loop {
        let (k, f) = c.recv().await.unwrap();
        match k {
            Some(PacketKind::LoginCompression) => {
                let p: LoginCompression = c.decode(&f).unwrap();
                c.set_compression(p.threshold);
            }
            Some(PacketKind::LoginFinished) => break,
            other => panic!("login: {other:?}"),
        }
    }
    c.send(&LoginAcknowledged).await.unwrap();
    c.phase = Phase::Configuration;
    c.send(&client_information()).await.unwrap();
    loop {
        let (k, f) = c.recv().await.unwrap();
        match k {
            Some(PacketKind::KeepAlive) => {
                let ka: KeepAlive = c.decode(&f).unwrap();
                c.send(&ka).await.unwrap();
            }
            Some(PacketKind::FinishConfiguration) => break,
            _ => {}
        }
    }
    c.send(&FinishConfiguration).await.unwrap();
    c.phase = Phase::Play;
    loop {
        let (k, _) = c.recv().await.unwrap();
        if k == Some(PacketKind::Login) {
            return c;
        }
    }
}

/// Reads until a disconnect (its text) or the end of the connection.
async fn wait_kick(c: &mut Client, within: Duration) -> Option<String> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match c.recv_within(left).await {
            Ok((Some(PacketKind::Disconnect), f)) => {
                let d: pumbo_protocol::packets::common::Disconnect = c.decode(&f).unwrap();
                return Some(
                    pumbo_text::Component::from_nbt(&d.reason)
                        .unwrap()
                        .plain_text(),
                );
            }
            Ok(_) => {}
            Err(ClientError::Closed) => return Some(String::new()),
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------- tests

#[tokio::test(flavor = "multi_thread")]
async fn status_and_legacy_ping() {
    let (backend, _) = fake_backend(BackendOpts::default()).await;
    let (_proxy, addr) = start_proxy(&format!(
        "{}\nstatus:\n  motd: \"&6Pumbo &fE3\"\n  max-players: 42\n",
        config(backend, "")
    ))
    .await;
    for p in [767, 772, 777] {
        let (_, json) = pumbo_testclient::status(addr, module(p)).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["version"]["protocol"], p);
        assert_eq!(v["players"]["max"], 42);
        assert!(
            v["version"]["name"]
                .as_str()
                .unwrap()
                .starts_with("PumboProx 1.21-26.3")
        );
    }
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(&[0xFE, 0x01]).await.unwrap();
    let mut reply = Vec::new();
    s.read_to_end(&mut reply).await.unwrap();
    assert_eq!(reply.first(), Some(&0xFF));
    let units: Vec<u16> = reply[3..]
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    let text = String::from_utf16(&units).unwrap();
    assert!(text.starts_with("§1\u{0}127\u{0}PumboProx"), "{text}");
    assert!(text.ends_with("\u{0}Pumbo E3\u{0}0\u{0}42"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_login_with_forwarding_proxy_header_and_keep_alives() {
    let (backend, log) = fake_backend(BackendOpts::default()).await;
    let (proxy, addr) =
        start_proxy(&with_proxy_protocol(&config(backend, ""), "127.0.0.1/32")).await;
    let out = pumbo_testclient::run(
        addr,
        module(777),
        &SessionOptions {
            proxy_source: Some("203.0.113.7:4321".parse().unwrap()),
            idle: Duration::from_secs(2),
            ..opts("Steve")
        },
    )
    .await
    .unwrap();
    assert_eq!(out.ending, Ending::Finished, "{:?}", out.notes);
    assert!(out.encryption.is_none());
    assert!(out.keep_alives >= 5, "{}", out.keep_alives);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let l = log.lock().unwrap();
    let f = &l.forwarded[0];
    assert_eq!(
        f.version, 4,
        "Velocity modern, lazy session (backend asked for 4)"
    );
    assert_eq!(f.ip, "203.0.113.7");
    assert_eq!(f.uuid, pumbo_identity::offline_uuid("Steve"));
    assert_eq!(f.name, "Steve");
    assert!(f.properties.is_empty());
    assert_eq!(
        l.keep_alive_answers.first(),
        Some(&1),
        "configuration keep-alive"
    );
    assert!(l.keep_alive_answers.len() >= 5);
    assert!(l.unknown_keep_alives.is_empty());
    assert_eq!(proxy.online(), 0, "player unregistered after leaving");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_keep_alive_answers_and_duplicates_are_refused() {
    let (backend, log) = fake_backend(BackendOpts::default()).await;
    let (proxy, addr) = start_proxy(&config(backend, "")).await;
    let mut c = login_to_play(addr, "Alex").await;
    assert_eq!(proxy.online(), 1);
    c.send(&KeepAlive { id: 424_242 }).await.unwrap();
    // Same name again while online.
    let second = pumbo_testclient::run(addr, module(777), &opts("Alex"))
        .await
        .unwrap();
    match &second.ending {
        Ending::Disconnected(r) => assert!(r.contains("already connected"), "{r}"),
        other => panic!("{other:?}"),
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(log.lock().unwrap().unknown_keep_alives.is_empty());
    assert!(
        proxy
            .metrics
            .dropped_keep_alives
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn backend_without_forwarding_or_with_other_secret_is_refused() {
    let (backend, _) = fake_backend(BackendOpts {
        ask: false,
        ..BackendOpts::default()
    })
    .await;
    let (_p, addr) = start_proxy(&config(backend, "")).await;
    let out = pumbo_testclient::run(addr, module(777), &opts("Steve"))
        .await
        .unwrap();
    assert_eq!(
        out.ending,
        Ending::Disconnected("No server is available right now.".into())
    );

    let (backend, log) = fake_backend(BackendOpts {
        secret: "another-secret-another-secret".into(),
        ..BackendOpts::default()
    })
    .await;
    let (_p, addr) = start_proxy(&config(backend, "")).await;
    let out = pumbo_testclient::run(addr, module(777), &opts("Steve"))
        .await
        .unwrap();
    // The backend's refusal reaches the player.
    assert_eq!(out.ending, Ending::Disconnected("bad signature".into()));
    assert_eq!(log.lock().unwrap().rejected, ["bad signature"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn online_login_with_fake_sessionserver_forwards_textures() {
    let (mojang, mlog) = fake_mojang().await;
    let (backend, log) = fake_backend(BackendOpts::default()).await;
    let extra = format!(
        "\nauthentication:\n  service: mojang\n  session-server: \"{mojang}\"\n  profile-lookup: \"{mojang}/lookup\"\n"
    );
    let cfg = config(backend, "").replace("online-mode: false", "online-mode: true") + &extra;
    let (proxy, addr) = start_proxy(&cfg).await;
    let out = pumbo_testclient::run(addr, module(777), &opts("Notch"))
        .await
        .unwrap();
    assert_eq!(out.ending, Ending::Finished, "{:?}", out.notes);
    let enc = out.encryption.unwrap();
    assert!(enc.should_authenticate);
    assert_eq!(enc.public_key_len, 294, "RSA-2048 SubjectPublicKeyInfo");
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The proxy asked with the hash the client computed.
    assert_eq!(
        mlog.lock().unwrap().has_joined,
        [("Notch".to_string(), enc.server_hash)]
    );
    let f = log.lock().unwrap().forwarded[0].clone();
    assert_eq!(f.uuid, premium_uuid("Notch").unwrap());
    assert_eq!(
        f.properties,
        [(
            "textures".to_string(),
            TEXTURES.to_string(),
            Some("c2lnbmF0dXJl".to_string())
        )]
    );
    // A name the sessionserver does not know is refused (HTTP 204).
    let out = pumbo_testclient::run(addr, module(777), &opts("Nobody"))
        .await
        .unwrap();
    match out.ending {
        Ending::Disconnected(r) => assert!(r.contains("unverified_username"), "{r}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        proxy
            .metrics
            .auth_failures
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn per_player_online_mode() {
    let (mojang, mlog) = fake_mojang().await;
    let (backend, log) = fake_backend(BackendOpts::default()).await;
    let extra = format!(
        "\nauthentication:\n  service: mojang\n  session-server: \"{mojang}\"\n  profile-lookup: \"{mojang}/lookup\"\n"
    );
    let cfg = config(backend, "").replace("online-mode: false", "online-mode: per-player") + &extra;
    let (_p, addr) = start_proxy(&cfg).await;
    // Premium name: encryption request with should_authenticate.
    let out = pumbo_testclient::run(
        addr,
        module(777),
        &SessionOptions {
            stop_at_encryption: true,
            ..opts("jeb_")
        },
    )
    .await
    .unwrap();
    assert!(out.encryption.unwrap().should_authenticate);
    // Other names go offline without encryption.
    let out = pumbo_testclient::run(addr, module(777), &opts("Cracked1"))
        .await
        .unwrap();
    assert_eq!(out.ending, Ending::Finished);
    assert!(out.encryption.is_none());
    // Lookup outage: refused (fail-closed), never offline.
    let out = pumbo_testclient::run(addr, module(777), &opts("Broken"))
        .await
        .unwrap();
    match out.ending {
        Ending::Disconnected(r) => assert!(r.contains("Could not check your account"), "{r}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(mlog.lock().unwrap().lookups, ["jeb_", "Cracked1", "Broken"]);
    assert_eq!(log.lock().unwrap().forwarded.len(), 1);

    // encrypt-offline: offline players get an encryption request without authentication.
    let cfg = cfg.replace(
        "online-mode: per-player",
        "online-mode: false\n  encrypt-offline: true",
    );
    let (_p, addr) = start_proxy(&cfg).await;
    let out = pumbo_testclient::run(addr, module(777), &opts("Cracked2"))
        .await
        .unwrap();
    assert_eq!(out.ending, Ending::Finished, "{:?}", out.notes);
    assert!(!out.encryption.unwrap().should_authenticate);
}

#[tokio::test(flavor = "multi_thread")]
async fn packet_flood_is_kicked_and_suggestion_flood_dropped() {
    let (backend, log) = fake_backend(BackendOpts::default()).await;
    let (proxy, addr) = start_proxy(&config(backend, "")).await;
    let mut c = login_to_play(addr, "Spammer").await;
    for i in 0..100 {
        c.send(&CommandSuggestion {
            id: i,
            text: "/tim".into(),
        })
        .await
        .unwrap();
    }
    // Still connected: the next keep-alive arrives and is answered.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut answered = None;
    while tokio::time::Instant::now() < deadline {
        if let Ok((Some(PacketKind::KeepAlive), f)) = c.recv_within(Duration::from_secs(1)).await {
            let k: KeepAlive = c.decode(&f).unwrap();
            c.send(&k).await.unwrap();
            answered = Some(k.id);
            break;
        }
    }
    let id = answered.expect("still connected");
    // The answer travels behind the suggestions on the same stream, so once the
    // backend has it, every forwarded suggestion has been counted.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !log.lock().unwrap().keep_alive_answers.contains(&id) {
        assert!(tokio::time::Instant::now() < deadline, "answer lost");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let suggestions = log.lock().unwrap().suggestions;
    assert!(
        (10..=12).contains(&suggestions),
        "{suggestions} suggestions reached the backend"
    );
    // 2000 packets at once: over 500/s.
    for i in 0..2000 {
        if c.send(&Pong { id: i }).await.is_err() {
            break;
        }
    }
    assert_eq!(
        wait_kick(&mut c, Duration::from_secs(5)).await.as_deref(),
        Some("Too many packets.")
    );
    assert!(
        proxy
            .metrics
            .kicked_packet_rate
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn compression_bomb_and_oversized_frames_close_the_connection() {
    let (backend, _) = fake_backend(BackendOpts::default()).await;
    let (proxy, addr) = start_proxy(&config(backend, "")).await;
    let mut c = login_to_play(addr, "Bomber").await;
    // Declares 2 GiB uncompressed (threshold 256 is on).
    let mut body = Vec::new();
    body.put_varint(i32::MAX);
    body.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00]);
    let mut frame = Vec::new();
    frame.put_varint(body.len() as i32);
    frame.extend_from_slice(&body);
    c.write_bytes(&frame).await.unwrap();
    assert_eq!(
        wait_kick(&mut c, Duration::from_secs(3)).await.as_deref(),
        Some(""),
        "closed without a message"
    );
    assert!(
        proxy
            .metrics
            .protocol_errors
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 1
    );
    // A login frame over 8 KiB is refused before decoding.
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut hs = Vec::new();
    Intention {
        protocol: 777,
        address: "x".into(),
        port: 1,
        intent: 2,
    }
    .encode(&mut hs, &Ctx::new(&*module(777), Direction::Serverbound))
    .unwrap();
    let mut out = Vec::new();
    out.put_varint(hs.len() as i32 + 1);
    out.put_varint(0);
    out.extend_from_slice(&hs);
    out.put_varint(20_000);
    out.extend_from_slice(&[0u8; 100]);
    s.write_all(&out).await.unwrap();
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut rest)).await;
    assert!(matches!(read, Ok(Ok(_))), "closed");
    // The proxy keeps serving.
    let out = pumbo_testclient::run(addr, module(777), &opts("AfterBomb"))
        .await
        .unwrap();
    assert_eq!(out.ending, Ending::Finished);
}

#[tokio::test(flavor = "multi_thread")]
async fn three_thousand_idle_connections() {
    let (backend, _) = fake_backend(BackendOpts::default()).await;
    let (proxy, addr) = start_proxy(&config(backend, "  handshake-timeout-ms: 1500")).await;
    let mut conns = Vec::new();
    for _ in 0..3000 {
        conns.push(TcpStream::connect(addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let rejected = proxy
        .metrics
        .rejected_pending
        .load(std::sync::atomic::Ordering::Relaxed);
    let pending = proxy
        .metrics
        .pending_logins
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        rejected, 1000,
        "over max-pending-logins (2000) closed at once"
    );
    assert_eq!(pending, 2000);
    // After the handshake timeout every slot is free again.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        proxy
            .metrics
            .pending_logins
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let mut closed = 0;
    for mut s in conns {
        let mut b = [0u8; 1];
        if let Ok(Ok(0)) = tokio::time::timeout(Duration::from_millis(50), s.read(&mut b)).await {
            closed += 1;
        }
    }
    assert_eq!(closed, 3000);
    let out = pumbo_testclient::run(addr, module(777), &opts("AfterFlood"))
        .await
        .unwrap();
    assert_eq!(out.ending, Ending::Finished);
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_header_only_from_trusted_addresses() {
    let (backend, log) = fake_backend(BackendOpts::default()).await;
    let cfg = with_proxy_protocol(&config(backend, ""), "10.0.0.0/8");
    let (_p, addr) = start_proxy(&cfg).await;
    // 127.0.0.1 is not trusted here: a PROXY header is just a broken handshake.
    let out = pumbo_testclient::run(
        addr,
        module(777),
        &SessionOptions {
            proxy_source: Some("198.51.100.1:1".parse().unwrap()),
            ..opts("Faker")
        },
    )
    .await
    .unwrap();
    assert_ne!(out.ending, Ending::Finished);
    let out = pumbo_testclient::run(addr, module(777), &opts("Plain"))
        .await
        .unwrap();
    assert_eq!(out.ending, Ending::Finished);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let l = log.lock().unwrap();
    assert_eq!(l.forwarded.len(), 1);
    assert_eq!(l.forwarded[0].ip, "127.0.0.1");
}

#[tokio::test(flavor = "multi_thread")]
async fn versions_outside_the_range_are_refused() {
    let (backend, _) = fake_backend(BackendOpts::default()).await;
    let (_p, addr) = start_proxy(&config(backend, "")).await;
    for (protocol, expected) in [(778, "up to 26.3"), (766, "Use 1.21 to 26.3")] {
        let mut c = Client::connect(addr, module(777)).await.unwrap();
        c.send(&Intention {
            protocol,
            address: "localhost".into(),
            port: 25565,
            intent: 2,
        })
        .await
        .unwrap();
        c.phase = Phase::Login;
        c.send(&LoginStart {
            name: "Old".into(),
            uuid: Uuid::nil(),
        })
        .await
        .unwrap();
        let (k, f) = c.recv().await.unwrap();
        assert_eq!(k, Some(PacketKind::LoginDisconnect));
        let d: LoginDisconnect = c.decode(&f).unwrap();
        assert!(d.reason_json.contains(expected), "{}", d.reason_json);
    }
    // Every protocol of the range logs in (a backend per protocol is the vanilla matrix).
    let (_, json) = pumbo_testclient::status(addr, module(767)).await.unwrap();
    assert!(json.contains("767"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_and_metrics_endpoint() {
    let (backend, _) = fake_backend(BackendOpts::default()).await;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let text = config(backend, "") + &format!("\nlogging:\n  metrics-bind: \"127.0.0.1:{port}\"\n");
    let path = std::env::temp_dir().join(format!("pumbo-e3-reload-{}.yml", std::process::id()));
    std::fs::write(&path, format!("{text}\nstatus:\n  motd: first\n")).unwrap();
    let (proxy, addr) =
        start_proxy_at(&std::fs::read_to_string(&path).unwrap(), Some(path.clone())).await;
    let status = || async {
        let json = pumbo_testclient::status(addr, module(777)).await.unwrap().1;
        serde_json::from_str::<serde_json::Value>(&json).unwrap()
    };
    let motd = |json: String| {
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        pumbo_text::Component::from_json_value(&v["description"])
            .unwrap()
            .plain_text()
    };
    assert_eq!(
        motd(pumbo_testclient::status(addr, module(777)).await.unwrap().1),
        "first"
    );
    assert!(status().await.get("favicon").is_none());
    // The server icon comes with a reload, without a restart.
    let icon = path.with_extension("png");
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend([0, 0, 0, 64, 0, 0, 0, 64, 8, 6, 0, 0, 0]);
    std::fs::write(&icon, png).unwrap();
    std::fs::write(
        &path,
        format!("{text}\nstatus:\n  motd: second\n  favicon: {icon:?}\n"),
    )
    .unwrap();
    proxy.reload().unwrap();
    assert_eq!(
        motd(pumbo_testclient::status(addr, module(777)).await.unwrap().1),
        "second"
    );
    let favicon = status().await["favicon"].as_str().unwrap().to_string();
    assert!(
        favicon.starts_with("data:image/png;base64,iVBORw0KGgo"),
        "{favicon}"
    );
    // A broken file keeps the old config: a value of the wrong type...
    std::fs::write(&path, "listener:\n  - bind: [5]\n").unwrap();
    assert!(proxy.reload().is_err());
    // ...and a file that is not valid YAML (bad indentation), with its line.
    std::fs::write(
        &path,
        format!("{text}\nstatus:\n    motd: third\n  max-players: 5\n"),
    )
    .unwrap();
    let err = proxy.reload().unwrap_err().to_string();
    assert!(err.contains("line"), "{err}");
    assert_eq!(
        motd(pumbo_testclient::status(addr, module(777)).await.unwrap().1),
        "second"
    );

    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    s.read_to_string(&mut reply).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert!(
        reply.contains("pumboprox_status_total{kind=\"modern\"} 5"),
        "{reply}"
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_file(&icon).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_kicks_players_and_closes_listeners() {
    let (backend, _) = fake_backend(BackendOpts::default()).await;
    let (proxy, addr) = start_proxy(&config(backend, "")).await;
    let mut c = login_to_play(addr, "Leaver").await;
    proxy.stop();
    assert_eq!(
        wait_kick(&mut c, Duration::from_secs(3)).await.as_deref(),
        Some("The proxy is shutting down.")
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(TcpStream::connect(addr).await.is_err(), "listener closed");
}

/// Pumpkin 0.2.0 sends an eyeblossom's `trail` without options (Pumpkin issue
/// #3065); a 26.3 client gets it with vanilla's options, correct packets
/// unchanged, compressed or not (D-COMPAT-1).
#[tokio::test(flavor = "multi_thread")]
async fn pumpkin_trail_particle_is_repaired() {
    let particles = pumbo_data::tables(pumbo_protocol::ProtocolVersion::V777)
        .unwrap()
        .registry("particle_type")
        .unwrap();
    let particle = |name| i32::try_from(particles.id(name).unwrap()).unwrap();
    let (trail, flame) = (particle("trail"), particle("flame"));
    let packet = |particle: i32, options: &[u8]| {
        let mut p = Vec::new();
        p.put_varint(particle);
        p.put_bytes(options);
        p.put_bool(false);
        p.put_bool(false);
        [10.5, 64.5, -3.5].iter().for_each(|v| p.put_f64(*v));
        (0..6).for_each(|_| p.put_f32(0.0));
        p.put_varint(1);
        p.put_varint(0);
        p
    };
    let options = |target: [f64; 3], color: i32, duration: i32| {
        let mut o = Vec::new();
        target.iter().for_each(|v| o.put_f64(*v));
        o.put_i32(color);
        o.put_varint(duration);
        o
    };
    let broken = packet(trail, &[]);
    let repaired = packet(trail, &options([10.5, 66.0, -3.5], 0xFC7812, 20));
    let correct = packet(trail, &options([1.0, 2.0, 3.0], 0x5F5F5F, 7));
    let other = packet(flame, &[]);
    let id = module(777)
        .packet_id(
            Phase::Play,
            Direction::Clientbound,
            PacketKind::LevelParticles,
        )
        .unwrap();
    // 256: the particles go uncompressed; 16: compressed.
    for compression in [256, 16] {
        let sent = [&broken, &correct, &other, &broken];
        let (backend, _) = fake_backend(BackendOpts {
            compression: Some(compression),
            play: sent.iter().map(|p| (id, p.to_vec())).collect(),
            ..BackendOpts::default()
        })
        .await;
        let (proxy, addr) = start_proxy(&config(backend, "")).await;
        let mut c = login_to_play(addr, "Bloom").await;
        let mut got = Vec::new();
        while got.len() < sent.len() {
            let (k, f) = c.recv().await.unwrap();
            if k == Some(PacketKind::LevelParticles) {
                got.push(f.payload.to_vec());
            }
        }
        let want = [&repaired, &correct, &other, &repaired].map(Vec::clone);
        assert_eq!(got, want, "{compression}");
        assert_eq!(
            proxy
                .metrics
                .fixed_trails
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
    }
}
