//! E4 end-to-end tests without external servers: `pumbo-testclient`'s
//! `Player` through the proxy to scripted backends (`forwarding = none` on
//! 127.0.0.1). Server switching, fallback, reconnect, keep-alives of a slow
//! next server, proxy commands and the command tree, chat cancelled with a
//! replacement `chat_ack`, plugin messages and `bungeecord:main`, resource
//! packs, cleanup before a switch, forced hosts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{module, start_proxy};
use pumbo_nbt::Tag;
use pumbo_protocol::frame::FrameConfig;
use pumbo_protocol::packets::commands::{Commands, FLAG_EXECUTABLE, NODE_LITERAL, NODE_ROOT, Node};
use pumbo_protocol::packets::common::{
    ClientboundCustomPayload, Disconnect, KeepAlive, ResourcePackPush, ResourcePackResponse,
    ServerboundCustomPayload,
};
use pumbo_protocol::packets::configuration::FinishConfiguration;
use pumbo_protocol::packets::login::{
    LoginAcknowledged, LoginCompression, LoginFinished, LoginStart,
};
use pumbo_protocol::packets::play::{
    BossAction, BossEvent, BundleDelimiter, Chat, ChatAck, ChatCommand, ChatCommandSigned,
    DialogRef, Login, ShowDialog, TabList,
};
use pumbo_protocol::packets::status::Intention;
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::types::{Reader, WriteExt};
use pumbo_protocol::{Direction, PacketKind, Phase, VersionModule};
use pumbo_prox::bungee;
use pumbo_prox::conn::Conn;
use pumbo_prox::server::ChatInput;
use pumbo_testclient::player::saw;
use pumbo_testclient::{JoinOptions, Player};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use uuid::Uuid;

const PROTOCOL: i32 = 777;
const WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- backend

/// What a scripted backend sends.
#[derive(Debug, Clone, Default)]
struct Spec {
    name: &'static str,
    compression: Option<i32>,
    /// Resource pack pushed in configuration (id, hash).
    pack: Option<(Uuid, &'static str)>,
    /// Bytes of a plugin message sent in configuration.
    config_bulk: usize,
    /// Keep-alives every 300 ms in configuration for this long; each must
    /// be answered within 1 s or the backend kicks.
    slow_config: Duration,
    /// Literal commands of its tree; `None` sends no tree.
    commands: Option<Vec<&'static str>>,
    /// Sent in play after login.
    play_extras: bool,
}

/// Commands from the test to the backend's latest connection.
#[derive(Debug)]
enum Ctl {
    Kick(&'static str),
    Close,
    Payload(&'static str, Vec<u8>),
    Bundle,
}

#[derive(Debug, Default)]
struct Log {
    joins: usize,
    online: HashSet<Uuid>,
    duplicates: usize,
    client_information: usize,
    brands: Vec<String>,
    chats: Vec<String>,
    acks: Vec<i32>,
    commands: Vec<String>,
    signed: Vec<String>,
    payloads: Vec<(String, Vec<u8>)>,
    pack_statuses: Vec<(Uuid, i32)>,
    missed_keep_alives: usize,
    config_keep_alives: usize,
    ctl: Vec<mpsc::UnboundedSender<Ctl>>,
}

#[derive(Clone)]
struct Backend {
    addr: SocketAddr,
    log: Arc<Mutex<Log>>,
}

impl Backend {
    async fn start(spec: Spec) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let l = log.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (spec, log) = (spec.clone(), l.clone());
                tokio::spawn(async move {
                    let mut id = None;
                    let r = serve(stream, &spec, &log, &mut id).await;
                    if let Some(id) = id {
                        log.lock().unwrap().online.remove(&id);
                    }
                    let _ = r;
                });
            }
        });
        Self { addr, log }
    }

    fn ctl(&self, c: Ctl) {
        let tx = self.log.lock().unwrap().ctl.last().cloned().unwrap();
        tx.send(c).unwrap();
    }

    fn with<T>(&self, f: impl FnOnce(&Log) -> T) -> T {
        f(&self.log.lock().unwrap())
    }
}

fn send<P: Packet>(c: &mut Conn, m: &dyn VersionModule, phase: Phase, p: &P) {
    let id = m.packet_id(phase, Direction::Clientbound, P::KIND).unwrap();
    let payload = packets::encode(p, &Ctx::new(m, Direction::Clientbound)).unwrap();
    c.queue(id, &payload).unwrap();
}

fn decode<P: Packet>(m: &dyn VersionModule, payload: &[u8]) -> P {
    packets::decode(payload, &Ctx::new(m, Direction::Serverbound)).unwrap()
}

fn text(s: &str) -> Tag {
    Tag::String(s.into())
}

fn tree(names: &[&str]) -> Commands {
    let mut nodes = vec![Node {
        flags: NODE_ROOT,
        children: Vec::new(),
        redirect: None,
        name: None,
        parser: None,
        suggestions: None,
    }];
    for n in names {
        let at = i32::try_from(nodes.len()).unwrap();
        nodes[0].children.push(at);
        nodes.push(Node {
            flags: NODE_LITERAL | FLAG_EXECUTABLE,
            children: Vec::new(),
            redirect: None,
            name: Some((*n).into()),
            parser: None,
            suggestions: None,
        });
    }
    Commands { nodes, root: 0 }
}

/// Records one serverbound packet; returns `false` on the client's
/// `finish_configuration` in configuration.
fn record(m: &dyn VersionModule, phase: Phase, id: i32, payload: &[u8], log: &Mutex<Log>) -> bool {
    let mut l = log.lock().unwrap();
    match m.packet_kind(phase, Direction::Serverbound, id) {
        Some(PacketKind::ClientInformation) => l.client_information += 1,
        Some(PacketKind::CustomPayload) => {
            let p: ServerboundCustomPayload = decode(m, payload);
            if p.channel == "minecraft:brand" {
                l.brands.push(Reader::new(&p.data).string(64).unwrap());
            } else {
                l.payloads.push((p.channel, p.data));
            }
        }
        Some(PacketKind::ResourcePack) => {
            let r: ResourcePackResponse = decode(m, payload);
            l.pack_statuses.push((r.id, r.result));
        }
        Some(PacketKind::Chat) => {
            let c: Chat = decode(m, payload);
            l.chats.push(c.message);
        }
        Some(PacketKind::ChatAck) => {
            let a: ChatAck = decode(m, payload);
            l.acks.push(a.offset);
        }
        Some(PacketKind::ChatCommand) => {
            let c: ChatCommand = decode(m, payload);
            l.commands.push(c.command);
        }
        Some(PacketKind::ChatCommandSigned) => {
            let c: ChatCommandSigned = decode(m, payload);
            l.signed.push(c.command);
        }
        Some(PacketKind::FinishConfiguration) if phase == Phase::Configuration => return false,
        _ => {}
    }
    true
}

async fn serve(
    stream: TcpStream,
    spec: &Spec,
    log: &Arc<Mutex<Log>>,
    online_id: &mut Option<Uuid>,
) -> Result<(), String> {
    let e = |e: pumbo_prox::conn::ConnError| e.to_string();
    let mut c = Conn::new(
        Box::pin(stream),
        FrameConfig::from_client(),
        FrameConfig::from_backend(),
    );
    let (f, _) = c.read_frame().await.map_err(e)?;
    let hs: Intention = decode(&*module(PROTOCOL), &f.payload);
    let m = module(hs.protocol);
    let m = &*m;
    let (f, _) = c.read_frame().await.map_err(e)?;
    let start: LoginStart = decode(m, &f.payload);
    // Like vanilla, a login waits for an old connection of the same player
    // to go; one still there after a second is a duplicate.
    let mut duplicate = true;
    for _ in 0..20 {
        if log.lock().unwrap().online.insert(start.uuid) {
            duplicate = false;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if duplicate {
        log.lock().unwrap().duplicates += 1;
    }
    *online_id = Some(start.uuid);
    if let Some(t) = spec.compression {
        send(&mut c, m, Phase::Login, &LoginCompression { threshold: t });
        c.flush().await.map_err(e)?;
        c.set_compression(t);
    }
    send(
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
    let _: LoginAcknowledged = decode(m, &f.payload);
    let cfg = Phase::Configuration;
    let mut brand = Vec::new();
    brand.put_string(spec.name, 64).unwrap();
    send(
        &mut c,
        m,
        cfg,
        &ClientboundCustomPayload {
            channel: "minecraft:brand".into(),
            data: brand,
        },
    );
    if let Some((id, hash)) = spec.pack {
        send(
            &mut c,
            m,
            cfg,
            &ResourcePackPush {
                id,
                url: format!("https://example.invalid/{hash}.zip"),
                hash: hash.into(),
                forced: false,
                prompt: None,
            },
        );
    }
    if spec.config_bulk > 0 {
        send(
            &mut c,
            m,
            cfg,
            &ClientboundCustomPayload {
                channel: "pumbo:bulk".into(),
                data: vec![7; spec.config_bulk],
            },
        );
    }
    c.flush().await.map_err(e)?;
    // Slow configuration: keep-alives that must be answered.
    let until = tokio::time::Instant::now() + spec.slow_config;
    let mut pending: Option<(i64, tokio::time::Instant)> = None;
    let mut next_id = 1000;
    let mut next_at = tokio::time::Instant::now();
    while tokio::time::Instant::now() < until {
        if pending.is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(1)) {
            log.lock().unwrap().missed_keep_alives += 1;
            send(
                &mut c,
                m,
                cfg,
                &Disconnect {
                    reason: text("Timed out"),
                },
            );
            c.flush().await.map_err(e)?;
            return Ok(());
        }
        if pending.is_none() && tokio::time::Instant::now() >= next_at {
            next_at += Duration::from_millis(300);
            next_id += 1;
            send(&mut c, m, cfg, &KeepAlive { id: next_id });
            c.flush().await.map_err(e)?;
            pending = Some((next_id, tokio::time::Instant::now()));
        }
        match tokio::time::timeout(Duration::from_millis(300), c.fill()).await {
            Ok(r) => {
                r.map_err(e)?;
            }
            Err(_) => continue,
        }
        while let Some((f, _)) = c.next_frame().map_err(|e| e.to_string())? {
            if m.packet_kind(cfg, Direction::Serverbound, f.id) == Some(PacketKind::KeepAlive) {
                let k: KeepAlive = decode(m, &f.payload);
                if pending.is_some_and(|(id, _)| id == k.id) {
                    pending = None;
                    log.lock().unwrap().config_keep_alives += 1;
                }
            } else {
                record(m, cfg, f.id, &f.payload, log);
            }
        }
    }
    send(&mut c, m, cfg, &FinishConfiguration);
    c.flush().await.map_err(e)?;
    loop {
        let (f, _) = c.read_frame().await.map_err(e)?;
        if !record(m, cfg, f.id, &f.payload, log) {
            break;
        }
    }
    let p = Phase::Play;
    send(&mut c, m, p, &play_login());
    if let Some(names) = &spec.commands {
        send(&mut c, m, p, &tree(names));
    }
    if spec.play_extras {
        send(
            &mut c,
            m,
            p,
            &BossEvent {
                id: Uuid::from_u128(0xb055),
                action: BossAction::Add {
                    title: text("Boss"),
                    progress: 0.5,
                    color: 1,
                    overlay: 0,
                    flags: 0,
                },
            },
        );
        send(
            &mut c,
            m,
            p,
            &TabList {
                header: text("header"),
                footer: text("footer"),
            },
        );
        send(
            &mut c,
            m,
            p,
            &ShowDialog {
                dialog: DialogRef::Registry(0),
            },
        );
    }
    c.flush().await.map_err(e)?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    {
        let mut l = log.lock().unwrap();
        l.joins += 1;
        l.ctl.push(tx);
    }
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut ka = 1;
    loop {
        tokio::select! {
            r = c.fill() => {
                r.map_err(e)?;
                while let Some((f, _)) = c.next_frame().map_err(|e| e.to_string())? {
                    record(m, p, f.id, &f.payload, log);
                }
            }
            cmd = rx.recv() => match cmd {
                Some(Ctl::Kick(why)) => {
                    send(&mut c, m, p, &Disconnect { reason: text(why) });
                    c.flush().await.map_err(e)?;
                    return Ok(());
                }
                Some(Ctl::Close) | None => return Ok(()),
                Some(Ctl::Payload(channel, data)) => {
                    send(&mut c, m, p, &ClientboundCustomPayload { channel: channel.into(), data });
                    c.flush().await.map_err(e)?;
                }
                Some(Ctl::Bundle) => {
                    send(&mut c, m, p, &BundleDelimiter);
                    c.flush().await.map_err(e)?;
                }
            },
            _ = tick.tick() => {
                ka += 1;
                send(&mut c, m, p, &KeepAlive { id: ka });
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

fn config(servers: &[(&str, &Backend)], extra: &str) -> String {
    let list: String = servers
        .iter()
        .map(|(n, b)| {
            format!(
                "  {n}: {{ address: \"{}\", protocol: {PROTOCOL} }}\n",
                b.addr
            )
        })
        .collect();
    let order: Vec<String> = servers.iter().map(|(n, _)| n.to_string()).collect();
    format!(
        r#"
listener:
  - bind: "127.0.0.1:0"

login:
  online-mode: false

forwarding:
  mode: none

servers:
{list}
routing:
  try: [{}]

limits:
  connections-per-ip-per-second: 100000
  concurrent-per-ip: 100000

switching:
  reconnect-cooldown-ms: 0
{extra}
"#,
        order.join(", ")
    )
}

async fn join(addr: SocketAddr, name: &str) -> Player {
    Player::join(addr, module(PROTOCOL), JoinOptions::new(name))
        .await
        .unwrap()
}

/// Waits for the next play login (a finished switch).
async fn next_login(p: &mut Player) {
    let n = p.logins.len();
    assert!(
        p.pump(WAIT, |p| p.logins.len() > n).await.unwrap(),
        "no new login: {:?} {:?}",
        p.disconnect,
        p.messages
    );
}

async fn message(p: &mut Player, needle: &str) -> bool {
    p.pump(WAIT, |p| p.messages.iter().any(|m| m.contains(needle)))
        .await
        .unwrap()
}

/// Pumps until the backend log satisfies `f`.
async fn until(p: &mut Player, b: &Backend, f: impl Fn(&Log) -> bool) -> bool {
    for _ in 0..100 {
        if b.with(&f) {
            return true;
        }
        let _ = p.pump(Duration::from_millis(100), |_| false).await;
    }
    false
}

fn spec(name: &'static str) -> Spec {
    Spec {
        name,
        compression: Some(256),
        ..Spec::default()
    }
}

// ---------------------------------------------------------------- tests

#[tokio::test(flavor = "multi_thread")]
async fn server_command_switches_back_and_forth() {
    let lobby = Backend::start(spec("lobby")).await;
    // Another compression threshold: frames are encoded again on the way.
    let games = Backend::start(Spec {
        compression: None,
        ..spec("games")
    })
    .await;
    let (proxy, addr) = start_proxy(&config(&[("lobby", &lobby), ("games", &games)], "")).await;
    let mut p = join(addr, "Switcher").await;
    p.command("server").await.unwrap();
    assert!(
        message(&mut p, "You are on lobby").await,
        "{:?}",
        p.messages
    );
    for i in 0..10 {
        let to = if i % 2 == 0 { "games" } else { "lobby" };
        p.command(&format!("server {to}")).await.unwrap();
        next_login(&mut p).await;
        assert_eq!(
            proxy.find_player("switcher").unwrap().server.as_deref(),
            Some(to)
        );
    }
    assert_eq!(p.logins.len(), 11);
    assert_eq!(p.reconfigurations, 10);
    p.command("server lobby").await.unwrap();
    assert!(message(&mut p, "already connected to lobby").await);
    p.command("server nowhere").await.unwrap();
    assert!(message(&mut p, "no server named nowhere").await);
    // /glist needs an operator.
    p.command("glist").await.unwrap();
    assert!(
        until(&mut p, &lobby, |l| l
            .commands
            .contains(&"glist".to_string()))
        .await
    );
    // Every join got the client's settings and brand, nothing got /server.
    for b in [&lobby, &games] {
        b.with(|l| {
            assert_eq!(l.duplicates, 0);
            assert_eq!(l.client_information, l.joins, "{l:?}");
            assert!(l.brands.iter().all(|x| x == "vanilla") && l.brands.len() == l.joins);
            assert!(l.commands.iter().all(|c| !c.starts_with("server")));
        });
    }
    // The server brand gets the suffix.
    let brand = p
        .payloads
        .iter()
        .rev()
        .find(|(c, _)| c == "minecraft:brand")
        .unwrap();
    assert_eq!(
        Reader::new(&brand.1).string(64).unwrap(),
        "lobby (PumboProx)"
    );
    let metrics = proxy.metrics.render();
    assert!(
        metrics.contains("pumboprox_switches_total{result=\"ok\"} 10"),
        "{metrics}"
    );
    p.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn fallback_after_shutdown_kick_or_lost_server_and_kick_otherwise() {
    let lobby = Backend::start(spec("lobby")).await;
    let games = Backend::start(spec("games")).await;
    let (proxy, addr) = start_proxy(&config(&[("lobby", &lobby), ("games", &games)], "")).await;
    let mut p = join(addr, "Faller").await;
    p.command("server games").await.unwrap();
    next_login(&mut p).await;
    // A shutdown kick moves the player to the next server with a notice.
    games.ctl(Ctl::Kick("Server closed"));
    next_login(&mut p).await;
    assert!(
        message(&mut p, "You were moved to lobby: Server closed").await,
        "{:?}",
        p.messages
    );
    // A server that just goes away: the same.
    lobby.ctl(Ctl::Close);
    next_login(&mut p).await;
    assert!(
        message(&mut p, "You were moved to games").await,
        "{:?}",
        p.messages
    );
    assert_eq!(
        proxy.find_player("Faller").unwrap().server.as_deref(),
        Some("games")
    );
    // A plain kick disconnects with its reason.
    games.ctl(Ctl::Kick("You are banned"));
    p.pump(WAIT, |_| false).await.unwrap();
    assert_eq!(p.disconnect.as_deref(), Some("You are banned"));
    assert!(
        proxy
            .metrics
            .render()
            .contains("pumboprox_fallbacks_total 2")
    );
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_to_the_same_server_without_a_duplicate_login() {
    let lobby = Backend::start(spec("lobby")).await;
    let (proxy, addr) = start_proxy(&config(&[("lobby", &lobby)], "")).await;
    let mut p = join(addr, "Again").await;
    let id = pumbo_identity::offline_uuid("Again");
    for _ in 0..5 {
        assert!(proxy.reconnect(id));
        next_login(&mut p).await;
    }
    assert_eq!(p.logins.len(), 6);
    lobby.with(|l| {
        assert_eq!(l.duplicates, 0, "the old connection closed first");
        assert_eq!(l.joins, 6);
    });
    assert!(p.disconnect.is_none());
    p.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_next_server_keep_alives_are_answered_while_buffering() {
    let lobby = Backend::start(spec("lobby")).await;
    let slow = Backend::start(Spec {
        slow_config: Duration::from_secs(12),
        ..spec("slow")
    })
    .await;
    let (proxy, addr) = start_proxy(&config(&[("lobby", &lobby), ("slow", &slow)], "")).await;
    let mut p = join(addr, "Patient").await;
    // The client takes 4 s to leave the old server: the next one's
    // keep-alives in that time are answered by the proxy (§2.9).
    p.ack_delay = Duration::from_secs(4);
    p.command("server slow").await.unwrap();
    let n = p.logins.len();
    assert!(
        p.pump(Duration::from_secs(25), |p| p.logins.len() > n)
            .await
            .unwrap(),
        "{:?}",
        p.disconnect
    );
    slow.with(|l| {
        assert_eq!(l.missed_keep_alives, 0);
        assert!(l.config_keep_alives >= 30, "{}", l.config_keep_alives);
    });
    assert!(p.disconnect.is_none());
    p.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn command_tree_permissions_suggestions_and_collisions() {
    let lobby = Backend::start(Spec {
        commands: Some(vec!["help", "msg"]),
        ..spec("lobby")
    })
    .await;
    // This backend has its own /server: the proxy leaves it alone.
    let own = Backend::start(Spec {
        commands: Some(vec!["server"]),
        ..spec("own")
    })
    .await;
    let operator = pumbo_identity::offline_uuid("Admin");
    let cfg = config(
        &[("lobby", &lobby), ("own", &own)],
        &format!("commands:\n  operators: [{operator}]\n  sensitive: [login]\n"),
    );
    let (proxy, addr) = start_proxy(&cfg).await;
    let names = |p: &Player| -> Vec<String> {
        let t = p.commands.as_ref().unwrap();
        t.nodes[t.root as usize]
            .children
            .iter()
            .filter_map(|c| t.nodes[*c as usize].name.clone())
            .collect()
    };
    let mut admin = join(addr, "Admin").await;
    let mut guest = join(addr, "Guest").await;
    assert!(admin.pump(WAIT, |p| p.commands.is_some()).await.unwrap());
    assert!(guest.pump(WAIT, |p| p.commands.is_some()).await.unwrap());
    assert_eq!(
        names(&admin),
        [
            "help", "msg", "server", "glist", "send", "find", "alert", "prox"
        ]
    );
    assert_eq!(names(&guest), ["help", "msg", "server", "prox"]);
    // The proxy's nodes are a literal and a greedy string asking the server for suggestions.
    let t = admin.commands.as_ref().unwrap();
    let server = t
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("server"))
        .unwrap();
    let arg = &t.nodes[server.children[0] as usize];
    assert_eq!(arg.suggestions.as_deref(), Some("minecraft:ask_server"));
    // Tab completion for proxy commands comes from the proxy.
    admin
        .c
        .send(&pumbo_protocol::packets::play::CommandSuggestion {
            id: 41,
            text: "/server lo".into(),
        })
        .await
        .unwrap();
    assert!(
        admin
            .pump(WAIT, |p| !p.suggestions.is_empty())
            .await
            .unwrap()
    );
    let s = &admin.suggestions[0];
    assert_eq!((s.id, s.start, s.length), (41, 8, 2));
    assert_eq!(
        s.matches
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["lobby"]
    );
    // A proxy command the player may not use is still the proxy's: an empty answer,
    // never the backend's suggestions for some other command of that name.
    guest
        .c
        .send(&pumbo_protocol::packets::play::CommandSuggestion {
            id: 42,
            text: "/send Ad".into(),
        })
        .await
        .unwrap();
    assert!(
        guest
            .pump(WAIT, |p| !p.suggestions.is_empty())
            .await
            .unwrap()
    );
    let s = &guest.suggestions[0];
    assert_eq!((s.id, s.start, s.length), (42, 8, 0));
    assert!(s.matches.is_empty());
    // /glist and /find for the operator.
    admin.command("glist").await.unwrap();
    assert!(
        message(&mut admin, "[lobby] (2): Admin, Guest").await,
        "{:?}",
        admin.messages
    );
    admin.command("find guest").await.unwrap();
    assert!(message(&mut admin, "Guest is on lobby.").await);
    // /prox: the help (only what the sender may use) and the version.
    admin.command("prox").await.unwrap();
    assert!(message(&mut admin, "PumboProx 0.").await);
    assert!(message(&mut admin, "glist   Lists players").await);
    assert!(message(&mut admin, "reload   Reloads").await);
    // Without the plugin host its commands are not listed.
    assert!(!admin.messages.iter().any(|m| m.contains("plugins")));
    guest.command("prox help").await.unwrap();
    assert!(message(&mut guest, "· Proxy   /prox").await);
    assert!(message(&mut guest, "version   Proxy version").await);
    assert!(!guest.messages.iter().any(|m| m.contains("/glist")));
    admin.command("prox version").await.unwrap();
    assert!(
        message(&mut admin, "protocols").await,
        "{:?}",
        admin.messages
    );
    // A guest's /glist goes to the backend.
    guest.command("glist").await.unwrap();
    assert!(
        until(&mut guest, &lobby, |l| l
            .commands
            .contains(&"glist".to_string()))
        .await
    );
    // /alert reaches everyone.
    admin.command("alert Restart soon").await.unwrap();
    assert!(
        message(&mut guest, "[Alert] Restart soon").await,
        "{:?}",
        guest.messages
    );
    // /send moves another player.
    admin.command("send Guest own").await.unwrap();
    next_login(&mut guest).await;
    // On "own" the backend's /server wins: not in the proxy's tree, sent to the backend.
    assert!(
        guest
            .pump(WAIT, |p| names(p) == ["server", "prox"])
            .await
            .unwrap()
    );
    guest.command("server lobby").await.unwrap();
    assert!(
        until(&mut guest, &own, |l| l
            .commands
            .contains(&"server lobby".to_string()))
        .await
    );
    // A sensitive command never reaches a backend.
    admin.command("login hunter2").await.unwrap();
    assert!(message(&mut admin, "Login is temporarily unavailable.").await);
    lobby.with(|l| assert!(l.commands.iter().all(|c| !c.contains("hunter2"))));
    admin.close().await;
    guest.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_chat_and_commands_become_chat_ack() {
    let lobby = Backend::start(spec("lobby")).await;
    let cfg = config(&[("lobby", &lobby)], "commands:\n  sensitive: [login]\n");
    let (proxy, addr) = start_proxy(&cfg).await;
    proxy.set_chat_filter(Some(Arc::new(
        |_p: &pumbo_core::profile::GameProfile, input: ChatInput<'_>| match input {
            ChatInput::Message(m) => m.contains("secret"),
            ChatInput::Command(c) => c.starts_with("msg"),
        },
    )));
    let mut p = join(addr, "Chatter").await;
    p.chat("hello", Some(2)).await.unwrap();
    p.chat("my secret", Some(3)).await.unwrap();
    p.signed_command("msg Bob a secret", &[("message", "a secret")], Some(4))
        .await
        .unwrap();
    p.command("msg Bob unsigned").await.unwrap();
    p.signed_command("login hunter2", &[("password", "hunter2")], Some(5))
        .await
        .unwrap();
    // A cancelled message without new acknowledgements needs no chat_ack.
    p.chat("secret again", Some(0)).await.unwrap();
    for i in 0..10 {
        p.chat(&format!("after {i}"), Some(1)).await.unwrap();
    }
    assert!(until(&mut p, &lobby, |l| l.chats.len() == 11).await);
    lobby.with(|l| {
        assert_eq!(l.chats[0], "hello");
        assert!(l.chats.iter().all(|c| !c.contains("secret")));
        assert_eq!(l.acks, [3, 4, 5]);
        assert!(l.commands.is_empty() && l.signed.is_empty(), "{l:?}");
    });
    assert!(p.disconnect.is_none());
    assert!(
        proxy
            .metrics
            .render()
            .contains("pumboprox_chat_cancelled_total 5")
    );
    p.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn bungeecord_channel_and_client_brand() {
    let lobby = Backend::start(spec("lobby")).await;
    let games = Backend::start(spec("games")).await;
    let cfg = config(
        &[("lobby", &lobby), ("games", &games)],
        "bungeecord-channel:\n  subchannels: [Connect, UUID, GetServer, GetServers, PlayerCount, PlayerList, ConnectOther]\n",
    );
    let (proxy, addr) = start_proxy(&cfg).await;
    let mut p = join(addr, "Bungee").await;
    // From a client the channel is dropped.
    p.c.send(&ServerboundCustomPayload {
        channel: bungee::CHANNEL.into(),
        data: bungee::request(&["Connect", "games"]),
    })
    .await
    .unwrap();
    let ask = |parts: &[&str]| Ctl::Payload(bungee::CHANNEL, bungee::request(parts));
    for parts in [
        vec!["GetServers"],
        vec!["GetServer"],
        vec!["UUID"],
        vec!["IP"],
        vec!["PlayerCount", "ALL"],
        vec!["PlayerList", "lobby"],
    ] {
        lobby.ctl(ask(&parts));
    }
    assert!(until(&mut p, &lobby, |l| l.payloads.len() >= 5).await);
    let replies = lobby.with(|l| l.payloads.clone());
    let parse = |i: usize, n: usize| bungee::parse_reply(&replies[i].1, n).unwrap();
    assert!(replies.iter().all(|(c, _)| c == bungee::CHANNEL));
    assert_eq!(parse(0, 2).0, ["GetServers", "games, lobby"]);
    // The client's own request never reached the backend.
    assert!(
        replies
            .iter()
            .all(|(_, d)| bungee::parse_reply(d, 1).unwrap().0[0] != "Connect")
    );
    assert_eq!(parse(1, 2).0, ["GetServer", "lobby"]);
    assert_eq!(
        parse(2, 2).0,
        [
            "UUID".to_string(),
            pumbo_identity::offline_uuid("Bungee").simple().to_string()
        ]
    );
    // IP is off in this config: no answer, so the next reply is PlayerCount.
    assert_eq!(
        parse(3, 2),
        (vec!["PlayerCount".into(), "ALL".into()], Some(1))
    );
    assert_eq!(parse(4, 3).0, ["PlayerList", "lobby", "Bungee"]);
    // Connect moves the sender, ConnectOther another player.
    lobby.ctl(ask(&["Connect", "games"]));
    next_login(&mut p).await;
    let mut other = join(addr, "Other").await;
    games.ctl(ask(&["ConnectOther", "Other", "games"]));
    next_login(&mut other).await;
    // The client's brand reached both servers.
    games.with(|l| assert_eq!(l.brands, ["vanilla", "vanilla"]));
    assert_eq!(
        proxy.find_player("other").unwrap().server.as_deref(),
        Some("games")
    );
    p.close().await;
    other.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn resource_packs_are_kept_or_popped_across_switches() {
    let pack = Uuid::from_u128(0x9ac4);
    let a = Backend::start(Spec {
        pack: Some((pack, "aaaa")),
        ..spec("a")
    })
    .await;
    let b = Backend::start(Spec {
        pack: Some((pack, "aaaa")),
        ..spec("b")
    })
    .await;
    let other = Uuid::from_u128(0x0123);
    let c = Backend::start(Spec {
        pack: Some((other, "cccc")),
        ..spec("c")
    })
    .await;
    let (proxy, addr) = start_proxy(&config(&[("a", &a), ("b", &b), ("c", &c)], "")).await;
    let mut p = join(addr, "Packer").await;
    assert_eq!(p.packs_pushed, [pack]);
    p.command("server b").await.unwrap();
    next_login(&mut p).await;
    // Same pack: not sent again; the proxy answered b itself.
    assert_eq!(p.packs_pushed, [pack]);
    b.with(|l| assert_eq!(l.pack_statuses, [(pack, 3), (pack, 4), (pack, 0)]));
    p.command("server c").await.unwrap();
    next_login(&mut p).await;
    // Another pack: pushed, and the old one popped when c's configuration ends.
    assert_eq!(p.packs_pushed, [pack, other]);
    assert_eq!(p.packs_popped, [Some(pack)]);
    // Statuses go only to the server that sent the pack.
    c.with(|l| assert!(l.pack_statuses.iter().all(|(id, _)| *id == other)));
    p.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_before_switch_and_bundles() {
    let lobby = Backend::start(Spec {
        play_extras: true,
        ..spec("lobby")
    })
    .await;
    let games = Backend::start(spec("games")).await;
    let (proxy, addr) = start_proxy(&config(&[("lobby", &lobby), ("games", &games)], "")).await;
    let mut p = join(addr, "Tidy").await;
    assert!(
        p.pump(WAIT, |p| saw(p, Phase::Play, PacketKind::ShowDialog) == 1)
            .await
            .unwrap()
    );
    // The server opens a bundle; the switch waits for its end.
    lobby.ctl(Ctl::Bundle);
    assert!(
        p.pump(WAIT, |p| saw(p, Phase::Play, PacketKind::BundleDelimiter)
            == 1)
            .await
            .unwrap()
    );
    p.command("server games").await.unwrap();
    p.pump(Duration::from_millis(800), |_| false).await.unwrap();
    assert_eq!(p.reconfigurations, 0, "start_configuration inside a bundle");
    lobby.ctl(Ctl::Bundle);
    next_login(&mut p).await;
    let play: Vec<PacketKind> = p
        .kinds
        .iter()
        .filter(|(ph, k)| *ph == Phase::Play && *k != PacketKind::KeepAlive)
        .map(|(_, k)| *k)
        .collect();
    let start = play
        .iter()
        .position(|k| *k == PacketKind::StartConfiguration)
        .unwrap();
    assert_eq!(
        &play[start - 5..=start],
        [
            PacketKind::BundleDelimiter,
            PacketKind::BossEvent,
            PacketKind::TabList,
            PacketKind::ClearTitles,
            PacketKind::ClearDialog,
            PacketKind::StartConfiguration
        ]
    );
    p.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn too_much_configuration_falls_back() {
    let lobby = Backend::start(spec("lobby")).await;
    let big = Backend::start(Spec {
        config_bulk: 30_000,
        ..spec("big")
    })
    .await;
    let cfg = config(
        &[("lobby", &lobby), ("big", &big)],
        "  config-buffer-bytes: 10000\n",
    );
    let (proxy, addr) = start_proxy(&cfg).await;
    let mut p = join(addr, "Greedy").await;
    p.ack_delay = Duration::from_millis(500);
    p.command("server big").await.unwrap();
    next_login(&mut p).await;
    // The client had left lobby already, so it goes back there with a notice.
    assert_eq!(
        proxy.find_player("greedy").unwrap().server.as_deref(),
        Some("lobby")
    );
    assert!(
        message(&mut p, "You were moved to lobby").await,
        "{:?}",
        p.messages
    );
    p.close().await;
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn forced_hosts_and_the_version_message() {
    let lobby = Backend::start(spec("lobby")).await;
    let games = Backend::start(spec("games")).await;
    let cfg = config(
        &[("lobby", &lobby), ("games", &games)],
        "forced-hosts:\n  Games.Example.ORG: [games]\n",
    );
    let (proxy, addr) = start_proxy(&cfg).await;
    let mut o = JoinOptions::new("Forced");
    o.host = "games.example.org.".into();
    let p = Player::join(addr, module(PROTOCOL), o).await.unwrap();
    assert_eq!(
        proxy.find_player("forced").unwrap().server.as_deref(),
        Some("games")
    );
    p.close().await;
    proxy.stop();
    // No server speaks the client's protocol: the message says so.
    let cfg = config(&[("lobby", &lobby)], "").replace("protocol: 777", "protocol: 776");
    let (proxy, addr) = start_proxy(&cfg).await;
    let err = match Player::join(addr, module(PROTOCOL), JoinOptions::new("Old")).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("joined"),
    };
    assert!(
        err.contains("no server for Minecraft 26.3. Please join with 26.2."),
        "{err}"
    );
    proxy.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_chat_cancel_kick_policy() {
    let lobby = Backend::start(spec("lobby")).await;
    let cfg = config(&[("lobby", &lobby)], "").replace(
        "protocol: 777 }",
        "protocol: 777, signed-chat-cancel: kick }",
    );
    let (proxy, addr) = start_proxy(&cfg).await;
    proxy.set_chat_filter(Some(Arc::new(|_p: &pumbo_core::profile::GameProfile, input: ChatInput<'_>| {
        matches!(input, ChatInput::Message(m) if m.contains("secret"))
    })));
    let mut p = join(addr, "Signer").await;
    // Unsigned (no chat session): dropping is safe, the player stays.
    p.chat("a secret", Some(1)).await.unwrap();
    p.chat("plain", None).await.unwrap();
    assert!(until(&mut p, &lobby, |l| l.chats == ["plain"]).await);
    // With a session the gap breaks the chain: this server wants a kick.
    let key = pumbo_testclient::ChatKey::generate().unwrap();
    p.start_chat_session(key, i64::MAX, vec![0; 256])
        .await
        .unwrap();
    p.chat("signed secret", None).await.unwrap();
    p.pump(WAIT, |_| false).await.unwrap();
    assert_eq!(
        p.disconnect.as_deref(),
        Some("Your message was blocked. Please reconnect.")
    );
    lobby.with(|l| assert_eq!(l.acks, [1]));
    proxy.stop();
}

// ---------------------------------------------------------------- E5

/// The test plugin of pumbo-host, built once for wasm32-wasip2.
fn test_plugin() -> std::path::PathBuf {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target/wasm-plugins");
        let status = std::process::Command::new(env!("CARGO"))
            .current_dir(&root)
            .args([
                "build",
                "--locked",
                "-p",
                "pumbo-host-test-plugin",
                "--target",
                "wasm32-wasip2",
            ])
            .args(["--profile", "plugin", "--target-dir"])
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success());
        target.join("wasm32-wasip2/plugin/pumbo_host_test_plugin.wasm")
    })
    .clone()
}

/// The plugin host in play (E5): a plugin command answered through the
/// session queue, `/pumbo` refused without permission, chat cancelled by
/// `on-chat`, `on-server-connect` denying and redirecting a switch,
/// `on-server-kicked` moving a kicked player to another server.
#[tokio::test(flavor = "multi_thread")]
async fn plugin_host_in_play() {
    let lobby = Backend::start(spec("lobby")).await;
    let games = Backend::start(spec("games")).await;
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-e5-play-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("evt.yml"),
        "id: evt\nversion: 0.1.0\napi: \"0.1\"\nevents: [chat, server-connect, server-kicked]\n",
    )
    .unwrap();
    std::fs::copy(test_plugin(), dir.join("evt.wasm")).unwrap();
    let cfg = config(
        &[("lobby", &lobby), ("games", &games)],
        &format!("\nplugins:\n  dir: {:?}\n", dir.display().to_string()),
    );
    let (proxy, addr) = start_proxy(&cfg).await;
    let plugins = pumbo_prox::plugins::Plugins::start(&cfg, &proxy, Vec::new())
        .await
        .unwrap()
        .unwrap();
    proxy.plugins.set(plugins).unwrap();

    let mut p = join(addr, "nogames_a").await;
    p.command("echo hello plugins").await.unwrap();
    assert!(
        message(&mut p, "echo hello plugins").await,
        "{:?}",
        p.messages
    );
    p.command("pumbo").await.unwrap();
    assert!(
        message(&mut p, "do not have permission").await,
        "{:?}",
        p.messages
    );
    p.chat("echo:0", Some(1)).await.unwrap();
    p.chat("visible", Some(1)).await.unwrap();
    assert!(until(&mut p, &lobby, |l| l.chats.len() == 1).await);
    lobby.with(|l| assert_eq!(l.chats, ["visible"]));
    p.command("server games").await.unwrap();
    assert!(message(&mut p, "games closed").await, "{:?}", p.messages);
    assert_eq!(
        proxy.find_player("nogames_a").unwrap().server.as_deref(),
        Some("lobby")
    );
    p.close().await;

    // A join redirected by on-server-connect, then a kick moved by on-server-kicked.
    let mut q = join(addr, "togo_b").await;
    assert_eq!(
        proxy.find_player("togo_b").unwrap().server.as_deref(),
        Some("games")
    );
    games.ctl(Ctl::Kick("redirect me"));
    next_login(&mut q).await;
    assert_eq!(
        proxy.find_player("togo_b").unwrap().server.as_deref(),
        Some("lobby")
    );
    assert!(q.disconnect.is_none());
    q.close().await;
    proxy.stop();
}
