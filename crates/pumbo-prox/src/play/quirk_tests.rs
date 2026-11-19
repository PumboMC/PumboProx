//! The repair of a `trail` particle without options on a server connection
//! without a translator: which client versions get it and in which layout,
//! that the proxy's own filters come first and what it reports.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use pumbo_protocol::types::WriteExt;
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion, VersionModule};

use super::test_rig::{Rig, attach, backend_reads, finish_configuration, join, to_client};

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
fn trail_options() -> Vec<u8> {
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
