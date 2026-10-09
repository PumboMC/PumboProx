//! The `commands` packet: the Brigadier graph (plan §2.8).
//!
//! Argument parsers are identified by ID; the name comes from the version's
//! `command_argument_type` registry. Parsers with properties are decoded from
//! their description on minecraft.wiki ("Command data"); every other parser
//! must be on the list of parsers known to have none. An unknown parser is a
//! decode error, so the proxy forwards the original graph instead of merging.

use super::{Ctx, Packet};
use crate::PacketKind;
use crate::types::{DecodeError, EncodeError, Reader, WriteExt};

/// Node types (lowest two flag bits).
pub const NODE_ROOT: u8 = 0;
pub const NODE_LITERAL: u8 = 1;
pub const NODE_ARGUMENT: u8 = 2;
/// Flag bits.
pub const FLAG_EXECUTABLE: u8 = 0x04;
pub const FLAG_REDIRECT: u8 = 0x08;
pub const FLAG_SUGGESTIONS: u8 = 0x10;
/// Node needs a permission level above 0 (771+).
pub const FLAG_RESTRICTED: u8 = 0x20;

const MAX_NODES: usize = 1 << 18;

/// Parsers without properties, by name (without the `minecraft:` namespace for
/// the game's own). Up to 26.3.
const NO_PROPERTIES: &[&str] = &[
    "brigadier:bool",
    "angle",
    "block_pos",
    "block_predicate",
    "block_state",
    "color",
    "column_pos",
    "component",
    "context_float_provider",
    "context_int_provider",
    "dialog",
    "dimension",
    "entity_anchor",
    "feature",
    "float_range",
    "function",
    "game_profile",
    "gamemode",
    "heightmap",
    "hex_color",
    "int_range",
    "item_predicate",
    "item_slot",
    "item_slots",
    "item_stack",
    "loot_modifier",
    "loot_predicate",
    "loot_table",
    "message",
    "nbt_compound_tag",
    "nbt_path",
    "nbt_tag",
    "objective",
    "objective_criteria",
    "operation",
    "particle",
    "resource_location",
    "rotation",
    "scoreboard_slot",
    "slot_source",
    "style",
    "swing_animation",
    "swizzle",
    "team",
    "team_color",
    "template_mirror",
    "template_rotation",
    "uuid",
    "vec2",
    "vec3",
];

/// Numeric range of a Brigadier number parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range<T> {
    pub min: Option<T>,
    pub max: Option<T>,
}

/// Properties of an argument parser.
#[derive(Debug, Clone, PartialEq)]
pub enum ParserProperties {
    None,
    Float(Range<f32>),
    Double(Range<f64>),
    Integer(Range<i32>),
    Long(Range<i64>),
    /// 0 single word, 1 quotable phrase, 2 greedy phrase.
    String(i32),
    /// Entity selector flags (0x01 single, 0x02 players only).
    Entity(u8),
    /// Score holder flags (0x01 multiple).
    ScoreHolder(u8),
    /// Minimum ticks.
    Time(i32),
    /// Registry identifier (`resource`, `resource_key`, `resource_or_tag`,
    /// `resource_or_tag_key`, `resource_selector`).
    Registry(String),
}

/// An argument node's parser.
#[derive(Debug, Clone, PartialEq)]
pub struct Parser {
    pub id: i32,
    pub properties: ParserProperties,
}

/// One node.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub flags: u8,
    pub children: Vec<i32>,
    pub redirect: Option<i32>,
    /// Literal or argument name.
    pub name: Option<String>,
    pub parser: Option<Parser>,
    pub suggestions: Option<String>,
}

impl Node {
    pub fn kind(&self) -> u8 {
        self.flags & 0x03
    }
}

/// `commands`.
#[derive(Debug, Clone, PartialEq)]
pub struct Commands {
    pub nodes: Vec<Node>,
    pub root: i32,
}

fn flags_range<T>(
    r: &mut Reader<'_>,
    read: impl Fn(&mut Reader<'_>) -> Result<T, DecodeError>,
) -> Result<(u8, Range<T>), DecodeError> {
    let flags = r.u8()?;
    let min = if flags & 0x01 != 0 {
        Some(read(r)?)
    } else {
        None
    };
    let max = if flags & 0x02 != 0 {
        Some(read(r)?)
    } else {
        None
    };
    Ok((flags, Range { min, max }))
}

fn put_range<T: Copy>(out: &mut Vec<u8>, range: &Range<T>, write: fn(&mut Vec<u8>, T)) {
    let flags = u8::from(range.min.is_some()) | (u8::from(range.max.is_some()) << 1);
    out.put_u8(flags);
    if let Some(v) = range.min {
        write(out, v);
    }
    if let Some(v) = range.max {
        write(out, v);
    }
}

fn decode_properties(r: &mut Reader<'_>, name: &str) -> Result<ParserProperties, DecodeError> {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    Ok(match short {
        "brigadier:float" => ParserProperties::Float(range_strict(flags_range(r, |r| r.f32())?)?),
        "brigadier:double" => ParserProperties::Double(range_strict(flags_range(r, |r| r.f64())?)?),
        "brigadier:integer" => {
            ParserProperties::Integer(range_strict(flags_range(r, |r| r.i32())?)?)
        }
        "brigadier:long" => ParserProperties::Long(range_strict(flags_range(r, |r| r.i64())?)?),
        "brigadier:string" => ParserProperties::String(r.varint()?),
        "entity" => ParserProperties::Entity(r.u8()?),
        "score_holder" => ParserProperties::ScoreHolder(r.u8()?),
        "time" => ParserProperties::Time(r.i32()?),
        "resource"
        | "resource_key"
        | "resource_or_tag"
        | "resource_or_tag_key"
        | "resource_selector" => ParserProperties::Registry(r.identifier()?),
        other if NO_PROPERTIES.contains(&other) => ParserProperties::None,
        _ => return Err(DecodeError::Invalid("unknown command argument parser")),
    })
}

/// Number ranges use only the two low flag bits; anything else would not
/// encode back to the same bytes.
fn range_strict<T>((flags, range): (u8, Range<T>)) -> Result<Range<T>, DecodeError> {
    if flags & !0x03 != 0 {
        return Err(DecodeError::Invalid("number range flags"));
    }
    Ok(range)
}

fn encode_properties(out: &mut Vec<u8>, p: &ParserProperties) -> Result<(), EncodeError> {
    match p {
        ParserProperties::None => {}
        ParserProperties::Float(r) => put_range(out, r, |o, v| o.put_f32(v)),
        ParserProperties::Double(r) => put_range(out, r, |o, v| o.put_f64(v)),
        ParserProperties::Integer(r) => put_range(out, r, |o, v| o.put_i32(v)),
        ParserProperties::Long(r) => put_range(out, r, |o, v| o.put_i64(v)),
        ParserProperties::String(mode) => out.put_varint(*mode),
        ParserProperties::Entity(f) | ParserProperties::ScoreHolder(f) => out.put_u8(*f),
        ParserProperties::Time(min) => out.put_i32(*min),
        ParserProperties::Registry(id) => out.put_identifier(id)?,
    }
    Ok(())
}

impl Packet for Commands {
    const KIND: PacketKind = PacketKind::Commands;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let n = r.count(MAX_NODES, 2, "command nodes")?;
        let mut nodes = Vec::with_capacity(n);
        for _ in 0..n {
            let flags = r.u8()?;
            let kind = flags & 0x03;
            if kind == 3 {
                return Err(DecodeError::Invalid("command node type"));
            }
            let c = r.count(MAX_NODES, 1, "command children")?;
            let children = (0..c).map(|_| r.varint()).collect::<Result<Vec<_>, _>>()?;
            let redirect = if flags & FLAG_REDIRECT != 0 {
                Some(r.varint()?)
            } else {
                None
            };
            let name = if kind == NODE_LITERAL || kind == NODE_ARGUMENT {
                Some(r.string(32_767)?)
            } else {
                None
            };
            let parser = if kind == NODE_ARGUMENT {
                let id = r.varint()?;
                let parser_name = ctx
                    .module
                    .command_argument_type(id)
                    .ok_or(DecodeError::Invalid("command argument parser id"))?;
                Some(Parser {
                    id,
                    properties: decode_properties(r, parser_name)?,
                })
            } else {
                None
            };
            let suggestions = if flags & FLAG_SUGGESTIONS != 0 {
                Some(r.identifier()?)
            } else {
                None
            };
            nodes.push(Node {
                flags,
                children,
                redirect,
                name,
                parser,
                suggestions,
            });
        }
        let root = r.varint()?;
        let in_range = |i: i32| usize::try_from(i).is_ok_and(|i| i < nodes.len());
        let links_ok = nodes.iter().all(|node| {
            node.children.iter().all(|c| in_range(*c)) && node.redirect.is_none_or(in_range)
        });
        if !in_range(root) || !links_ok {
            return Err(DecodeError::Invalid("command node index"));
        }
        Ok(Self { nodes, root })
    }

    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_len(self.nodes.len(), MAX_NODES, "command nodes")?;
        for node in &self.nodes {
            out.put_u8(node.flags);
            out.put_len(node.children.len(), MAX_NODES, "command children")?;
            for c in &node.children {
                out.put_varint(*c);
            }
            match (node.flags & FLAG_REDIRECT != 0, node.redirect) {
                (true, Some(r)) => out.put_varint(r),
                (false, None) => {}
                _ => return Err(EncodeError::Invalid("redirect flag")),
            }
            let kind = node.kind();
            match (kind, &node.name) {
                (NODE_LITERAL | NODE_ARGUMENT, Some(n)) => out.put_string(n, 32_767)?,
                (NODE_ROOT, None) => {}
                _ => return Err(EncodeError::Invalid("node name")),
            }
            match (kind, &node.parser) {
                (NODE_ARGUMENT, Some(p)) => {
                    out.put_varint(p.id);
                    encode_properties(out, &p.properties)?;
                }
                (NODE_ROOT | NODE_LITERAL, None) => {}
                _ => return Err(EncodeError::Invalid("node parser")),
            }
            match (node.flags & FLAG_SUGGESTIONS != 0, &node.suggestions) {
                (true, Some(s)) => out.put_identifier(s)?,
                (false, None) => {}
                _ => return Err(EncodeError::Invalid("suggestions flag")),
            }
        }
        out.put_varint(self.root);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::TestVersion;
    use super::super::{decode, encode};
    use super::*;
    use crate::Direction;

    fn parsers() -> Vec<String> {
        [
            "brigadier:bool",
            "brigadier:float",
            "brigadier:double",
            "brigadier:integer",
            "brigadier:long",
            "brigadier:string",
            "minecraft:entity",
            "minecraft:time",
            "minecraft:resource",
            "minecraft:vec3",
            "minecraft:mystery",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    fn graph() -> Commands {
        let arg = |name: &str, id: i32, properties| Node {
            flags: NODE_ARGUMENT | FLAG_EXECUTABLE,
            children: vec![],
            redirect: None,
            name: Some(name.into()),
            parser: Some(Parser { id, properties }),
            suggestions: None,
        };
        Commands {
            nodes: vec![
                Node {
                    flags: NODE_ROOT,
                    children: vec![1, 2],
                    redirect: None,
                    name: None,
                    parser: None,
                    suggestions: None,
                },
                Node {
                    flags: NODE_LITERAL | FLAG_EXECUTABLE | FLAG_RESTRICTED,
                    children: vec![3, 4, 5, 6, 7, 8, 9, 10],
                    redirect: None,
                    name: Some("server".into()),
                    parser: None,
                    suggestions: None,
                },
                Node {
                    flags: NODE_LITERAL | FLAG_REDIRECT,
                    children: vec![],
                    redirect: Some(1),
                    name: Some("srv".into()),
                    parser: None,
                    suggestions: None,
                },
                Node {
                    suggestions: Some("minecraft:ask_server".into()),
                    flags: NODE_ARGUMENT | FLAG_SUGGESTIONS,
                    ..arg("name", 5, ParserProperties::String(2))
                },
                arg(
                    "f",
                    1,
                    ParserProperties::Float(Range {
                        min: Some(0.0),
                        max: None,
                    }),
                ),
                arg(
                    "d",
                    2,
                    ParserProperties::Double(Range {
                        min: None,
                        max: Some(1.5),
                    }),
                ),
                arg(
                    "i",
                    3,
                    ParserProperties::Integer(Range {
                        min: Some(-1),
                        max: Some(9),
                    }),
                ),
                arg("e", 6, ParserProperties::Entity(3)),
                arg("t", 7, ParserProperties::Time(0)),
                arg("r", 8, ParserProperties::Registry("minecraft:item".into())),
                arg("v", 9, ParserProperties::None),
            ],
            root: 0,
        }
    }

    #[test]
    fn graph_round_trip() {
        let v = TestVersion(777, parsers());
        let ctx = Ctx::new(&v, Direction::Clientbound);
        let g = graph();
        let bytes = encode(&g, &ctx).unwrap();
        let back: Commands = decode(&bytes, &ctx).unwrap();
        assert_eq!(back, g);
        assert_eq!(encode(&back, &ctx).unwrap(), bytes);
    }

    #[test]
    fn unknown_parser_and_bad_links_fail() {
        let v = TestVersion(777, parsers());
        let ctx = Ctx::new(&v, Direction::Clientbound);
        let mut g = graph();
        if let Some(Node {
            parser: Some(p), ..
        }) = g.nodes.get_mut(10)
        {
            p.id = 10; // "minecraft:mystery"
        }
        let bytes = encode(&g, &ctx).unwrap();
        assert_eq!(
            decode::<Commands>(&bytes, &ctx),
            Err(DecodeError::Invalid("unknown command argument parser"))
        );
        let mut g = graph();
        g.root = 99;
        let bytes = encode(&g, &ctx).unwrap();
        assert_eq!(
            decode::<Commands>(&bytes, &ctx),
            Err(DecodeError::Invalid("command node index"))
        );
    }
}
