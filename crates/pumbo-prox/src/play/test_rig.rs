//! A session over in-memory pipes: the test holds the far end of the client
//! connection and of each server connection, sends frames from there and
//! reads what the session wrote, without sockets or timers.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;

use pumbo_core::profile::GameProfile;
use pumbo_protocol::frame::FrameConfig;
use pumbo_protocol::packets::configuration::FinishConfiguration;
use pumbo_protocol::packets::play::{ConfigurationAcknowledged, Login};
use pumbo_protocol::packets::world::{AcceptTeleportation, PlayerPosition};
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion, RawFrame, VersionModule};
use tokio::io::DuplexStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use uuid::Uuid;

use crate::config::Config;
use crate::conn::Conn;
use crate::server::{PlayerGuard, Proxy, SessionCmd};

use super::{End, InVirtual, Next, Play, Purpose, Request};

/// Bytes a pipe holds before a write waits for its reader: room for a frame
/// over the 1 MiB plugin message limit.
const PIPE: usize = 4 << 20;

/// The client's address and port.
fn peer() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 40000))
}

/// A proxy with the servers `lobby` and `games` and one registered player.
pub(super) struct Rig {
    pub(super) proxy: Arc<Proxy>,
    pub(super) module: Arc<dyn VersionModule>,
    pub(super) profile: GameProfile,
    guard: PlayerGuard,
}

impl Rig {
    /// `extra` goes at the end of the config.
    pub(super) fn new(extra: &str) -> (Self, mpsc::Receiver<SessionCmd>) {
        let text = format!(
            "listener:\n  - bind: \"127.0.0.1:0\"\n\
             forwarding:\n  mode: none\n\
             servers:\n  \
               lobby: {{ address: \"127.0.0.1:1\", protocol: 777 }}\n  \
               games: {{ address: \"127.0.0.1:2\", protocol: 777 }}\n\
             routing:\n  try: [lobby]\n\
             limits:\n  \
               client-packets-per-second: 4000000000\n  \
               client-bytes-per-second: 4000000000\n\
             {extra}"
        );
        let proxy = Proxy::new(Config::parse(&text).unwrap(), None).unwrap();
        let profile = GameProfile {
            id: Uuid::from_u128(0x7e57),
            name: "Rig".into(),
            properties: Vec::new(),
        };
        let (guard, cmds) = proxy.register_player(&profile, peer()).unwrap();
        let module = proxy.versions.get(ProtocolVersion::V777).cloned().unwrap();
        let rig = Rig {
            proxy,
            module,
            profile,
            guard,
        };
        (rig, cmds)
    }

    /// The session in configuration without a server, and the client's end.
    pub(super) fn session(&self, cmds: mpsc::Receiver<SessionCmd>) -> (Play<'_>, Peer) {
        let (ours, theirs) = tokio::io::duplex(PIPE);
        let client = Conn::new(
            Box::pin(ours),
            FrameConfig::from_client(),
            FrameConfig::from_backend(),
        );
        let rt = self.proxy.runtime();
        let urgent = self.guard.urgent.clone();
        let (profile, module) = (self.profile.clone(), &self.module);
        let play = Play::new(
            &self.proxy,
            &rt,
            client,
            module,
            profile,
            cmds,
            urgent,
            peer(),
            "localhost",
        );
        (play, Peer::new(theirs, module, Direction::Serverbound))
    }

    pub(super) fn gate(&self) -> crate::bridge::TpGate {
        self.proxy.teleport_gate(self.profile.id).unwrap()
    }
}

/// The test's end of a connection.
pub(super) struct Peer {
    conn: Conn,
    module: Arc<dyn VersionModule>,
    /// Direction of what this end sends.
    sends: Direction,
}

impl Peer {
    fn new(stream: DuplexStream, module: &Arc<dyn VersionModule>, sends: Direction) -> Self {
        let conn = Conn::new(
            Box::pin(stream),
            FrameConfig::from_backend(),
            FrameConfig::from_client(),
        );
        Peer {
            conn,
            module: module.clone(),
            sends,
        }
    }

    pub(super) async fn send<P: Packet>(&mut self, phase: Phase, p: &P) {
        let id = self.module.packet_id(phase, self.sends, P::KIND).unwrap();
        self.send_raw(id, &encode(&*self.module, self.sends, p))
            .await;
    }

    pub(super) async fn send_raw(&mut self, id: i32, payload: &[u8]) {
        self.conn.queue(id, payload).unwrap();
        self.conn.flush().await.unwrap();
    }

    /// What `from`, the session's side of this connection, wrote since the
    /// last call.
    async fn take(&mut self, from: &mut Conn) -> Vec<RawFrame> {
        let want = self.conn.buffered().len() + from.pending();
        from.flush().await.unwrap();
        while self.conn.buffered().len() < want {
            self.conn.fill().await.unwrap();
        }
        let mut frames = Vec::new();
        while let Some((f, _)) = self.conn.next_frame().unwrap() {
            frames.push(f);
        }
        frames
    }
}

pub(super) fn encode<P: Packet>(m: &dyn VersionModule, d: Direction, p: &P) -> Vec<u8> {
    packets::encode(p, &Ctx::new(m, d)).unwrap()
}

/// A server for the session: logged in and in configuration, as `attach`
/// leaves it.
pub(super) fn attach(play: &mut Play<'_>, name: &str) -> Peer {
    let server = ready(play, name, Purpose::Join);
    assert!(play.progress().is_ok());
    server
}

/// The session switches to `name`: the client leaves its server through
/// configuration and the new one takes it.
pub(super) async fn switch(play: &mut Play<'_>, client: &mut Peer, name: &str) -> Peer {
    let server = ready(play, name, Purpose::Switch { quiet: true });
    assert!(play.progress().is_ok());
    from_client(play, client, Phase::Play, &ConfigurationAcknowledged).await;
    assert!(play.progress().is_ok());
    server
}

fn ready(play: &mut Play<'_>, name: &str, purpose: Purpose) -> Peer {
    let (ours, theirs) = tokio::io::duplex(PIPE);
    let conn = Conn::new(
        Box::pin(ours),
        FrameConfig::from_backend(),
        FrameConfig::from_client(),
    );
    play.request = Some(Request {
        purpose,
        candidates: VecDeque::new(),
        target: name.into(),
        refusal: None,
        redirects: 0,
    });
    play.next = Next::Ready {
        conn,
        buffer: Vec::new(),
        bytes: 0,
        since: Instant::now(),
        sent_settings: true,
        sent_brand: true,
    };
    Peer::new(theirs, &play.module_arc, Direction::Clientbound)
}

/// Reads until a whole frame waits in `conn`.
async fn fill_frame(conn: &mut Conn) {
    conn.fill().await.unwrap();
    while conn.next_size().is_none() {
        conn.fill().await.unwrap();
    }
}

/// The session reads what its server sent; `Some` when the session ends.
pub(super) async fn backend_reads(play: &mut Play<'_>) -> Option<End> {
    if let Some(b) = play.backend.as_mut() {
        fill_frame(&mut b.conn).await;
    }
    play.backend_frames()
}

/// The session reads what its client sent; `Some` when the session ends.
pub(super) async fn client_reads(play: &mut Play<'_>) -> Option<End> {
    fill_frame(&mut play.client).await;
    play.client_frames().await
}

/// The server sends `p` and the session, which goes on, reads it.
pub(super) async fn from_backend<P: Packet>(
    play: &mut Play<'_>,
    server: &mut Peer,
    phase: Phase,
    p: &P,
) {
    server.send(phase, p).await;
    let end = backend_reads(play).await;
    assert!(end.is_none(), "{}", ended(&end));
}

/// The client sends `p` and the session, which goes on, reads it.
pub(super) async fn from_client<P: Packet>(
    play: &mut Play<'_>,
    client: &mut Peer,
    phase: Phase,
    p: &P,
) {
    client.send(phase, p).await;
    let end = client_reads(play).await;
    assert!(end.is_none(), "{}", ended(&end));
}

pub(super) async fn to_client(play: &mut Play<'_>, client: &mut Peer) -> Vec<RawFrame> {
    client.take(&mut play.client).await
}

pub(super) async fn to_backend(play: &mut Play<'_>, server: &mut Peer) -> Vec<RawFrame> {
    let b = play.backend.as_mut().unwrap();
    server.take(&mut b.conn).await
}

/// The server finishes the configuration and the client follows; what they
/// sent each other is read.
pub(super) async fn finish_configuration(
    play: &mut Play<'_>,
    client: &mut Peer,
    server: &mut Peer,
) {
    let cfg = Phase::Configuration;
    from_backend(play, server, cfg, &FinishConfiguration).await;
    from_client(play, client, cfg, &FinishConfiguration).await;
    let _ = to_client(play, client).await;
    let _ = to_backend(play, server).await;
}

/// The configuration ends and the server's play `login` reaches the client.
pub(super) async fn join(play: &mut Play<'_>, client: &mut Peer, server: &mut Peer) {
    finish_configuration(play, client, server).await;
    from_backend(play, server, Phase::Play, &login()).await;
    let _ = to_client(play, client).await;
}

pub(super) fn login() -> Login {
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

pub(super) fn position(teleport_id: i32) -> PlayerPosition {
    PlayerPosition {
        teleport_id,
        x: 0.5,
        y: 65.0,
        z: 0.5,
        velocity: (0.0, 0.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        flags: 0,
    }
}

pub(super) fn accept(id: i32) -> AcceptTeleportation {
    AcceptTeleportation {
        id,
        at: Some((0.5, 65.0, 0.5, 0.0, 0.0)),
    }
}

/// The player in the Pumbox's world, as a virtual world holds it.
pub(super) fn in_virtual(rig: &Rig) -> InVirtual {
    let (x, y, z) = crate::world::PUMBOX_SPAWN;
    InVirtual {
        player: pumbo_virtual::VirtualPlayer::new(
            rig.module.clone(),
            crate::session::to_wire(&rig.profile),
            crate::world::pumbox_world(),
            pumbo_virtual::Position::at(x, y, z),
            Vec::new(),
        ),
        tag: 0,
        inputs: mpsc::channel(1).0,
    }
}

/// Frames of `kind` among those sent in `phase` towards `d`.
pub(super) fn of_kind<'f>(
    m: &dyn VersionModule,
    phase: Phase,
    d: Direction,
    kind: PacketKind,
    frames: &'f [RawFrame],
) -> Vec<&'f RawFrame> {
    frames
        .iter()
        .filter(|f| m.packet_kind(phase, d, f.id) == Some(kind))
        .collect()
}

/// The text of a session's end, for assertions.
pub(super) fn ended(end: &Option<End>) -> String {
    match end {
        None => "running".into(),
        Some(End::Protocol(t)) => format!("protocol: {t}"),
        Some(End::Kick(t)) => format!("kick: {}", t.plain_text()),
        Some(End::BackendKicked) => "backend kicked".into(),
        Some(End::ClientClosed) => "client closed".into(),
        Some(End::ClientLost(t)) => format!("client lost: {t}"),
    }
}
