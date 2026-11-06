//! `multiversion`: translation inside the proxy for clients 1.21–26.2 on a
//! 26.3 backend, with `pumbo-translate-mv` (plan E3b). This module feeds it
//! the tables of both versions from `pumbo-data`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use pumbo_core::registry::{ModuleConfig, ModuleError};
use pumbo_core::translate::{
    Dropped, PacketTranslator, Translated, TranslationPlan, TranslatorProvider,
};
use pumbo_protocol::{Direction, Phase, ProtocolVersion, RawFrame};
use pumbo_translate_mv as mv;

pub const MULTIVERSION: &str = "multiversion";

/// Tables per client protocol, built on first use (tens of milliseconds) and
/// shared by all sessions of that protocol.
#[derive(Debug, Default)]
pub struct Multiversion {
    tables: Mutex<HashMap<i32, Option<Arc<mv::Tables>>>>,
}

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

impl Multiversion {
    fn tables(&self, client: ProtocolVersion) -> Option<Arc<mv::Tables>> {
        let mut cache = self.tables.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .entry(client.0)
            .or_insert_with(|| {
                // Built under the lock on the first join of a protocol; fine
                // while it takes milliseconds, move to spawn_blocking if it grows.
                let client = version_data(client)?;
                let server = version_data(ProtocolVersion(mv::SERVER_PROTOCOL))?;
                mv::Tables::new(client, server).ok().map(Arc::new)
            })
            .clone()
    }
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
        let backend = backend?;
        if backend.0 != mv::SERVER_PROTOCOL || client.0 < mv::OLDEST_CLIENT || client >= backend {
            return None;
        }
        let tables = self.tables(client)?;
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
        let client = self.inner.tables().client_protocol();
        out.dropped.extend(self.out.dropped.drain(..).map(|d| {
            let (protocol, direction) = match d.bound {
                mv::Bound::Client => (mv::SERVER_PROTOCOL, Direction::Clientbound),
                mv::Bound::Server => (client, Direction::Serverbound),
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
    use super::*;

    /// Dropped packets come out by name: one the client cannot read, and an
    /// ID no table has.
    #[test]
    fn drops_carry_the_packet_name() {
        let Some(TranslationPlan::Translate(mut t)) = Multiversion::default().plan(
            ProtocolVersion(769),
            Some(ProtocolVersion(mv::SERVER_PROTOCOL)),
        ) else {
            panic!("no translator");
        };
        let tables = pumbo_data::tables(ProtocolVersion(mv::SERVER_PROTOCOL)).unwrap();
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
}
