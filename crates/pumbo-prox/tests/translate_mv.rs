//! Built-in version translation (plan E3b) against real 26.3 servers: for
//! every client protocol 767–776 a test client goes through the proxy to
//! Pumpkin 0.2.0 and to vanilla 26.3, gets chunks (checked in its own
//! format), answers keep-alives for `PUMBO_MV_IDLE_SECS` (default 60),
//! chats, runs a command and gets kicked from the server console with a
//! reason. All clients of one backend play at the same time, joining 2 s
//! apart: every player spawn must follow that player's info, stacks given
//! with components (`GIVES`) must arrive, a bed, a pot and a sign placed at
//! the spawn must come in the client's layout (bed block entities before
//! 26.2), and a `tellraw` link must keep its click event in the client's
//! layout. `PUMBO_ONLY` with 777 adds a 26.3 client (no translation) first.
//! Every client digs dirt by hand, stone with a pickaxe and a log with an axe
//! placed next to it (`DIGS`), in survival, after the vanilla mining time:
//! each block must end as air, with the actions acknowledged. Player chat from
//! Pumpkin (`disguised_chat` with its own `raw` chat type) must point at a
//! `raw` entry of the client's registry. Every client shoots a bow: every arrow
//! must fall and land (`in_ground`) with the velocities a 26.3 client got.
//!
//! Ignored by default (starts a Pumpkin binary and a Java server):
//! `cargo test -p pumbo-prox --test translate_mv -- --ignored --nocapture`
//!
//! - `PUMBO_PUMPKIN_BIN`: Pumpkin 0.2.0 binary; its directory needs a
//!   `pumpkin.toml` template like the E3 tests (telemetry off, proxy off,
//!   offline); the test writes the forwarding and the address.
//! - `PUMBO_JARS`: server jars (default `~/.cache/pumbo-datagen`).
//! - `PUMBO_MV_WORK`: working directory (default `target/pumbo-mv`).
//! - `PUMBO_MV_RECORD`: also plays a 26.3 client (no translation) on the first
//!   backend and stores what it got there, for `pumbo-translate`'s benchmark.
//! - `PUMBO_ONLY`: comma-separated protocols; `PUMBO_MV_BACKENDS`: `pumpkin`, `vanilla` or both.
//!
//! `older_clients_switch_servers`: the same clients on two Pumpkin 0.2.0
//! servers, `/server` to the second and back (a new translator per backend),
//! chunks after every join, then the kick.
//!
//! Ports 25650–25653.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{module, secret_file, start_proxy, toml_path};
use pumbo_datagen::record::{Server, prepare_dir};
use pumbo_protocol::packets::common::{
    ClientInformation, KeepAlive, Ping, Pong, ResourcePackPush, ResourcePackResponse,
    ServerboundCustomPayload,
};
use pumbo_protocol::packets::configuration::{
    AcceptCodeOfConduct, FinishConfiguration, SelectKnownPacks,
};
use pumbo_protocol::packets::login::{LoginAcknowledged, LoginCompression, LoginStart};
use pumbo_protocol::packets::play::{Chat, ChatCommand, LastSeen};
use pumbo_protocol::packets::status::Intention;
use pumbo_protocol::types::WriteExt;
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion};
use pumbo_testclient::Client;

const KICK: &str = "Bye from the server";
/// Items given to every player before the kick.
const GIVES: &[&str] = &[
    "diamond_sword[enchantments={sharpness:5}]",
    "cooked_beef 5",
    "diamond_chestplate[trim={material:\"minecraft:gold\",pattern:\"minecraft:sentry\"}]",
    "player_head[profile={name:\"Notch\"}]",
    "decorated_pot",
    "bread[consumable={consume_seconds:3.0}]",
    "iron_pickaxe[tool={rules:[{blocks:\"minecraft:stone\",speed:20.0}]}]",
    "leather_helmet[equippable={slot:\"head\",equip_sound:\"item.armor.equip_gold\"}]",
];

/// Blocks each client digs: block, hotbar slot (0: iron pickaxe, 1: iron axe,
/// 8: empty hand) and vanilla mining ticks with that item.
const DIGS: &[(&str, i16, u64)] = &[("dirt", 8, 15), ("stone", 0, 8), ("oak_log", 1, 10)];

/// Where client `protocol` digs block `k` of `DIGS`: a cell two blocks around
/// its spawn `at`, one to three above its feet, different for every client.
fn dig_cell(protocol: i32, k: usize, at: [i64; 3]) -> [i64; 3] {
    let ring: Vec<(i64, i64)> = (-2..=2_i64)
        .flat_map(|dx| (-2..=2_i64).map(move |dz| (dx, dz)))
        .filter(|(dx, dz)| dx.abs().max(dz.abs()) == 2)
        .collect();
    let c = (protocol.min(777) - 767) as usize * DIGS.len() + k;
    let (dx, dz) = ring[c % ring.len()];
    [at[0] + dx, at[1] + 1 + (c / ring.len()) as i64, at[2] + dz]
}

fn block_pos(p: [i64; 3]) -> i64 {
    ((p[0] & 0x3FF_FFFF) << 38) | ((p[2] & 0x3FF_FFFF) << 12) | (p[1] & 0xFFF)
}

fn env_path(key: &str, default: &str) -> PathBuf {
    std::env::var_os(key).map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn idle() -> Duration {
    Duration::from_secs(
        std::env::var("PUMBO_MV_IDLE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60),
    )
}

fn player(protocol: i32) -> String {
    format!("Mv{protocol}")
}

// ---------------------------------------------------------------- chunk check

fn var_int(r: &mut &[u8]) -> Result<i32, String> {
    let mut v = 0u32;
    for shift in 0..5 {
        let (&b, rest) = r.split_first().ok_or("eof")?;
        *r = rest;
        v |= u32::from(b & 0x7F) << (7 * shift);
        if b & 0x80 == 0 {
            return Ok(v as i32);
        }
    }
    Err("varint".into())
}

fn take<'a>(r: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
    let (a, b) = r.split_at_checked(n).ok_or("eof")?;
    *r = b;
    Ok(a)
}

fn skip_nbt(r: &mut &[u8], tag: u8, depth: usize) -> Result<(), String> {
    if depth > 64 {
        return Err("nbt depth".into());
    }
    let be = |r: &mut &[u8], n: usize| -> Result<usize, String> {
        Ok(take(r, n)?
            .iter()
            .fold(0usize, |a, b| (a << 8) | usize::from(*b)))
    };
    match tag {
        0 => {}
        1 => drop(take(r, 1)?),
        2 => drop(take(r, 2)?),
        3 | 5 => drop(take(r, 4)?),
        4 | 6 => drop(take(r, 8)?),
        7 => {
            let n = be(r, 4)?;
            take(r, n)?;
        }
        8 => {
            let n = be(r, 2)?;
            take(r, n)?;
        }
        9 => {
            let t = take(r, 1)?[0];
            for _ in 0..be(r, 4)? {
                skip_nbt(r, t, depth + 1)?;
            }
        }
        10 => loop {
            let t = take(r, 1)?[0];
            if t == 0 {
                break;
            }
            let n = be(r, 2)?;
            take(r, n)?;
            skip_nbt(r, t, depth + 1)?;
        },
        11 => {
            let n = be(r, 4)?;
            take(r, n * 4)?;
        }
        12 => {
            let n = be(r, 4)?;
            take(r, n * 8)?;
        }
        t => return Err(format!("nbt tag {t}")),
    }
    Ok(())
}

fn container(
    r: &mut &[u8],
    protocol: i32,
    entries: usize,
    indirect: u8,
    limit: usize,
) -> Result<(), String> {
    let bits = take(r, 1)?[0];
    let check = |id: i32| {
        if (0..limit as i32).contains(&id) {
            Ok(())
        } else {
            Err(format!("id {id} >= {limit}"))
        }
    };
    if bits == 0 {
        check(var_int(r)?)?;
    } else if bits <= indirect {
        for _ in 0..var_int(r)? {
            check(var_int(r)?)?;
        }
    }
    let longs = if bits == 0 {
        0
    } else {
        entries.div_ceil(64 / usize::from(bits))
    };
    if protocol < 770 {
        let n = var_int(r)? as usize;
        if n != longs {
            return Err(format!("{n} longs for {bits} bits"));
        }
    }
    take(r, longs * 8)?;
    Ok(())
}

/// A `level_chunk_with_light` in the client's own format, read to the end.
fn check_chunk(protocol: i32, payload: &[u8], states: usize, biomes: usize) -> Result<(), String> {
    let mut r = payload;
    take(&mut r, 8)?;
    if protocol < 770 {
        let t = take(&mut r, 1)?[0];
        skip_nbt(&mut r, t, 0)?;
    } else {
        for _ in 0..var_int(&mut r)? {
            var_int(&mut r)?;
            let n = var_int(&mut r)? as usize;
            take(&mut r, n * 8)?;
        }
    }
    let n = var_int(&mut r)? as usize;
    let mut data = take(&mut r, n)?;
    let mut sections = 0;
    while !data.is_empty() {
        if protocol == 770 && data.iter().all(|b| *b == 0) && sections == 24 {
            break; // 1.21.5 pads the buffer.
        }
        take(&mut data, if protocol >= 775 { 4 } else { 2 })?;
        container(&mut data, protocol, 4096, 8, states)?;
        container(&mut data, protocol, 64, 3, biomes)?;
        sections += 1;
    }
    if sections != 24 {
        return Err(format!("{sections} sections"));
    }
    for _ in 0..var_int(&mut r)? {
        take(&mut r, 3)?;
        var_int(&mut r)?;
        let t = take(&mut r, 1)?[0];
        skip_nbt(&mut r, t, 0)?;
    }
    for _ in 0..4 {
        let n = var_int(&mut r)? as usize;
        take(&mut r, if protocol >= 777 { n } else { n * 8 })?;
    }
    for _ in 0..2 {
        for _ in 0..var_int(&mut r)? {
            let n = var_int(&mut r)? as usize;
            if n != 2048 {
                return Err(format!("light array of {n}"));
            }
            take(&mut r, n)?;
        }
    }
    if !r.is_empty() {
        return Err(format!("{} bytes left", r.len()));
    }
    Ok(())
}

/// An arrow as one client got it: the vertical velocities (spawn, then `set_entity_motion`),
/// and whether it ended in the ground (`in_ground`, 1.21.2+) or was removed.
#[derive(Debug, Default)]
struct Arrow {
    vys: Vec<f64>,
    in_ground: bool,
    removed: bool,
}

/// The vertical part of a velocity: `LpVec3` since 1.21.9, shorts (1/8000) before.
fn velocity_y(protocol: i32, r: &mut &[u8]) -> Result<f64, String> {
    if protocol < 773 {
        let v = take(r, 6)?;
        return Ok(f64::from(i16::from_be_bytes([v[2], v[3]])) / 8000.0);
    }
    let b0 = take(r, 1)?[0];
    if b0 == 0 {
        return Ok(0.0);
    }
    let b1 = take(r, 1)?[0];
    let rest = u64::from(u32::from_be_bytes(take(r, 4)?.try_into().unwrap()));
    let packed = (rest << 16) | (u64::from(b1) << 8) | u64::from(b0);
    let mut scale = u64::from(b0 & 3);
    if b0 & 4 != 0 {
        scale |= (var_int(r)? as u64) << 2;
    }
    let unit = ((packed >> 18) & 0x7FFF) as f64;
    Ok((unit.min(32766.0) * 2.0 / 32766.0 - 1.0) * scale as f64)
}

/// Follows the arrows (`add_entity` of `arrow_type`).
fn track_arrow(
    arrows: &mut std::collections::BTreeMap<i32, Arrow>,
    protocol: i32,
    arrow_type: i32,
    name: &str,
    payload: &[u8],
) -> Result<(), String> {
    let mut r = payload;
    if name == "remove_entities" {
        for _ in 0..var_int(&mut r)? {
            if let Some(a) = arrows.get_mut(&var_int(&mut r)?) {
                a.removed = true;
            }
        }
        return Ok(());
    }
    let id = var_int(&mut r)?;
    if name == "add_entity" {
        take(&mut r, 16)?;
        if var_int(&mut r)? != arrow_type {
            return Ok(());
        }
        // The position, then the velocity (after the angles and the data before 1.21.9).
        take(&mut r, 24)?;
        if protocol < 773 {
            take(&mut r, 3)?;
            var_int(&mut r)?;
        }
        let vys = vec![velocity_y(protocol, &mut r)?];
        arrows.insert(
            id,
            Arrow {
                vys,
                ..Arrow::default()
            },
        );
        return Ok(());
    }
    let Some(a) = arrows.get_mut(&id) else {
        return Ok(());
    };
    if name == "set_entity_motion" {
        a.vys.push(velocity_y(protocol, &mut r)?);
        return Ok(());
    }
    // `set_entity_data`: bytes, ints and booleans (serializers 0, 1 and 8 in every version);
    // `in_ground` is 10.
    loop {
        let index = take(&mut r, 1)?[0];
        if index == u8::MAX {
            return Ok(());
        }
        match var_int(&mut r)? {
            0 => {
                take(&mut r, 1)?;
            }
            1 => {
                var_int(&mut r)?;
            }
            8 => {
                let v = take(&mut r, 1)?[0] == 1;
                if index == 10 {
                    a.in_ground = v;
                }
            }
            _ => return Ok(()),
        }
    }
}

// ---------------------------------------------------------------- client

#[derive(Debug, Default)]
struct Outcome {
    protocol: i32,
    joined: bool,
    chunks: usize,
    bad_chunks: Vec<String>,
    keep_alives: u32,
    chat: bool,
    command: bool,
    kick: Option<String>,
    error: Option<String>,
    frames: usize,
    bytes: usize,
    /// Play packet name → count.
    kinds: std::collections::BTreeMap<String, usize>,
    /// Valid chunks after each play `login` (one per server visited).
    chunks_per_login: Vec<usize>,
    /// Players in the client's player list (`player_info_update` add, minus removals).
    infos: std::collections::HashSet<uuid::Uuid>,
    /// Other players spawned with `add_entity` after their player info.
    players_seen: std::collections::BTreeSet<uuid::Uuid>,
    /// Players spawned before their player info (invisible in a real client).
    players_without_info: Vec<uuid::Uuid>,
    /// `player_info_update` payloads the client's codec rejects.
    bad_infos: Vec<String>,
    /// `block_entity_data` by block entity type, and NBT not in the client's layout.
    block_entities: std::collections::BTreeMap<String, usize>,
    bad_block_entities: Vec<String>,
    /// The `tellraw` link arrived with its click event in the client's layout.
    link: Option<bool>,
    /// Block changes to a cobblestone wall state of the client's version.
    walls: usize,
    /// Last state of each `DIGS` cell, the last `player_action` sequence sent and the highest
    /// acknowledged.
    dig_states: [Option<u32>; 3],
    dig_sequence: i32,
    dig_acked: i32,
    /// The player's own attributes (name, base value) as last updated.
    attributes: std::collections::BTreeMap<String, f64>,
    /// Chat types (in the client's registry) of `disguised_chat` with a test token.
    disguised_chat_types: std::collections::BTreeSet<String>,
    /// Arrows by entity ID (every client shoots one) and the client's own shot.
    arrows: std::collections::BTreeMap<i32, Arrow>,
    shot: bool,
}

/// Where each client (proxy port, protocol) spawned, for blocks placed from the console.
static SPAWN: std::sync::Mutex<Vec<(u16, i32, [i64; 3])>> = std::sync::Mutex::new(Vec::new());

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// One client: joins, plays, visits `visits` in order with `/server` (8 s on
/// each) and waits for the kick.
async fn session(
    addr: SocketAddr,
    protocol: i32,
    joined: tokio::sync::mpsc::Sender<i32>,
    visits: Vec<String>,
) -> Outcome {
    let mut o = Outcome {
        protocol,
        ..Outcome::default()
    };
    let module = module(protocol);
    let tables = pumbo_data::tables(ProtocolVersion(protocol)).unwrap();
    let states = tables.block_state_count() as usize;
    let biomes = pumbo_data::synced(ProtocolVersion(protocol))
        .unwrap()
        .unwrap()
        .registry("worldgen/biome")
        .unwrap()
        .entries
        .len();
    let id = |name: &str| {
        tables
            .packet_id(Phase::Play, Direction::Serverbound, name)
            .unwrap()
    };
    let cb = |name: &str| tables.packet_id(Phase::Play, Direction::Clientbound, name);
    let (chunk_id, position_id, batch_id) = (
        cb("level_chunk_with_light"),
        cb("player_position"),
        cb("chunk_batch_finished"),
    );
    let (info_id, info_remove_id, add_entity_id) = (
        cb("player_info_update"),
        cb("player_info_remove"),
        cb("add_entity"),
    );
    let block_entity_id = cb("block_entity_data");
    let block_update_id = cb("block_update");
    let (ack_id, attributes_id) = (cb("block_changed_ack"), cb("update_attributes"));
    let attribute_names = tables.registry("attribute").unwrap().entries.clone();
    let dig_blocks: Vec<std::ops::Range<u32>> = DIGS
        .iter()
        .map(|(name, ..)| {
            let b = tables.block(name).unwrap();
            b.first_state..b.first_state + b.state_count()
        })
        .collect();
    let mut me: Option<[i64; 3]> = None;
    let registry_data_id = tables.packet_id(
        Phase::Configuration,
        Direction::Clientbound,
        "registry_data",
    );
    let disguised_id = cb("disguised_chat");
    // The client's `chat_type` registry as it got it.
    let mut chat_types: Vec<String> = Vec::new();
    let section_update_id = cb("section_blocks_update");
    // Block changes (position, state) of the last frame.
    let mut updates: Vec<(i64, u32)> = Vec::new();
    let mut entity_id = None;
    let wall = tables.block("cobblestone_wall").unwrap();
    let walls = wall.first_state..wall.first_state + wall.state_count();
    let block_entity_types = tables
        .registry("block_entity_type")
        .unwrap()
        .entries
        .clone();
    let player_type = tables
        .registry("entity_type")
        .and_then(|r| {
            r.entries
                .iter()
                .position(|e| e.trim_start_matches("minecraft:") == "player")
        })
        .unwrap() as i32;
    let arrow_type = tables
        .registry("entity_type")
        .and_then(|r| {
            r.entries
                .iter()
                .position(|e| e.trim_start_matches("minecraft:") == "arrow")
        })
        .unwrap() as i32;
    let arrow_packets: Vec<(i32, &str)> = [
        "add_entity",
        "set_entity_motion",
        "set_entity_data",
        "remove_entities",
    ]
    .into_iter()
    .filter_map(|n| Some((cb(n)?, n)))
    .collect();
    let chat_ids: Vec<i32> = ["system_chat", "player_chat", "disguised_chat"]
        .iter()
        .filter_map(|n| cb(n))
        .collect();
    let name = player(protocol);
    let token = format!("hello-{name}");
    let mut c = match Client::connect(addr, module.clone()).await {
        Ok(c) => c,
        Err(e) => {
            o.error = Some(format!("connect: {e}"));
            return o;
        }
    };
    let r: Result<(), pumbo_testclient::ClientError> = async {
        c.send(&Intention {
            protocol,
            address: "127.0.0.1".into(),
            port: addr.port(),
            intent: Intention::LOGIN,
        })
        .await?;
        c.phase = Phase::Login;
        c.send(&LoginStart {
            name: name.clone(),
            uuid: pumbo_identity::offline_uuid(&name),
        })
        .await?;
        let mut play_since = None;
        let mut last_login = Instant::now();
        let mut switch_sent = 0;
        let mut chat_sent = false;
        let mut command_at = None;
        let deadline = Instant::now() + idle() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let (kind, f) = match c.recv_within(Duration::from_secs(1)).await {
                Ok(x) => x,
                Err(pumbo_testclient::ClientError::Timeout) => (
                    None,
                    pumbo_protocol::RawFrame {
                        id: -1,
                        payload: Default::default(),
                    },
                ),
                Err(e) => return Err(e),
            };
            // A 26.3 client stores the stacks and block entities it got (`PUMBO_MV_SLOTS`, fixtures for
            // `pumbo-translate`).
            if protocol == 777
                && c.phase == Phase::Play
                && let Some(path) = std::env::var_os("PUMBO_MV_SLOTS")
                && let Some(n) = [
                    "container_set_slot",
                    "set_player_inventory",
                    "container_set_content",
                    "block_entity_data",
                ]
                .into_iter()
                .find(|n| cb(n) == Some(f.id))
            {
                let hex: String = f.payload.iter().map(|b| format!("{b:02x}")).collect();
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap();
                let _ = writeln!(file, "{n} {hex}");
            }
            if f.id >= 0 {
                o.frames += 1;
                o.bytes += f.payload.len();
                if c.phase == Phase::Play {
                    let n = tables
                        .packets
                        .iter()
                        .find(|p| {
                            p.phase == Phase::Play
                                && p.direction == Direction::Clientbound
                                && p.id == f.id
                        })
                        .map_or_else(|| f.id.to_string(), |p| p.name.clone());
                    *o.kinds.entry(n).or_default() += 1;
                }
            }
            if c.phase == Phase::Play
                && let Some((_, name)) = arrow_packets.iter().find(|(id, _)| *id == f.id)
                && let Err(e) = track_arrow(&mut o.arrows, protocol, arrow_type, name, &f.payload)
            {
                o.error = Some(format!("{name}: {e}"));
            }
            match (c.phase, kind) {
                (Phase::Login, Some(PacketKind::LoginCompression)) => {
                    let p: LoginCompression = c.decode(&f)?;
                    c.set_compression(p.threshold);
                }
                (Phase::Login, Some(PacketKind::LoginFinished)) => {
                    c.send(&LoginAcknowledged).await?;
                    c.phase = Phase::Configuration;
                    c.send(&ClientInformation {
                        locale: "en_us".into(),
                        view_distance: 3,
                        chat_mode: 0,
                        chat_colors: true,
                        skin_parts: 0x7F,
                        main_hand: 1,
                        text_filtering: false,
                        server_listing: true,
                        particle_status: 0,
                    })
                    .await?;
                    let mut brand = Vec::new();
                    let _ = brand.put_string("vanilla", 32767);
                    c.send(&ServerboundCustomPayload {
                        channel: "minecraft:brand".into(),
                        data: brand,
                    })
                    .await?;
                }
                (Phase::Login, Some(PacketKind::LoginDisconnect)) => {
                    o.error = Some(format!(
                        "login refused: {}",
                        String::from_utf8_lossy(&f.payload)
                    ));
                    return Ok(());
                }
                (Phase::Configuration, Some(PacketKind::SelectKnownPacks)) => {
                    let p: SelectKnownPacks = c.decode(&f)?;
                    c.send(&p).await?;
                }
                (Phase::Configuration, Some(PacketKind::CodeOfConduct)) => {
                    c.send(&AcceptCodeOfConduct).await?
                }
                (_, Some(PacketKind::ResourcePackPush)) => {
                    let p: ResourcePackPush = c.decode(&f)?;
                    c.send(&ResourcePackResponse {
                        id: p.id,
                        result: ResourcePackResponse::DECLINED,
                    })
                    .await?;
                }
                (Phase::Configuration, Some(PacketKind::FinishConfiguration)) => {
                    c.send(&FinishConfiguration).await?;
                    c.phase = Phase::Play;
                    play_since.get_or_insert_with(Instant::now);
                }
                (_, Some(PacketKind::KeepAlive)) => {
                    let k: KeepAlive = c.decode(&f)?;
                    c.send(&k).await?;
                    if c.phase == Phase::Play {
                        o.keep_alives += 1;
                    }
                }
                (_, Some(PacketKind::Ping)) => {
                    let p: Ping = c.decode(&f)?;
                    c.send(&Pong { id: p.id }).await?;
                }
                (_, Some(PacketKind::Disconnect)) => {
                    o.kick = Some(
                        String::from_utf8_lossy(&f.payload)
                            .chars()
                            .filter(|c| !c.is_control())
                            .collect(),
                    );
                    return Ok(());
                }
                (Phase::Play, Some(PacketKind::StartConfiguration)) => {
                    // The proxy moves the client to the next server.
                    c.send(&pumbo_protocol::packets::play::ConfigurationAcknowledged)
                        .await?;
                    c.phase = Phase::Configuration;
                }
                (Phase::Play, Some(PacketKind::Login)) => {
                    entity_id = f
                        .payload
                        .get(..4)
                        .map(|b| i32::from_be_bytes(b.try_into().unwrap()));
                    o.chunks_per_login.push(0);
                    last_login = Instant::now();
                    if !o.joined {
                        o.joined = true;
                        let _ = joined.send(protocol).await;
                    }
                }
                (Phase::Play, _) if Some(f.id) == batch_id => {
                    // Like a client: ask for the next batch (Pumpkin waits for it).
                    let mut p = Vec::new();
                    p.extend_from_slice(&20.0f32.to_be_bytes());
                    c.send_raw(id("chunk_batch_received"), &p).await?;
                }
                (Phase::Play, _) if Some(f.id) == info_id => {
                    match c.decode::<pumbo_protocol::packets::world::PlayerInfoUpdate>(&f) {
                        Ok(p) if p.actions & 1 != 0 => {
                            o.infos.extend(p.entries.iter().map(|e| e.id));
                        }
                        Ok(_) => {}
                        Err(e) => o.bad_infos.push(e.to_string()),
                    }
                }
                (Phase::Play, _) if Some(f.id) == info_remove_id => {
                    let mut r = &f.payload[..];
                    for _ in 0..var_int(&mut r).unwrap_or(0) {
                        if let Ok(id) = take(&mut r, 16) {
                            o.infos.remove(&uuid::Uuid::from_slice(id).unwrap());
                        }
                    }
                }
                (Phase::Play, _) if Some(f.id) == add_entity_id => {
                    let mut r = &f.payload[..];
                    let _ = var_int(&mut r);
                    let id = uuid::Uuid::from_slice(take(&mut r, 16).unwrap()).unwrap();
                    if var_int(&mut r) == Ok(player_type) {
                        if o.infos.contains(&id) {
                            o.players_seen.insert(id);
                        } else {
                            o.players_without_info.push(id);
                        }
                    }
                }
                (Phase::Play, _) if Some(f.id) == chunk_id => {
                    match check_chunk(protocol, &f.payload, states, biomes) {
                        Ok(()) => {
                            o.chunks += 1;
                            if let Some(n) = o.chunks_per_login.last_mut() {
                                *n += 1;
                            }
                        }
                        Err(e) => o.bad_chunks.push(e),
                    }
                }
                (Phase::Play, _) if Some(f.id) == block_update_id => {
                    let pos = i64::from_be_bytes(f.payload[..8].try_into().unwrap());
                    let mut r = &f.payload[8..];
                    updates.push((pos, var_int(&mut r).unwrap_or(-1) as u32));
                }
                (Phase::Play, _) if Some(f.id) == section_update_id => {
                    // Vanilla sends blocks changed in one tick per section.
                    let section = i64::from_be_bytes(f.payload[..8].try_into().unwrap());
                    let (sx, sy, sz) = (section >> 42, section << 44 >> 44, section << 22 >> 42);
                    let mut r = &f.payload[8..];
                    for _ in 0..var_int(&mut r).unwrap_or(0) {
                        let mut v = 0_i64;
                        for shift in (0..70).step_by(7) {
                            let b = take(&mut r, 1).unwrap()[0];
                            v |= i64::from(b & 0x7F) << shift;
                            if b & 0x80 == 0 {
                                break;
                            }
                        }
                        let at = [
                            sx * 16 + (v >> 8 & 15),
                            sy * 16 + (v & 15),
                            sz * 16 + (v >> 4 & 15),
                        ];
                        updates.push((block_pos(at), (v >> 12) as u32));
                    }
                }
                (Phase::Play, _) if Some(f.id) == ack_id => {
                    let mut r = &f.payload[..];
                    o.dig_acked = o.dig_acked.max(var_int(&mut r).unwrap_or(0));
                }
                (Phase::Play, _) if Some(f.id) == attributes_id => {
                    let mut r = &f.payload[..];
                    if var_int(&mut r).ok() == entity_id {
                        for _ in 0..var_int(&mut r).unwrap_or(0) {
                            let name = usize::try_from(var_int(&mut r).unwrap_or(-1))
                                .ok()
                                .and_then(|i| attribute_names.get(i))
                                .map_or("?".to_string(), |n| {
                                    n.trim_start_matches("minecraft:").to_string()
                                });
                            let base =
                                f64::from_be_bytes(take(&mut r, 8).unwrap().try_into().unwrap());
                            o.attributes.insert(name, base);
                            for _ in 0..var_int(&mut r).unwrap_or(0) {
                                let len = var_int(&mut r).unwrap_or(0) as usize;
                                let _ = take(&mut r, len + 9);
                            }
                        }
                    }
                }
                (Phase::Play, _) if Some(f.id) == block_entity_id => {
                    let mut r = &f.payload[8..];
                    let kind = var_int(&mut r).unwrap_or(-1);
                    let name = usize::try_from(kind)
                        .ok()
                        .and_then(|k| block_entity_types.get(k))
                        .map_or("?", |n| n.trim_start_matches("minecraft:"))
                        .to_string();
                    match pumbo_nbt::read_network(&mut r, pumbo_nbt::Limits::BACKEND) {
                        Ok(tag) => {
                            // Sherds: a list of items before 26.3, by side since.
                            let sherds = tag.as_ref().and_then(|t| t.as_compound()?.get("sherds"));
                            let pot_ok = match sherds {
                                Some(pumbo_nbt::Tag::List(_)) => protocol < 777,
                                Some(pumbo_nbt::Tag::Compound(_)) => protocol >= 777,
                                _ => true,
                            };
                            if !pot_ok {
                                o.bad_block_entities.push(format!("{name}: {tag:?}"));
                            }
                        }
                        Err(e) => o.bad_block_entities.push(format!("{name}: {e}")),
                    }
                    *o.block_entities.entry(name).or_default() += 1;
                }
                (Phase::Play, _) if Some(f.id) == position_id => {
                    // Teleport ID: first field since 1.21.2, last before.
                    let mut r = &f.payload[..];
                    let tp = if protocol >= 768 {
                        var_int(&mut r).unwrap_or(0)
                    } else {
                        let mut tail = &f.payload[33..];
                        var_int(&mut tail).unwrap_or(0)
                    };
                    let pos: Vec<i64> = r
                        .chunks(8)
                        .take(3)
                        .map(|b| f64::from_be_bytes(b.try_into().unwrap_or([0; 8])).floor() as i64)
                        .collect();
                    if let (Ok(mut spawn), [x, y, z], None) = (SPAWN.lock(), pos.as_slice(), me) {
                        me = Some([*x, *y, *z]);
                        spawn.push((addr.port(), protocol, [*x, *y, *z]));
                    }
                    let mut p = Vec::new();
                    p.put_varint(tp);
                    if protocol >= 777 {
                        // 26.3 confirms with the position (24 bytes) and rotation (8 bytes after
                        // the 24-byte velocity).
                        p.extend_from_slice(&r[..24]);
                        p.extend_from_slice(&r[48..56]);
                    }
                    c.send_raw(id("accept_teleportation"), &p).await?;
                }
                (Phase::Configuration, _) if Some(f.id) == registry_data_id => {
                    let mut r = &f.payload[..];
                    let len = var_int(&mut r).unwrap() as usize;
                    if take(&mut r, len).unwrap() == b"minecraft:chat_type" {
                        chat_types.clear();
                        for _ in 0..var_int(&mut r).unwrap() {
                            let len = var_int(&mut r).unwrap() as usize;
                            chat_types
                                .push(String::from_utf8_lossy(take(&mut r, len).unwrap()).into());
                            if take(&mut r, 1).unwrap()[0] != 0 {
                                pumbo_nbt::read_network(&mut r, pumbo_nbt::Limits::BACKEND)
                                    .unwrap();
                            }
                        }
                    }
                }
                (Phase::Play, _) if chat_ids.contains(&f.id) => {
                    if Some(f.id) == disguised_id && contains(&f.payload, "hello-") {
                        let mut r = &f.payload[..];
                        if pumbo_nbt::read_network(&mut r, pumbo_nbt::Limits::BACKEND).is_ok() {
                            let holder = var_int(&mut r).unwrap_or(0);
                            o.disguised_chat_types.insert(
                                usize::try_from(holder - 1)
                                    .ok()
                                    .and_then(|i| chat_types.get(i).cloned())
                                    .unwrap_or_else(|| format!("holder {holder}")),
                            );
                        }
                    }
                    if contains(&f.payload, "pumbo-link") {
                        // `clickEvent` before 1.21.5, `click_event` since.
                        let old = contains(&f.payload, "clickEvent");
                        let new = contains(&f.payload, "click_event");
                        o.link = Some(if protocol < 770 {
                            old && !new
                        } else {
                            new && !old
                        });
                    }
                    if contains(&f.payload, &token) {
                        o.chat = true;
                    }
                    if command_at.is_some() && contains(&f.payload, "commands.list") {
                        o.command = true;
                    }
                }
                _ => {}
            }
            for (pos, state) in updates.drain(..) {
                if walls.contains(&state) {
                    o.walls += 1;
                }
                if let Some(at) = me
                    && let Some(k) =
                        (0..DIGS.len()).find(|k| block_pos(dig_cell(protocol, *k, at)) == pos)
                {
                    o.dig_states[k] = Some(state);
                }
            }
            if let Some(at) = me {
                let placed = o
                    .dig_states
                    .iter()
                    .zip(&dig_blocks)
                    .all(|(s, b)| s.is_some_and(|s| b.contains(&s)));
                if placed && o.dig_sequence == 0 {
                    // Like a client: on the ground, the tool in hand, start, the mining time,
                    // finish (2 before 26.3, which inserted an action at 1).
                    let tick_end =
                        tables.packet_id(Phase::Play, Direction::Serverbound, "client_tick_end");
                    for (k, (_, slot, ticks)) in DIGS.iter().enumerate() {
                        c.send_raw(id("set_carried_item"), &slot.to_be_bytes())
                            .await?;
                        c.send_raw(id("move_player_status_only"), &[1]).await?;
                        if let Some(t) = tick_end {
                            c.send_raw(t, &[]).await?;
                        }
                        for action in [0, if protocol >= 777 { 3 } else { 2 }] {
                            if action != 0 {
                                tokio::time::sleep(Duration::from_millis(50 * ticks)).await;
                            }
                            o.dig_sequence += 1;
                            let mut p = Vec::new();
                            p.put_varint(action);
                            p.extend_from_slice(
                                &block_pos(dig_cell(protocol, k, at)).to_be_bytes(),
                            );
                            p.push(1); // face: up
                            p.put_varint(o.dig_sequence);
                            c.send_raw(id("player_action"), &p).await?;
                        }
                    }
                    // A bow shot (bow in hotbar slot 2, arrows in 3), upwards: draw for 8 ticks
                    // (lands within the view distance), release (5 before 26.3, which inserted an
                    // action at 1).
                    c.send_raw(id("set_carried_item"), &2i16.to_be_bytes())
                        .await?;
                    let mut p = Vec::new();
                    p.put_varint(0); // main hand
                    p.put_varint(o.dig_sequence + 1);
                    // A different direction per client.
                    p.extend_from_slice(&(protocol as f32 * 36.0).to_be_bytes());
                    p.extend_from_slice(&(-30.0f32).to_be_bytes());
                    c.send_raw(id("use_item"), &p).await?;
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    let mut p = Vec::new();
                    p.put_varint(if protocol >= 777 { 6 } else { 5 });
                    p.extend_from_slice(&0i64.to_be_bytes());
                    p.push(0);
                    p.put_varint(0);
                    c.send_raw(id("player_action"), &p).await?;
                    o.shot = true;
                }
            }
            if let Some(since) = play_since {
                if !chat_sent && since.elapsed() > Duration::from_secs(3) {
                    chat_sent = true;
                    c.send(&Chat {
                        message: token.clone(),
                        timestamp: 0,
                        salt: 0,
                        signature: None,
                        last_seen: LastSeen {
                            offset: 0,
                            acknowledged: [0; 3],
                            checksum: 0,
                        },
                    })
                    .await?;
                }
                if command_at.is_none() && since.elapsed() > Duration::from_secs(5) {
                    command_at = Some(Instant::now());
                    c.send(&ChatCommand {
                        command: "list".into(),
                    })
                    .await?;
                }
                if c.phase == Phase::Play
                    && switch_sent < visits.len()
                    && switch_sent + 1 == o.chunks_per_login.len()
                    && last_login.elapsed() > Duration::from_secs(8)
                {
                    c.send(&ChatCommand {
                        command: format!("server {}", visits[switch_sent]),
                    })
                    .await?;
                    switch_sent += 1;
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = r {
        o.error = Some(e.to_string());
    }
    // A 26.3 session (no translation) is kept for the translator benchmark.
    if protocol == 777
        && let Some(path) = std::env::var_os("PUMBO_MV_RECORD")
    {
        let frames = c.close().await;
        std::fs::write(path, pumbo_testclient::recording::encode(&frames)).unwrap();
    }
    o
}

// ---------------------------------------------------------------- backends

enum Backend {
    Pumpkin(Child),
    Vanilla(Server),
}

impl Backend {
    fn command(&mut self, cmd: &str) {
        match self {
            Backend::Pumpkin(child) => {
                let stdin = child.stdin.as_mut().unwrap();
                let _ = writeln!(stdin, "{cmd}");
                let _ = stdin.flush();
            }
            Backend::Vanilla(s) => s.command(cmd),
        }
    }

    fn stop(self) {
        match self {
            Backend::Vanilla(s) => s.stop(),
            Backend::Pumpkin(mut child) => {
                let _ = child.stdin.as_mut().map(|s| writeln!(s, "stop"));
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(30) {
                    if let Ok(Some(_)) = child.try_wait() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                eprintln!("pumpkin pid {} did not stop, killing it", child.id());
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn wait_port(addr: SocketAddr, limit: Duration) {
    let start = Instant::now();
    while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err() {
        assert!(start.elapsed() < limit, "{addr} not ready");
        std::thread::sleep(Duration::from_millis(300));
    }
}

const SECRET: &str = "pumbo-mv-pumpkin-secret-0123456789";
/// All test clients come from 127.0.0.1 at once.
const LIMITS: &str = "limits:\n  connections-per-ip-per-second: 1000\n  concurrent-per-ip: 1000\n";

fn start_pumpkin(dir: &Path, port: u16) -> Backend {
    let bin = env_path("PUMBO_PUMPKIN_BIN", "pumpkin");
    let template = std::fs::read_to_string(bin.with_file_name("pumpkin.toml")).unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let mut cfg = template.replacen(
        "[networking.proxy]\nenabled = false",
        "[networking.proxy]\nenabled = true",
        1,
    );
    cfg = cfg.replacen(
        "[networking.proxy.velocity]\nenabled = false\nsecret = \"\"",
        &format!("[networking.proxy.velocity]\nenabled = true\nsecret = \"{SECRET}\""),
        1,
    );
    let java = cfg.find("[networking.java]").unwrap();
    let at = java + cfg[java..].find("address = ").unwrap();
    let end = at + cfg[at..].find('\n').unwrap();
    cfg.replace_range(at..end, &format!("address = \"127.0.0.1:{port}\""));
    assert!(
        cfg.contains("[telemetry]\nenabled = false"),
        "telemetry must be off"
    );
    // Idle players must not die.
    cfg = cfg.replacen(
        "default_difficulty = \"Normal\"",
        "default_difficulty = \"Peaceful\"",
        1,
    );
    // Players dig next to the spawn (`DIGS`), like vanilla's `spawn-protection=0` in the tests.
    cfg = cfg.replacen("spawn_protection = 16", "spawn_protection = 0", 1);
    assert!(
        cfg.contains(&format!("secret = \"{SECRET}\"")),
        "template must have the proxy off"
    );
    std::fs::write(dir.join("pumpkin.toml"), cfg).unwrap();
    let log = std::fs::File::create(dir.join("server.out")).unwrap();
    let child = Command::new(&bin)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    eprintln!("pumpkin pid {}", child.id());
    Backend::Pumpkin(child)
}

fn start_vanilla(dir: &Path) -> Backend {
    let home = std::env::var("HOME").unwrap_or_default();
    let jar =
        env_path("PUMBO_JARS", &format!("{home}/.cache/pumbo-datagen")).join("26.3/server.jar");
    prepare_dir(dir, 25652, 777).unwrap();
    // Every test player may join; the code of conduct stays on (older clients get it accepted).
    let props = std::fs::read_to_string(dir.join("server.properties")).unwrap();
    let props = props
        .replace("white-list=true", "white-list=false")
        .replace("enforce-whitelist=true", "enforce-whitelist=false")
        .replace("max-players=5", "max-players=20\ndifficulty=peaceful");
    std::fs::write(dir.join("server.properties"), props).unwrap();
    let mut server = Server::start(dir, &jar, "java").unwrap();
    eprintln!("vanilla pid {}", server.pid);
    server
        .wait_ready(
            SocketAddr::from(([127, 0, 0, 1], 25652)),
            Duration::from_secs(240),
        )
        .unwrap();
    Backend::Vanilla(server)
}

fn run_backend(label: &str, protocols: &[i32]) -> Vec<Outcome> {
    // A fresh world per run: players saved dead or far away get no chunks.
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let work = env_path("PUMBO_MV_WORK", "target/pumbo-mv").join(run.to_string());
    let modern = |servers: &str| {
        let secret = secret_file("mv-pumpkin", SECRET);
        format!(
            "listener:\n  - bind: \"127.0.0.1:25651\"\nlogin:\n  online-mode: false\n{LIMITS}forwarding:\n  mode: modern\n  secret-file: \"{}\"\n\
             servers:\n{servers}routing:\n  try: [lobby]\n",
            toml_path(&secret)
        )
    };
    let wait = |port: u16| {
        wait_port(
            SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_secs(120),
        )
    };
    let mut visits = Vec::new();
    let (mut backends, proxy_cfg) = if label == "pumpkin" {
        let b = start_pumpkin(&work.join("pumpkin"), 25650);
        wait(25650);
        (
            vec![b],
            modern("  lobby: { address: \"127.0.0.1:25650\", protocol: 777 }\n"),
        )
    } else if label == "switch" {
        // Two Pumpkins; every client goes to the second and back with `/server`.
        let a = start_pumpkin(&work.join("pumpkin-a"), 25650);
        let b = start_pumpkin(&work.join("pumpkin-b"), 25652);
        wait(25650);
        wait(25652);
        visits = vec!["lobby2".to_string(), "lobby".to_string()];
        (
            vec![a, b],
            modern(
                "  lobby: { address: \"127.0.0.1:25650\", protocol: 777 }\n\
                 \x20 lobby2: { address: \"127.0.0.1:25652\", protocol: 777 }\n",
            ),
        )
    } else {
        (
            vec![start_vanilla(&work.join("vanilla"))],
            format!(
                "listener:\n  - bind: \"127.0.0.1:25653\"\nlogin:\n  online-mode: false\n{LIMITS}forwarding:\n  mode: none\n\
                 servers:\n  lobby: {{ address: \"127.0.0.1:25652\", protocol: 777 }}\nrouting:\n  try: [lobby]\n"
            ),
        )
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let outcomes = rt.block_on(async {
        let (proxy, addr) = start_proxy(&proxy_cfg).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        // Clients join 2 s apart, so earlier ones see later ones join (player info, then spawn).
        let tasks: Vec<_> = protocols
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                let (tx, visits) = (tx.clone(), visits.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2 * i as u64)).await;
                    session(addr, p, tx, visits).await
                })
            })
            .collect();
        drop(tx);
        // Everyone in, then the idle time, then a kick from the console.
        let mut joined = 0;
        let wait = tokio::time::Instant::now() + Duration::from_secs(60);
        while joined < protocols.len() {
            match tokio::time::timeout_at(wait, rx.recv()).await {
                Ok(Some(_)) => joined += 1,
                _ => break,
            }
        }
        eprintln!("{label}: {joined}/{} joined", protocols.len());
        tokio::time::sleep(idle()).await;
        // Blocks to dig next to every client, with the tools in hotbar slots 0 and 1 (`DIGS`).
        if label != "switch" {
            let spawns: Vec<(i32, [i64; 3])> = SPAWN
                .lock()
                .unwrap()
                .iter()
                .filter(|(port, ..)| *port == addr.port())
                .map(|(_, p, at)| (*p, *at))
                .collect();
            for b in &mut backends {
                for &p in protocols {
                    b.command(&format!("gamemode survival {}", player(p)));
                    b.command(&format!("give {} iron_pickaxe", player(p)));
                    b.command(&format!("give {} iron_axe", player(p)));
                    b.command(&format!("give {} bow", player(p)));
                    b.command(&format!("give {} arrow 16", player(p)));
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            for b in &mut backends {
                for &(p, at) in &spawns {
                    for (k, (block, ..)) in DIGS.iter().enumerate() {
                        let [x, y, z] = dig_cell(p, k, at);
                        b.command(&format!("setblock {x} {y} {z} {block}"));
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        // Stacks with Pumpkin's components (tools, food, armor, heads, pots) must arrive.
        for b in &mut backends {
            for &p in protocols {
                for item in GIVES {
                    b.command(&format!("give {} {item}", player(p)));
                }
            }
        }
        // A link with a click event (its layout changed in 1.21.5).
        for b in &mut backends {
            b.command(
                r#"tellraw @a {"text":"pumbo-link","click_event":{"action":"open_url","url":"https://example.org"}}"#,
            );
        }
        // A bed, a pot, a sign and a wall next to the spawn (beds need a block entity before 26.2).
        let spawn = SPAWN.lock().ok().and_then(|s| {
            s.iter()
                .find(|(port, ..)| *port == addr.port())
                .map(|(.., p)| *p)
        });
        if let Some([x, y, z]) = spawn {
            for b in &mut backends {
                b.command(&format!("setblock {} {y} {z} red_bed", x + 2));
                b.command(&format!("setblock {} {y} {z} decorated_pot", x - 2));
                b.command(&format!("setblock {x} {y} {} oak_sign", z + 2));
                b.command(&format!("setblock {x} {y} {} cobblestone_wall", z - 2));
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        for b in &mut backends {
            for &p in protocols {
                b.command(&format!("kick {} {KICK}", player(p)));
            }
        }
        let mut out = Vec::new();
        for t in tasks {
            out.push(t.await.unwrap());
        }
        proxy.stop();
        out
    });
    for b in backends {
        b.stop();
    }
    outcomes
}

#[test]
#[ignore = "starts Pumpkin 0.2.0 and a vanilla 26.3 server"]
fn older_clients_on_26_3() {
    let only: Vec<i32> = std::env::var("PUMBO_ONLY")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let mut protocols: Vec<i32> = (767..=776)
        .filter(|p| only.is_empty() || only.contains(p))
        .collect();
    // A 26.3 client (no translation) when recording or asked for in `PUMBO_ONLY`.
    if std::env::var_os("PUMBO_MV_RECORD").is_some() {
        protocols.push(777);
    } else if only.contains(&777) {
        // First in, so it sees the translated clients join.
        protocols.insert(0, 777);
    }
    let started = Instant::now();
    let backends = std::env::var("PUMBO_MV_BACKENDS").unwrap_or_else(|_| "pumpkin,vanilla".into());
    let handles: Vec<_> = ["pumpkin", "vanilla"]
        .into_iter()
        .filter(|b| backends.contains(b))
        .map(|label| {
            let protocols = protocols.clone();
            (
                label,
                std::thread::spawn(move || run_backend(label, &protocols)),
            )
        })
        .collect();
    let mut failed = Vec::new();
    let min_keep_alives = u32::try_from(idle().as_secs() / 15)
        .unwrap_or(0)
        .saturating_sub(1);
    for (label, h) in handles {
        let outcomes = h.join().unwrap();
        let reference = outcomes
            .iter()
            .find(|o| o.protocol == 777)
            .map(|o| &o.arrows);
        for o in &outcomes {
            let kicked = o.kick.as_deref().is_some_and(|k| k.contains(KICK));
            // All clients join at the spawn point, so each sees every other one. Pumpkin 0.2.0
            // spawns a joining player before its info; the translator holds the spawn, while a
            // 26.3 client (no translation) gets them in Pumpkin's order.
            let players_ok = (o.players_without_info.is_empty() || o.protocol == 777)
                && o.bad_infos.is_empty()
                && o.players_seen.len() + 1 >= protocols.len();
            let slots = ["container_set_slot", "set_player_inventory"]
                .iter()
                .filter_map(|n| o.kinds.get(*n))
                .sum::<usize>();
            let beds_ok =
                o.protocol >= 776 || o.block_entities.get("bed").copied().unwrap_or(0) > 0;
            // Every dug block ends as air, every action acknowledged.
            let dug = o.dig_sequence == 2 * DIGS.len() as i32
                && o.dig_acked >= o.dig_sequence
                && o.dig_states.iter().all(|s| *s == Some(0));
            // Pumpkin decorates player chat itself and sends it with its own `raw` chat type,
            // which the client must get (its `chat` would add an empty `<>`).
            let chat_type_ok =
                label != "pumpkin" || o.disguised_chat_types.iter().eq(["minecraft:raw"].iter());
            // Every arrow falls and ends in the ground or removed (no `in_ground` before 1.21.2),
            // with the velocities and the landing a 26.3 client got.
            let arrows_ok = o.shot
                && !o.arrows.is_empty()
                && o.arrows.iter().all(|(id, a)| {
                    let falls = a.vys.iter().any(|v| *v < a.vys[0] - 0.1);
                    let landed = a.in_ground || a.removed || o.protocol < 768;
                    let same = reference.and_then(|r| r.get(id)).is_none_or(|b| {
                        let mut tail = a.vys.iter().rev().zip(b.vys.iter().rev());
                        tail.all(|(x, y)| (x - y).abs() < 0.01)
                            && (a.in_ground == b.in_ground || o.protocol < 768)
                    });
                    falls && landed && same
                });
            let ok = o.joined
                && dug
                && arrows_ok
                && chat_type_ok
                && !o.attributes.is_empty()
                && o.link == Some(true)
                && o.walls > 0
                && slots >= GIVES.len()
                && beds_ok
                && o.bad_block_entities.is_empty()
                && o.chunks >= 9
                && o.bad_chunks.is_empty()
                && o.keep_alives >= min_keep_alives
                && o.chat
                && o.command
                && kicked
                && players_ok
                && o.error.is_none();
            eprintln!(
                "{} {label} {}: joined {}, chunks {} (bad {}), keep-alives {}, chat {}, command {}, kick {:?}, error {:?}, {} frames / {} KB",
                if ok { "OK  " } else { "FAIL" },
                o.protocol,
                o.joined,
                o.chunks,
                o.bad_chunks
                    .first()
                    .map_or("0".to_string(), |e| format!("{}: {e}", o.bad_chunks.len())),
                o.keep_alives,
                o.chat,
                o.command,
                o.kick,
                o.error,
                o.frames,
                o.bytes / 1024
            );
            eprintln!(
                "     slot updates {slots}, players seen {}, spawned before their info {:?}, bad player info {:?}",
                o.players_seen.len(),
                o.players_without_info,
                o.bad_infos.first()
            );
            eprintln!(
                "     link {:?}, walls {}, block entities {:?}, not in the client's layout {:?}",
                o.link,
                o.walls,
                o.block_entities,
                o.bad_block_entities.first()
            );
            eprintln!(
                "     dug {dug}: states {:?}, sequence {} acked {}, attributes {:?}",
                o.dig_states, o.dig_sequence, o.dig_acked, o.attributes
            );
            eprintln!("     disguised chat types {:?}", o.disguised_chat_types);
            for (id, a) in &o.arrows {
                let vys: Vec<String> = a.vys.iter().map(|v| format!("{v:.2}")).collect();
                eprintln!(
                    "     arrow {id}: in ground {}, removed {}, vy {}",
                    a.in_ground,
                    a.removed,
                    vys.join(" ")
                );
            }
            let mut top: Vec<_> = o.kinds.iter().collect();
            top.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
            eprintln!("     most frequent: {:?}", &top[..top.len().min(5)]);
            if !ok {
                failed.push(format!("{label} {}", o.protocol));
            }
        }
    }
    eprintln!("{} s", started.elapsed().as_secs());
    assert!(failed.is_empty(), "failed: {failed:?}");
}

#[test]
#[ignore = "starts two Pumpkin 0.2.0 servers"]
fn older_clients_switch_servers() {
    let only: Vec<i32> = std::env::var("PUMBO_ONLY")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let protocols: Vec<i32> = (767..=776)
        .filter(|p| only.is_empty() || only.contains(p))
        .collect();
    let mut failed = Vec::new();
    for o in run_backend("switch", &protocols) {
        let kicked = o.kick.as_deref().is_some_and(|k| k.contains(KICK));
        let ok = o.chunks_per_login.len() == 3
            && o.chunks_per_login.iter().all(|n| *n >= 9)
            && o.bad_chunks.is_empty()
            && kicked
            && o.error.is_none();
        eprintln!(
            "{} switch {}: chunks per server {:?} (bad {}), keep-alives {}, kick {:?}, error {:?}",
            if ok { "OK  " } else { "FAIL" },
            o.protocol,
            o.chunks_per_login,
            o.bad_chunks.len(),
            o.keep_alives,
            o.kick,
            o.error
        );
        if !ok {
            failed.push(o.protocol);
        }
    }
    assert!(failed.is_empty(), "failed: {failed:?}");
}
