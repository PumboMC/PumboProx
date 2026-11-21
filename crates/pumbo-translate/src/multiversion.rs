//! `multiversion`: translation inside the proxy with `pumbo-translate-mv`
//! (plan E3b), for the pairs it lists in `PAIRS` (clients 1.21–26.2 on a 26.3
//! backend). This module feeds it the tables of both versions from `pumbo-data`.

use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use pumbo_core::registry::{ModuleConfig, ModuleError};
use pumbo_core::translate::{
    Dropped, PacketTranslator, Translated, TranslationPlan, TranslatorProvider,
};
use pumbo_protocol::{Direction, Phase, ProtocolVersion, RawFrame};
use pumbo_translate_mv as mv;

use crate::MULTIVERSION;

/// The translator of `pumbo-translate-mv` for the pairs it lists. Its tables
/// are in [`TABLES`], so every provider (a new one comes with each reload)
/// shares them.
#[derive(Debug, Default)]
pub struct Multiversion {}

/// A value made once for the whole process; `None`: it could not be made.
type Slot<T> = OnceLock<Option<Arc<T>>>;

/// Tables of what this crate's code translates directly (one cell per
/// entry of [`mv::PAIRS`]), built on the first plan that needs them and
/// shared by every session; a reload keeps them. `None`: they could not be
/// built.
static TABLES: [Slot<mv::Tables>; mv::PAIRS.len()] = [const { OnceLock::new() }; mv::PAIRS.len()];

fn short(name: &str) -> String {
    pumbo_data::short_name(name).to_string()
}

/// `registry_data` payloads of a recording with full registry NBT (`PUMBOREC1`).
fn full_registry_data(v: ProtocolVersion, tables: &pumbo_data::Tables) -> Vec<Vec<u8>> {
    let Some(data) = pumbo_data::full_registry_data(v) else {
        return Vec::new();
    };
    let Some(id) = tables.packet_id(
        Phase::Configuration,
        Direction::Clientbound,
        "registry_data",
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut r =
        pumbo_protocol::types::Reader::new(data.strip_prefix(b"PUMBOREC1\n").unwrap_or_default());
    while !r.is_empty() {
        let (Ok(phase), Ok(direction), Ok(frame_id), Ok(len)) =
            (r.u8(), r.u8(), r.varint(), r.varint())
        else {
            break;
        };
        let Ok(payload) = r.take(usize::try_from(len).unwrap_or(usize::MAX)) else {
            break;
        };
        // Phase 3 is configuration, direction 0 clientbound (recording format).
        if phase == 3 && direction == 0 && frame_id == id {
            out.push(payload.to_vec());
        }
    }
    out
}

/// What `pumbo-translate-mv` needs to know about a protocol, from `pumbo-data`.
pub fn version_data(v: ProtocolVersion) -> Option<mv::VersionData> {
    let tables = pumbo_data::tables(v).ok()?;
    let mut packets: [[Vec<String>; 2]; 2] = Default::default();
    for p in &tables.packets {
        let phase = match p.phase {
            Phase::Configuration => 0,
            Phase::Play => 1,
            _ => continue,
        };
        let bound = usize::from(p.direction == Direction::Serverbound);
        let (Some(list), Ok(id)) = (
            packets.get_mut(phase).and_then(|b| b.get_mut(bound)),
            usize::try_from(p.id),
        ) else {
            continue;
        };
        if list.len() <= id {
            list.resize(id + 1, String::new());
        }
        if let Some(slot) = list.get_mut(id) {
            *slot = p.name.clone();
        }
    }
    let registries = tables
        .registries
        .iter()
        .map(|r| (short(&r.name), r.entries.iter().map(|e| short(e)).collect()))
        .collect();
    let blocks = tables
        .blocks
        .iter()
        .map(|b| mv::Block {
            name: short(&b.name),
            first_state: b.first_state,
            default_offset: b.default_offset,
            properties: b
                .properties
                .iter()
                .map(|p| (p.name.clone(), p.values.clone()))
                .collect(),
        })
        .collect();
    let synced = pumbo_data::synced(v).ok().flatten()?;
    Some(mv::VersionData {
        protocol: v.0,
        releases: tables.release_names(),
        packets,
        registries,
        blocks,
        synced: mv::Synced {
            registries: synced
                .registries
                .iter()
                .map(|r| (short(&r.name), r.entries.iter().map(|e| short(e)).collect()))
                .collect(),
            tags: synced
                .tags
                .iter()
                .map(|(registry, tags)| {
                    (
                        short(registry),
                        tags.iter()
                            .map(|(t, ids)| (short(t), ids.clone()))
                            .collect(),
                    )
                })
                .collect(),
            features: synced.features.iter().map(|f| short(f)).collect(),
        },
        full_registry_data: full_registry_data(v, tables),
    })
}

/// The value of `cell`, made on its first use. A failure is kept, and its
/// reason goes to `report` that once (`None`: reported elsewhere).
fn once<T: Clone>(
    cell: &OnceLock<Option<T>>,
    make: impl FnOnce() -> Result<T, Option<String>>,
    report: impl FnOnce(&str),
) -> Option<T> {
    cell.get_or_init(|| {
        make()
            .map_err(|reason| {
                if let Some(reason) = reason {
                    report(&reason);
                }
            })
            .ok()
    })
    .clone()
}

/// Why protocol `v` has no version data.
fn no_data(v: i32) -> String {
    format!("protocol {v}: no tables or no recorded configuration data")
}

/// What the translator needs about protocol `v`, built once and shared by
/// every pair with that protocol; a protocol without it is logged once.
fn data(v: i32) -> Option<Arc<mv::VersionData>> {
    // One cell per protocol of `mv::PAIRS`, also one this build does not
    // embed, so that one gets its line in the log too.
    static DATA: OnceLock<Vec<(i32, Slot<mv::VersionData>)>> = OnceLock::new();
    let cells = DATA.get_or_init(|| {
        let mut protocols: Vec<i32> = mv::PAIRS
            .iter()
            .flat_map(|p| [p.client, p.server])
            .collect();
        protocols.sort_unstable();
        protocols.dedup();
        protocols
            .into_iter()
            .map(|p| (p, OnceLock::new()))
            .collect()
    });
    let (_, cell) = cells.iter().find(|(p, _)| *p == v)?;
    once(
        cell,
        || {
            version_data(ProtocolVersion(v))
                .map(Arc::new)
                .ok_or_else(|| Some(no_data(v)))
        },
        |reason| tracing::warn!("no version translation with {reason}"),
    )
}

/// Tables of `client` on `server` on the data `data` gives; `Err(None)` when
/// a side has no data (its own cell reports that).
fn build(
    data: impl Fn(i32) -> Option<Arc<mv::VersionData>>,
    client: i32,
    server: i32,
) -> Result<Arc<mv::Tables>, Option<String>> {
    let (Some(c), Some(s)) = (data(client), data(server)) else {
        return Err(None);
    };
    mv::Tables::shared(c, s)
        .map(Arc::new)
        .map_err(|e| Some(e.to_string()))
}

/// The tables for `client` on `server`. A pair that has its data and still
/// cannot be built is logged once.
fn tables(client: i32, server: i32) -> Option<Arc<mv::Tables>> {
    let i = mv::PAIRS
        .iter()
        .position(|p| p.client == client && p.server == server)?;
    once(
        TABLES.get(i)?,
        || build(data, client, server),
        |reason| {
            tracing::warn!(
                "no version translation for protocol {client} clients on protocol {server} servers: {reason}"
            );
        },
    )
}

/// Why `pair` has no tables, for `check-config`; `None` when it has them.
/// Builds them without keeping or logging anything.
pub fn why_not(pair: &mv::Pair) -> Option<String> {
    let Some(client) = version_data(ProtocolVersion(pair.client)) else {
        return Some(no_data(pair.client));
    };
    let Some(server) = version_data(ProtocolVersion(pair.server)) else {
        return Some(no_data(pair.server));
    };
    mv::Tables::new(client, server).err().map(|e| e.to_string())
}

impl TranslatorProvider for Multiversion {
    fn name(&self) -> &str {
        MULTIVERSION
    }

    fn plan(
        &self,
        client: ProtocolVersion,
        backend: Option<ProtocolVersion>,
    ) -> Option<TranslationPlan> {
        let pair = mv::find(client.0, backend?.0)?;
        let tables = tables(pair.client, pair.server)?;
        Some(TranslationPlan::Translate(Box::new(Session {
            inner: mv::Translator::new(tables),
            out: mv::Output::default(),
        })))
    }
}

struct Session {
    inner: mv::Translator,
    out: mv::Output,
}

impl Session {
    fn drain(&mut self, out: &mut Translated) {
        let frame = |(id, payload): (i32, Vec<u8>)| RawFrame {
            id,
            payload: Bytes::from(payload),
        };
        out.to_client
            .extend(self.out.to_client.drain(..).map(frame));
        out.to_backend
            .extend(self.out.to_server.drain(..).map(frame));
        let tables = self.inner.tables();
        out.dropped.extend(self.out.dropped.drain(..).map(|d| {
            let (protocol, direction) = match d.bound {
                mv::Bound::Client => (tables.server_protocol(), Direction::Clientbound),
                mv::Bound::Server => (tables.client_protocol(), Direction::Serverbound),
            };
            let phase = match d.phase {
                mv::Phase::Configuration => Phase::Configuration,
                mv::Phase::Play => Phase::Play,
            };
            Dropped {
                to_client: d.bound == mv::Bound::Client,
                packet: packet_name(ProtocolVersion(protocol), phase, direction, d.id),
            }
        }));
    }
}

/// The name of a packet in the embedded tables, `unknown` when they lack it.
fn packet_name(v: ProtocolVersion, phase: Phase, direction: Direction, id: i32) -> &'static str {
    pumbo_data::tables(v)
        .ok()
        .and_then(|t| {
            t.packets
                .iter()
                .find(|p| p.phase == phase && p.direction == direction && p.id == id)
        })
        .map_or("unknown", |p| p.name.as_str())
}

impl PacketTranslator for Session {
    fn client_to_backend(&mut self, frame: &RawFrame, out: &mut Translated) {
        self.inner
            .to_server(frame.id, &frame.payload, &mut self.out);
        self.drain(out);
    }

    fn backend_to_client(&mut self, frame: &RawFrame, out: &mut Translated) {
        self.inner
            .to_client(frame.id, &frame.payload, &mut self.out);
        self.drain(out);
    }
}

pub(crate) fn factory(_: &ModuleConfig) -> Result<Arc<dyn TranslatorProvider>, ModuleError> {
    Ok(Arc::new(Multiversion::default()))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// Dropped packets come out by name: one the client cannot read, and an
    /// ID no table has.
    #[test]
    fn drops_carry_the_packet_name() {
        let Some(TranslationPlan::Translate(mut t)) =
            Multiversion::default().plan(ProtocolVersion(769), Some(ProtocolVersion(777)))
        else {
            panic!("no translator");
        };
        let tables = pumbo_data::tables(ProtocolVersion(777)).unwrap();
        let id = |phase, name| {
            tables
                .packets
                .iter()
                .find(|p| {
                    p.phase == phase && p.direction == Direction::Clientbound && p.name == name
                })
                .unwrap()
                .id
        };
        let frame = |id| RawFrame {
            id,
            payload: Bytes::new(),
        };
        let mut out = Translated::default();
        t.backend_to_client(
            &frame(id(Phase::Configuration, "finish_configuration")),
            &mut out,
        );
        assert!(out.dropped.is_empty());
        t.backend_to_client(&frame(id(Phase::Play, "add_entity")), &mut out);
        t.backend_to_client(&frame(0x7fff), &mut out);
        let dropped = |packet| Dropped {
            to_client: true,
            packet,
        };
        assert_eq!(out.dropped, [dropped("add_entity"), dropped("unknown")]);
    }

    /// A dropped server packet is named in the server's tables, not the
    /// client's: `level_chunk_with_light` without a payload cannot be read,
    /// and its 26.3 ID is another packet for a 1.21 client.
    #[test]
    fn server_drops_are_named_in_the_servers_tables() {
        let v = ProtocolVersion;
        let Some(TranslationPlan::Translate(mut t)) =
            Multiversion::default().plan(v(767), Some(v(777)))
        else {
            panic!("no translator");
        };
        let id = |protocol, phase, name| {
            pumbo_data::tables(v(protocol))
                .unwrap()
                .packets
                .iter()
                .find(|p| {
                    p.phase == phase && p.direction == Direction::Clientbound && p.name == name
                })
                .map(|p| p.id)
        };
        let chunk = id(777, Phase::Play, "level_chunk_with_light").unwrap();
        assert_ne!(
            packet_name(v(767), Phase::Play, Direction::Clientbound, chunk),
            "level_chunk_with_light"
        );
        let frame = |id| RawFrame {
            id,
            payload: Bytes::new(),
        };
        let mut out = Translated::default();
        let finish = id(777, Phase::Configuration, "finish_configuration").unwrap();
        t.backend_to_client(&frame(finish), &mut out);
        t.backend_to_client(&frame(chunk), &mut out);
        assert_eq!(
            out.dropped,
            [Dropped {
                to_client: true,
                packet: "level_chunk_with_light",
            }]
        );
    }

    #[test]
    fn plans_only_older_clients_on_26_3() {
        let m = Multiversion::default();
        let v = ProtocolVersion;
        assert!(m.plan(v(769), None).is_none());
        assert!(m.plan(v(777), Some(v(777))).is_none());
        assert!(m.plan(v(766), Some(v(777))).is_none());
        assert!(m.plan(v(769), Some(v(776))).is_none());
        assert!(matches!(
            m.plan(v(769), Some(v(777))),
            Some(TranslationPlan::Translate(_))
        ));
    }

    #[test]
    fn tables_are_built_once_per_pair() {
        let first = tables(769, 777).unwrap();
        assert!(Arc::ptr_eq(&first, &tables(769, 777).unwrap()));
    }

    #[test]
    fn pairs_share_the_servers_data() {
        let (oldest, newest) = (tables(767, 777).unwrap(), tables(776, 777).unwrap());
        assert!(Arc::ptr_eq(oldest.server_data(), newest.server_data()));
    }

    #[test]
    fn a_pair_without_data_says_why() {
        let pair = |client| mv::Pair {
            client,
            server: 777,
            experimental: false,
        };
        let why = why_not(&pair(700)).unwrap();
        assert!(why.contains("protocol 700"), "{why}");
        assert_eq!(why_not(&pair(769)), None);
    }

    #[test]
    fn a_failure_is_reported_once() {
        let cell: OnceLock<Option<u8>> = OnceLock::new();
        let (made, reported) = (Cell::new(0), Cell::new(0));
        for _ in 0..2 {
            let got = once(
                &cell,
                || {
                    made.set(made.get() + 1);
                    Err(Some("broken".to_string()))
                },
                |reason| {
                    assert_eq!(reason, "broken");
                    reported.set(reported.get() + 1);
                },
            );
            assert_eq!(got, None);
        }
        assert_eq!((made.get(), reported.get()), (1, 1));
    }

    /// A version without data is reported by its own cell; the pairs that
    /// need it stay silent.
    #[test]
    fn a_missing_version_is_reported_once_for_all_its_pairs() {
        let reported = Cell::new(0);
        let report = |_: &str| reported.set(reported.get() + 1);
        let version: Slot<mv::VersionData> = OnceLock::new();
        let data = |_| once(&version, || Err(Some(no_data(700))), report);
        let pairs: [Slot<mv::Tables>; 2] = Default::default();
        for (pair, client) in pairs.iter().zip([700, 769]) {
            assert!(once(pair, || build(data, client, 777), report).is_none());
        }
        assert_eq!(reported.get(), 1);
    }
}
