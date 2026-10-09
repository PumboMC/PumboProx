//! Configuration phase.

use super::common::empty_packet;
use super::{Ctx, Packet, Text};
use crate::types::{DecodeError, EncodeError, MAX_STRING, Reader, WriteExt};
use crate::{Direction, PacketKind};

empty_packet!(
    /// `finish_configuration`, both directions (the serverbound one is the
    /// acknowledgement).
    FinishConfiguration,
    FinishConfiguration
);
empty_packet!(
    /// `reset_chat`.
    ResetChat,
    ResetChat
);
empty_packet!(
    /// `accept_code_of_conduct` (773+).
    AcceptCodeOfConduct,
    AcceptCodeOfConduct
);

/// One registry entry: name and optional data (absent when the client takes
/// it from a known pack).
#[derive(Debug, Clone, PartialEq)]
pub struct RegistryEntry {
    pub id: String,
    pub data: Option<pumbo_nbt::Tag>,
}

/// `registry_data`: one synchronized registry.
#[derive(Debug, Clone, PartialEq)]
pub struct RegistryData {
    pub registry: String,
    pub entries: Vec<RegistryEntry>,
}

impl Packet for RegistryData {
    const KIND: PacketKind = PacketKind::RegistryData;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let registry = r.identifier()?;
        let n = r.count(1 << 20, 2, "registry entries")?;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            entries.push(RegistryEntry {
                id: r.identifier()?,
                data: r.option(|r| r.nbt_required(ctx.nbt))?,
            });
        }
        Ok(Self { registry, entries })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_identifier(&self.registry)?;
        out.put_len(self.entries.len(), 1 << 20, "registry entries")?;
        for e in &self.entries {
            out.put_identifier(&e.id)?;
            out.put_bool(e.data.is_some());
            if let Some(d) = &e.data {
                out.put_nbt(Some(d))?;
            }
        }
        Ok(())
    }
}

/// Tags of one registry: (tag name, entry IDs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryTags {
    pub registry: String,
    pub tags: Vec<(String, Vec<i32>)>,
}

/// `update_tags`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateTags {
    pub registries: Vec<RegistryTags>,
}

impl Packet for UpdateTags {
    const KIND: PacketKind = PacketKind::UpdateTags;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        let n = r.count(1024, 2, "tag registries")?;
        let mut registries = Vec::with_capacity(n);
        for _ in 0..n {
            let registry = r.identifier()?;
            let t = r.count(1 << 20, 2, "tags")?;
            let mut tags = Vec::with_capacity(t);
            for _ in 0..t {
                let name = r.identifier()?;
                let e = r.count(1 << 20, 1, "tag entries")?;
                let ids = (0..e).map(|_| r.varint()).collect::<Result<_, _>>()?;
                tags.push((name, ids));
            }
            registries.push(RegistryTags { registry, tags });
        }
        Ok(Self { registries })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_len(self.registries.len(), 1024, "tag registries")?;
        for reg in &self.registries {
            out.put_identifier(&reg.registry)?;
            out.put_len(reg.tags.len(), 1 << 20, "tags")?;
            for (name, ids) in &reg.tags {
                out.put_identifier(name)?;
                out.put_len(ids.len(), 1 << 20, "tag entries")?;
                for id in ids {
                    out.put_varint(*id);
                }
            }
        }
        Ok(())
    }
}

fn identifiers(
    r: &mut Reader<'_>,
    max: usize,
    what: &'static str,
) -> Result<Vec<String>, DecodeError> {
    let n = r.count(max, 1, what)?;
    (0..n).map(|_| r.identifier()).collect()
}

fn put_identifiers(
    out: &mut Vec<u8>,
    list: &[String],
    max: usize,
    what: &'static str,
) -> Result<(), EncodeError> {
    out.put_len(list.len(), max, what)?;
    for s in list {
        out.put_identifier(s)?;
    }
    Ok(())
}

/// `update_enabled_features`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateEnabledFeatures {
    pub features: Vec<String>,
}

impl Packet for UpdateEnabledFeatures {
    const KIND: PacketKind = PacketKind::UpdateEnabledFeatures;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            features: identifiers(r, 1024, "feature flags")?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        put_identifiers(out, &self.features, 1024, "feature flags")
    }
}

/// A data pack reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

/// `select_known_packs`, both directions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectKnownPacks {
    pub packs: Vec<KnownPack>,
}

impl SelectKnownPacks {
    /// Vanilla accepts at most 64 packs from a client.
    const MAX_FROM_CLIENT: usize = 64;
    const MAX_FROM_SERVER: usize = 4096;

    fn max(ctx: &Ctx<'_>) -> usize {
        match ctx.direction {
            Direction::Serverbound => Self::MAX_FROM_CLIENT,
            Direction::Clientbound => Self::MAX_FROM_SERVER,
        }
    }
}

impl Packet for SelectKnownPacks {
    const KIND: PacketKind = PacketKind::SelectKnownPacks;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let n = r.count(Self::max(ctx), 3, "known packs")?;
        let mut packs = Vec::with_capacity(n);
        for _ in 0..n {
            packs.push(KnownPack {
                namespace: r.string(MAX_STRING)?,
                id: r.string(MAX_STRING)?,
                version: r.string(MAX_STRING)?,
            });
        }
        Ok(Self { packs })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_len(self.packs.len(), Self::max(ctx), "known packs")?;
        for p in &self.packs {
            out.put_string(&p.namespace, MAX_STRING)?;
            out.put_string(&p.id, MAX_STRING)?;
            out.put_string(&p.version, MAX_STRING)?;
        }
        Ok(())
    }
}

/// `show_dialog` in configuration: always an inline dialog (771+).
#[derive(Debug, Clone, PartialEq)]
pub struct ShowDialog {
    pub dialog: Text,
}

impl Packet for ShowDialog {
    const KIND: PacketKind = PacketKind::ShowDialog;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            dialog: r.nbt_required(ctx.nbt)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_nbt(Some(&self.dialog))
    }
}

/// `code_of_conduct` (773+).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeOfConduct {
    pub text: String,
}

impl Packet for CodeOfConduct {
    const KIND: PacketKind = PacketKind::CodeOfConduct;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            text: r.string(MAX_STRING)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.text, MAX_STRING)
    }
}

/// `post_effects` (777+): post-processing effects to activate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostEffects {
    pub effects: Vec<String>,
}

impl Packet for PostEffects {
    const KIND: PacketKind = PacketKind::PostEffects;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            effects: identifiers(r, 1024, "post effects")?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        put_identifiers(out, &self.effects, 1024, "post effects")
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::round_trip;
    use super::*;
    use pumbo_nbt::{Compound, Tag};

    #[test]
    fn round_trips() {
        round_trip(&FinishConfiguration, 767);
        round_trip(&ResetChat, 767);
        round_trip(&AcceptCodeOfConduct, 773);
        round_trip(
            &RegistryData {
                registry: "minecraft:dimension_type".into(),
                entries: vec![
                    RegistryEntry {
                        id: "minecraft:overworld".into(),
                        data: None,
                    },
                    RegistryEntry {
                        id: "minecraft:the_end".into(),
                        data: Some(Tag::Compound(Compound(vec![(
                            "has_skylight".into(),
                            Tag::Byte(0),
                        )]))),
                    },
                ],
            },
            767,
        );
        round_trip(
            &UpdateTags {
                registries: vec![RegistryTags {
                    registry: "minecraft:block".into(),
                    tags: vec![("minecraft:climbable".into(), vec![1, 2, 300])],
                }],
            },
            767,
        );
        round_trip(
            &UpdateEnabledFeatures {
                features: vec!["minecraft:vanilla".into()],
            },
            767,
        );
        round_trip(
            &SelectKnownPacks {
                packs: vec![KnownPack {
                    namespace: "minecraft".into(),
                    id: "core".into(),
                    version: "1.21.1".into(),
                }],
            },
            767,
        );
        round_trip(
            &ShowDialog {
                dialog: Tag::Compound(Compound::new()),
            },
            771,
        );
        round_trip(
            &CodeOfConduct {
                text: "Be nice".into(),
            },
            773,
        );
        round_trip(
            &PostEffects {
                effects: vec!["minecraft:creeper".into()],
            },
            777,
        );
    }
}
