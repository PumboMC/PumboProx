//! The bridge's teleport gate inside a session: the server's teleports, the
//! client's confirmations and each play `login`.

use pumbo_protocol::{Direction, PacketKind, Phase};

use crate::bridge::Tp;

use super::test_rig::{
    Rig, accept, attach, encode, finish_configuration, from_backend, from_client, login, of_kind,
    position, to_backend, to_client,
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
