//! The engine a session takes for its server and what it gives the
//! connection: the chat defaults and the repairs; and what the session does
//! with the server's brand.

use pumbo_core::engine::SignedChatCancel;
use pumbo_protocol::packets::common::ClientboundCustomPayload;
use pumbo_protocol::types::WriteExt;
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion};

use super::Play;
use super::quirk_tests::{level_particles, particle, trail_options};
use super::test_rig::{Peer, Rig, attach, backend_reads, encode, from_backend, join, to_client};

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

#[tokio::test]
async fn a_declared_engine_sets_the_chat_default() {
    let (rig, cmds) = Rig::lobby(", engine: vanilla", "");
    let (mut play, _client) = rig.session(cmds);
    let _lobby = attach(&mut play, "lobby");
    let b = play.backend.as_ref().unwrap();
    assert_eq!(b.signed_cancel, SignedChatCancel::Kick);
    assert!(b.quirks.is_none());
}

/// An empty `trail` from the server and what the client got of it.
async fn trail_through(
    rig: &Rig,
    play: &mut Play<'_>,
    client: &mut Peer,
    server: &mut Peer,
) -> (Vec<u8>, Vec<u8>) {
    let trail = particle(ProtocolVersion::V777, "trail");
    let empty = level_particles(true, trail, &[]);
    let id = rig
        .module
        .packet_id(
            Phase::Play,
            Direction::Clientbound,
            PacketKind::LevelParticles,
        )
        .unwrap();
    server.send_raw(id, &empty).await;
    assert!(backend_reads(play).await.is_none());
    let got = to_client(play, client).await;
    assert_eq!(got.len(), 1);
    (empty, got[0].payload.to_vec())
}

/// The connection's chat default and whether it has repairs.
fn fields(play: &Play<'_>) -> (SignedChatCancel, bool) {
    let b = play.backend.as_ref().unwrap();
    (b.signed_cancel, b.quirks.is_some())
}

/// The first player of a fresh proxy: the brand comes before the server's
/// login, so the player gets the engine's values.
#[tokio::test]
async fn a_vanilla_brand_means_kick_and_no_repair() {
    let (rig, cmds) = Rig::new("");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    assert_eq!(fields(&play), (SignedChatCancel::Drop, true));
    from_backend(
        &mut play,
        &mut lobby,
        Phase::Configuration,
        &brand("vanilla"),
    )
    .await;
    assert_eq!(fields(&play), (SignedChatCancel::Kick, false));
    join(&mut play, &mut client, &mut lobby).await;
    let (sent, got) = trail_through(&rig, &mut play, &mut client, &mut lobby).await;
    assert_eq!(got, sent);
}

#[tokio::test]
async fn a_pumpkin_brand_keeps_drop_and_the_repair() {
    let (rig, cmds) = Rig::new("");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    from_backend(
        &mut play,
        &mut lobby,
        Phase::Configuration,
        &brand("Pumpkin"),
    )
    .await;
    assert_eq!(fields(&play), (SignedChatCancel::Drop, true));
    join(&mut play, &mut client, &mut lobby).await;
    let (sent, got) = trail_through(&rig, &mut play, &mut client, &mut lobby).await;
    assert_ne!(got, sent);
}

#[tokio::test]
async fn a_brand_in_play_changes_nothing() {
    let (rig, cmds) = Rig::new("");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    from_backend(&mut play, &mut lobby, Phase::Play, &brand("vanilla")).await;
    assert_eq!(fields(&play), (SignedChatCancel::Drop, true));
}

/// The proxy keeps the engine the first brand named: the next player gets
/// it when the server is attached, before any brand.
#[tokio::test]
async fn the_next_player_gets_the_engine_when_the_server_is_attached() {
    let (rig, cmds) = Rig::new("");
    let (mut play, _client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    from_backend(
        &mut play,
        &mut lobby,
        Phase::Configuration,
        &brand("vanilla"),
    )
    .await;
    let (next, cmds) = rig.another(2);
    let (mut play, _client) = next.session(cmds);
    let _lobby = attach(&mut play, "lobby");
    assert_eq!(fields(&play), (SignedChatCancel::Kick, false));
}

/// What a session saw itself counts for it, whatever the proxy keeps.
#[tokio::test]
async fn a_session_follows_the_brand_of_its_own_connection() {
    let (rig, cmds) = Rig::new("");
    let (mut first, _client) = rig.session(cmds);
    let mut lobby = attach(&mut first, "lobby");
    from_backend(
        &mut first,
        &mut lobby,
        Phase::Configuration,
        &brand("vanilla"),
    )
    .await;
    let (next, cmds) = rig.another(2);
    let (mut play, mut client) = next.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    from_backend(
        &mut play,
        &mut lobby,
        Phase::Configuration,
        &brand("Pumpkin"),
    )
    .await;
    assert_eq!(fields(&play), (SignedChatCancel::Drop, true));
    join(&mut play, &mut client, &mut lobby).await;
    let (sent, got) = trail_through(&next, &mut play, &mut client, &mut lobby).await;
    assert_ne!(got, sent);
}

#[tokio::test]
async fn the_entrys_chat_keys_win_over_a_recognised_brand() {
    let (rig, cmds) = Rig::lobby(", signed-chat-cancel: drop", "");
    let (mut play, _client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    from_backend(
        &mut play,
        &mut lobby,
        Phase::Configuration,
        &brand("vanilla"),
    )
    .await;
    assert_eq!(fields(&play), (SignedChatCancel::Drop, false));
}
