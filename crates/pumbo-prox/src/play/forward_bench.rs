//! The cost of forwarding one frame through a session in play, for an
//! instruction counter: `PUMBO_BENCH_KIND` picks the frame, `PUMBO_BENCH_FRAMES`
//! how many (see `docs/test-results.md`).

use pumbo_protocol::frame::FrameConfig;
use pumbo_protocol::packets::world::{MOVE_ON_GROUND, MovePlayer, MovePlayerPos, SetTime};
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion};

use crate::conn::Conn;

use super::test_rig::{Rig, accept, attach, encode, join, position};

/// Reads `block` `times` times, then ends.
struct Repeat {
    block: bytes::Bytes,
    at: usize,
    times: usize,
}

impl tokio::io::AsyncRead for Repeat {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let r = &mut *self;
        while buf.remaining() > 0 && r.times > 0 {
            let rest = &r.block[r.at..];
            let n = rest.len().min(buf.remaining());
            buf.put_slice(&rest[..n]);
            r.at += n;
            if r.at == r.block.len() {
                r.at = 0;
                r.times -= 1;
            }
        }
        std::task::Poll::Ready(Ok(()))
    }
}

/// A connection that reads `from` and swallows what it is sent.
fn into_sink(
    from: impl tokio::io::AsyncRead + Send + Unpin + 'static,
    inbound: FrameConfig,
) -> Conn {
    let stream = tokio::io::join(from, tokio::io::sink());
    Conn::new(Box::pin(stream), inbound, FrameConfig::from_client())
}

/// Forwards `PUMBO_BENCH_FRAMES` frames (rounded up to 1024) of one kind,
/// `PUMBO_BENCH_KIND`, through a session in play: `entity`, `time` and
/// `teleport` from the server, `move` and `accept` from the client. Run it in
/// release under an instruction counter with many frames and with 1024; the
/// difference over the frames is the cost of one. Setting the proxy up makes
/// an RSA key, whose cost varies by up to 1 G instructions between runs.
#[tokio::test]
#[ignore = "benchmark"]
async fn forward_one_frame() {
    let kind = std::env::var("PUMBO_BENCH_KIND").unwrap_or_else(|_| "entity".into());
    let frames: usize = std::env::var("PUMBO_BENCH_FRAMES").map_or(1 << 20, |n| n.parse().unwrap());
    let (rig, cmds) = Rig::new("");
    let m = &*rig.module;
    let (pl, cb, sb) = (Phase::Play, Direction::Clientbound, Direction::Serverbound);
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let id = |d, k| m.packet_id(pl, d, k).unwrap();
    let (from_client, frame_id, payload) = match kind.as_str() {
        "entity" => {
            let tables = pumbo_data::tables(ProtocolVersion::V777).unwrap();
            let id = tables.packet_id(pl, cb, "move_entity_pos").unwrap();
            assert_eq!(m.packet_kind(pl, cb, id), None);
            (false, id, vec![5, 0, 1, 0, 2, 0, 3, 1])
        }
        "time" => {
            let time = SetTime {
                world_age: 1000,
                time_of_day: 1000,
                ticking: true,
                clocks: Vec::new(),
            };
            (false, id(cb, PacketKind::SetTime), encode(m, cb, &time))
        }
        "teleport" => (
            false,
            id(cb, PacketKind::PlayerPosition),
            encode(m, cb, &position(1)),
        ),
        "move" => {
            let step = MovePlayerPos(MovePlayer {
                position: Some((0.7, 65.0, 0.5)),
                rotation: None,
                flags: MOVE_ON_GROUND,
            });
            (
                true,
                id(sb, PacketKind::MovePlayerPos),
                encode(m, sb, &step),
            )
        }
        "accept" => (
            true,
            id(sb, PacketKind::AcceptTeleportation),
            encode(m, sb, &accept(1)),
        ),
        other => panic!("PUMBO_BENCH_KIND {other}: entity, time, teleport, move or accept"),
    };
    let mut block = bytes::BytesMut::new();
    for _ in 0..1024 {
        pumbo_protocol::frame::encode(&mut block, frame_id, &payload, &FrameConfig::from_client())
            .unwrap();
    }
    let times = frames.div_ceil(1024);
    let source = Repeat {
        block: block.freeze(),
        at: 0,
        times,
    };
    let started = std::time::Instant::now();
    let b = play.backend.as_mut().unwrap();
    if from_client {
        play.client = into_sink(source, FrameConfig::from_client());
        b.conn = into_sink(tokio::io::empty(), FrameConfig::from_backend());
        while play.client.fill().await.is_ok() {
            assert!(play.client_frames().await.is_none());
            let b = play.backend.as_mut().unwrap();
            b.conn.try_write().unwrap();
        }
    } else {
        b.conn = into_sink(source, FrameConfig::from_backend());
        play.client = into_sink(tokio::io::empty(), FrameConfig::from_client());
        while play.backend.as_mut().unwrap().conn.fill().await.is_ok() {
            assert!(play.backend_frames().is_none());
            play.client.try_write().unwrap();
        }
    }
    println!("{kind}: {} frames in {:?}", times * 1024, started.elapsed());
}
