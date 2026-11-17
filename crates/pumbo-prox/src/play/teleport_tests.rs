//! The bridge's teleport gate inside a session: the server's teleports, the
//! client's confirmations and each play `login`.

use std::time::Duration;

use pumbo_protocol::packets::world::{AcceptTeleportation, PlayerPosition};
use pumbo_protocol::packets::{self, Ctx};
use pumbo_protocol::{Direction, PacketKind, Phase};
use tokio::time::Instant;

use crate::bridge::Tp;
use crate::session::Fail;

use super::End;
use super::test_rig::{
    Rig, accept, attach, backend_reads, client_reads, encode, ended, finish_configuration,
    from_backend, from_client, in_virtual, join, login, of_kind, position, switch, to_backend,
    to_client,
};

#[tokio::test]
async fn the_teleport_gate_follows_the_session() {
    let (rig, cmds) = Rig::new("");
    let m = &*rig.module;
    let (pl, cb, sb) = (Phase::Play, Direction::Clientbound, Direction::Serverbound);
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    let gate = rig.gate();
    finish_configuration(&mut play, &mut client, &mut lobby).await;
    // Before its play login the server's teleport goes nowhere.
    from_backend(&mut play, &mut lobby, pl, &position(7)).await;
    let got = to_client(&mut play, &mut client).await;
    assert!(of_kind(m, pl, cb, PacketKind::PlayerPosition, &got).is_empty());
    assert_eq!(*gate.borrow(), Tp::Free);
    // The login makes the spawn teleport due, before the server is recorded.
    from_backend(&mut play, &mut lobby, pl, &login()).await;
    assert!(matches!(*gate.borrow(), Tp::Sent(_)));
    assert_eq!(
        rig.proxy.server_of(rig.profile.id).as_deref(),
        Some("lobby")
    );
    let _ = to_client(&mut play, &mut client).await;
    from_backend(&mut play, &mut lobby, pl, &position(8)).await;
    let got = to_client(&mut play, &mut client).await;
    let teleports = of_kind(m, pl, cb, PacketKind::PlayerPosition, &got);
    assert_eq!(teleports.len(), 1);
    assert_eq!(teleports[0].payload, encode(m, cb, &position(8)));
    assert_eq!(*gate.borrow(), Tp::Awaiting(8));
    // Another id leaves the teleport waiting; the client's own frees it.
    from_client(&mut play, &mut client, pl, &accept(7)).await;
    let got = to_backend(&mut play, &mut lobby).await;
    assert_eq!(
        of_kind(m, pl, sb, PacketKind::AcceptTeleportation, &got).len(),
        1
    );
    assert_eq!(*gate.borrow(), Tp::Awaiting(8));
    from_client(&mut play, &mut client, pl, &accept(8)).await;
    let got = to_backend(&mut play, &mut lobby).await;
    assert_eq!(
        of_kind(m, pl, sb, PacketKind::AcceptTeleportation, &got).len(),
        1
    );
    assert!(matches!(*gate.borrow(), Tp::Confirmed(_)));
}

#[tokio::test]
async fn a_switch_makes_the_teleport_due_again() {
    let (rig, cmds) = Rig::new("");
    let pl = Phase::Play;
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    let gate = rig.gate();
    join(&mut play, &mut client, &mut lobby).await;
    from_backend(&mut play, &mut lobby, pl, &position(3)).await;
    from_client(&mut play, &mut client, pl, &accept(3)).await;
    assert!(matches!(*gate.borrow(), Tp::Confirmed(_)));
    let mut games = switch(&mut play, &mut client, "games").await;
    join(&mut play, &mut client, &mut games).await;
    assert!(matches!(*gate.borrow(), Tp::Sent(_)));
    assert_eq!(
        rig.proxy.server_of(rig.profile.id).as_deref(),
        Some("games")
    );
}

#[tokio::test]
async fn a_broken_teleport_from_the_server_ends_the_session() {
    let (rig, cmds) = Rig::new("");
    let m = &*rig.module;
    let cb = Direction::Clientbound;
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let full = encode(m, cb, &position(8));
    let cut = &full[..3];
    let why = packets::decode::<PlayerPosition>(cut, &Ctx::new(m, cb))
        .unwrap_err()
        .to_string();
    let id = m
        .packet_id(Phase::Play, cb, PacketKind::PlayerPosition)
        .unwrap();
    lobby.send_raw(id, cut).await;
    let end = backend_reads(&mut play).await;
    let want = format!("backend: {:?}", Fail::Protocol(why));
    assert!(
        matches!(&end, Some(End::Protocol(t)) if *t == want),
        "{}",
        ended(&end)
    );
}

#[tokio::test]
async fn a_broken_confirmation_from_the_client_ends_the_session() {
    let (rig, cmds) = Rig::new("");
    let m = &*rig.module;
    let sb = Direction::Serverbound;
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let full = encode(m, sb, &accept(8));
    let cut = &full[..1];
    let why = packets::decode::<AcceptTeleportation>(cut, &Ctx::new(m, sb))
        .unwrap_err()
        .to_string();
    let id = m
        .packet_id(Phase::Play, sb, PacketKind::AcceptTeleportation)
        .unwrap();
    client.send_raw(id, cut).await;
    let end = client_reads(&mut play).await;
    assert!(
        matches!(&end, Some(End::Protocol(t)) if *t == why),
        "{}",
        ended(&end)
    );
}

/// While the client leaves its server and inside a virtual world a
/// confirmation goes nowhere and leaves the gate as it is.
#[tokio::test]
async fn confirmations_outside_a_server_leave_the_gate() {
    let (rig, cmds) = Rig::new("");
    let m = &*rig.module;
    let (pl, sb) = (Phase::Play, Direction::Serverbound);
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    let gate = rig.gate();
    join(&mut play, &mut client, &mut lobby).await;
    from_backend(&mut play, &mut lobby, pl, &position(5)).await;
    assert_eq!(*gate.borrow(), Tp::Awaiting(5));
    play.ack_deadline = Some(Instant::now() + Duration::from_secs(5));
    from_client(&mut play, &mut client, pl, &accept(5)).await;
    let got = to_backend(&mut play, &mut lobby).await;
    assert!(of_kind(m, pl, sb, PacketKind::AcceptTeleportation, &got).is_empty());
    assert_eq!(*gate.borrow(), Tp::Awaiting(5));
    play.ack_deadline = None;
    play.vworld = Some(in_virtual(&rig));
    from_client(&mut play, &mut client, pl, &accept(5)).await;
    let got = to_backend(&mut play, &mut lobby).await;
    assert!(of_kind(m, pl, sb, PacketKind::AcceptTeleportation, &got).is_empty());
    assert_eq!(*gate.borrow(), Tp::Awaiting(5));
}
