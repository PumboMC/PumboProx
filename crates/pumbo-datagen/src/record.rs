//! Recording vanilla servers (plan §2.5, second step).
//!
//! For each protocol the newest release's server runs locally (127.0.0.1,
//! offline mode, flat world) and `pumbo-testclient` plays two sessions: one
//! acknowledging the `minecraft:core` pack, which then runs the play script,
//! and one without known packs, which receives the full registry data.
//!
//! Outputs:
//! - `tables/<protocol>/synced.txt` (committed): synchronized registries with
//!   entry names in order, tags with entry IDs and the enabled feature flags;
//! - `<full>/<protocol>/` (never committed): both recordings and the full
//!   registry data for clients without known packs.

use std::io::Write as _;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pumbo_data::{DataVersion, Synced, SyncedRegistry, Tables};
use pumbo_protocol::packets::configuration::{RegistryData, UpdateEnabledFeatures, UpdateTags};
use pumbo_protocol::packets::{self, Ctx};
use pumbo_protocol::{Direction, PacketKind, Phase, VersionModule};
use pumbo_testclient::session::FINAL_KICK;
use pumbo_testclient::{Ending, Recorded, SessionOptions, recording};

use crate::Error;

pub const PLAYER: &str = "PumboRec";
/// Not on the whitelist: the server answers with `login_disconnect`.
pub const STRANGER: &str = "PumboStranger";

/// Points every Mojang API host of the server at a closed local port.
const NO_SERVICES: &[&str] = &[
    "-Dminecraft.api.auth.host=http://127.0.0.1:9",
    "-Dminecraft.api.account.host=http://127.0.0.1:9",
    "-Dminecraft.api.session.host=http://127.0.0.1:9",
    "-Dminecraft.api.services.host=http://127.0.0.1:9",
    "-Dminecraft.api.profiles.host=http://127.0.0.1:9",
    // authlib 10 (26.x) finds the services through a discovery document.
    "-Dminecraft.api.discovery.host=http://127.0.0.1:9",
];

/// Options of the `record` command.
#[derive(Debug, Clone)]
pub struct RecordOptions {
    pub cache: PathBuf,
    /// Where servers run (worlds, logs); one directory per release.
    pub work: PathBuf,
    /// Recordings and full registry data (outside git).
    pub full: PathBuf,
    /// Committed tables directory (`synced.txt` goes here).
    pub tables: PathBuf,
    pub java: String,
    /// First local port; one port per parallel server.
    pub base_port: u16,
    pub parallel: usize,
    /// Only these protocols (empty: all).
    pub only: Vec<i32>,
}

/// Result for one protocol.
#[derive(Debug)]
pub struct Recorded1 {
    pub protocol: i32,
    pub release: String,
    pub known: Ending,
    pub none: Ending,
    /// `chat_session_update` probe: any ending but a decode failure.
    pub probe: Ending,
    pub notes: Vec<String>,
    pub frames: usize,
}

fn write_file(path: &Path, content: &[u8]) -> Result<(), Error> {
    std::fs::write(path, content).map_err(|e| Error::io(path, e))
}

/// Server directory: EULA, `server.properties` (offline, 127.0.0.1, flat
/// world), operator and whitelist entry for [`PLAYER`].
pub fn prepare_dir(dir: &Path, port: u16, protocol: i32) -> Result<(), Error> {
    std::fs::create_dir_all(dir.join("codeofconduct")).map_err(|e| Error::io(dir, e))?;
    write_file(&dir.join("eula.txt"), b"eula=true\n")?;
    let props = format!(
        "server-ip=127.0.0.1\nserver-port={port}\nonline-mode=false\nenforce-secure-profile=false\n\
         level-type=minecraft\\:flat\ngenerate-structures=false\nspawn-protection=0\nview-distance=3\n\
         simulation-distance=3\nmax-players=5\nmotd=pumbo-datagen\nsync-chunk-writes=false\n\
         log-ips=false\nenable-rcon=false\nenable-query=false\npause-when-empty-seconds=-1\n\
         bug-report-link=https\\://example.org/bugs\n\
         resource-pack=https\\://example.invalid/pack.zip\n\
         resource-pack-sha1=0123456789abcdef0123456789abcdef01234567\n\
         require-resource-pack=false\nenable-code-of-conduct=true\n\
         network-compression-threshold=256\nwhite-list=true\nenforce-whitelist=true\n"
    );
    write_file(&dir.join("server.properties"), props.as_bytes())?;
    let uuid = pumbo_identity::offline_uuid(PLAYER);
    let ops = format!(
        "[{{\"uuid\":\"{uuid}\",\"name\":\"{PLAYER}\",\"level\":4,\"bypassesPlayerLimit\":false}}]\n"
    );
    write_file(&dir.join("ops.json"), ops.as_bytes())?;
    let whitelist = format!("[{{\"uuid\":\"{uuid}\",\"name\":\"{PLAYER}\"}}]\n");
    write_file(&dir.join("whitelist.json"), whitelist.as_bytes())?;
    write_file(
        &dir.join("codeofconduct").join("en_us.txt"),
        format!("Be nice. (pumbo-datagen, protocol {protocol})\n").as_bytes(),
    )?;
    Ok(())
}

/// A vanilla server process, stopped by its own PID only.
pub struct Server {
    child: Child,
    pub pid: u32,
}

impl Server {
    pub fn start(dir: &Path, jar: &Path, java: &str) -> Result<Self, Error> {
        let log = std::fs::File::create(dir.join("server-output.log"))
            .map_err(|e| Error::io(&dir.join("server-output.log"), e))?;
        let log_err = log.try_clone().map_err(|e| Error::io(dir, e))?;
        let child = Command::new(java)
            .current_dir(dir)
            .args(["-Xms256M", "-Xmx1G"])
            // Keep the server off Mojang's services: nothing in a recording
            // needs them, and the server should not reach the network.
            .args(NO_SERVICES)
            .arg("-jar")
            .arg(jar)
            .arg("nogui")
            .stdin(Stdio::piped())
            .stdout(log)
            .stderr(log_err)
            .spawn()
            .map_err(|e| Error::Tool(format!("{java}: {e}")))?;
        let pid = child.id();
        Ok(Self { child, pid })
    }

    pub fn wait_ready(&mut self, addr: SocketAddr, limit: Duration) -> Result<(), Error> {
        let start = Instant::now();
        while start.elapsed() < limit {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(Error::Tool(format!(
                    "server (pid {}) exited early: {status}",
                    self.pid
                )));
            }
            if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err(Error::Tool(format!(
            "server (pid {}) not ready on {addr} after {limit:?}",
            self.pid
        )))
    }

    /// A console command.
    pub fn command(&mut self, cmd: &str) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = stdin.write_all(format!("{cmd}\n").as_bytes());
            let _ = stdin.flush();
        }
    }

    /// `stop` on the console, then a kill of this PID if it does not exit.
    pub fn stop(mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = stdin.write_all(b"stop\n");
            let _ = stdin.flush();
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(60) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        eprintln!("server pid {} did not stop, killing it", self.pid);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Synchronized registries, tags and features from a known-packs session.
pub fn synced_from(frames: &[Recorded], module: &dyn VersionModule) -> Result<Synced, Error> {
    let ctx = Ctx::new(module, Direction::Clientbound);
    let mut synced = Synced::default();
    for f in frames {
        if f.phase != Phase::Configuration || f.direction != Direction::Clientbound {
            continue;
        }
        match module.packet_kind(f.phase, f.direction, f.id) {
            Some(PacketKind::RegistryData) => {
                let r: RegistryData = packets::decode(&f.payload, &ctx)
                    .map_err(|e| Error::Json(format!("registry_data: {e}")))?;
                synced.registries.push(SyncedRegistry {
                    name: r.registry,
                    entries: r.entries.into_iter().map(|e| e.id).collect(),
                });
            }
            Some(PacketKind::UpdateTags) => {
                let t: UpdateTags = packets::decode(&f.payload, &ctx)
                    .map_err(|e| Error::Json(format!("update_tags: {e}")))?;
                synced.tags = t
                    .registries
                    .into_iter()
                    .map(|r| (r.registry, r.tags))
                    .collect();
            }
            Some(PacketKind::UpdateEnabledFeatures) => {
                let u: UpdateEnabledFeatures = packets::decode(&f.payload, &ctx)
                    .map_err(|e| Error::Json(format!("update_enabled_features: {e}")))?;
                synced.features = u.features;
            }
            _ => {}
        }
    }
    // Vanilla sends tags in hash-map order; sort so reruns give the same file.
    synced.tags.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, tags) in &mut synced.tags {
        tags.sort_by(|a, b| a.0.cmp(&b.0));
    }
    Ok(synced)
}

/// Rewrites `synced.txt` of every protocol from existing recordings (no
/// servers needed). Returns the protocols written.
pub fn resync(full: &Path, tables_dir: &Path) -> Result<Vec<i32>, Error> {
    let mut done = Vec::new();
    for v in pumbo_data::protocols() {
        let path = full.join(v.0.to_string()).join("session-known.rec");
        let Ok(frames) = recording::read(&path) else {
            continue;
        };
        let module = DataVersion::new(pumbo_data::tables(v)?);
        let synced = synced_from(&frames, &module)?;
        let out = tables_dir.join(v.0.to_string()).join("synced.txt");
        write_file(&out, synced.to_text().as_bytes())?;
        done.push(v.0);
    }
    Ok(done)
}

fn record_one(
    opts: &RecordOptions,
    tables: &'static Tables,
    port: u16,
) -> Result<Recorded1, Error> {
    let protocol = tables.protocol.0;
    let release = tables
        .releases
        .last()
        .map(|r| r.name.clone())
        .ok_or_else(|| Error::Tool(format!("protocol {protocol} without releases")))?;
    let jar = opts.cache.join(&release).join("server.jar");
    if !jar.is_file() {
        return Err(Error::Tool(format!(
            "{}: missing, run `pumbo-datagen tables` first",
            jar.display()
        )));
    }
    let dir = opts.work.join(&release);
    prepare_dir(&dir, port, protocol)?;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut server = Server::start(&dir, &jar, &opts.java)?;
    eprintln!("{release}: server pid {} on {addr}", server.pid);
    let ready = server.wait_ready(addr, Duration::from_secs(240));
    let sessions = ready.and_then(|()| run_sessions(addr, tables));
    server.stop();
    let (known, none, probe) = sessions?;

    let module = DataVersion::new(tables);
    let out = opts.full.join(protocol.to_string());
    std::fs::create_dir_all(&out).map_err(|e| Error::io(&out, e))?;
    recording::write(&out.join("session-known.rec"), &known.frames)
        .map_err(|e| Error::io(&out, e))?;
    recording::write(&out.join("session-none.rec"), &none.frames)
        .map_err(|e| Error::io(&out, e))?;
    // Full registry data for clients without known packs.
    let full: Vec<Recorded> = none
        .frames
        .iter()
        .filter(|f| {
            f.phase == Phase::Configuration
                && f.direction == Direction::Clientbound
                && matches!(
                    module.packet_kind(f.phase, f.direction, f.id),
                    Some(PacketKind::RegistryData | PacketKind::UpdateTags)
                )
        })
        .cloned()
        .collect();
    recording::write(&out.join("registries-full.rec"), &full).map_err(|e| Error::io(&out, e))?;

    let synced = synced_from(&known.frames, &module)?;
    let path = opts.tables.join(protocol.to_string()).join("synced.txt");
    write_file(&path, synced.to_text().as_bytes())?;
    recording::write(&out.join("session-probe.rec"), &probe.frames)
        .map_err(|e| Error::io(&out, e))?;
    let mut notes = known.notes;
    notes.extend(none.notes.into_iter().map(|n| format!("(no packs) {n}")));
    notes.push(format!("chat_session_update probe: {:?}", probe.ending));
    notes.extend(probe.notes);
    Ok(Recorded1 {
        protocol,
        release,
        known: known.ending,
        none: none.ending,
        probe: probe.ending,
        notes,
        frames: known.frames.len() + none.frames.len() + probe.frames.len(),
    })
}

fn run_sessions(
    addr: SocketAddr,
    tables: &'static Tables,
) -> Result<
    (
        pumbo_testclient::SessionOutcome,
        pumbo_testclient::SessionOutcome,
        pumbo_testclient::SessionOutcome,
    ),
    Error,
> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Tool(format!("tokio: {e}")))?;
    let module: Arc<dyn VersionModule> = Arc::new(DataVersion::new(tables));
    rt.block_on(async {
        let known = pumbo_testclient::run(
            addr,
            module.clone(),
            &SessionOptions {
                name: PLAYER.into(),
                acknowledge_core: true,
                play: true,
                wait_keep_alive: true,
                session_update_probe: false,
                ..SessionOptions::default()
            },
        )
        .await
        .map_err(|e| Error::Tool(format!("session: {e}")))?;
        let none = pumbo_testclient::run(
            addr,
            module.clone(),
            &SessionOptions {
                name: PLAYER.into(),
                acknowledge_core: false,
                play: false,
                wait_keep_alive: false,
                session_update_probe: false,
                ..SessionOptions::default()
            },
        )
        .await
        .map_err(|e| Error::Tool(format!("session: {e}")))?;
        let probe = pumbo_testclient::run(
            addr,
            module.clone(),
            &SessionOptions {
                name: PLAYER.into(),
                acknowledge_core: true,
                play: true,
                wait_keep_alive: false,
                session_update_probe: true,
                ..SessionOptions::default()
            },
        )
        .await
        .map_err(|e| Error::Tool(format!("session: {e}")))?;
        let stranger = pumbo_testclient::run(
            addr,
            module.clone(),
            &SessionOptions {
                name: STRANGER.into(),
                acknowledge_core: true,
                play: false,
                wait_keep_alive: false,
                session_update_probe: false,
                ..SessionOptions::default()
            },
        )
        .await
        .map_err(|e| Error::Tool(format!("session: {e}")))?;
        let (status_frames, status_json) = pumbo_testclient::status(addr, module)
            .await
            .map_err(|e| Error::Tool(format!("status: {e}")))?;
        let mut probe = probe;
        probe.notes.push(format!(
            "whitelist session: {:?}; status: {} bytes of JSON",
            stranger.ending,
            status_json.len()
        ));
        // The extra sessions go into the probe recording.
        probe.frames.extend(stranger.frames);
        probe.frames.extend(status_frames);
        Ok((known, none, probe))
    })
}

/// Records every protocol with committed tables. Returns per-protocol results
/// (errors included) so one broken version does not hide the others.
pub fn record(opts: &RecordOptions) -> Vec<(i32, Result<Recorded1, Error>)> {
    let protocols: Vec<&'static Tables> = pumbo_data::protocols()
        .filter(|p| opts.only.is_empty() || opts.only.contains(&p.0))
        .filter_map(|p| pumbo_data::tables(p).ok())
        .collect();
    let mut results = Vec::new();
    for chunk in protocols.chunks(opts.parallel.max(1)) {
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let port = opts.base_port + u16::try_from(i).unwrap_or(0);
                    (t.protocol.0, s.spawn(move || record_one(opts, t, port)))
                })
                .collect();
            for (p, h) in handles {
                let r = h
                    .join()
                    .unwrap_or_else(|_| Err(Error::Tool("recorder thread panicked".into())));
                results.push((p, r));
            }
        });
    }
    results
}

/// True if the play script ran to its final kick.
pub fn script_completed(r: &Recorded1) -> bool {
    let probe_decoded = match &r.probe {
        Ending::Finished => true,
        Ending::Disconnected(reason) => !reason.to_lowercase().contains("decode"),
    };
    r.known == Ending::Disconnected(FINAL_KICK.into())
        && r.none == Ending::Finished
        && probe_decoded
}
