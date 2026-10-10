//! PumboBridge, proxy side, against a fake bridge and fake backends: the
//! handshake, pairing by status ping, the `pumbo:bridge` service (access,
//! `no-bridge`, `unsupported`, `timeout`, `disconnected`), held teleports,
//! and attacks (wrong key, replayed frame).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pumbo_bridge_proto::api::{self, GameMode};
use pumbo_bridge_proto::wire::{self, Codec, Msg, Side};
use pumbo_bridge_proto::{err, method};
use pumbo_core::profile::GameProfile;
use pumbo_host::wit::services::ServiceError;
use pumbo_prox::bridge::{Bridge, Tp};
use pumbo_prox::config::Config;
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
}

impl FakeBridge {
    /// Connects and runs the handshake; `Err` when the proxy refuses.
    async fn connect(addr: SocketAddr, key: &wire::Key) -> Result<FakeBridge, String> {
        let sock = TcpStream::connect(addr).await.unwrap();
        let mut b = FakeBridge {
            sock,
            codec: Codec::new(Side::Bridge),
            buf: Vec::new(),
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
    );
    let cfg = Config::parse(&text).unwrap();
    let proxy = Proxy::new(cfg.clone(), None).unwrap();
    let bridge = Bridge::new(&proxy, cfg.bridge.clone()).unwrap();
    proxy.bridge.set(bridge.clone()).unwrap();
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
    let (_guard, _rx) = s.proxy.register_player(&profile).unwrap();
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
    let (_guard, _rx) = s.proxy.register_player(&profile).unwrap();
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
    // Chat lines are MiniMessage with the host styles.
    assert!(lines.iter().any(|l| l.contains("<ok>connected</ok>")));
    assert!(lines.iter().any(|l| l.contains("<err>rejected</err>")));

    let key = s.bridge.admin(&["key".into()], false, false);
    assert!(key[0].contains("<err>") && !key.concat().contains(&wire::key_hex(&s.key)));
    let key = s.bridge.admin(&["key".into()], true, true);
    assert!(key.concat().contains(&wire::key_hex(&s.key)));
    drop((lobby, survival));
}
