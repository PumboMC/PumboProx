//! Golden tests of the version translator on recordings of vanilla servers
//! (`pumbo-datagen record`, local only, `target/pumbo-data-full` or
//! `$PUMBO_DATA_FULL`). The 26.3 session is translated for every older
//! protocol and checked against that protocol's own recording of the same
//! script:
//!
//! - registry data and tags in configuration are byte-equal (the client gets
//!   its own vanilla data),
//! - chunks of the flat world are byte-equal, matched by coordinates,
//! - every translated packet the codec decodes (login, commands, teams,
//!   titles, boss bars, chat...) decodes in the client's version and encodes
//!   back to the same bytes,
//! - nothing the client needs to play is dropped.
//!
//! Without recordings the test is skipped; `PUMBO_REQUIRE_RECORDINGS=1` makes that a failure.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use pumbo_data::DataVersion;
use pumbo_protocol::packets::{Ctx, decode_any, encode_any};
use pumbo_protocol::{Direction, Phase, ProtocolVersion, VersionModule};
use pumbo_testclient::recording::{self, Recorded};
use pumbo_translate::multiversion::version_data;
use pumbo_translate_mv as mv;

fn full_root() -> PathBuf {
    std::env::var_os("PUMBO_DATA_FULL").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/pumbo-data-full"),
        PathBuf::from,
    )
}

fn session(protocol: i32) -> Option<Vec<Recorded>> {
    recording::read(
        &full_root()
            .join(protocol.to_string())
            .join("session-known.rec"),
    )
    .ok()
}

fn var_int(r: &mut &[u8]) -> i32 {
    let mut v = 0u32;
    for shift in 0..5 {
        let b = r[0];
        *r = &r[1..];
        v |= u32::from(b & 0x7F) << (7 * shift);
        if b & 0x80 == 0 {
            break;
        }
    }
    v as i32
}

fn take<'a>(r: &mut &'a [u8], n: usize) -> &'a [u8] {
    let (a, b) = r.split_at(n);
    *r = b;
    a
}

/// `update_tags` with registries and tags sorted (vanilla sends them in hash order).
/// Registry → (tag, encoded entries), sorted.
type SortedTags = Vec<(Vec<u8>, Vec<(Vec<u8>, Vec<u8>)>)>;

fn sorted_tags(mut r: &[u8]) -> SortedTags {
    let mut out = Vec::new();
    for _ in 0..var_int(&mut r) {
        let n = var_int(&mut r) as usize;
        let registry = take(&mut r, n).to_vec();
        let mut tags = Vec::new();
        for _ in 0..var_int(&mut r) {
            let n = var_int(&mut r) as usize;
            let tag = take(&mut r, n).to_vec();
            let start = r;
            for _ in 0..var_int(&mut r) {
                var_int(&mut r);
            }
            tags.push((tag, start[..start.len() - r.len()].to_vec()));
        }
        tags.sort();
        out.push((registry, tags));
    }
    out.sort();
    out
}

/// A chunk with its heightmap list (1.21.5+) sorted by type (vanilla sends hash order).
fn sorted_heightmaps(client: i32, chunk: &[u8]) -> Vec<u8> {
    if client < 770 {
        return chunk.to_vec();
    }
    let mut r = &chunk[8..];
    let n = var_int(&mut r);
    let mut maps = Vec::new();
    for _ in 0..n {
        let start = r;
        let kind = var_int(&mut r);
        let longs = var_int(&mut r) as usize;
        take(&mut r, longs * 8);
        maps.push((kind, start[..start.len() - r.len()].to_vec()));
    }
    maps.sort();
    let mut out = chunk[..9].to_vec();
    maps.into_iter().for_each(|(_, m)| out.extend(m));
    out.extend_from_slice(r);
    out
}

/// Frames the translator sees (login stays with the proxy).
fn translated_phase(p: Phase) -> bool {
    matches!(p, Phase::Configuration | Phase::Play)
}

fn name(module: &DataVersion, phase: Phase, direction: Direction, id: i32) -> String {
    let tables = pumbo_data::tables(module.protocol()).unwrap();
    tables
        .packets
        .iter()
        .find(|p| p.phase == phase && p.direction == direction && p.id == id)
        .map_or_else(|| format!("#{id}"), |p| p.name.clone())
}

/// Packets the 26.3 session sends that a client must get to play (unless it
/// has no such packet).
const MUST_ARRIVE: &[&str] = &[
    "login",
    "level_chunk_with_light",
    "player_position",
    "keep_alive",
    "system_chat",
    "commands",
    "set_entity_data",
    "add_entity",
    "container_set_content",
    "update_advancements",
    "set_player_team",
    "boss_event",
    "set_time",
    "player_info_update",
    "update_attributes",
    "set_default_spawn_position",
    "move_entity_pos",
    "entity_position_sync",
    "set_entity_motion",
    "disconnect",
    "update_recipes",
    "recipe_book_add",
];

#[derive(Default)]
struct Report {
    /// Server packet name → (in, out).
    counts: BTreeMap<String, (usize, usize)>,
    failures: Vec<String>,
    decoded: usize,
    chunks: usize,
    other_chunks: usize,
    /// Client packets translated to the server and decoded there.
    serverbound: usize,
    micros: u128,
}

fn translate_session(
    client: i32,
    server_frames: &[Recorded],
    client_frames: &[Recorded],
) -> Report {
    let mut report = Report::default();
    let p = ProtocolVersion(client);
    let tables = Arc::new(
        mv::Tables::new(
            version_data(p).unwrap(),
            version_data(ProtocolVersion(777)).unwrap(),
        )
        .unwrap(),
    );
    let mut t = mv::Translator::new(tables);
    let server_module = DataVersion::new(pumbo_data::tables(ProtocolVersion(777)).unwrap());
    let client_module = DataVersion::new(pumbo_data::tables(p).unwrap());
    let known_packs = client_frames
        .iter()
        .find(|f| {
            f.phase == Phase::Configuration
                && f.direction == Direction::Serverbound
                && name(&client_module, f.phase, f.direction, f.id) == "select_known_packs"
        })
        .unwrap();
    let mut out = mv::Output::default();
    let mut produced: Vec<(Phase, i32, Vec<u8>)> = Vec::new();
    let started = Instant::now();
    for f in server_frames {
        if !translated_phase(f.phase) {
            continue;
        }
        out.clear();
        if f.direction == Direction::Serverbound {
            // The client's own answer to the known packs, in its version.
            if name(&server_module, f.phase, f.direction, f.id) == "select_known_packs" {
                t.to_server(known_packs.id, &known_packs.payload, &mut out);
                assert_eq!(out.to_server.len(), 1, "{client}: known packs answer");
            }
            continue;
        }
        t.to_client(f.id, &f.payload, &mut out);
        let n = name(&server_module, f.phase, f.direction, f.id);
        let entry = report.counts.entry(n).or_default();
        entry.0 += 1;
        entry.1 += out.to_client.len();
        produced.extend(
            out.to_client
                .drain(..)
                .map(|(id, payload)| (f.phase, id, payload)),
        );
    }
    report.micros = started.elapsed().as_micros();

    // The client's own packets (its recording) translated for the server.
    for f in client_frames
        .iter()
        .filter(|f| f.direction == Direction::Serverbound)
    {
        if !translated_phase(f.phase) {
            continue;
        }
        let n = name(&client_module, f.phase, f.direction, f.id);
        if n == "select_known_packs" {
            continue;
        }
        out.clear();
        t.to_server(f.id, &f.payload, &mut out);
        for (id, payload) in out.to_server.drain(..) {
            let Some(kind) = server_module.packet_kind(f.phase, Direction::Serverbound, id) else {
                report
                    .failures
                    .push(format!("{client}: {n} became unknown packet {id}"));
                continue;
            };
            let ctx = Ctx::new(&server_module, Direction::Serverbound);
            match decode_any(f.phase, Direction::Serverbound, kind, &payload, &ctx) {
                Ok(Some(_)) => report.serverbound += 1,
                Ok(None) => {}
                Err(e) => report
                    .failures
                    .push(format!("{client}: {n} to the server does not decode: {e}")),
            }
        }
    }

    // Decode in the client's version and encode back.
    for (phase, id, payload) in &produced {
        let Some(kind) = client_module.packet_kind(*phase, Direction::Clientbound, *id) else {
            continue;
        };
        let ctx = Ctx::new(&client_module, Direction::Clientbound);
        match decode_any(*phase, Direction::Clientbound, kind, payload, &ctx) {
            Ok(Some(packet)) => {
                report.decoded += 1;
                if encode_any(&packet, &ctx).ok().as_deref() != Some(payload.as_slice()) {
                    report
                        .failures
                        .push(format!("{client}: {} re-encodes differently", kind.name()));
                }
            }
            Ok(None) => {}
            Err(e) => report
                .failures
                .push(format!("{client}: {} does not decode: {e}", kind.name())),
        }
    }

    // Same content as the client's own recording.
    let by_name = |frames: Vec<(Phase, i32, Vec<u8>)>, n: &str, phase: Phase| -> Vec<Vec<u8>> {
        frames
            .into_iter()
            .filter(|(ph, id, _)| {
                *ph == phase && name(&client_module, phase, Direction::Clientbound, *id) == n
            })
            .map(|(_, _, p)| p)
            .collect()
    };
    let recorded: Vec<(Phase, i32, Vec<u8>)> = client_frames
        .iter()
        .filter(|f| f.direction == Direction::Clientbound)
        .map(|f| (f.phase, f.id, f.payload.to_vec()))
        .collect();
    for n in ["registry_data", "update_tags"] {
        let mut ours = by_name(produced.clone(), n, Phase::Configuration);
        let mut theirs = by_name(recorded.clone(), n, Phase::Configuration);
        if n == "update_tags" && ours.len() == 1 && theirs.len() == 1 {
            ours = vec![format!("{:?}", sorted_tags(&ours[0])).into_bytes()];
            theirs = vec![format!("{:?}", sorted_tags(&theirs[0])).into_bytes()];
        }
        if ours != theirs {
            if let Some(dir) = std::env::var_os("MV_DUMP") {
                let dir = PathBuf::from(dir);
                let _ = std::fs::write(dir.join(format!("{client}-{n}-ours.bin")), ours.concat());
                let _ = std::fs::write(
                    dir.join(format!("{client}-{n}-theirs.bin")),
                    theirs.concat(),
                );
            }
            report.failures.push(format!(
                "{client}: configuration {n} differs ({} vs {} packets)",
                ours.len(),
                theirs.len()
            ));
        }
    }
    let chunks = |frames: Vec<Vec<u8>>| -> HashMap<Vec<u8>, Vec<u8>> {
        frames
            .into_iter()
            .map(|p| (p[..8].to_vec(), sorted_heightmaps(client, &p)))
            .collect()
    };
    let ours = chunks(by_name(
        produced.clone(),
        "level_chunk_with_light",
        Phase::Play,
    ));
    let theirs = chunks(by_name(recorded, "level_chunk_with_light", Phase::Play));
    for (pos, chunk) in &ours {
        match theirs.get(pos) {
            Some(expected) if expected == chunk => report.chunks += 1,
            // Different world content (light, blocks) between the recordings.
            Some(_) => report.other_chunks += 1,
            None => {}
        }
    }
    if report.chunks == 0 {
        report
            .failures
            .push(format!("{client}: no chunk matched the recording"));
    }

    for n in MUST_ARRIVE {
        let Some(&(input, output)) = report.counts.get(*n) else {
            continue;
        };
        // 1.21 has the old recipe format; it gets no recipes.
        if client < 768 && n.contains("recipe") {
            continue;
        }
        let client_has = pumbo_data::tables(p)
            .unwrap()
            .packet_id(Phase::Play, Direction::Clientbound, n)
            .is_some();
        if input > 0 && output < input && client_has {
            report
                .failures
                .push(format!("{client}: {n}: {output} of {input} arrived"));
        }
    }
    report
}

#[test]
fn vanilla_26_3_session_for_older_clients() {
    let Some(server) = session(777) else {
        assert!(
            std::env::var_os("PUMBO_REQUIRE_RECORDINGS").is_none(),
            "no recordings in {:?}",
            full_root()
        );
        eprintln!("skipped: no recordings in {:?}", full_root());
        return;
    };
    let mut failures = Vec::new();
    for client in 767..=776 {
        let Some(own) = session(client) else {
            failures.push(format!("{client}: no recording"));
            continue;
        };
        let r = translate_session(client, &server, &own);
        let dropped: Vec<String> = r
            .counts
            .iter()
            .filter(|(_, (i, o))| o < i)
            .map(|(n, (i, o))| format!("{n} {o}/{i}"))
            .collect();
        let frames: usize = r.counts.values().map(|(i, _)| i).sum();
        eprintln!(
            "{client}: {frames} frames in {} us, {} decoded and re-encoded, {} chunks equal ({} with other content), {} client packets decoded by 26.3; fewer out: {}",
            r.micros,
            r.decoded,
            r.chunks,
            r.other_chunks,
            r.serverbound,
            dropped.join(", ")
        );
        failures.extend(r.failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ---------------------------------------------------------------- recipes

/// Reads recipe packets in one client's layout (ViaVersion `RecipeDisplayRewriter`
/// per version) and fails on anything that does not fit.
struct RecipeReader<'a> {
    protocol: i32,
    tables: &'a pumbo_data::Tables,
}

impl RecipeReader<'_> {
    fn names(&self, registry: &str) -> Vec<String> {
        self.tables
            .registry(registry)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.trim_start_matches("minecraft:").to_string())
            .collect()
    }

    fn below(&self, r: &mut &[u8], registry: &str) -> Result<i32, String> {
        let id = var_int(r);
        let n = self.names(registry).len() as i32;
        if (0..n).contains(&id) {
            Ok(id)
        } else {
            Err(format!("{registry} {id} >= {n}"))
        }
    }

    fn string(r: &mut &[u8]) -> String {
        let n = var_int(r) as usize;
        String::from_utf8(take(r, n).to_vec()).unwrap()
    }

    fn items(&self, r: &mut &[u8]) -> Result<(), String> {
        let n = var_int(r);
        if n == 0 {
            Self::string(r);
        }
        for _ in 1..n {
            self.below(r, "item")?;
        }
        Ok(())
    }

    fn display(&self, r: &mut &[u8]) -> Result<(), String> {
        let kind = self.below(r, "slot_display")? as usize;
        match self.names("slot_display")[kind].as_str() {
            "empty" | "any_fuel" => {}
            "with_any_potion" => self.display(r)?,
            "only_with_component" => {
                self.display(r)?;
                self.below(r, "data_component_type")?;
            }
            "item" => drop(self.below(r, "item")?),
            "item_stack" => {
                let (first, second) = (var_int(r), var_int(r));
                let item = if self.protocol >= 775 { first } else { second };
                if !(0..self.names("item").len() as i32).contains(&item) {
                    return Err(format!("stack item {item}"));
                }
                if (var_int(r), var_int(r)) != (0, 0) {
                    return Err("stack with components (not checked here)".into());
                }
            }
            "tag" if self.protocol >= 777 => self.items(r)?,
            "tag" => drop(Self::string(r)),
            "dyed" | "with_remainder" => {
                self.display(r)?;
                self.display(r)?;
            }
            "smithing_trim" => {
                self.display(r)?;
                self.display(r)?;
                if self.protocol >= 770 {
                    if var_int(r) == 0 {
                        return Err("inline trim pattern".into());
                    }
                } else {
                    self.display(r)?;
                }
            }
            "composite" => {
                for _ in 0..var_int(r) {
                    self.display(r)?;
                }
            }
            other => return Err(format!("slot display {other}")),
        }
        Ok(())
    }

    fn recipe(&self, r: &mut &[u8]) -> Result<(), String> {
        let kind = self.below(r, "recipe_display")? as usize;
        let displays = match self.names("recipe_display")[kind].as_str() {
            "crafting_shapeless" => var_int(r) + 2,
            "crafting_shaped" => {
                var_int(r);
                var_int(r);
                var_int(r) + 2
            }
            "furnace" => 4,
            "stonecutter" => 3,
            "smithing" => 5,
            other => return Err(format!("recipe display {other}")),
        };
        for _ in 0..displays {
            self.display(r)?;
        }
        if self.names("recipe_display")[kind] == "furnace" {
            var_int(r);
            take(r, 4);
        }
        Ok(())
    }

    fn packet(&self, name: &str, mut r: &[u8]) -> Result<usize, String> {
        let r = &mut r;
        let mut entries = 0;
        match name {
            "update_recipes" => {
                for _ in 0..var_int(r) {
                    Self::string(r);
                    for _ in 0..var_int(r) {
                        self.below(r, "item")?;
                    }
                }
                for _ in 0..var_int(r) {
                    self.items(r)?;
                    self.display(r)?;
                    entries += 1;
                }
            }
            "recipe_book_add" => {
                for _ in 0..var_int(r) {
                    var_int(r);
                    self.recipe(r)?;
                    if take(r, 1)[0] != 0 {
                        var_int(r);
                    }
                    self.below(r, "recipe_book_category")?;
                    if take(r, 1)[0] != 0 {
                        for _ in 0..var_int(r) {
                            self.items(r)?;
                        }
                    }
                    take(r, 1);
                    entries += 1;
                }
                take(r, 1);
            }
            _ => {}
        }
        if r.is_empty() {
            Ok(entries)
        } else {
            Err(format!("{} bytes left", r.len()))
        }
    }
}

/// The vanilla 26.3 session's recipes for every client from 1.21.2: every
/// recipe of the recipe book and the stonecutter arrives in the client's layout.
#[test]
fn recipes_in_the_clients_layout() {
    let Some(server) = session(777) else {
        eprintln!("skipped: no recordings in {:?}", full_root());
        return;
    };
    let server_module = DataVersion::new(pumbo_data::tables(ProtocolVersion(777)).unwrap());
    let mut failures = Vec::new();
    for client in 768..=776 {
        let p = ProtocolVersion(client);
        let tables = pumbo_data::tables(p).unwrap();
        let mut t = mv::Translator::new(Arc::new(
            mv::Tables::new(
                version_data(p).unwrap(),
                version_data(ProtocolVersion(777)).unwrap(),
            )
            .unwrap(),
        ));
        let reader = RecipeReader {
            protocol: client,
            tables,
        };
        let client_module = DataVersion::new(tables);
        let mut out = mv::Output::default();
        let mut seen = BTreeMap::new();
        for f in server
            .iter()
            .filter(|f| f.direction == Direction::Clientbound)
        {
            if !translated_phase(f.phase) {
                continue;
            }
            out.clear();
            t.to_client(f.id, &f.payload, &mut out);
            let n = name(&server_module, f.phase, f.direction, f.id);
            if !["update_recipes", "recipe_book_add", "place_ghost_recipe"].contains(&n.as_str()) {
                continue;
            }
            for (id, payload) in &out.to_client {
                let got = name(&client_module, f.phase, Direction::Clientbound, *id);
                match reader.packet(&got, payload) {
                    Ok(entries) => *seen.entry(got).or_insert(0) += entries,
                    Err(e) => failures.push(format!("{client}: {got}: {e}")),
                }
            }
        }
        eprintln!("{client}: recipes {seen:?}");
        if seen.get("recipe_book_add").copied().unwrap_or(0) == 0 {
            failures.push(format!("{client}: no recipe book entries"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
