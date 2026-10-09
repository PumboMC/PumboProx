//! Table model and its text format.
//!
//! One directory per protocol with four line-oriented files. The format is
//! plain text on purpose: a generator run for a new Minecraft release shows up
//! as a readable diff in git, which is part of the new-version routine (§2.10).
//! The default `minecraft:` namespace is left out of names in the files.
//!
//! - `meta.txt`: `protocol <n>` and one `release <name> <world-version> <server-jar-sha1>`
//!   per release with this protocol (newest last);
//! - `packets.txt`: `<phase> <direction> <id> <name>` with names normalised to
//!   the current report names, plus `alias <phase> <direction> <report-name> <name>`
//!   for every rename the generator applied;
//! - `registries.txt`: `registry <name> <count>` followed by `count` entry names
//!   in ID order;
//! - `blocks.txt`: `<name> <first-state-id> <default-offset> [<property>=<v1>,<v2>...]...`
//!   with properties sorted by name. State IDs are the cartesian product of the
//!   property values in that order, last property varying fastest.

use std::collections::HashMap;
use std::fmt::Write as _;

use pumbo_protocol::{Direction, Phase, ProtocolVersion};

use crate::DataError;

const DEFAULT_NS: &str = "minecraft:";

/// Adds the default namespace to a short name.
pub fn full_name(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("{DEFAULT_NS}{name}")
    }
}

/// Removes the default namespace.
pub fn short_name(name: &str) -> &str {
    name.strip_prefix(DEFAULT_NS).unwrap_or(name)
}

/// One Minecraft release with this protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub name: String,
    pub world_version: i32,
    pub server_sha1: String,
}

/// One packet ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PacketEntry {
    pub phase: Phase,
    pub direction: Direction,
    pub id: i32,
    /// Current report name, without namespace.
    pub name: String,
}

/// A rename applied by the generator (report name → current name).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PacketAlias {
    pub phase: Phase,
    pub direction: Direction,
    pub report_name: String,
    pub name: String,
}

/// A static registry: entry ID is the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    /// Full name, e.g. `minecraft:item`.
    pub name: String,
    /// Full entry names in ID order.
    pub entries: Vec<String>,
    index: HashMap<String, u32>,
}

impl Registry {
    pub fn new(name: &str, entries: Vec<String>) -> Self {
        let index = entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| u32::try_from(i).ok().map(|i| (e.clone(), i)))
            .collect();
        Self {
            name: full_name(name),
            entries,
            index,
        }
    }

    /// ID of an entry; accepts names with or without the default namespace.
    pub fn id(&self, name: &str) -> Option<u32> {
        if name.contains(':') {
            self.index.get(name).copied()
        } else {
            self.index.get(&full_name(name)).copied()
        }
    }

    pub fn name(&self, id: u32) -> Option<&str> {
        self.entries.get(id as usize).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Block state property: name and allowed values in report order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockProperty {
    pub name: String,
    pub values: Vec<String>,
}

/// A block in compact form: all states follow from the properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Full name, e.g. `minecraft:barrier`.
    pub name: String,
    pub first_state: u32,
    pub default_offset: u32,
    /// Sorted by name (this is the state order).
    pub properties: Vec<BlockProperty>,
}

impl Block {
    pub fn state_count(&self) -> u32 {
        self.properties
            .iter()
            .map(|p| u32::try_from(p.values.len()).unwrap_or(u32::MAX))
            .fold(1u32, u32::saturating_mul)
    }

    pub fn default_state(&self) -> u32 {
        self.first_state.saturating_add(self.default_offset)
    }

    /// State ID for the given property values; properties not given take the
    /// default state's value. `None` for an unknown property or value.
    pub fn state_id(&self, props: &[(&str, &str)]) -> Option<u32> {
        if props
            .iter()
            .any(|(n, _)| !self.properties.iter().any(|p| p.name == *n))
        {
            return None;
        }
        let defaults = self.state_values(self.default_offset)?;
        let mut offset = 0u32;
        for (p, default) in self.properties.iter().zip(defaults) {
            let value = props
                .iter()
                .find(|(n, _)| *n == p.name)
                .map_or(default, |(_, v)| v);
            let idx = p.values.iter().position(|v| v == value)?;
            let len = u32::try_from(p.values.len()).ok()?;
            offset = offset
                .checked_mul(len)?
                .checked_add(u32::try_from(idx).ok()?)?;
        }
        self.first_state.checked_add(offset)
    }

    /// Property values of the state at `offset` from the first state.
    pub fn state_values(&self, offset: u32) -> Option<Vec<&str>> {
        if offset >= self.state_count() {
            return None;
        }
        let mut rest = offset;
        let mut values = vec![""; self.properties.len()];
        for (slot, p) in values.iter_mut().zip(&self.properties).rev() {
            let len = u32::try_from(p.values.len()).ok()?;
            let idx = rest.checked_rem(len)?;
            rest /= len;
            *slot = p.values.get(idx as usize)?.as_str();
        }
        Some(values)
    }
}

/// All tables of one protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tables {
    pub protocol: ProtocolVersion,
    pub releases: Vec<Release>,
    pub packets: Vec<PacketEntry>,
    pub aliases: Vec<PacketAlias>,
    pub registries: Vec<Registry>,
    pub blocks: Vec<Block>,
    packet_index: HashMap<(Phase, Direction, String), i32>,
    block_index: HashMap<String, usize>,
}

impl Tables {
    pub fn new(
        protocol: ProtocolVersion,
        releases: Vec<Release>,
        mut packets: Vec<PacketEntry>,
        mut aliases: Vec<PacketAlias>,
        mut registries: Vec<Registry>,
        mut blocks: Vec<Block>,
    ) -> Self {
        packets.sort();
        aliases.sort();
        registries.sort_by(|a, b| a.name.cmp(&b.name));
        blocks.sort_by_key(|b| b.first_state);
        let packet_index = packets
            .iter()
            .map(|p| ((p.phase, p.direction, p.name.clone()), p.id))
            .collect();
        let block_index = blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (b.name.clone(), i))
            .collect();
        Self {
            protocol,
            releases,
            packets,
            aliases,
            registries,
            blocks,
            packet_index,
            block_index,
        }
    }

    pub fn release_names(&self) -> Vec<String> {
        self.releases.iter().map(|r| r.name.clone()).collect()
    }

    /// Packet ID by current report name (without namespace).
    pub fn packet_id(&self, phase: Phase, direction: Direction, name: &str) -> Option<i32> {
        self.packet_index
            .get(&(phase, direction, short_name(name).to_string()))
            .copied()
    }

    pub fn registry(&self, name: &str) -> Option<&Registry> {
        let name = full_name(name);
        self.registries.iter().find(|r| r.name == name)
    }

    pub fn block(&self, name: &str) -> Option<&Block> {
        self.block_index
            .get(&full_name(name))
            .and_then(|i| self.blocks.get(*i))
    }

    /// Block state ID; missing properties take default values.
    pub fn block_state(&self, name: &str, props: &[(&str, &str)]) -> Option<u32> {
        self.block(name)?.state_id(props)
    }

    /// Total number of block states.
    pub fn block_state_count(&self) -> u32 {
        self.blocks
            .iter()
            .map(Block::state_count)
            .fold(0u32, u32::saturating_add)
    }

    /// Writes the four files: (file name, content).
    pub fn to_files(&self) -> [(&'static str, String); 4] {
        let mut meta = format!("protocol {}\n", self.protocol.0);
        for r in &self.releases {
            let _ = writeln!(
                meta,
                "release {} {} {}",
                r.name, r.world_version, r.server_sha1
            );
        }
        let mut packets = String::new();
        for a in &self.aliases {
            let _ = writeln!(
                packets,
                "alias {} {} {} {}",
                a.phase.report_name(),
                a.direction.report_name(),
                a.report_name,
                a.name
            );
        }
        for p in &self.packets {
            let _ = writeln!(
                packets,
                "{} {} {} {}",
                p.phase.report_name(),
                p.direction.report_name(),
                p.id,
                p.name
            );
        }
        let mut registries = String::new();
        for r in &self.registries {
            let _ = writeln!(
                registries,
                "registry {} {}",
                short_name(&r.name),
                r.entries.len()
            );
            for e in &r.entries {
                registries.push_str(short_name(e));
                registries.push('\n');
            }
        }
        let mut blocks = String::new();
        for b in &self.blocks {
            let _ = write!(
                blocks,
                "{} {} {}",
                short_name(&b.name),
                b.first_state,
                b.default_offset
            );
            for p in &b.properties {
                let _ = write!(blocks, " {}={}", p.name, p.values.join(","));
            }
            blocks.push('\n');
        }
        [
            ("meta.txt", meta),
            ("packets.txt", packets),
            ("registries.txt", registries),
            ("blocks.txt", blocks),
        ]
    }

    /// Parses the four files.
    pub fn parse(
        meta: &str,
        packets: &str,
        registries: &str,
        blocks: &str,
    ) -> Result<Self, DataError> {
        let (protocol, releases) = parse_meta(meta)?;
        let err = |file: &'static str, line: usize, message: String| DataError::Parse {
            protocol: protocol.0,
            file,
            line,
            message,
        };

        let mut packet_list = Vec::new();
        let mut aliases = Vec::new();
        for (n, line) in lines(packets) {
            let f: Vec<&str> = line.split(' ').collect();
            let field = |i: usize| {
                f.get(i)
                    .copied()
                    .ok_or_else(|| err("packets.txt", n, "missing field".into()))
            };
            if field(0)? == "alias" {
                aliases.push(PacketAlias {
                    phase: parse_phase(field(1)?)
                        .ok_or_else(|| err("packets.txt", n, "bad phase".into()))?,
                    direction: parse_direction(field(2)?)
                        .ok_or_else(|| err("packets.txt", n, "bad direction".into()))?,
                    report_name: field(3)?.to_string(),
                    name: field(4)?.to_string(),
                });
                continue;
            }
            if f.len() != 4 {
                return Err(err("packets.txt", n, "expected 4 fields".into()));
            }
            packet_list.push(PacketEntry {
                phase: parse_phase(field(0)?)
                    .ok_or_else(|| err("packets.txt", n, "bad phase".into()))?,
                direction: parse_direction(field(1)?)
                    .ok_or_else(|| err("packets.txt", n, "bad direction".into()))?,
                id: field(2)?
                    .parse()
                    .map_err(|_| err("packets.txt", n, "bad id".into()))?,
                name: field(3)?.to_string(),
            });
        }

        let mut registry_list = Vec::new();
        let mut it = lines(registries);
        while let Some((n, line)) = it.next() {
            let f: Vec<&str> = line.split(' ').collect();
            let (Some(&"registry"), Some(name), Some(count), None) =
                (f.first(), f.get(1), f.get(2), f.get(3))
            else {
                return Err(err(
                    "registries.txt",
                    n,
                    "expected a registry header".into(),
                ));
            };
            let count: usize = count
                .parse()
                .map_err(|_| err("registries.txt", n, "bad count".into()))?;
            let mut entries = Vec::with_capacity(count.min(1 << 16));
            for _ in 0..count {
                let (_, e) = it
                    .next()
                    .ok_or_else(|| err("registries.txt", n, "registry ends early".into()))?;
                entries.push(full_name(e));
            }
            registry_list.push(Registry::new(name, entries));
        }

        let mut block_list = Vec::new();
        for (n, line) in lines(blocks) {
            let mut f = line.split(' ');
            let mut next = |what: &str| {
                f.next()
                    .ok_or_else(|| err("blocks.txt", n, format!("missing {what}")))
            };
            let name = full_name(next("name")?);
            let first_state = next("first state")?
                .parse()
                .map_err(|_| err("blocks.txt", n, "bad first state".into()))?;
            let default_offset = next("default offset")?
                .parse()
                .map_err(|_| err("blocks.txt", n, "bad default offset".into()))?;
            let mut properties = Vec::new();
            for p in f {
                let (pname, values) = p
                    .split_once('=')
                    .ok_or_else(|| err("blocks.txt", n, "bad property".into()))?;
                properties.push(BlockProperty {
                    name: pname.to_string(),
                    values: values.split(',').map(str::to_string).collect(),
                });
            }
            let block = Block {
                name,
                first_state,
                default_offset,
                properties,
            };
            if block.default_offset >= block.state_count() {
                return Err(err("blocks.txt", n, "default outside states".into()));
            }
            block_list.push(block);
        }

        Ok(Self::new(
            protocol,
            releases,
            packet_list,
            aliases,
            registry_list,
            block_list,
        ))
    }
}

fn lines(s: &str) -> impl Iterator<Item = (usize, &str)> {
    s.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim_end()))
        .filter(|(_, l)| !l.is_empty())
}

fn parse_phase(s: &str) -> Option<Phase> {
    Phase::from_report_name(s)
}

fn parse_direction(s: &str) -> Option<Direction> {
    Direction::from_report_name(s)
}

fn parse_meta(meta: &str) -> Result<(ProtocolVersion, Vec<Release>), DataError> {
    let err = |line: usize, message: &str| DataError::Parse {
        protocol: 0,
        file: "meta.txt",
        line,
        message: message.into(),
    };
    let mut protocol = None;
    let mut releases = Vec::new();
    for (n, line) in lines(meta) {
        let f: Vec<&str> = line.split(' ').collect();
        match f.as_slice() {
            ["protocol", p] => {
                protocol = Some(ProtocolVersion(
                    p.parse().map_err(|_| err(n, "bad protocol"))?,
                ));
            }
            ["release", name, world, sha1] => releases.push(Release {
                name: (*name).to_string(),
                world_version: world.parse().map_err(|_| err(n, "bad world version"))?,
                server_sha1: (*sha1).to_string(),
            }),
            _ => return Err(err(n, "unknown line")),
        }
    }
    let protocol = protocol.ok_or_else(|| err(0, "missing protocol"))?;
    if releases.is_empty() {
        return Err(err(0, "no releases"));
    }
    Ok((protocol, releases))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tables {
        Tables::new(
            ProtocolVersion(767),
            vec![Release {
                name: "1.21.1".into(),
                world_version: 3955,
                server_sha1: "abc".into(),
            }],
            vec![PacketEntry {
                phase: Phase::Login,
                direction: Direction::Clientbound,
                id: 2,
                name: "login_finished".into(),
            }],
            vec![PacketAlias {
                phase: Phase::Login,
                direction: Direction::Clientbound,
                report_name: "game_profile".into(),
                name: "login_finished".into(),
            }],
            vec![Registry::new(
                "minecraft:item",
                vec!["minecraft:air".into(), "other:thing".into()],
            )],
            vec![
                Block {
                    name: "minecraft:air".into(),
                    first_state: 0,
                    default_offset: 0,
                    properties: vec![],
                },
                Block {
                    name: "minecraft:chest".into(),
                    first_state: 10,
                    default_offset: 1,
                    properties: vec![
                        BlockProperty {
                            name: "facing".into(),
                            values: vec!["north".into(), "south".into()],
                        },
                        BlockProperty {
                            name: "waterlogged".into(),
                            values: vec!["true".into(), "false".into()],
                        },
                    ],
                },
            ],
        )
    }

    #[test]
    fn text_round_trip() {
        let t = sample();
        let [meta, packets, registries, blocks] = t.to_files();
        let back = Tables::parse(&meta.1, &packets.1, &registries.1, &blocks.1).unwrap();
        assert_eq!(back, t);
        assert!(registries.1.contains("registry item 2\nair\nother:thing\n"));
    }

    #[test]
    fn lookups() {
        let t = sample();
        assert_eq!(
            t.packet_id(Phase::Login, Direction::Clientbound, "login_finished"),
            Some(2)
        );
        let item = t.registry("item").unwrap();
        assert_eq!(item.id("air"), Some(0));
        assert_eq!(item.id("minecraft:air"), Some(0));
        assert_eq!(item.id("other:thing"), Some(1));
        assert_eq!(item.name(1), Some("other:thing"));
        let chest = t.block("chest").unwrap();
        assert_eq!(chest.state_count(), 4);
        assert_eq!(chest.default_state(), 11);
        assert_eq!(t.block_state("chest", &[]), Some(11));
        assert_eq!(t.block_state("chest", &[("facing", "south")]), Some(13));
        assert_eq!(
            t.block_state("chest", &[("facing", "south"), ("waterlogged", "true")]),
            Some(12)
        );
        assert_eq!(t.block_state("chest", &[("color", "red")]), None);
        assert_eq!(t.block_state("chest", &[("facing", "up")]), None);
        assert_eq!(chest.state_values(3), Some(vec!["south", "false"]));
        assert_eq!(chest.state_values(4), None);
        assert_eq!(t.block_state_count(), 5);
    }

    #[test]
    fn parse_errors_name_the_line() {
        let e = Tables::parse("protocol 767\nrelease a 1 x\n", "login x 1 y\n", "", "")
            .unwrap_err()
            .to_string();
        assert!(e.contains("packets.txt:1"), "{e}");
    }
}
