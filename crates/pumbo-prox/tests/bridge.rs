//! PumboBridge, proxy side, against a fake bridge and fake backends: the
//! handshake, pairing by status ping, the `pumbo:bridge` service (access,
//! `no-bridge`, `unsupported`, `timeout`, `disconnected`), held teleports,
//! attacks (wrong key, replayed frame), and permission sets and exports
//! from the plugin host.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use pumbo_bridge_proto::api::{self, GameMode};
use pumbo_bridge_proto::wire::{self, Codec, Msg, Side};
use pumbo_bridge_proto::{err, method};
use pumbo_core::profile::GameProfile;
use pumbo_host::wit::services::ServiceError;
use pumbo_prox::bridge::{Bridge, Tp};
use pumbo_prox::config::Config;
use pumbo_prox::plugins::Plugins;
use pumbo_prox::server::Proxy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use uuid::Uuid;

const CAPS: &[&str] = &["teleport", "set-gamemode", "q-server", "perm-set"];

/// A backend that only records the host names of status pings.
async fn fake_backend() -> (SocketAddr, mpsc::UnboundedReceiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 512];
                let n = s.read(&mut buf).await.unwrap_or(0);
                if let Some(host) = handshake_host(&buf[..n]) {
                    let _ = tx.send(host);
                }
            });
        }
    });
    (addr, rx)
}

fn varint(b: &[u8], i: &mut usize) -> Option<u32> {
    let mut v = 0u32;
    for shift in (0..35).step_by(7) {
        let x = *b.get(*i)?;
        *i += 1;
        v |= u32::from(x & 0x7f) << shift;
        if x & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn handshake_host(b: &[u8]) -> Option<String> {
    let mut i = 0;
    varint(b, &mut i)?; // length
    varint(b, &mut i)?; // id
    varint(b, &mut i)?; // protocol
    let n = varint(b, &mut i)? as usize;
    String::from_utf8(b.get(i..i + n)?.to_vec()).ok()
}

struct FakeBridge {
    sock: TcpStream,
    codec: Codec,
    buf: Vec<u8>,
    /// Messages read by `alive_during`, for `recv`.
    pending: std::collections::VecDeque<Msg>,
}

impl FakeBridge {
    /// Connects and runs the handshake; `Err` when the proxy refuses.
    async fn connect(addr: SocketAddr, key: &wire::Key) -> Result<FakeBridge, String> {
        Self::handshake(TcpStream::connect(addr).await.unwrap(), key).await
    }

    /// `connect` with a small receive buffer, so the proxy's writes block
    /// soon once the bridge stops reading.
    async fn connect_small(addr: SocketAddr, key: &wire::Key) -> FakeBridge {
        let sock = tokio::net::TcpSocket::new_v4().unwrap();
        sock.set_recv_buffer_size(2048).unwrap();
        Self::handshake(sock.connect(addr).await.unwrap(), key)
            .await
            .unwrap()
    }

    async fn handshake(sock: TcpStream, key: &wire::Key) -> Result<FakeBridge, String> {
        let mut b = FakeBridge {
            sock,
            codec: Codec::new(Side::Bridge),
            buf: Vec::new(),
            pending: std::collections::VecDeque::new(),
        };
        let nb = [7u8; 32];
        let instance = [9u8; 16];
        b.send(&Msg::Hello {
            proto: pumbo_bridge_proto::PROTO,
            bridge: "0.1.0-test".into(),
            pumpkin: "0.2.0".into(),
            mc: "26.3".into(),
            instance,
            nb,
        })
        .await;
        let Some(Msg::Challenge { np, proof }) = b.recv().await else {
            return Err("no challenge".into());
        };
        // A real bridge stops here on a wrong proof (fake proxy); the test
        // bridge with a wrong key goes on to see the proxy refuse it.
        let _proxy_ok = wire::check_proxy_proof(key, &nb, &np, &instance, &proof);
        b.send(&Msg::Auth {
            proof: wire::bridge_proof(key, &np, &nb, &instance),
        })
        .await;
        b.codec.set_key(wire::session_key(key, &nb, &np));
        b.send(&Msg::Info {
            caps: CAPS.iter().map(|s| s.to_string()).collect(),
            catalog: vec!["minecraft:command.gamemode".into()],
        })
        .await;
        Ok(b)
    }

    async fn send(&mut self, m: &Msg) {
        let f = self.codec.encode(m).unwrap();
        let _ = self.sock.write_all(&f).await;
    }

    /// The next message other than a ping; `None` when the proxy closed.
    async fn recv(&mut self) -> Option<Msg> {
        if let Some(m) = self.pending.pop_front() {
            return Some(m);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some((m, n)) = self.codec.decode(&self.buf).ok()? {
                self.buf.drain(..n);
                if m == Msg::Ping {
                    self.send(&Msg::Pong).await;
                    continue;
                }
                return Some(m);
            }
            let r = tokio::time::timeout_at(deadline, self.sock.read_buf(&mut self.buf)).await;
            match r {
                Ok(Ok(n)) if n > 0 => {}
                _ => return None,
            }
        }
    }

    /// The next message as it comes, pings included; `None` when the proxy closed.
    async fn next_raw(&mut self) -> Option<Msg> {
        loop {
            if let Some((m, n)) = self.codec.decode(&self.buf).ok()? {
                self.buf.drain(..n);
                return Some(m);
            }
            match self.sock.read_buf(&mut self.buf).await {
                Ok(n) if n > 0 => {}
                _ => return None,
            }
        }
    }

    /// Runs `work` while answering the proxy's pings, as a real bridge does
    /// from its own task; other messages wait for `recv`. A slow step (a
    /// plugin reload on a busy runner) would otherwise leave the link silent
    /// past the proxy's 15 s, and the proxy drops it.
    async fn alive_during<T>(&mut self, work: impl std::future::Future<Output = T>) -> T {
        tokio::pin!(work);
        loop {
            tokio::select! {
                out = &mut work => return out,
                m = self.next_raw() => match m {
                    Some(Msg::Ping) => self.send(&Msg::Pong).await,
                    Some(m) => self.pending.push_back(m),
                    None => return work.await,
                },
            }
        }
    }

    /// Answers the pairing ping seen by `backend` and waits for `welcome`.
    async fn pair(&mut self, backend: &mut mpsc::UnboundedReceiver<String>) -> String {
        let host = tokio::time::timeout(Duration::from_secs(5), backend.recv())
            .await
            .unwrap()
            .unwrap();
        let nonce = wire::ping_nonce(&host).unwrap().to_string();
        self.send(&Msg::Seen { nonce }).await;
        match self.recv().await {
            Some(Msg::Welcome { server, .. }) => {
                self.send(&Msg::Sync {
                    players: Vec::new(),
                    worlds: Vec::new(),
                })
                .await;
                server
            }
            other => panic!("expected welcome, got {other:?}"),
        }
    }

    /// The next command (skipping `perm-set` and `perms-export` from the proxy).
    async fn command(&mut self) -> Option<(u64, String, ciborium::Value)> {
        loop {
            match self.recv().await? {
                Msg::Cmd { method, .. }
                    if method == method::PERM_SET || method == method::PERMS_EXPORT => {}
                Msg::Cmd { id, method, args } => return Some((id, method, args)),
                _ => {}
            }
        }
    }

    /// The next `perm-set` or `perms-export` (`method`), skipping the rest;
    /// `None` when none comes within `ms`.
    async fn next_of(&mut self, method: &str, ms: u64) -> Option<ciborium::Value> {
        let wait = async {
            loop {
                match self.recv().await? {
                    Msg::Cmd {
                        method: m, args, ..
                    } if m == method => return Some(args),
                    _ => {}
                }
            }
        };
        tokio::time::timeout(Duration::from_millis(ms), wait)
            .await
            .ok()
            .flatten()
    }

    async fn perm_set(&mut self) -> Option<api::PermSet> {
        let v = self.next_of(method::PERM_SET, 2000).await?;
        Some(v.deserialized().unwrap())
    }

    async fn export(&mut self) -> Option<String> {
        let v = self.next_of(method::PERMS_EXPORT, 5000).await?;
        Some(v.deserialized::<api::PermsExport>().unwrap().data)
    }

    async fn join(&mut self, player: Uuid) {
        self.send(&Msg::Ev {
            ev: wire::Event::Join {
                player,
                name: "Steve".into(),
                pos: api::Pos {
                    world: "minecraft:overworld".into(),
                    x: 0.0,
                    y: 64.0,
                    z: 0.0,
                    yaw: None,
                    pitch: None,
                },
            },
        })
        .await;
    }

    async fn ok(&mut self, id: u64) {
        self.send(&Msg::Res {
            id,
            ok: Some(ciborium::Value::Null),
            err: None,
            detail: None,
        })
        .await;
    }
}

struct Setup {
    proxy: Arc<Proxy>,
    bridge: Arc<Bridge>,
    addr: SocketAddr,
    key: wire::Key,
    lobby: mpsc::UnboundedReceiver<String>,
    survival: mpsc::UnboundedReceiver<String>,
}

async fn setup(tag: &str, timeout_ms: u64) -> Setup {
    setup_with(tag, timeout_ms, None).await
}

/// `setup` with the plugin host started from the same config, as the proxy
/// does: `extra` adds to the config (`plugins:`, `permissions:`).
async fn setup_with(tag: &str, timeout_ms: u64, extra: Option<&str>) -> Setup {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-bridge-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key_file = dir.join("bridge.key");
    let _ = std::fs::remove_file(&key_file);
    let (lobby_addr, lobby) = fake_backend().await;
    let (survival_addr, survival) = fake_backend().await;
    let (creative_addr, _creative) = fake_backend().await;
    let text = format!(
        r#"
listener:
  - bind: "127.0.0.1:0"
forwarding:
  mode: none
servers:
  lobby: {{ address: "{lobby_addr}" }}
  survival: {{ address: "{survival_addr}" }}
  creative: {{ address: "{creative_addr}" }}
bridge:
  enabled: true
  listen: "127.0.0.1:0"
  key-file: {key:?}
  command-timeout-ms: {timeout_ms}
"#,
        key = key_file.display().to_string()
    ) + extra.unwrap_or_default();
    let path = dir.join("pumboprox.yml");
    std::fs::write(&path, &text).unwrap();
    let cfg = Config::parse(&text).unwrap();
    let proxy = Proxy::new(cfg.clone(), Some(path)).unwrap();
    let bridge = Bridge::new(&proxy, cfg.bridge.clone()).unwrap();
    proxy.bridge.set(bridge.clone()).unwrap();
    if extra.is_some() {
        let plugins = Plugins::start(&text, &proxy, vec![bridge.native()])
            .await
            .unwrap()
            .unwrap();
        assert!(proxy.plugins.set(plugins).is_ok());
    }
    let addr = bridge.start().await.unwrap();
    let key = wire::parse_key(&std::fs::read_to_string(&key_file).unwrap()).unwrap();
    Setup {
        proxy,
        bridge,
        addr,
        key,
        lobby,
        survival,
    }
}

async fn call<T: serde::Serialize>(
    b: &Arc<Bridge>,
    caller: &str,
    m: &str,
    args: &T,
) -> Result<Vec<u8>, ServiceError> {
    let mut payload = Vec::new();
    ciborium::into_writer(args, &mut payload).unwrap();
    b.native()
        .provider
        .call(caller.into(), m.into(), payload)
        .await
}

fn rejected(r: &Result<Vec<u8>, ServiceError>) -> String {
    match r {
        Err(ServiceError::Rejected(c)) => c.clone(),
        other => format!("{other:?}"),
    }
}

/// The log of this test binary so far.
fn logs() -> String {
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
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    let log = LOG.get_or_init(|| {
        let log = Arc::new(Mutex::new(Vec::new()));
        let w = Arc::clone(&log);
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Buf(Arc::clone(&w)))
            .init();
        log
    });
    String::from_utf8_lossy(&log.lock().unwrap()).into_owned()
}

/// Waits up to 2 s for a line containing `text` in the log.
async fn logged(text: &str) -> bool {
    for _ in 0..100 {
        if logs().contains(text) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

fn gamemode(server: &str) -> api::SetGamemode {
    api::SetGamemode {
        player: Uuid::from_u128(1),
        mode: GameMode::Creative,
        server: Some(server.into()),
    }
}

#[tokio::test]
async fn pairs_by_ping_and_serves_plugins() {
    let mut s = setup("pair", 5000).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    assert_eq!(fb.pair(&mut s.lobby).await, "lobby");
    let status = s.bridge.status(None);
    // Servers in name order: creative, lobby, survival.
    assert_eq!(status[1].state, api::State::Connected, "{status:?}");
    assert_eq!(status[1].bridge, "0.1.0-test");
    assert_eq!(status[2].state, api::State::None, "survival has no bridge");

    // A command goes to the bridge and its answer back to the plugin.
    let b = s.bridge.clone();
    let task = tokio::spawn(async move {
        call(&b, "pumbo-core", method::SET_GAMEMODE, &gamemode("lobby")).await
    });
    let (id, m, args) = fb.command().await.unwrap();
    assert_eq!(m, method::SET_GAMEMODE);
    let got: api::SetGamemode = args.deserialized().unwrap();
    assert_eq!(got.mode, GameMode::Creative);
    fb.ok(id).await;
    assert!(task.await.unwrap().is_ok());

    // An error code of the bridge reaches the plugin.
    let b = s.bridge.clone();
    let task = tokio::spawn(async move {
        call(&b, "pumbo-core", method::SET_GAMEMODE, &gamemode("lobby")).await
    });
    let (id, _, _) = fb.command().await.unwrap();
    fb.send(&Msg::Res {
        id,
        ok: None,
        err: Some(err::NO_PLAYER.into()),
        detail: None,
    })
    .await;
    assert_eq!(rejected(&task.await.unwrap()), err::NO_PLAYER);

    let r = call(
        &s.bridge,
        "pumbo-core",
        method::SET_GAMEMODE,
        &gamemode("survival"),
    )
    .await;
    assert_eq!(rejected(&r), err::NO_BRIDGE, "no bridge there: at once");
    let r = call(&s.bridge, "other", method::SET_GAMEMODE, &gamemode("lobby")).await;
    assert_eq!(
        rejected(&r),
        err::NOT_ALLOWED,
        "commands: trusted plugins only"
    );
    let r = call(
        &s.bridge,
        "other",
        method::PERM_SET,
        &api::PermSet::default(),
    )
    .await;
    assert_eq!(rejected(&r), err::NOT_ALLOWED, "perm-set is the proxy's");
    let r = call(&s.bridge, "other", "nonsense", &()).await;
    assert_eq!(r, Err(ServiceError::UnknownMethod));
    let fly = api::Fly {
        player: Uuid::from_u128(1),
        allow: true,
        flying: None,
        speed: None,
        server: Some("lobby".into()),
    };
    let r = call(&s.bridge, "pumbo-core", method::FLY, &fly).await;
    assert_eq!(rejected(&r), err::UNSUPPORTED, "not in the bridge's caps");

    // Queries are open to any plugin that uses the service.
    let b = s.bridge.clone();
    let task = tokio::spawn(async move {
        let q = api::QServer {
            server: Some("lobby".into()),
        };
        call(&b, "other", method::Q_SERVER, &q).await
    });
    let (id, m, _) = fb.command().await.unwrap();
    assert_eq!(m, method::Q_SERVER);
    let info = api::ServerInfo {
        tps: 20.0,
        mspt: 3.5,
        players: 0,
        worlds: Vec::new(),
    };
    fb.send(&Msg::Res {
        id,
        ok: Some(wire::value(&info).unwrap()),
        err: None,
        detail: None,
    })
    .await;
    let out = task.await.unwrap().unwrap();
    let back: api::ServerInfo = ciborium::from_reader(out.as_slice()).unwrap();
    assert_eq!(back, info);
    let st = call(&s.bridge, "other", method::STATUS, &api::Status::default())
        .await
        .unwrap();
    let st: Vec<api::ServerStatus> = ciborium::from_reader(st.as_slice()).unwrap();
    assert_eq!(st.len(), 3);
    drop(s.proxy);
}

#[tokio::test]
async fn refuses_a_wrong_key_and_a_replayed_frame() {
    let mut s = setup("attack", 5000).await;
    let mut bad = FakeBridge::connect(s.addr, &[0x5a; 32]).await.unwrap();
    assert!(bad.recv().await.is_none(), "wrong key: the proxy closes");
    assert_eq!(s.bridge.status(Some("lobby"))[0].state, api::State::None);

    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    assert_eq!(fb.pair(&mut s.lobby).await, "lobby");
    let frame = fb.codec.encode(&Msg::Pong).unwrap();
    fb.sock.write_all(&frame).await.unwrap();
    fb.sock.write_all(&frame).await.unwrap();
    assert!(fb.recv().await.is_none(), "replayed frame: session ends");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.bridge.status(Some("lobby"))[0].state, api::State::Lost);
    let r = call(
        &s.bridge,
        "pumbo-core",
        method::SET_GAMEMODE,
        &gamemode("lobby"),
    )
    .await;
    assert_eq!(rejected(&r), err::NO_BRIDGE);

    // A bridge that never matches a server is closed after three rounds is
    // too slow for a test; a client on the bridge port without the
    // handshake is dropped after 5 s.
    let mut idle = TcpStream::connect(s.addr).await.unwrap();
    let mut b = [0u8; 1];
    let r = tokio::time::timeout(Duration::from_secs(8), idle.read(&mut b)).await;
    assert!(matches!(r, Ok(Ok(0))), "{r:?}");
}

#[tokio::test]
async fn timeout_disconnect_and_reconnect() {
    let mut s = setup("timeout", 300).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    fb.pair(&mut s.lobby).await;
    let r = call(
        &s.bridge,
        "pumbo-core",
        method::SET_GAMEMODE,
        &gamemode("lobby"),
    )
    .await;
    assert_eq!(rejected(&r), err::TIMEOUT);
    let _late = fb.command().await.unwrap();

    let b = s.bridge.clone();
    let task = tokio::spawn(async move {
        call(&b, "pumbo-core", method::SET_GAMEMODE, &gamemode("lobby")).await
    });
    let _ = fb.command().await.unwrap();
    drop(fb);
    assert_eq!(rejected(&task.await.unwrap()), err::DISCONNECTED);

    // The bridge comes back (new connection, new nonces) and pairs again.
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    while s.lobby.try_recv().is_ok() {}
    assert_eq!(fb.pair(&mut s.lobby).await, "lobby");
    let b = s.bridge.clone();
    let task = tokio::spawn(async move {
        call(&b, "pumbo-core", method::SET_GAMEMODE, &gamemode("lobby")).await
    });
    let (id, _, _) = fb.command().await.unwrap();
    fb.ok(id).await;
    assert!(task.await.unwrap().is_ok());
}

/// A bridge that stops answering (its server froze) is closed after 15 s,
/// and the log says why. Takes 15 to 20 s.
#[tokio::test]
async fn silent_bridge_is_closed_with_the_reason() {
    logs();
    let mut s = setup("silent", 5000).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    assert_eq!(fb.pair(&mut s.survival).await, "survival");
    // From here on the bridge reads nothing and sends nothing.
    let mut rest = Vec::new();
    let end = tokio::time::timeout(Duration::from_secs(25), fb.sock.read_to_end(&mut rest)).await;
    assert!(end.is_ok(), "the proxy closes a silent bridge");
    assert!(
        logged("bridge on survival closed: silent for 15 s").await,
        "{}",
        logs()
    );
    assert_eq!(s.bridge.status(Some("survival"))[0].state, api::State::Lost);
}

#[tokio::test]
async fn teleports_wait_for_the_client() {
    let mut s = setup("tp", 5000).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    fb.pair(&mut s.lobby).await;
    let player = Uuid::from_u128(42);
    let profile = GameProfile {
        id: player,
        name: "Steve".into(),
        properties: Vec::new(),
    };
    let (_guard, _rx) = s.proxy.register_player(&profile, s.addr).unwrap();
    let gate = s.proxy.teleport_gate(player).unwrap();
    gate.send_replace(Tp::Awaiting(3));
    let tp = |x: f64| api::Teleport {
        player,
        to: api::Target::Pos(api::Pos {
            world: "minecraft:overworld".into(),
            x,
            y: 64.0,
            z: 0.0,
            yaw: None,
            pitch: None,
        }),
        server: Some("lobby".into()),
    };
    let (b1, b2) = (s.bridge.clone(), s.bridge.clone());
    let (t1, t2) = (tp(1.0), tp(2.0));
    let first = tokio::spawn(async move { call(&b1, "pumbo-core", method::TELEPORT, &t1).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let second = tokio::spawn(async move { call(&b2, "pumbo-core", method::TELEPORT, &t2).await });
    assert_eq!(rejected(&first.await.unwrap()), err::SUPERSEDED);
    // Nothing reached the bridge while the client had a teleport to confirm.
    let early = tokio::time::timeout(Duration::from_millis(300), fb.command()).await;
    assert!(early.is_err(), "held: {early:?}");
    gate.send_replace(Tp::Confirmed(tokio::time::Instant::now()));
    let (id, m, args) = fb.command().await.unwrap();
    assert_eq!(m, method::TELEPORT);
    let got: api::Teleport = args.deserialized().unwrap();
    assert_eq!(got, tp(2.0));
    assert!(
        matches!(*gate.borrow(), Tp::Sent(_)),
        "the next one waits for its position"
    );
    fb.ok(id).await;
    assert!(second.await.unwrap().is_ok());
}

/// Takes the full 10 s of the hold.
#[tokio::test]
async fn a_held_teleport_expires() {
    let mut s = setup("expire", 60_000).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    fb.pair(&mut s.lobby).await;
    let player = Uuid::from_u128(43);
    let profile = GameProfile {
        id: player,
        name: "Alex".into(),
        properties: Vec::new(),
    };
    let (_guard, _rx) = s.proxy.register_player(&profile, s.addr).unwrap();
    s.proxy
        .teleport_gate(player)
        .unwrap()
        .send_replace(Tp::Awaiting(1));
    let t = api::Teleport {
        player,
        to: api::Target::Spawn(None),
        server: Some("lobby".into()),
    };
    let r = call(&s.bridge, "pumbo-core", method::TELEPORT, &t).await;
    assert_eq!(rejected(&r), err::EXPIRED);
}

/// `/pumbo bridge`: one row per server; two with a bridge, one whose bridge
/// has a wrong key; `key` only with the right.
#[tokio::test]
async fn status_table() {
    let mut s = setup("table", 5000).await;
    let mut lobby = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    assert_eq!(lobby.pair(&mut s.lobby).await, "lobby");
    let mut survival = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    while s.survival.try_recv().is_ok() {}
    assert_eq!(survival.pair(&mut s.survival).await, "survival");
    let mut bad = FakeBridge::connect(s.addr, &[1; 32]).await.unwrap();
    assert!(bad.recv().await.is_none());

    let lines = s.bridge.admin(&[], true, false);
    let plain: Vec<String> = lines.iter().map(|l| pumbo_prox::bridge::plain(l)).collect();
    assert_eq!(
        plain.len(),
        5,
        "title, column names, three servers: {plain:#?}"
    );
    assert!(plain[1].starts_with("server"), "{plain:#?}");
    let row = |name: &str| plain.iter().find(|l| l.starts_with(name)).unwrap().clone();
    assert!(row("creative").contains("rejected"), "{plain:#?}");
    assert!(row("creative").contains("key"), "the reason: {plain:#?}");
    assert!(row("lobby").contains("connected") && row("lobby").contains("0.1.0-test / 1.0"));
    assert!(row("survival").contains("connected"));
    // Console columns line up.
    let col = |l: &str| l.find("connected").or_else(|| l.find("rejected")).unwrap();
    assert_eq!(col(&row("lobby")), col(&row("creative")));
    // Chat lines are text tags with the host styles.
    assert!(lines.iter().any(|l| l.contains("<ok>connected</ok>")));
    assert!(lines.iter().any(|l| l.contains("<err>rejected</err>")));

    let key = s.bridge.admin(&["key".into()], false, false);
    assert!(key[0].contains("<err>") && !key.concat().contains(&wire::key_hex(&s.key)));
    let key = s.bridge.admin(&["key".into()], true, true);
    assert!(key.concat().contains(&wire::key_hex(&s.key)));
    drop((lobby, survival));
}

// ------------------------------------------------------------ permissions

const STEVE: Uuid = Uuid::from_u128(0x5e7e);

/// A folder with `permissions.yml` (Steve may change his game mode) and a
/// plugin folder, with the test plugin of pumbo-host as the permission
/// provider when `provider`; returns the config lines for them.
fn host_files(tag: &str, provider: bool) -> (PathBuf, String) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-bridge-host-{tag}-{}", std::process::id()));
    let plugins = dir.join("plugins");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(
        dir.join("permissions.yml"),
        format!("players:\n  {STEVE}:\n    permissions: [\"minecraft:command.gamemode\"]\n"),
    )
    .unwrap();
    if provider {
        std::fs::write(
            plugins.join("perm.yml"),
            "id: perm\nversion: 0.1.0\napi: \"0.1\"\npermission-provider: true\nprovides:\n  - { service: \"pumbo:permissions\", version: \"1.0\" }\n",
        )
        .unwrap();
        std::fs::copy(test_plugin(), plugins.join("perm.wasm")).unwrap();
    }
    let extra = format!(
        "plugins:\n  dir: {:?}\npermissions:\n  file: {:?}\n  on-load-failure: file-only\n  provider: {}\n",
        plugins.display().to_string(),
        dir.join("permissions.yml").display().to_string(),
        if provider { "perm" } else { "file" }
    );
    (dir, extra)
}

/// The test plugin of pumbo-host, built once for wasm32-wasip2.
fn test_plugin() -> PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target/wasm-plugins");
        let ok = std::process::Command::new(env!("CARGO"))
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
    })
    .clone()
}

/// Steve logged in to the proxy (the host knows him).
async fn steve(s: &Setup) -> pumbo_prox::server::PlayerGuard {
    let profile = GameProfile {
        id: STEVE,
        name: "Steve".into(),
        properties: Vec::new(),
    };
    let (guard, _rx) = s.proxy.register_player(&profile, s.addr).unwrap();
    let conn = pumbo_prox::plugins::connection(s.addr, "localhost", 777);
    s.proxy
        .plugins
        .get()
        .unwrap()
        .joined(profile, false, conn)
        .await
        .unwrap();
    guard
}

fn gamemode_set() -> std::collections::BTreeMap<String, bool> {
    [("minecraft:command.gamemode".to_string(), true)].into()
}

/// The bridge keeps a set that came before the player at most 30 s; a login
/// that takes longer finds none. The proxy cannot see that, so the join
/// reported by the bridge always brings the set again.
#[tokio::test]
async fn join_after_pending_expiry_resends_set() {
    let (_dir, extra) = host_files("join", false);
    let mut s = setup_with("join", 5000, Some(&extra)).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    fb.pair(&mut s.lobby).await;
    let _guard = steve(&s).await;
    s.bridge.connecting(STEVE, "lobby");
    let first = fb.perm_set().await.unwrap();
    assert_eq!(first.nodes, gamemode_set());
    fb.join(STEVE).await;
    let again = fb.perm_set().await.expect("a set on join");
    assert_eq!(again.nodes, gamemode_set());
    // A change of nothing sends nothing.
    s.bridge.perms_changed(STEVE, Some("lobby"));
    assert!(fb.perm_set().await.is_none());
}

/// A login to the backend that failed (no join, no leave), then another
/// attempt: each one sends the set, and so does the join.
#[tokio::test]
async fn reconnect_after_failed_login_resends_set() {
    let (_dir, extra) = host_files("relogin", false);
    let mut s = setup_with("relogin", 5000, Some(&extra)).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    fb.pair(&mut s.lobby).await;
    let _guard = steve(&s).await;
    s.bridge.connecting(STEVE, "lobby");
    assert_eq!(fb.perm_set().await.unwrap().nodes, gamemode_set());
    s.bridge.connecting(STEVE, "lobby");
    assert_eq!(
        fb.perm_set()
            .await
            .expect("a set for the second attempt")
            .nodes,
        gamemode_set()
    );
    fb.join(STEVE).await;
    assert!(fb.perm_set().await.is_some());
}

/// A provider that is down or does not answer leaves the info the bridge
/// has: a set sent meanwhile carries the last one.
#[tokio::test(flavor = "multi_thread")]
async fn provider_down_keeps_last_info() {
    let (_dir, extra) = host_files("info-down", true);
    let mut s = setup_with("info-down", 5000, Some(&extra)).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    fb.pair(&mut s.lobby).await;
    let _guard = steve(&s).await;
    s.bridge.connecting(STEVE, "lobby");
    let info = loop {
        let set = fb.perm_set().await.expect("a set with the info");
        if set.info.is_some() {
            break set.info;
        }
    };
    assert_eq!(info.as_deref(), Some("info lobby"));
    let host = s.proxy.plugins.get().unwrap().host.clone();
    fb.alive_during(host.plugin("perm").unwrap().unload())
        .await
        .unwrap();
    s.bridge.connecting(STEVE, "lobby");
    assert_eq!(fb.perm_set().await.expect("the forced set").info, info);
    assert!(fb.perm_set().await.is_none(), "the info was dropped");
}

/// `permissions.yml` read again (`reload` without a permission provider):
/// every bridge gets the new export.
#[tokio::test]
async fn export_resent_after_file_reload() {
    let (dir, extra) = host_files("file-reload", false);
    let mut s = setup_with("file-reload", 5000, Some(&extra)).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    fb.pair(&mut s.lobby).await;
    let first = fb.export().await.unwrap();
    assert!(
        first.contains("minecraft:command.gamemode") && !first.contains("minecraft:command.kick")
    );
    std::fs::write(
        dir.join("permissions.yml"),
        format!("players:\n  {STEVE}:\n    permissions: [\"minecraft:command.kick\"]\n"),
    )
    .unwrap();
    s.proxy.reload().unwrap();
    let second = fb.export().await.expect("an export after the reload");
    assert!(second.contains("minecraft:command.kick"), "{second}");
}

/// The permission provider stops and starts: every bridge gets the export
/// again, and it stays `proxy-rules` while the provider is down.
#[tokio::test(flavor = "multi_thread")]
async fn export_resent_on_provider_state_change() {
    let (_dir, extra) = host_files("provider-state", true);
    let mut s = setup_with("provider-state", 5000, Some(&extra)).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    fb.pair(&mut s.lobby).await;
    let rules = |x: &str| {
        let v: serde_json::Value = serde_json::from_str(x).unwrap();
        v["proxy-rules"] == serde_json::json!(true)
    };
    assert!(rules(&fb.export().await.unwrap()));
    let host = s.proxy.plugins.get().unwrap().host.clone();
    fb.alive_during(host.plugin("perm").unwrap().unload())
        .await
        .unwrap();
    assert!(rules(&fb.export().await.expect("an export when it stops")));
    fb.alive_during(host.reload_plugin("perm")).await.unwrap();
    assert!(rules(
        &fb.export().await.expect("an export when it runs again")
    ));
}

/// The permission provider reloads while Steve is on lobby: the bridge stays
/// connected and gets Steve's new set, and a bridge that comes back gets it
/// with `sync`.
#[tokio::test(flavor = "multi_thread")]
async fn provider_reload_with_a_player_keeps_the_bridge() {
    let (dir, extra) = host_files("reload-player", true);
    let provider_gives = |node: &str| {
        let d = dir.join("plugins/perm");
        std::fs::create_dir_all(&d).unwrap();
        let cfg = format!("perms:\n  - {{ node: \"{node}\", value: true, ctx: global }}\n");
        std::fs::write(d.join("config.yml"), cfg).unwrap();
    };
    let only = |node: &str| std::collections::BTreeMap::from([(node.to_string(), true)]);
    provider_gives("minecraft:command.kick");
    let mut s = setup_with("reload-player", 5000, Some(&extra)).await;
    let mut fb = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    fb.pair(&mut s.lobby).await;
    let _guard = steve(&s).await;
    let plugins = s.proxy.plugins.get().unwrap().clone();
    s.proxy.set_server(STEVE, Some("lobby"));
    assert!(plugins.gates(STEVE).await.is_ok());
    // What the play session does when the player's permissions change.
    let signal = s.proxy.commands_signal(STEVE).unwrap();
    let (proxy, bridge) = (s.proxy.clone(), s.bridge.clone());
    tokio::spawn(async move {
        loop {
            signal.notified().await;
            bridge.perms_changed(STEVE, proxy.server_of(STEVE).as_deref());
        }
    });
    s.bridge.connecting(STEVE, "lobby");
    fb.join(STEVE).await;
    loop {
        let set = fb.perm_set().await.expect("Steve's set from the provider");
        if set.nodes == only("minecraft:command.kick") {
            break;
        }
    }

    provider_gives("minecraft:command.ban");
    fb.alive_during(plugins.host.reload_plugin("perm"))
        .await
        .unwrap();
    loop {
        let set = fb.perm_set().await.expect("the new set after the reload");
        if set.nodes == only("minecraft:command.ban") {
            break;
        }
    }
    // A ping round trip later the session is still up.
    let _ = fb.next_of(method::PERM_SET, 6000).await;
    assert_eq!(
        s.bridge.status(Some("lobby"))[0].state,
        api::State::Connected
    );

    drop(fb);
    while s.lobby.try_recv().is_ok() {}
    let mut back = FakeBridge::connect(s.addr, &s.key).await.unwrap();
    back.pair(&mut s.lobby).await;
    let set = back.perm_set().await.expect("Steve's set with sync");
    assert_eq!(set.nodes, only("minecraft:command.ban"));
}

/// Fills the proxy's queue to the bridge, which reads nothing: commands go
/// out until the writes block and the queue is full (a call is `busy`).
async fn fill_queue(s: &Setup) {
    let busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for _ in 0..200 {
        for _ in 0..100 {
            let (b, busy) = (s.bridge.clone(), busy.clone());
            tokio::spawn(async move {
                let r = call(&b, "pumbo-core", method::SET_GAMEMODE, &gamemode("lobby")).await;
                if rejected(&r) == err::BUSY {
                    busy.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        if busy.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
    }
    panic!("the queue to the bridge never filled");
}

/// A bridge that stops reading (its server hangs in a write) blocks the
/// proxy's write; after 10 s the session ends with the reason. Takes 10 to 15 s.
#[tokio::test]
async fn bridge_that_reads_nothing_is_closed() {
    logs();
    let mut s = setup("stuck", 60_000).await;
    let mut fb = FakeBridge::connect_small(s.addr, &s.key).await;
    assert_eq!(fb.pair(&mut s.lobby).await, "lobby");
    fill_queue(&s).await;
    let mut lost = false;
    for _ in 0..100 {
        if s.bridge.status(Some("lobby"))[0].state == api::State::Lost {
            lost = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(lost, "the proxy closes a bridge that reads nothing");
    assert!(
        logged("bridge on lobby closed: write timed out").await,
        "{}",
        logs()
    );
}

/// A set that did not fit in the full queue does not count as sent: once the
/// bridge reads again, the next change sends the whole set.
#[tokio::test(flavor = "multi_thread")]
async fn set_dropped_by_a_full_queue_is_sent_again() {
    let (_dir, extra) = host_files("fullqueue", false);
    let mut s = setup_with("fullqueue", 5000, Some(&extra)).await;
    let mut fb = FakeBridge::connect_small(s.addr, &s.key).await;
    fb.pair(&mut s.lobby).await;
    let _guard = steve(&s).await;
    fill_queue(&s).await;
    s.bridge.connecting(STEVE, "lobby");
    // The bridge reads again and gets everything that was queued.
    while fb.next_of(method::PERM_SET, 500).await.is_some() {}
    s.bridge.perms_changed(STEVE, Some("lobby"));
    let set = fb.perm_set().await.expect("the set again");
    assert_eq!(set.nodes, gamemode_set());
}
