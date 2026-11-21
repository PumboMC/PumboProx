//! The repair of a `trail` particle without options on a server connection
//! without a translator: which client versions get it and in which layout,
//! that the proxy's own filters come first and what it reports; and what an
//! engine's packet filter sees and does.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use pumbo_core::engine::{Filtered, PacketFilter, Repair};
use pumbo_protocol::packets::common::KeepAlive;
use pumbo_protocol::packets::world::SetTime;
use pumbo_protocol::types::WriteExt;
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion, RawFrame, VersionModule};

use super::Quirks;
use super::quirks::delivered;
use super::test_rig::{
    Rig, attach, backend_reads, finish_configuration, from_backend, join, login, to_client,
};

/// A `level_particles` payload at (10.5, 64.5, -3.5): in the 26.3 layout
/// (`particle_first`) the particle and its options lead, in 26.2's they
/// come last.
pub(super) fn level_particles(particle_first: bool, particle: i32, options: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    if particle_first {
        p.put_varint(particle);
        p.put_bytes(options);
    }
    p.put_bytes(&[0, 0]);
    [10.5, 64.5, -3.5].iter().for_each(|v| p.put_f64(*v));
    if particle_first {
        (0..6).for_each(|_| p.put_f32(0.0));
        p.put_varint(1);
        p.put_varint(0);
    } else {
        (0..4).for_each(|_| p.put_f32(0.0));
        p.put_i32(1);
        p.put_varint(particle);
        p.put_bytes(options);
    }
    p
}

/// The options vanilla gives that trail: 1.5 blocks up, opening color, 20 ticks.
pub(super) fn trail_options() -> Vec<u8> {
    let mut o = Vec::new();
    [10.5, 66.0, -3.5].iter().for_each(|v| o.put_f64(*v));
    o.put_i32(0x00FC_7812);
    o.put_varint(20);
    o
}

pub(super) fn particle(v: ProtocolVersion, name: &str) -> i32 {
    let tables = pumbo_data::tables(v).unwrap();
    let id = tables.registry("particle_type").unwrap().id(name).unwrap();
    i32::try_from(id).unwrap()
}

fn particles_id(m: &dyn VersionModule) -> i32 {
    let (pl, cb) = (Phase::Play, Direction::Clientbound);
    m.packet_id(pl, cb, PacketKind::LevelParticles).unwrap()
}

fn fixed(rig: &Rig) -> u64 {
    rig.proxy.metrics.fixed_trails.load(Ordering::Relaxed)
}

/// What a client of version `v` gets when its server sends `payloads` as
/// `level_particles` after the play login, and the repairs counted.
async fn relayed(v: ProtocolVersion, payloads: &[Vec<u8>]) -> (Vec<Vec<u8>>, u64) {
    let (mut rig, cmds) = Rig::new("");
    rig.module = rig.proxy.versions.get(v).cloned().unwrap();
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let id = particles_id(&*rig.module);
    for p in payloads {
        lobby.send_raw(id, p).await;
        assert!(backend_reads(&mut play).await.is_none());
    }
    let got = to_client(&mut play, &mut client).await;
    assert!(got.iter().all(|f| f.id == id));
    let got = got.into_iter().map(|f| f.payload.to_vec()).collect();
    (got, fixed(&rig))
}

/// The log of this thread while the guard lives.
fn capture_logs() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<u8>>>) {
    // With one live subscriber, tracing asks only the current thread whether
    // a log line met for the first time is wanted, so a line another test
    // reaches first would stay off here. A second one that lives as long as
    // the tests makes it ask every subscriber.
    static SECOND: OnceLock<tracing::Dispatch> = OnceLock::new();
    SECOND.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let log = Arc::new(Mutex::new(Vec::new()));
    let w = log.clone();
    let sub = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || Buf(w.clone()))
        .finish();
    (tracing::subscriber::set_default(sub), log)
}

#[tokio::test]
async fn a_26_2_client_gets_the_trail_repaired_in_its_layout() {
    let v = ProtocolVersion::V776;
    let trail = particle(v, "trail");
    let broken = level_particles(false, trail, &[]);
    let correct = level_particles(false, trail, &trail_options());
    let flame = level_particles(false, particle(v, "flame"), &[]);
    let sent = [broken.clone(), correct.clone(), flame.clone(), broken];
    let (got, fixed) = relayed(v, &sent).await;
    assert_eq!(got, [correct.clone(), correct.clone(), flame, correct]);
    assert_eq!(fixed, 2);
}

#[tokio::test]
async fn an_older_client_without_a_translator_gets_no_repair() {
    let v = ProtocolVersion::V775;
    let broken = level_particles(false, particle(v, "trail"), &[]);
    let (got, fixed) = relayed(v, std::slice::from_ref(&broken)).await;
    assert_eq!(got, [broken]);
    assert_eq!(fixed, 0);
}

#[tokio::test]
async fn a_particle_before_the_play_login_is_dropped_uncounted() {
    let (rig, cmds) = Rig::new("");
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    finish_configuration(&mut play, &mut client, &mut lobby).await;
    let broken = level_particles(true, particle(ProtocolVersion::V777, "trail"), &[]);
    lobby.send_raw(particles_id(&*rig.module), &broken).await;
    assert!(backend_reads(&mut play).await.is_none());
    assert!(to_client(&mut play, &mut client).await.is_empty());
    assert_eq!(fixed(&rig), 0);
}

#[tokio::test]
async fn the_first_repair_of_a_server_is_logged_once() {
    let (_guard, log) = capture_logs();
    let v = ProtocolVersion::V777;
    let broken = level_particles(true, particle(v, "trail"), &[]);
    let (_, fixed) = relayed(v, &[broken.clone(), broken]).await;
    assert_eq!(fixed, 2);
    let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("trail")).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(
        lines[0].ends_with(
            "fixed a malformed trail particle from lobby (Pumpkin issue #3065); this stops by itself once the server sends correct packets"
        ),
        "{log}"
    );
}

/// A repair without a counter of its own.
static ODD: Repair = Repair {
    kind: "odd",
    what: "an odd packet",
    cause: "a test",
};

/// What a filter was shown.
type Seen = Arc<Mutex<Vec<(Phase, PacketKind)>>>;

/// A filter that wants `wants`, notes what it is shown and replaces it.
struct Swap {
    wants: &'static [PacketKind],
    seen: Seen,
}

impl Swap {
    fn quirks(wants: &'static [PacketKind]) -> (Quirks, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let swap = Swap {
            wants,
            seen: seen.clone(),
        };
        (Quirks::new(Box::new(swap)), seen)
    }
}

impl PacketFilter for Swap {
    fn interests(&self) -> &'static [PacketKind] {
        self.wants
    }

    fn backend_to_client(&mut self, phase: Phase, kind: PacketKind, _: &RawFrame) -> Filtered {
        self.seen.lock().unwrap().push((phase, kind));
        Filtered::Replace {
            payload: vec![1, 2, 3],
            repair: &ODD,
        }
    }
}

#[tokio::test]
async fn a_filter_gets_the_kinds_it_wants_and_its_payload_goes_on() {
    let (rig, cmds) = Rig::new("");
    let pl = Phase::Play;
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    join(&mut play, &mut client, &mut lobby).await;
    let (quirks, seen) = Swap::quirks(&[PacketKind::SetTime]);
    play.backend.as_mut().unwrap().quirks = Some(quirks);
    let time = SetTime {
        world_age: 1,
        time_of_day: 1,
        ticking: true,
        clocks: Vec::new(),
    };
    from_backend(&mut play, &mut lobby, pl, &time).await;
    from_backend(&mut play, &mut lobby, pl, &KeepAlive { id: 9 }).await;
    let flame = level_particles(true, particle(ProtocolVersion::V777, "flame"), &[]);
    lobby.send_raw(particles_id(&*rig.module), &flame).await;
    assert!(backend_reads(&mut play).await.is_none());
    assert_eq!(*seen.lock().unwrap(), [(pl, PacketKind::SetTime)]);
    let got = to_client(&mut play, &mut client).await;
    let set_time = rig
        .module
        .packet_id(pl, Direction::Clientbound, PacketKind::SetTime);
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(
        (Some(got[0].id), &got[0].payload[..]),
        (set_time, &[1, 2, 3][..])
    );
    assert_eq!(got[2].payload, flame);
    // A repair without a counter counts nowhere.
    assert_eq!(fixed(&rig), 0);
}

#[tokio::test]
async fn a_filter_sees_nothing_before_the_servers_login() {
    let (rig, cmds) = Rig::new("");
    let (cfg, pl) = (Phase::Configuration, Phase::Play);
    let (mut play, mut client) = rig.session(cmds);
    let mut lobby = attach(&mut play, "lobby");
    let (quirks, seen) = Swap::quirks(&[PacketKind::KeepAlive]);
    play.backend.as_mut().unwrap().quirks = Some(quirks);
    from_backend(&mut play, &mut lobby, cfg, &KeepAlive { id: 1 }).await;
    finish_configuration(&mut play, &mut client, &mut lobby).await;
    from_backend(&mut play, &mut lobby, pl, &KeepAlive { id: 2 }).await;
    assert!(seen.lock().unwrap().is_empty());
    // The keep-alive in play before the login reached the client unchanged.
    let got = to_client(&mut play, &mut client).await;
    let want = super::test_rig::encode(&*rig.module, Direction::Clientbound, &KeepAlive { id: 2 });
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].payload, want);
    from_backend(&mut play, &mut lobby, pl, &login()).await;
    from_backend(&mut play, &mut lobby, pl, &KeepAlive { id: 3 }).await;
    assert_eq!(*seen.lock().unwrap(), [(pl, PacketKind::KeepAlive)]);
}

#[test]
fn a_filter_may_want_only_what_reaches_it() {
    let reach = [PacketKind::KeepAlive, PacketKind::LevelParticles];
    assert!(delivered(&reach));
    for never in [
        PacketKind::RegistryData,
        PacketKind::Login,
        PacketKind::Disconnect,
        PacketKind::MovePlayerPos,
    ] {
        assert!(!delivered(&[never]), "{never:?}");
    }
}
