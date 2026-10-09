//! Configuration data of the virtual world (plan §2.6, §5.2): known packs,
//! synchronized registries and tags, plus the void dimension.
//!
//! A client that confirms `minecraft:core` of its release gets entry names
//! only (`synced.txt`); any other client gets the full data recorded from a
//! vanilla server, if this build has it (§2.5). Both get two entries of our
//! own with data: the dimension type `pumbo:void` and, before 774, the biome
//! `pumbo:void`. Appended at the end, they leave the vanilla IDs (and tags)
//! as they are.
//!
//! Pure void (requirement from the old gate on Pumpkin: no End sky, no
//! island): no skybox, black fog and sky, full ambient light so the platform,
//! the player and a map in hand stay bright.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use pumbo_nbt::{Compound, Tag};
use pumbo_protocol::packets::configuration::{
    KnownPack, RegistryData, RegistryEntry, RegistryTags, SelectKnownPacks, UpdateEnabledFeatures,
    UpdateTags,
};
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::types::Reader;
use pumbo_protocol::{Direction, PacketKind, Phase, VersionFeatures, VersionModule};

use crate::Error;

/// Name of our dimension type and of the pre-774 biome.
pub const VOID: &str = "pumbo:void";
const DIMENSION_TYPES: &str = "minecraft:dimension_type";
const BIOMES: &str = "minecraft:worldgen/biome";
/// The biome of the void from 774, when biomes no longer carry colours.
const END_BIOME: &str = "minecraft:the_end";

/// Registries, tags and IDs for one protocol and pack state, shared by all
/// players of that version.
#[derive(Debug)]
pub struct Prepared {
    /// `registry_data` and `update_tags` payloads with their IDs.
    pub frames: Vec<(i32, Bytes)>,
    /// Index of `pumbo:void` in `dimension_type`.
    pub dimension_type: i32,
    /// Biome of the void chunks.
    pub biome: i32,
}

/// (protocol, core pack confirmed) → prepared data.
type Cache = Mutex<HashMap<(i32, bool), Arc<Prepared>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// `select_known_packs`: `minecraft:core` in every release of the protocol
/// (releases of one protocol have different pack versions, §2.6).
pub fn known_packs_offer(module: &dyn VersionModule) -> SelectKnownPacks {
    SelectKnownPacks {
        packs: module
            .release_names()
            .iter()
            .map(|r| KnownPack {
                namespace: "minecraft".into(),
                id: "core".into(),
                version: r.clone(),
            })
            .collect(),
    }
}

/// Whether the client confirmed one of the offered core packs. A pack in
/// another version counts as none (§2.6).
pub fn confirmed(module: &dyn VersionModule, reply: &SelectKnownPacks) -> bool {
    reply.packs.iter().any(|p| {
        p.namespace == "minecraft" && p.id == "core" && module.release_names().contains(&p.version)
    })
}

/// `update_enabled_features` of the protocol.
pub fn features(module: &dyn VersionModule) -> Result<UpdateEnabledFeatures, Error> {
    let synced =
        pumbo_data::synced(module.protocol())?.ok_or(Error::NoData("synced registries"))?;
    Ok(UpdateEnabledFeatures {
        features: synced.features.clone(),
    })
}

/// Registry data for a client with (`known`) or without the core pack.
pub fn prepared(module: &dyn VersionModule, known: bool) -> Result<Arc<Prepared>, Error> {
    let key = (module.protocol().0, known);
    if let Some(p) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(p);
    }
    let p = Arc::new(if known {
        names_only(module)?
    } else {
        full(module)?
    });
    if let Ok(mut c) = cache().lock() {
        c.insert(key, p.clone());
    }
    Ok(p)
}

fn encode<P: Packet>(module: &dyn VersionModule, p: &P) -> Result<(i32, Bytes), Error> {
    let id = module
        .packet_id(Phase::Configuration, Direction::Clientbound, P::KIND)
        .ok_or(Error::NoPacket(P::KIND))?;
    let payload = packets::encode(p, &Ctx::new(module, Direction::Clientbound))?;
    Ok((id, Bytes::from(payload)))
}

/// Appends our entries; returns the dimension type index and the biome.
fn add_void(f: VersionFeatures, registries: &mut [RegistryData]) -> Result<(i32, i32), Error> {
    let index = |n: usize| i32::try_from(n).map_err(|_| Error::NoData("registry size"));
    let mut dimension_type = None;
    let mut biome = None;
    for r in registries.iter_mut() {
        if r.registry == DIMENSION_TYPES {
            dimension_type = Some(index(r.entries.len())?);
            r.entries.push(RegistryEntry {
                id: VOID.into(),
                data: Some(dimension_type_nbt(f)),
            });
        } else if r.registry == BIOMES {
            biome = Some(match biome_nbt(f) {
                Some(data) => {
                    let at = index(r.entries.len())?;
                    r.entries.push(RegistryEntry {
                        id: VOID.into(),
                        data: Some(data),
                    });
                    at
                }
                None => index(
                    r.entries
                        .iter()
                        .position(|e| e.id == END_BIOME)
                        .ok_or(Error::NoData("biome minecraft:the_end"))?,
                )?,
            });
        }
    }
    Ok((
        dimension_type.ok_or(Error::NoData("dimension types"))?,
        biome.ok_or(Error::NoData("biomes"))?,
    ))
}

fn names_only(module: &dyn VersionModule) -> Result<Prepared, Error> {
    let synced =
        pumbo_data::synced(module.protocol())?.ok_or(Error::NoData("synced registries"))?;
    let mut registries: Vec<RegistryData> = synced
        .registries
        .iter()
        .map(|r| RegistryData {
            registry: r.name.clone(),
            entries: r
                .entries
                .iter()
                .map(|e| RegistryEntry {
                    id: e.clone(),
                    data: None,
                })
                .collect(),
        })
        .collect();
    let (dimension_type, biome) = add_void(module.features(), &mut registries)?;
    let mut frames = registries
        .iter()
        .map(|r| encode(module, r))
        .collect::<Result<Vec<_>, _>>()?;
    let tags = UpdateTags {
        registries: synced
            .tags
            .iter()
            .map(|(registry, tags)| RegistryTags {
                registry: registry.clone(),
                tags: tags.clone(),
            })
            .collect(),
    };
    frames.push(encode(module, &tags)?);
    Ok(Prepared {
        frames,
        dimension_type,
        biome,
    })
}

/// Frames of the embedded recording (`pumbo-testclient` format `PUMBOREC1`:
/// phase, direction, ID, length, payload per frame).
fn recorded(data: &'static [u8]) -> Result<Vec<(i32, &'static [u8])>, Error> {
    let rest = data
        .strip_prefix(b"PUMBOREC1\n".as_slice())
        .ok_or(Error::NoData("recording header"))?;
    let mut r = Reader::new(rest);
    let mut out = Vec::new();
    while !r.is_empty() {
        r.u8()?;
        r.u8()?;
        let id = r.varint()?;
        let len = usize::try_from(r.varint()?).map_err(|_| Error::NoData("recording"))?;
        out.push((id, r.take(len)?));
    }
    Ok(out)
}

fn full(module: &dyn VersionModule) -> Result<Prepared, Error> {
    let data = pumbo_data::full_registry_data(module.protocol()).ok_or(Error::NoData(
        "full registry data (this build was made without it)",
    ))?;
    let ctx = Ctx::new(module, Direction::Clientbound);
    let mut registries = Vec::new();
    let mut tags = Vec::new();
    for (id, payload) in recorded(data)? {
        match module.packet_kind(Phase::Configuration, Direction::Clientbound, id) {
            Some(PacketKind::RegistryData) => {
                registries.push(packets::decode::<RegistryData>(payload, &ctx)?);
            }
            Some(PacketKind::UpdateTags) => tags.push((id, Bytes::from_static(payload))),
            _ => {}
        }
    }
    let (dimension_type, biome) = add_void(module.features(), &mut registries)?;
    let mut frames = registries
        .iter()
        .map(|r| encode(module, r))
        .collect::<Result<Vec<_>, _>>()?;
    frames.extend(tags);
    Ok(Prepared {
        frames,
        dimension_type,
        biome,
    })
}

fn compound(entries: Vec<(&str, Tag)>) -> Tag {
    let mut c = Compound::new();
    for (k, v) in entries {
        c.insert(k, v);
    }
    Tag::Compound(c)
}

fn s(v: &str) -> Tag {
    Tag::String(v.into())
}

fn flag(v: bool) -> Tag {
    Tag::Byte(i8::from(v))
}

/// `pumbo:void` in `dimension_type`, in the schema of the version. Fields as
/// in vanilla `minecraft:the_end` of the same version (compared by a test with
/// the recorded data), with no sky, black fog and full ambient light.
pub fn dimension_type_nbt(f: VersionFeatures) -> Tag {
    let mut e: Vec<(&str, Tag)> = Vec::new();
    if f.dimension_type_clock {
        e.push(("default_clock", s("minecraft:the_end")));
        e.push(("has_ender_dragon_fight", flag(false)));
    }
    e.push(("ambient_light", Tag::Float(1.0)));
    e.push(("monster_spawn_block_light_limit", Tag::Int(0)));
    e.push(("infiniburn", s("#minecraft:infiniburn_end")));
    e.push(("has_skylight", flag(true)));
    e.push(("coordinate_scale", Tag::Double(1.0)));
    e.push(("logical_height", Tag::Int(crate::world::HEIGHT)));
    e.push(("monster_spawn_light_level", Tag::Int(0)));
    e.push(("min_y", Tag::Int(0)));
    e.push(("has_ceiling", flag(false)));
    e.push(("height", Tag::Int(crate::world::HEIGHT)));
    if f.dimension_type_attributes {
        e.push(("has_fixed_time", flag(true)));
        e.push(("skybox", s("none")));
        e.push(("timelines", s("#minecraft:in_end")));
        let mut attrs = vec![
            ("minecraft:visual/fog_color", s("#000000")),
            ("minecraft:visual/sky_color", s("#000000")),
        ];
        if f.dimension_type_clock {
            attrs.push(("minecraft:visual/ambient_light_color", s("#ffffff")));
        }
        e.push(("attributes", compound(attrs)));
    } else {
        // The nether effects draw no sky; the fog colour comes from the biome.
        e.push(("effects", s("minecraft:the_nether")));
        e.push(("piglin_safe", flag(false)));
        e.push(("natural", flag(false)));
        e.push(("respawn_anchor_works", flag(false)));
        e.push(("bed_works", flag(false)));
        e.push(("has_raids", flag(false)));
        e.push(("ultrawarm", flag(false)));
        e.push(("fixed_time", Tag::Long(6000)));
    }
    compound(e)
}

/// `pumbo:void` in `worldgen/biome` before 774 (black sky and fog); `None`
/// from 774, where the dimension type carries the colours.
pub fn biome_nbt(f: VersionFeatures) -> Option<Tag> {
    if f.dimension_type_attributes {
        return None;
    }
    let mut effects = Vec::new();
    if f.biome_music_volume {
        effects.push(("music_volume", Tag::Float(1.0)));
    }
    effects.extend([
        ("sky_color", Tag::Int(0)),
        ("water_fog_color", Tag::Int(329_011)),
        ("fog_color", Tag::Int(0)),
        ("water_color", Tag::Int(4_159_204)),
    ]);
    Some(compound(vec![
        ("effects", compound(effects)),
        ("has_precipitation", flag(false)),
        ("temperature", Tag::Float(0.5)),
        ("downfall", Tag::Float(0.5)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumbo_protocol::ProtocolVersion;

    fn modules() -> Vec<pumbo_data::DataVersion> {
        pumbo_data::protocols()
            .map(|v| pumbo_data::DataVersion::new(pumbo_data::tables(v).unwrap()))
            .collect()
    }

    #[test]
    fn names_only_for_every_protocol() {
        for m in modules() {
            let p = prepared(&m, true).unwrap();
            let synced = pumbo_data::synced(m.protocol()).unwrap().unwrap();
            let types = synced.registry(DIMENSION_TYPES).unwrap();
            assert_eq!(p.dimension_type as usize, types.entries.len());
            // registry_data per registry, then the tags.
            assert_eq!(p.frames.len(), synced.registries.len() + 1);
        }
    }

    #[test]
    fn known_packs() {
        for m in modules() {
            let offer = known_packs_offer(&m);
            assert!(confirmed(&m, &offer));
            let mut other = offer.clone();
            for p in &mut other.packs {
                p.version = "0.0".into();
            }
            assert!(!confirmed(&m, &other));
        }
        let m = pumbo_data::DataVersion::new(pumbo_data::tables(ProtocolVersion::V767).unwrap());
        assert_eq!(known_packs_offer(&m).packs.len(), 2);
    }

    /// Our entries have the keys of vanilla `the_end` of the same version
    /// (when this build has the recorded data).
    #[test]
    fn void_entries_follow_the_vanilla_schema() {
        for m in modules() {
            let Some(data) = pumbo_data::full_registry_data(m.protocol()) else {
                eprintln!("{}: no full registry data, skipped", m.protocol());
                continue;
            };
            let ctx = Ctx::new(&m, Direction::Clientbound);
            for (id, payload) in recorded(data).unwrap() {
                if m.packet_kind(Phase::Configuration, Direction::Clientbound, id)
                    != Some(PacketKind::RegistryData)
                {
                    continue;
                }
                let r: RegistryData = packets::decode(payload, &ctx).unwrap();
                let ours = match r.registry.as_str() {
                    DIMENSION_TYPES => Some(dimension_type_nbt(m.features())),
                    BIOMES => biome_nbt(m.features()),
                    _ => None,
                };
                let Some(ours) = ours else { continue };
                let end = r
                    .entries
                    .iter()
                    .find(|e| e.id == "minecraft:the_end")
                    .and_then(|e| e.data.as_ref())
                    .and_then(Tag::as_compound)
                    .unwrap();
                let keys = |c: &Compound| {
                    let mut k: Vec<String> = c
                        .iter()
                        .filter(|(k, _)| !matches!(*k, "mood_sound" | "cloud_height"))
                        .map(|(k, _)| k.to_string())
                        .collect();
                    k.sort();
                    k
                };
                let ours = ours.as_compound().unwrap();
                assert_eq!(keys(ours), keys(end), "{} {}", m.protocol(), r.registry);
                for (k, v) in ours.iter() {
                    // An int provider: a number or a distribution, both valid.
                    if k == "monster_spawn_light_level" {
                        continue;
                    }
                    if let Some(theirs) = end.get(k) {
                        assert_eq!(v.id(), theirs.id(), "{} {k}", m.protocol());
                    }
                }
            }
            let p = prepared(&m, false).unwrap();
            assert!(p.dimension_type >= 4);
        }
    }
}
