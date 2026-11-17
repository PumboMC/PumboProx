//! The old plugin channel of backends inside a session: answered in play,
//! never in configuration or when off, and never passed on to the client.

use pumbo_protocol::packets::common::ClientboundCustomPayload;
use pumbo_protocol::{Direction, Phase};

use crate::messaging;

use super::test_rig::{
    Rig, attach, finish_configuration, from_backend, join, login, messages, to_backend, to_client,
};

fn message(channel: &str, parts: &[&str]) -> ClientboundCustomPayload {
    ClientboundCustomPayload {
        channel: channel.into(),
        data: messaging::request(parts),
    }
}

#[tokio::test]
async fn the_messaging_channel_answers_the_backend_in_play() {
    let (rig, cmds) = Rig::new("");
    let m = &*rig.module;
    let (cfg, pl) = (Phase::Configuration, Phase::Play);
    let (cb, sb) = (Direction::Clientbound, Direction::Serverbound);
    let ch = messaging::CHANNEL;
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    let uuid = rig.profile.id.simple().to_string();
    // In configuration the request goes nowhere and gets no answer.
    from_backend(&mut play, &mut lobby, cfg, &message(ch, &["UUID"])).await;
    assert!(to_backend(&mut play, &mut lobby).await.is_empty());
    assert!(to_client(&mut play, &mut client).await.is_empty());
    // In play it is answered before the server's login too, without a server
    // to name yet.
    finish_configuration(&mut play, &mut client, &mut lobby).await;
    from_backend(&mut play, &mut lobby, pl, &message(ch, &["GetServer"])).await;
    assert!(to_backend(&mut play, &mut lobby).await.is_empty());
    from_backend(&mut play, &mut lobby, pl, &message(ch, &["UUID"])).await;
    let got = to_backend(&mut play, &mut lobby).await;
    let answers = messages(m, pl, sb, &got);
    assert_eq!((got.len(), answers.len()), (1, 1));
    assert_eq!(answers[0].0, ch);
    assert_eq!(
        messaging::parse_reply(&answers[0].1, 2).unwrap().0,
        ["UUID", uuid.as_str()]
    );
    from_backend(&mut play, &mut lobby, pl, &login()).await;
    let _ = to_client(&mut play, &mut client).await;
    // The client's address with its port.
    from_backend(&mut play, &mut lobby, pl, &message(ch, &["IP"])).await;
    let answers = messages(m, pl, sb, &to_backend(&mut play, &mut lobby).await);
    assert_eq!(answers.len(), 1);
    assert_eq!(
        messaging::parse_reply(&answers[0].1, 2).unwrap(),
        (vec!["IP".to_string(), "127.0.0.1".to_string()], Some(40000))
    );
    // Under both names of the channel; the answer goes on the current one.
    for name in [ch, messaging::LEGACY_CHANNEL] {
        from_backend(&mut play, &mut lobby, pl, &message(name, &["GetServer"])).await;
        let got = to_backend(&mut play, &mut lobby).await;
        let answers = messages(m, pl, sb, &got);
        assert_eq!((got.len(), answers.len()), (1, 1));
        assert_eq!(answers[0].0, ch);
        assert_eq!(
            messaging::parse_reply(&answers[0].1, 2).unwrap().0,
            ["GetServer", "lobby"]
        );
        assert!(messages(m, pl, cb, &to_client(&mut play, &mut client).await).is_empty());
    }
}

#[tokio::test]
async fn a_disabled_messaging_channel_answers_nothing() {
    let (rig, cmds) = Rig::new("messaging-channel:\n  enabled: false\n");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let ask = message(messaging::CHANNEL, &["UUID"]);
    from_backend(&mut play, &mut lobby, Phase::Play, &ask).await;
    assert!(to_backend(&mut play, &mut lobby).await.is_empty());
    assert!(to_client(&mut play, &mut client).await.is_empty());
}
