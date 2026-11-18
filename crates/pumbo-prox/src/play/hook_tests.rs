//! Session hooks in a session: which frames they see and in which phase, what
//! their verdicts and output do, and when `on_joined` runs.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pumbo_core::hook::{HookCx, HookInterests, SessionHook, Verdict};
use pumbo_protocol::packets::common::{
    ClientboundCustomPayload, KeepAlive, MAX_PAYLOAD_FROM_SERVER,
};
use pumbo_protocol::packets::world::{ChunkBatchReceived, SetTime, Sound, SoundRef};
use pumbo_protocol::types::{Reader, WriteExt};
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion, RawFrame};
use tokio::time::Instant;

use super::hooks::Hooks;
use super::test_rig::{
    Rig, accept, attach, backend_reads, client_reads, ended, finish_configuration, from_backend,
    from_client, in_virtual, join, login, messages, to_backend, to_client,
};
use super::{Play, Purpose};

/// A hook for tests: notes what it sees and answers with `react`.
struct Recorder {
    wants: HookInterests,
    seen: Arc<Mutex<Vec<String>>>,
    react: Box<dyn FnMut(&mut HookCx<'_>) -> Verdict + Send>,
}

impl Recorder {
    fn new(wants: HookInterests) -> (Self, Arc<Mutex<Vec<String>>>) {
        Self::reacting(wants, |_| Verdict::Forward)
    }

    fn reacting(
        wants: HookInterests,
        react: impl FnMut(&mut HookCx<'_>) -> Verdict + Send + 'static,
    ) -> (Self, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let r = Recorder {
            wants,
            seen: seen.clone(),
            react: Box::new(react),
        };
        (r, seen)
    }

    fn note(&self, side: &str, d: Direction, cx: &HookCx<'_>, f: &RawFrame) {
        let what = match cx.module.packet_kind(cx.phase, d, f.id) {
            Some(PacketKind::CustomPayload) => {
                let channel = Reader::new(&f.payload).identifier().unwrap();
                format!("custom_payload {channel}")
            }
            Some(k) => k.name().to_string(),
            None => format!("packet {}", f.id),
        };
        let line = format!("{side} {what} in {:?}", cx.phase);
        self.seen.lock().unwrap().push(line);
    }
}

impl SessionHook for Recorder {
    fn interests(&self) -> HookInterests {
        self.wants.clone()
    }

    fn on_client(&mut self, cx: &mut HookCx<'_>, f: &RawFrame) -> Verdict {
        self.note("client", Direction::Serverbound, cx, f);
        (self.react)(cx)
    }

    fn on_backend(&mut self, cx: &mut HookCx<'_>, f: &RawFrame) -> Verdict {
        self.note("backend", Direction::Clientbound, cx, f);
        (self.react)(cx)
    }

    fn on_joined(&mut self, cx: &mut HookCx<'_>) {
        let line = format!("joined {}", cx.server.unwrap_or("nowhere"));
        self.seen.lock().unwrap().push(line);
        let _ = (self.react)(cx);
    }
}

fn install(play: &mut Play<'_>, hooks: Vec<Recorder>) {
    let hooks = hooks
        .into_iter()
        .map(|h| Box::new(h) as Box<dyn SessionHook>)
        .collect();
    play.hooks = Hooks::new(hooks);
}

fn seen(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    log.lock().unwrap().clone()
}

fn sound() -> Sound {
    Sound {
        sound: SoundRef::Id(1),
        category: 0,
        x: 0,
        y: 64,
        z: 0,
        volume: 1.0,
        pitch: 1.0,
        seed: 7,
    }
}

fn chunks_read() -> ChunkBatchReceived {
    ChunkBatchReceived {
        chunks_per_tick: 9.0,
    }
}

fn channels(names: &[&'static str]) -> Cow<'static, [Cow<'static, str>]> {
    names
        .iter()
        .map(|n| Cow::Borrowed(*n))
        .collect::<Vec<_>>()
        .into()
}

fn dropped(rig: &Rig) -> u64 {
    rig.proxy
        .metrics
        .dropped_payloads
        .load(std::sync::atomic::Ordering::Relaxed)
}

fn payload(channel: &str) -> ClientboundCustomPayload {
    ClientboundCustomPayload {
        channel: channel.into(),
        data: vec![1, 2, 3],
    }
}

#[tokio::test]
async fn hooks_see_only_what_they_asked_for() {
    let (rig, cmds) = Rig::new("");
    let (pl, cb) = (Phase::Play, Direction::Clientbound);
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let (rec, log) = Recorder::new(HookInterests {
        client: &[PacketKind::ChunkBatchReceived],
        backend: &[PacketKind::Sound],
        backend_channels: channels(&["pumbo:test"]),
    });
    install(&mut play, vec![rec]);
    from_backend(&mut play, &mut lobby, pl, &sound()).await;
    let time = SetTime {
        world_age: 1,
        time_of_day: 1,
        ticking: true,
        clocks: Vec::new(),
    };
    from_backend(&mut play, &mut lobby, pl, &time).await;
    let tables = pumbo_data::tables(ProtocolVersion::V777).unwrap();
    let entity = tables.packet_id(pl, cb, "move_entity_pos").unwrap();
    lobby.send_raw(entity, &[5, 0, 1, 0, 2, 0, 3, 1]).await;
    assert!(backend_reads(&mut play).await.is_none());
    from_backend(&mut play, &mut lobby, pl, &KeepAlive { id: 9 }).await;
    from_backend(&mut play, &mut lobby, pl, &payload("pumbo:other")).await;
    from_backend(&mut play, &mut lobby, pl, &payload("pumbo:test")).await;
    from_client(&mut play, &mut client, pl, &chunks_read()).await;
    from_client(&mut play, &mut client, pl, &accept(1)).await;
    assert_eq!(
        seen(&log),
        [
            "backend sound in Play",
            "backend custom_payload pumbo:test in Play",
            "client chunk_batch_received in Play",
        ]
    );
    // The hook let everything through.
    assert_eq!(to_client(&mut play, &mut client).await.len(), 6);
    assert_eq!(to_backend(&mut play, &mut lobby).await.len(), 2);
}

#[tokio::test]
async fn kind_hooks_come_after_the_core_filters() {
    let (rig, cmds) = Rig::new("");
    let pl = Phase::Play;
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    let (rec, log) = Recorder::new(HookInterests {
        client: &[PacketKind::ChunkBatchReceived],
        backend: &[PacketKind::Sound],
        ..HookInterests::default()
    });
    install(&mut play, vec![rec]);
    finish_configuration(&mut play, &mut client, &mut lobby).await;
    // Before the play login the proxy drops the server's world packets.
    from_backend(&mut play, &mut lobby, pl, &sound()).await;
    from_backend(&mut play, &mut lobby, pl, &login()).await;
    assert_eq!(seen(&log), ["joined lobby"]);
    // While the client leaves its server, nothing of either side passes.
    play.ack_deadline = Some(Instant::now() + Duration::from_secs(5));
    from_backend(&mut play, &mut lobby, pl, &sound()).await;
    from_client(&mut play, &mut client, pl, &chunks_read()).await;
    play.ack_deadline = None;
    // A virtual world takes the client's packets itself.
    play.vworld = Some(in_virtual(&rig));
    from_client(&mut play, &mut client, pl, &chunks_read()).await;
    assert_eq!(seen(&log), ["joined lobby"]);
}

#[tokio::test]
async fn channel_hooks_see_every_phase_and_fall_through() {
    let (rig, cmds) = Rig::new("");
    let m = &*rig.module;
    let (cfg, pl, cb) = (Phase::Configuration, Phase::Play, Direction::Clientbound);
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    let (rec, log) = Recorder::new(HookInterests {
        backend_channels: channels(&["pumbo:test", "pumbo:bridge/test"]),
        ..HookInterests::default()
    });
    install(&mut play, vec![rec]);
    from_backend(&mut play, &mut lobby, cfg, &payload("pumbo:test")).await;
    let got = messages(m, cfg, cb, &to_client(&mut play, &mut client).await);
    assert_eq!(got, [("pumbo:test".to_string(), vec![1, 2, 3])]);
    finish_configuration(&mut play, &mut client, &mut lobby).await;
    from_backend(&mut play, &mut lobby, pl, &payload("pumbo:test")).await;
    // A bridge channel that a hook lets through still stops at the proxy.
    let before = dropped(&rig);
    from_backend(&mut play, &mut lobby, pl, &payload("pumbo:bridge/test")).await;
    assert_eq!(dropped(&rig), before + 1);
    let got = messages(m, pl, cb, &to_client(&mut play, &mut client).await);
    assert_eq!(got, [("pumbo:test".to_string(), vec![1, 2, 3])]);
    assert_eq!(
        seen(&log),
        [
            "backend custom_payload pumbo:test in Configuration",
            "backend custom_payload pumbo:test in Play",
            "backend custom_payload pumbo:bridge/test in Play",
        ]
    );
}

/// A channel hook that lets a message through does not lift the 1 MiB limit
/// on plugin messages from the server.
#[tokio::test]
async fn a_channel_hook_keeps_the_size_limit() {
    let (rig, cmds) = Rig::new("");
    let (pl, cb) = (Phase::Play, Direction::Clientbound);
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let (rec, log) = Recorder::new(HookInterests {
        backend_channels: channels(&["pumbo:test"]),
        ..HookInterests::default()
    });
    install(&mut play, vec![rec]);
    let mut big = Vec::new();
    big.put_string("pumbo:test", 32_767).unwrap();
    big.extend(std::iter::repeat_n(7u8, MAX_PAYLOAD_FROM_SERVER + 1));
    let id = rig
        .module
        .packet_id(pl, cb, PacketKind::CustomPayload)
        .unwrap();
    let before = dropped(&rig);
    lobby.send_raw(id, &big).await;
    let end = backend_reads(&mut play).await;
    assert!(end.is_none(), "{}", ended(&end));
    assert_eq!(seen(&log), ["backend custom_payload pumbo:test in Play"]);
    assert!(to_client(&mut play, &mut client).await.is_empty());
    assert_eq!(dropped(&rig), before + 1);
}

#[tokio::test]
async fn a_hook_gets_a_frame_once() {
    let (rig, cmds) = Rig::new("");
    let pl = Phase::Play;
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let (rec, log) = Recorder::new(HookInterests {
        backend: &[PacketKind::CustomPayload],
        backend_channels: channels(&["pumbo:test"]),
        ..HookInterests::default()
    });
    install(&mut play, vec![rec]);
    from_backend(&mut play, &mut lobby, pl, &payload("pumbo:test")).await;
    from_backend(&mut play, &mut lobby, pl, &payload("pumbo:other")).await;
    assert_eq!(
        seen(&log),
        [
            "backend custom_payload pumbo:test in Play",
            "backend custom_payload pumbo:other in Play",
        ]
    );
}

#[tokio::test]
async fn hook_verdicts() {
    let sounds = HookInterests {
        backend: &[PacketKind::Sound],
        ..HookInterests::default()
    };
    let pl = Phase::Play;
    let (rig, cmds) = Rig::new("");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let (first, first_log) = Recorder::reacting(sounds.clone(), |_| Verdict::Drop);
    let (second, second_log) = Recorder::new(sounds.clone());
    install(&mut play, vec![first, second]);
    from_backend(&mut play, &mut lobby, pl, &sound()).await;
    assert!(to_client(&mut play, &mut client).await.is_empty());
    assert_eq!(seen(&first_log), ["backend sound in Play"]);
    assert!(seen(&second_log).is_empty());
    let (ender, _) = Recorder::reacting(sounds, |_| Verdict::End("x".into()));
    install(&mut play, vec![ender]);
    lobby.send(pl, &sound()).await;
    let end = backend_reads(&mut play).await;
    assert_eq!(ended(&end), "protocol: backend: Protocol(\"x\")");
    // From the client the reason stays as it is.
    let (rig, cmds) = Rig::new("");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let wants = HookInterests {
        client: &[PacketKind::ChunkBatchReceived],
        ..HookInterests::default()
    };
    let (ender, _) = Recorder::reacting(wants, |_| Verdict::End("x".into()));
    install(&mut play, vec![ender]);
    client.send(pl, &chunks_read()).await;
    let end = client_reads(&mut play).await;
    assert_eq!(ended(&end), "protocol: x");
}

#[tokio::test]
async fn hook_output_goes_out_in_place() {
    let (rig, cmds) = Rig::new("");
    let m = &*rig.module;
    let (pl, sb) = (Phase::Play, Direction::Serverbound);
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let sounds = HookInterests {
        backend: &[PacketKind::Sound],
        ..HookInterests::default()
    };
    let (rec, _) = Recorder::reacting(sounds.clone(), |cx| {
        cx.out.to_backend.push(("pumbo:one".into(), vec![1]));
        cx.out.to_backend.push(("pumbo:two".into(), vec![2]));
        Verdict::Forward
    });
    install(&mut play, vec![rec]);
    from_backend(&mut play, &mut lobby, pl, &sound()).await;
    from_client(&mut play, &mut client, pl, &chunks_read()).await;
    let got = to_backend(&mut play, &mut lobby).await;
    assert_eq!(got.len(), 3);
    assert_eq!(
        messages(m, pl, sb, &got),
        [
            ("pumbo:one".to_string(), vec![1]),
            ("pumbo:two".to_string(), vec![2]),
        ]
    );
    assert_eq!(
        m.packet_kind(pl, sb, got[2].id),
        Some(PacketKind::ChunkBatchReceived)
    );
    let (rec, _) = Recorder::reacting(sounds, |cx| {
        cx.out.connect = Some("games".into());
        Verdict::Forward
    });
    install(&mut play, vec![rec]);
    from_backend(&mut play, &mut lobby, pl, &sound()).await;
    let req = play.request.as_ref().unwrap();
    assert!(matches!(req.purpose, Purpose::Switch { quiet: true }));
    assert_eq!(req.target, "games");
}

#[tokio::test]
async fn on_joined_runs_before_the_server_is_recorded() {
    let (rig, cmds) = Rig::new("");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    let (proxy, id) = (rig.proxy.clone(), rig.profile.id);
    let servers = Arc::new(Mutex::new(Vec::new()));
    let noted = servers.clone();
    let (rec, log) = Recorder::reacting(HookInterests::default(), move |cx| {
        noted.lock().unwrap().push(proxy.server_of(id));
        cx.out.connect = Some("games".into());
        Verdict::Forward
    });
    install(&mut play, vec![rec]);
    join(&mut play, &mut client, &mut lobby).await;
    assert_eq!(seen(&log), ["joined lobby"]);
    assert_eq!(*servers.lock().unwrap(), [None]);
    assert_eq!(rig.proxy.server_of(id).as_deref(), Some("lobby"));
    // Its output went out once the join was recorded: the switch started.
    let req = play.request.as_ref().unwrap();
    assert!(matches!(req.purpose, Purpose::Switch { quiet: true }));
    assert_eq!(req.target, "games");
}
