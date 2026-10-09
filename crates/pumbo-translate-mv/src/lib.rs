//! Version translation for Minecraft Java Edition: clients of protocols
//! 767–776 (1.21–26.2) on a server of protocol 777 (26.3), for the
//! configuration and play phases. Handshake, status and login stay with the
//! caller, in the client's version.
//!
//! Ported from pumpkin-java-multiversion by Zinedin Polic (MIT OR Apache-2.0,
//! see `NOTICE`), with conversions from ViaBackwards and ViaVersion
//! (GPL-3.0, marked where used): translation on raw payloads per packet, eras per encoding
//! change, per-connection state for entity types. The plugin's Pumpkin types
//! are replaced by readers and writers on bytes, and its generated remaps by
//! remaps built at start from both versions' tables ([`VersionData`]).
//!
//! A packet the translator cannot read or the client cannot represent is
//! dropped, never forwarded in the server's layout.

mod chunk;
mod config;
mod data;
mod entity;
mod entity_data;
mod item;
mod nbt;
mod play;
mod remap;
mod serverbound;
mod version;
mod wire;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub use data::{Block, Bound, Phase, Synced, TagList, VersionData};
pub use play::fix_empty_trail;
pub use version::{OLDEST_CLIENT, SERVER_PROTOCOL};

use remap::{IdMap, Via};

/// Frames produced by one translation call, as `(packet id, payload)`.
#[derive(Debug, Default)]
pub struct Output {
    pub to_client: Vec<(i32, Vec<u8>)>,
    pub to_server: Vec<(i32, Vec<u8>)>,
}

impl Output {
    pub fn clear(&mut self) {
        self.to_client.clear();
        self.to_server.clear();
    }
}

/// Why tables could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Only [`SERVER_PROTOCOL`] servers are read.
    UnsupportedServer(i32),
    /// Clients from [`OLDEST_CLIENT`] to just below the server are translated.
    UnsupportedClient(i32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnsupportedServer(p) => write!(f, "server protocol {p} is not translated"),
            Error::UnsupportedClient(p) => write!(f, "client protocol {p} is not translated"),
        }
    }
}

impl std::error::Error for Error {}

/// Payload translation of one packet: `Some` is sent under the mapped ID, `None`
/// drops it (the handler may have queued other packets in the context).
pub(crate) type Handler = fn(&mut Ctx<'_>, &[u8]) -> Option<Vec<u8>>;

#[derive(Clone, Copy, Default)]
struct Route {
    target: Option<i32>,
    handler: Option<Handler>,
}

/// Packets renamed between versions, as (older name, server name).
const RENAMES: &[(&str, &str)] = &[
    ("horse_screen_open", "mount_screen_open"),
    ("swing", "punch"),
];

/// Everything that depends only on the two versions; shared by sessions.
pub struct Tables {
    pub(crate) client: VersionData,
    pub(crate) server: VersionData,
    /// Per phase: server packet ID → route to the client.
    to_client: [Vec<Route>; 2],
    /// Per phase: client packet ID → route to the server.
    to_server: [Vec<Route>; 2],
    /// Packet IDs by name: `[phase][bound]` of the client and of the server.
    client_ids: [[HashMap<String, i32>; 2]; 2],
    server_ids: [[HashMap<String, i32>; 2]; 2],
    pub(crate) block_states: Vec<u32>,
    pub(crate) block_bits: u8,
    pub(crate) blocks: IdMap,
    pub(crate) items: IdMap,
    /// Client item → server item (exact names and renames only).
    pub(crate) items_back: IdMap,
    pub(crate) entity_types: IdMap,
    /// Other static registries by name, server → client.
    registries: HashMap<&'static str, IdMap>,
    pub(crate) entity_data: entity_data::Fields,
    /// The server's `player` entity type.
    pub(crate) player_type: Option<i32>,
    /// Server block states of beds (first, count), and the client's `bed`
    /// block entity type when the client still has one (before 26.2).
    pub(crate) beds: Vec<(u32, u32)>,
    pub(crate) bed_entity: Option<i32>,
}

/// Static registries remapped by name, with the stand-ins of a Mappings
/// section (empty: none).
const REMAPPED: &[(&str, &str)] = &[
    ("attribute", "attributes"),
    ("block_entity_type", ""),
    ("command_argument_type", ""),
    ("consume_effect_type", ""),
    ("custom_stat", ""),
    ("data_component_type", ""),
    ("game_event", ""),
    ("map_decoration_type", ""),
    ("menu", ""),
    ("mob_effect", ""),
    ("particle_type", "particles"),
    ("potion", ""),
    ("recipe_book_category", ""),
    ("recipe_display", ""),
    ("recipe_serializer", ""),
    ("slot_display", ""),
    ("sound_event", "sounds"),
    ("stat_type", ""),
    ("villager_profession", ""),
    ("villager_type", ""),
];

fn ids_by_name(v: &VersionData) -> [[HashMap<String, i32>; 2]; 2] {
    v.packets.clone().map(|phase| {
        phase.map(|names| {
            names
                .into_iter()
                .enumerate()
                .filter_map(|(i, n)| Some((n, i32::try_from(i).ok()?)))
                .collect()
        })
    })
}

fn rename<'a>(name: &'a str, other: &HashMap<String, i32>, older_first: bool) -> &'a str {
    if other.contains_key(name) {
        return name;
    }
    RENAMES
        .iter()
        .find_map(|(old, new)| match older_first {
            true if *old == name => Some(*new),
            false if *new == name => Some(*old),
            _ => None,
        })
        .unwrap_or(name)
}

impl std::fmt::Debug for Tables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Tables({} -> {})",
            self.client.protocol, self.server.protocol
        )
    }
}

impl Tables {
    pub fn new(client: VersionData, server: VersionData) -> Result<Self, Error> {
        if server.protocol != SERVER_PROTOCOL {
            return Err(Error::UnsupportedServer(server.protocol));
        }
        if client.protocol < OLDEST_CLIENT || client.protocol >= server.protocol {
            return Err(Error::UnsupportedClient(client.protocol));
        }
        let entity_data = entity_data::Fields::load(client.protocol)
            .ok_or(Error::UnsupportedClient(client.protocol))?;
        let via = Via::new(client.protocol);
        let client_ids = ids_by_name(&client);
        let server_ids = ids_by_name(&server);
        let mut to_client: [Vec<Route>; 2] = Default::default();
        let mut to_server: [Vec<Route>; 2] = Default::default();
        for phase in [Phase::Configuration, Phase::Play] {
            let p = phase.index();
            let (Some(sc), Some(cc), Some(cs), Some(ss)) = (
                server.packets.get(p).and_then(|b| b.first()),
                client_ids.get(p).and_then(|b| b.first()),
                client.packets.get(p).and_then(|b| b.get(1)),
                server_ids.get(p).and_then(|b| b.get(1)),
            ) else {
                continue;
            };
            if let Some(routes) = to_client.get_mut(p) {
                *routes = sc
                    .iter()
                    .map(|name| Route {
                        target: cc.get(rename(name, cc, false)).copied(),
                        handler: play::to_client(phase, name),
                    })
                    .collect();
            }
            if let Some(routes) = to_server.get_mut(p) {
                *routes = cs
                    .iter()
                    .map(|name| {
                        let server_name = rename(name, ss, true);
                        Route {
                            target: ss.get(server_name).copied(),
                            handler: serverbound::to_server(phase, server_name),
                        }
                    })
                    .collect();
            }
        }
        let block_states = remap::block_states(&via, &server, &client);
        let block_bits = remap::ceil_log2(client.block_state_count());
        let blocks = remap::block_ids(&block_states, &server, &client);
        let items = remap::registry(
            &via,
            "items",
            server.registry("item"),
            client.registry("item"),
        );
        let items_back = IdMap::by_name(
            client.registry("item"),
            server.registry("item"),
            &|n, has| {
                RENAMED_ITEMS
                    .iter()
                    .find(|(old, _)| *old == n)
                    .map(|(_, new)| (*new).to_string())
                    .filter(|s| has(s))
            },
        );
        let entity_types = remap::registry(
            &via,
            "entities",
            server.registry("entity_type"),
            client.registry("entity_type"),
        );
        let player_type = server
            .registry("entity_type")
            .iter()
            .position(|n| n == "player")
            .and_then(|i| i32::try_from(i).ok());
        let beds = server
            .blocks
            .iter()
            .filter(|b| b.name.ends_with("_bed"))
            .map(|b| (b.first_state, b.state_count()))
            .collect();
        let bed_entity = client
            .registry("block_entity_type")
            .iter()
            .position(|n| n == "bed")
            .and_then(|i| i32::try_from(i).ok())
            .filter(|_| {
                !server
                    .registry("block_entity_type")
                    .iter()
                    .any(|n| n == "bed")
            });
        let registries = REMAPPED
            .iter()
            .map(|&(r, section)| {
                let map = remap::registry(&via, section, server.registry(r), client.registry(r));
                (r, map)
            })
            .collect();
        Ok(Self {
            client,
            server,
            to_client,
            to_server,
            client_ids,
            server_ids,
            block_states,
            block_bits,
            blocks,
            items,
            items_back,
            entity_types,
            registries,
            entity_data,
            player_type,
            beds,
            bed_entity,
        })
    }

    pub fn client_protocol(&self) -> i32 {
        self.client.protocol
    }

    /// Client block state for a server block state.
    pub fn block_state_for_client(&self, id: i32) -> i32 {
        self.block_state(id)
    }

    /// Client item for a server item (with stand-ins).
    pub fn item_for_client(&self, id: i32) -> Option<i32> {
        self.items.get(id)
    }

    /// Client entity type for a server entity type (with stand-ins).
    pub fn entity_type_for_client(&self, id: i32) -> Option<i32> {
        self.entity_types.get(id)
    }

    pub(crate) fn registry(&self, name: &str) -> Option<&IdMap> {
        self.registries.get(name)
    }

    pub(crate) fn is_bed(&self, server_state: i32) -> bool {
        u32::try_from(server_state).is_ok_and(|s| {
            self.beds
                .iter()
                .any(|&(first, n)| (first..first + n).contains(&s))
        })
    }

    pub(crate) fn block_state(&self, id: i32) -> i32 {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.block_states.get(i))
            .map_or(0, |&s| s as i32)
    }
}

/// Items renamed since the oldest client, as (older name, server name).
const RENAMED_ITEMS: &[(&str, &str)] = &[("chain", "iron_chain")];

/// Translation state of one connection.
#[derive(Debug, Default)]
pub(crate) struct State {
    /// The server's `select_known_packs` payload, answered as known.
    pub(crate) server_packs: Option<Vec<u8>>,
    pub(crate) client_knows_core: bool,
    pub(crate) registries_sent: bool,
    /// Entry names of the server's synced registries.
    pub(crate) server_registries: HashMap<String, Vec<String>>,
    /// The server's own entries (name, data) the client gets after its vanilla ones, per registry.
    pub(crate) custom_entries: HashMap<String, Vec<(String, Vec<u8>)>>,
    dynamic: HashMap<String, IdMap>,
    /// Server entity type by entity ID.
    pub(crate) entities: HashMap<i32, i32>,
    /// Last known player position and rotation (for the 26.3 teleport confirmation).
    pub(crate) position: [f64; 3],
    pub(crate) rotation: [f32; 2],
    /// Teleport ID → target position and rotation sent by the server.
    pub(crate) teleports: Vec<(i32, [f64; 3], [f32; 2])>,
    /// `player_loaded` sent for clients that do not send it.
    pub(crate) loaded_sent: bool,
    /// Last time of day and whether it advances (clients before 26.1).
    pub(crate) day_time: Option<(i64, bool)>,
    /// Profiles in the client's player list (`player_info_update` adds minus removals).
    pub(crate) players: HashSet<[u8; 16]>,
    /// Players spawned before their `player_info_update`, and the frames held until it comes.
    pub(crate) waiting: Vec<[u8; 16]>,
    pub(crate) held: Vec<(i32, Vec<u8>)>,
}

/// Frames held at most while waiting for a player's info (Pumpkin sends it a
/// few frames after the spawn).
const HOLD_LIMIT: usize = 64;

impl State {
    pub(crate) fn dynamic_reset(&mut self) {
        self.dynamic.clear();
    }
}

/// Fallback entry of a synced registry for entries the client lacks.
fn dynamic_fallback(registry: &str) -> &'static str {
    match registry {
        "worldgen/biome" => "minecraft:plains",
        "dimension_type" => "minecraft:overworld",
        "damage_type" => "minecraft:generic",
        "chat_type" => "minecraft:chat",
        _ => "",
    }
}

pub(crate) struct Ctx<'a> {
    pub(crate) t: &'a Tables,
    pub(crate) s: &'a mut State,
    pub(crate) out: &'a mut Output,
    pub(crate) phase: Phase,
    /// Packets to the client (by name) that go after the translated one.
    pub(crate) after: Vec<(&'static str, Vec<u8>)>,
}

impl Ctx<'_> {
    pub(crate) fn client(&self) -> i32 {
        self.t.client.protocol
    }

    /// Queues a packet to the client by name (no-op when the client lacks it).
    pub(crate) fn send_client(&mut self, name: &str, payload: Vec<u8>) {
        let ids = self
            .t
            .client_ids
            .get(self.phase.index())
            .and_then(|b| b.get(Bound::Client.index()));
        if let Some(&id) = ids.and_then(|m| m.get(name)) {
            self.out.to_client.push((id, payload));
        }
    }

    pub(crate) fn send_server(&mut self, name: &str, payload: Vec<u8>) {
        let ids = self
            .t
            .server_ids
            .get(self.phase.index())
            .and_then(|b| b.get(Bound::Server.index()));
        if let Some(&id) = ids.and_then(|m| m.get(name)) {
            self.out.to_server.push((id, payload));
        }
    }

    /// Server → client IDs of a synced registry, by entry name.
    pub(crate) fn dynamic(&mut self, registry: &str) -> Option<&IdMap> {
        if !self.s.dynamic.contains_key(registry) {
            let server = self.s.server_registries.get(registry)?;
            let client = self
                .t
                .client
                .synced
                .registries
                .iter()
                .find(|(n, _)| n == registry)
                .map(|(_, e)| e)?;
            let full = |e: &String| {
                if e.contains(':') {
                    e.clone()
                } else {
                    format!("minecraft:{e}")
                }
            };
            let server: Vec<String> = server.iter().map(full).collect();
            let mut client: Vec<String> = client.iter().map(full).collect();
            if let Some(custom) = self.s.custom_entries.get(registry) {
                client.extend(custom.iter().map(|(name, _)| name.clone()));
            }
            let fallback = dynamic_fallback(registry);
            let first = client.first().cloned().unwrap_or_default();
            let map = IdMap::by_name(&server, &client, &|_, has| {
                Some(if has(fallback) {
                    fallback.to_string()
                } else {
                    first.clone()
                })
            });
            self.s.dynamic.insert(registry.to_string(), map);
        }
        self.s.dynamic.get(registry)
    }
}

/// Translator of one connection.
pub struct Translator {
    tables: Arc<Tables>,
    state: State,
    /// Phase of the server's stream and of the client's (both start in
    /// configuration, right after login).
    server_phase: Phase,
    client_phase: Phase,
}

impl std::fmt::Debug for Translator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Translator({} -> {})",
            self.tables.client.protocol, self.tables.server.protocol
        )
    }
}

impl Translator {
    pub fn new(tables: Arc<Tables>) -> Self {
        Self {
            tables,
            state: State::default(),
            server_phase: Phase::Configuration,
            client_phase: Phase::Configuration,
        }
    }

    pub fn tables(&self) -> &Arc<Tables> {
        &self.tables
    }

    /// A server packet; what the client gets (and any answers to the server) go to `out`.
    pub fn to_client(&mut self, id: i32, payload: &[u8], out: &mut Output) {
        let phase = self.server_phase;
        let route = usize::try_from(id)
            .ok()
            .and_then(|i| self.tables.to_client.get(phase.index())?.get(i).copied())
            .unwrap_or_default();
        let start = out.to_client.len();
        let waited = self.state.waiting.len();
        self.run(phase, route, payload, out, true);
        // The server's stream switches after `finish_configuration` and `start_configuration`.
        let name = self.tables.server.packet_name(phase, Bound::Client, id);
        self.server_phase = match (phase, name) {
            (Phase::Configuration, Some("finish_configuration")) => Phase::Play,
            (Phase::Play, Some("start_configuration")) => Phase::Configuration,
            _ => phase,
        };
        // Keep-alives and pings are never held: a late answer gets the client kicked.
        let urgent = phase != self.server_phase || matches!(name, Some("keep_alive" | "ping"));
        self.hold(out, start, waited, urgent);
    }

    /// A player spawned before its info is invisible to the client, so the
    /// spawn and everything after it wait for that info (Pumpkin 0.2.0 sends a
    /// joining player's `add_entity` first).
    // ponytail: frames, not just the entity's, are held; at most HOLD_LIMIT, then sent as they are.
    fn hold(&mut self, out: &mut Output, start: usize, waited: usize, urgent: bool) {
        let s = &mut self.state;
        if s.waiting.is_empty() && waited > 0 {
            // This frame carried the last awaited info: it goes first.
            out.to_client.append(&mut s.held);
        } else if !s.waiting.is_empty() {
            s.held.extend(out.to_client.drain(start..));
            if s.held.len() >= HOLD_LIMIT || urgent {
                s.waiting.clear();
                out.to_client.append(&mut s.held);
            }
        }
    }

    /// A client packet; what the server gets goes to `out`.
    pub fn to_server(&mut self, id: i32, payload: &[u8], out: &mut Output) {
        let phase = self.client_phase;
        let route = usize::try_from(id)
            .ok()
            .and_then(|i| self.tables.to_server.get(phase.index())?.get(i).copied())
            .unwrap_or_default();
        self.run(phase, route, payload, out, false);
        // The client's stream switches with its acknowledgements.
        let name = self.tables.client.packet_name(phase, Bound::Server, id);
        self.client_phase = match (phase, name) {
            (Phase::Configuration, Some("finish_configuration")) => Phase::Play,
            (Phase::Play, Some("configuration_acknowledged")) => Phase::Configuration,
            _ => phase,
        };
    }

    fn run(
        &mut self,
        phase: Phase,
        route: Route,
        payload: &[u8],
        out: &mut Output,
        to_client: bool,
    ) {
        let mut after = Vec::new();
        let translated = match route.handler {
            None => Some(payload.to_vec()),
            Some(handler) => {
                let mut ctx = Ctx {
                    t: &self.tables,
                    s: &mut self.state,
                    out,
                    phase,
                    after: Vec::new(),
                };
                let translated = handler(&mut ctx, payload);
                after = ctx.after;
                translated
            }
        };
        if let (Some(payload), Some(id)) = (translated, route.target) {
            if to_client {
                out.to_client.push((id, payload));
            } else {
                out.to_server.push((id, payload));
            }
        }
        for (name, payload) in after {
            let mut ctx = Ctx {
                t: &self.tables,
                s: &mut self.state,
                out,
                phase,
                after: Vec::new(),
            };
            ctx.send_client(name, payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Put;

    fn version(protocol: i32) -> VersionData {
        let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        VersionData {
            protocol,
            packets: [
                [names(&["finish_configuration"]), vec![]],
                [
                    names(&[
                        "add_entity",
                        "player_info_update",
                        "keep_alive",
                        "bundle_delimiter",
                    ]),
                    vec![],
                ],
            ],
            registries: [("entity_type".to_string(), names(&["pig", "player"]))].into(),
            ..VersionData::default()
        }
    }

    fn add_player(uuid: [u8; 16]) -> Vec<u8> {
        let mut p = Vec::new();
        p.put_var_int(7);
        p.put_slice(&uuid);
        p.put_var_int(1);
        p.put_slice(&[0; 24]);
        p.put_u8(0); // no velocity
        p.put_slice(&[0; 3]);
        p.put_var_int(0);
        p
    }

    /// 1.21.2 dropped the `generic.`/`player.` prefixes; without the Mappings
    /// stand-ins a 1.21 client got no attribute (mining, speed, health).
    #[test]
    fn attributes_keep_their_pre_1_21_2_names() {
        let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let registry = |protocol, n| VersionData {
            protocol,
            registries: [("attribute".to_string(), names(n))].into(),
            ..VersionData::default()
        };
        let tables = Tables::new(
            registry(767, &["generic.armor", "player.mining_efficiency"]),
            registry(777, &["mining_efficiency", "armor", "tempt_range"]),
        )
        .unwrap();
        let map = tables.registry("attribute").unwrap();
        assert_eq!(
            [map.get(0), map.get(1), map.get(2)],
            [Some(1), Some(0), None]
        );
    }

    #[test]
    fn player_spawn_waits_for_its_info() {
        let tables = Arc::new(Tables::new(version(775), version(777)).unwrap());
        let mut t = Translator::new(tables);
        let mut out = Output::default();
        t.to_client(0, &[], &mut out);
        out.clear();
        let uuid = [9u8; 16];
        t.to_client(0, &add_player(uuid), &mut out);
        t.to_client(3, &[], &mut out);
        assert!(out.to_client.is_empty(), "held until the info");
        let mut info = vec![0x01];
        info.put_var_int(1);
        info.put_slice(&uuid);
        info.put_str("Steve");
        info.put_var_int(0);
        t.to_client(1, &info, &mut out);
        let ids: Vec<i32> = out.to_client.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            [1, 0, 3],
            "info, then the spawn and what came after it"
        );
        out.clear();
        // A known player spawns right away.
        t.to_client(0, &add_player(uuid), &mut out);
        assert_eq!(out.to_client.len(), 1);
        out.clear();
        // A keep-alive is never held: it releases what waits for an info that may not come.
        t.to_client(0, &add_player([8; 16]), &mut out);
        t.to_client(2, &[0; 8], &mut out);
        let ids: Vec<i32> = out.to_client.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, [0, 2]);
    }
}
