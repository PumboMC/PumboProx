//! Data the server sends in configuration, recorded from vanilla servers
//! (`pumbo-datagen record`): synchronized registries with entry names in ID
//! order, tags with entry IDs and enabled feature flags. Only names and IDs;
//! the full registry NBT stays outside git (decision §9.2).
//!
//! Format of `synced.txt`: `registry <name> <count>` and `count` entry names;
//! `tags <registry> <count>` and `count` lines `<tag> <id>,<id>,...`;
//! `features <count>` and `count` names. Default namespace left out.

use std::fmt::Write as _;

use crate::{DataError, full_name, short_name};

/// One synchronized registry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncedRegistry {
    pub name: String,
    pub entries: Vec<String>,
}

/// Tags of one registry: (tag name, entry IDs).
pub type TagList = Vec<(String, Vec<i32>)>;

/// Everything recorded from the configuration phase.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Synced {
    pub registries: Vec<SyncedRegistry>,
    /// Registry → (tag, entry IDs).
    pub tags: Vec<(String, TagList)>,
    pub features: Vec<String>,
}

impl Synced {
    pub fn registry(&self, name: &str) -> Option<&SyncedRegistry> {
        let name = full_name(name);
        self.registries.iter().find(|r| r.name == name)
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for r in &self.registries {
            let _ = writeln!(out, "registry {} {}", short_name(&r.name), r.entries.len());
            for e in &r.entries {
                let _ = writeln!(out, "{}", short_name(e));
            }
        }
        for (registry, tags) in &self.tags {
            let _ = writeln!(out, "tags {} {}", short_name(registry), tags.len());
            for (tag, ids) in tags {
                let ids: Vec<String> = ids.iter().map(i32::to_string).collect();
                let _ = writeln!(out, "{} {}", short_name(tag), ids.join(","));
            }
        }
        let _ = writeln!(out, "features {}", self.features.len());
        for f in &self.features {
            let _ = writeln!(out, "{}", short_name(f));
        }
        out
    }

    pub fn parse(protocol: i32, text: &str) -> Result<Self, DataError> {
        let err = |line: usize, message: &str| DataError::Parse {
            protocol,
            file: "synced.txt",
            line,
            message: message.into(),
        };
        let mut out = Self::default();
        let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l));
        while let Some((n, line)) = lines.next() {
            let f: Vec<&str> = line.split(' ').collect();
            let count = |s: &str| s.parse::<usize>().map_err(|_| err(n, "bad count"));
            match f.as_slice() {
                ["registry", name, c] => {
                    let mut entries = Vec::new();
                    for _ in 0..count(c)? {
                        let (_, e) = lines.next().ok_or_else(|| err(n, "registry ends early"))?;
                        entries.push(full_name(e));
                    }
                    out.registries.push(SyncedRegistry {
                        name: full_name(name),
                        entries,
                    });
                }
                ["tags", registry, c] => {
                    let mut tags = Vec::new();
                    for _ in 0..count(c)? {
                        let (m, l) = lines.next().ok_or_else(|| err(n, "tags end early"))?;
                        let (tag, ids) = l.split_once(' ').ok_or_else(|| err(m, "bad tag line"))?;
                        let ids = if ids.is_empty() {
                            Vec::new()
                        } else {
                            ids.split(',')
                                .map(|i| i.parse().map_err(|_| err(m, "bad tag id")))
                                .collect::<Result<_, _>>()?
                        };
                        tags.push((full_name(tag), ids));
                    }
                    out.tags.push((full_name(registry), tags));
                }
                ["features", c] => {
                    for _ in 0..count(c)? {
                        let (_, l) = lines.next().ok_or_else(|| err(n, "features end early"))?;
                        out.features.push(full_name(l));
                    }
                }
                _ => return Err(err(n, "unknown line")),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip() {
        let s = Synced {
            registries: vec![SyncedRegistry {
                name: "minecraft:dimension_type".into(),
                entries: vec!["minecraft:overworld".into(), "mod:x".into()],
            }],
            tags: vec![(
                "minecraft:block".into(),
                vec![
                    ("minecraft:climbable".into(), vec![1, 2]),
                    ("minecraft:empty".into(), vec![]),
                ],
            )],
            features: vec!["minecraft:vanilla".into()],
        };
        assert_eq!(Synced::parse(767, &s.to_text()).unwrap(), s);
        assert!(Synced::parse(767, "nonsense").is_err());
    }
}
