//! ID remaps between the server's and the client's registries, built from the
//! names in both versions' tables. Entries the client lacks get the stand-in
//! of ViaVersion Mappings (`data/via_mappings.txt`), otherwise they are dropped.

use std::collections::HashMap;

use crate::data::{Block, VersionData};

const VIA: &str = include_str!("../data/via_mappings.txt");

/// Stand-ins from ViaVersion Mappings: the diffs of the steps from the server
/// down to one client, applied in order like the protocol chain of ViaBackwards.
#[derive(Debug, Default)]
pub(crate) struct Via {
    /// Per step, newest first: section → name → stand-in (`-`: none).
    steps: Vec<HashMap<&'static str, HashMap<&'static str, &'static str>>>,
}

impl Via {
    pub(crate) fn new(client: i32) -> Self {
        let mut steps = Vec::new();
        for line in VIA.lines() {
            let mut f = line.split(' ');
            match (f.next(), f.next(), f.next()) {
                (Some("step"), Some(protocol), _) => {
                    if protocol.parse::<i32>().is_ok_and(|p| p < client) {
                        break;
                    }
                    steps.push(HashMap::new());
                }
                (Some(section), Some(from), Some(to)) => {
                    if let Some(step) = steps.last_mut() {
                        step.entry(section)
                            .or_insert_with(HashMap::new)
                            .insert(from, to);
                    }
                }
                _ => {}
            }
        }
        Self { steps }
    }

    /// The client's name of a registry entry; `None` when a step drops it.
    pub(crate) fn name<'a>(&'a self, section: &str, name: &'a str) -> Option<&'a str> {
        let mut current = name;
        for step in &self.steps {
            match step.get(section).and_then(|m| m.get(current)) {
                Some(&"-") => return None,
                Some(to) => current = to,
                None => {}
            }
        }
        Some(current)
    }

    /// The client's state (`name[key=value,...]`) of a server state; per step
    /// as ViaVersion `MappingsLoader.mapEntry`: the whole state first, then the
    /// block name, whose stand-in ending in `[` keeps the properties.
    fn state(&self, state: String) -> Option<String> {
        let mut current = state;
        for step in &self.steps {
            let Some(map) = step.get("blockstates") else {
                continue;
            };
            let (name, props) = current.split_once('[').unwrap_or((&current, ""));
            let to = match (map.get(current.as_str()), map.get(name)) {
                (Some(to), _) => (*to).to_string(),
                (None, Some(to)) if !props.is_empty() && to.ends_with('[') => {
                    format!("{to}{props}")
                }
                (None, Some(to)) if !props.is_empty() => (*to).to_string(),
                _ => continue,
            };
            if to == "-" {
                return None;
            }
            current = to;
        }
        Some(current)
    }
}

/// Server ID → client ID of one registry.
#[derive(Debug, Clone, Default)]
pub(crate) struct IdMap {
    table: Vec<Option<u32>>,
}

/// Picks a stand-in name for an entry the target lacks, given a test for names it has.
pub(crate) type StandIn<'a> = &'a dyn Fn(&str, &dyn Fn(&str) -> bool) -> Option<String>;

impl IdMap {
    pub(crate) fn get(&self, id: i32) -> Option<i32> {
        let v = *self.table.get(usize::try_from(id).ok()?)?;
        v.map(|v| v as i32)
    }

    /// Like `get`, with `fallback` for entries without a counterpart.
    pub(crate) fn or(&self, id: i32, fallback: i32) -> i32 {
        self.get(id).unwrap_or(fallback)
    }

    /// By equal names; `stand_in` may name another entry for the missing ones.
    pub(crate) fn by_name(from: &[String], to: &[String], stand_in: StandIn<'_>) -> Self {
        let index: HashMap<&str, u32> = to
            .iter()
            .enumerate()
            .filter_map(|(i, n)| Some((n.as_str(), u32::try_from(i).ok()?)))
            .collect();
        let has = |n: &str| index.contains_key(n);
        let table =
            from.iter()
                .map(|name| {
                    index.get(name.as_str()).copied().or_else(|| {
                        stand_in(name, &has).and_then(|s| index.get(s.as_str()).copied())
                    })
                })
                .collect();
        Self { table }
    }
}

/// Server block state → client block state (air when Mappings has none).
pub(crate) fn block_states(via: &Via, server: &VersionData, client: &VersionData) -> Vec<u32> {
    let by_name: HashMap<&str, &Block> =
        client.blocks.iter().map(|b| (b.name.as_str(), b)).collect();
    let mut out = Vec::with_capacity(server.block_state_count() as usize);
    for b in &server.blocks {
        for offset in 0..b.state_count() {
            let mut props = b.values(offset);
            props.sort_unstable();
            let pairs: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let state = match pairs.is_empty() {
                true => b.name.clone(),
                false => format!("{}[{}]", b.name, pairs.join(",")),
            };
            let client_state = via.state(state).and_then(|s| {
                let (name, rest) = s.split_once('[').unwrap_or((&s, ""));
                let props: Vec<(&str, &str)> = rest
                    .trim_end_matches(']')
                    .split(',')
                    .filter_map(|kv| kv.split_once('='))
                    .collect();
                Some(state_with(by_name.get(name)?, &props))
            });
            out.push(client_state.unwrap_or(0));
        }
    }
    out
}

/// The state of `target` with the given property values where it allows
/// them, its defaults otherwise.
fn state_with(target: &Block, props: &[(&str, &str)]) -> u32 {
    let defaults = target.value_indices(target.default_offset);
    let mut state = 0u32;
    for ((name, allowed), default) in target.properties.iter().zip(defaults) {
        let wanted = props
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| allowed.iter().position(|a| a == v));
        let len = u32::try_from(allowed.len()).unwrap_or(1);
        state = state * len + u32::try_from(wanted.unwrap_or(default)).unwrap_or(0);
    }
    target.first_state + state
}

/// Block registry IDs: the client block of the default state's stand-in.
pub(crate) fn block_ids(states: &[u32], server: &VersionData, client: &VersionData) -> IdMap {
    let table = server
        .blocks
        .iter()
        .map(|b| {
            let s = *states.get((b.first_state + b.default_offset) as usize)?;
            let i = client.blocks.partition_point(|c| c.first_state <= s);
            u32::try_from(i.checked_sub(1)?).ok()
        })
        .collect();
    IdMap { table }
}

/// Server ID → client ID of a registry by name, with the stand-ins of a
/// Mappings section.
pub(crate) fn registry(via: &Via, section: &str, server: &[String], client: &[String]) -> IdMap {
    IdMap::by_name(server, client, &|n, has| {
        via.name(section, n).filter(|s| has(s)).map(str::to_string)
    })
}

/// Bits needed to index `count` entries (vanilla `Mth.ceillog2`).
pub(crate) const fn ceil_log2(count: u32) -> u8 {
    if count <= 1 {
        0
    } else {
        (32 - (count - 1).leading_zeros()) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(name: &str, first: u32, default: u32, props: &[(&str, &[&str])]) -> Block {
        Block {
            name: name.into(),
            first_state: first,
            default_offset: default,
            properties: props
                .iter()
                .map(|(n, v)| {
                    (
                        (*n).to_string(),
                        v.iter().map(|s| (*s).to_string()).collect(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn states_keep_allowed_properties() {
        // Without `waterlogged`, other value order.
        let target = block("oak_slab", 10, 1, &[("type", &["bottom", "double", "top"])]);
        let state = |props: &[(&str, &str)]| state_with(&target, props);
        assert_eq!(state(&[("type", "double"), ("waterlogged", "true")]), 11);
        assert_eq!(state(&[("type", "top")]), 12);
        // Unknown values take the default.
        assert_eq!(state(&[("type", "sideways")]), 11);
    }

    #[test]
    fn mappings_compose_down_to_the_client() {
        let via = Via::new(775);
        assert_eq!(via.steps.len(), 2);
        // 26.3 -> 26.2 -> 26.1: concrete stairs via a 26.2 block.
        assert_eq!(
            via.state(
                "red_concrete_stairs[facing=east,half=top,shape=straight,waterlogged=true]".into()
            )
            .as_deref(),
            Some("brick_stairs[facing=east,half=top,shape=straight,waterlogged=true]")
        );
        assert_eq!(
            via.state("poplar_planks".into()).as_deref(),
            Some("birch_planks")
        );
        assert_eq!(via.state("stone".into()).as_deref(), Some("stone"));
        assert_eq!(via.name("entities", "poplar_boat"), Some("birch_boat"));
        assert_eq!(via.name("particles", "geyser_base"), None);
        // Whole states with their own stand-ins (26.1 -> 1.21.11).
        let via = Via::new(774);
        assert_eq!(
            via.state("note_block[instrument=trumpet,note=0,powered=true]".into())
                .as_deref(),
            Some("note_block[instrument=didgeridoo,note=0,powered=true]")
        );
    }
}
