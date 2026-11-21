//! The engine a session takes for its server and what it gives the
//! connection: the chat defaults and the repairs; and what the session does
//! with the server's brand.

use pumbo_core::engine::SignedChatCancel;
use pumbo_protocol::packets::common::ClientboundCustomPayload;
use pumbo_protocol::types::WriteExt;
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion};

use super::quirk_tests::{level_particles, particle, trail_options};
use super::test_rig::{Rig, attach, backend_reads, encode, from_backend, join, to_client};

/// `minecraft:brand` as a server sends it.
fn brand(name: &str) -> ClientboundCustomPayload {
    let mut data = Vec::new();
    data.put_string(name, 32_767).unwrap();
    ClientboundCustomPayload {
        channel: "minecraft:brand".into(),
        data,
    }
}

#[tokio::test]
async fn a_server_without_an_engine_keeps_the_chat_defaults_and_the_repair() {
    let (rig, cmds) = Rig::new("");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    from_backend(
        &mut play,
        &mut lobby,
        Phase::Configuration,
        &brand("backend"),
    )
    .await;
    join(&mut play, &mut client, &mut lobby).await;
    let b = play.backend.as_ref().unwrap();
    assert!(b.chat_sessions);
    assert_eq!(b.signed_cancel, SignedChatCancel::Drop);
    assert!(b.quirks.is_some());
    let v = ProtocolVersion::V777;
    let trail = particle(v, "trail");
    let id = rig
        .module
        .packet_id(
            Phase::Play,
            Direction::Clientbound,
            PacketKind::LevelParticles,
        )
        .unwrap();
    lobby.send_raw(id, &level_particles(true, trail, &[])).await;
    assert!(backend_reads(&mut play).await.is_none());
    let got = to_client(&mut play, &mut client).await;
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].payload,
        level_particles(true, trail, &trail_options())
    );
}

/// With nothing to add to it, a brand the proxy cannot read reaches the
/// client as the server sent it.
#[tokio::test]
async fn a_brand_the_proxy_cannot_read_is_forwarded_untouched() {
    let (rig, cmds) = Rig::new("plugin-messages:\n  server-brand-suffix: \"\"\n");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    // A string of ten bytes cut after two.
    let cut = ClientboundCustomPayload {
        channel: "minecraft:brand".into(),
        data: vec![10, b'P', b'u'],
    };
    from_backend(&mut play, &mut lobby, Phase::Configuration, &cut).await;
    let got = to_client(&mut play, &mut client).await;
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].payload,
        encode(&*rig.module, Direction::Clientbound, &cut)
    );
}
