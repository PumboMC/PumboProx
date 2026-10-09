//! Runs Mojang's data generator and turns its reports into tables.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use pumbo_data::{
    Block, BlockProperty, LATER_REGISTRIES, PacketAlias, PacketEntry, Registry, Release,
    STATIC_REGISTRIES, Tables,
};
use pumbo_protocol::{Direction, Phase, ProtocolVersion};
use serde_json::Value;

use crate::Error;

/// Packets renamed in the reports: (phase, direction, old name, current name).
/// The diff report lists every name that disappears so new renames get noticed.
pub const ALIASES: &[(&str, &str, &str, &str)] = &[
    ("login", "clientbound", "game_profile", "login_finished"),
    ("play", "clientbound", "set_carried_item", "set_held_slot"),
];

const DONE_MARKER: &str = ".pumbo-datagen-ok";

/// What `version.json` inside the server jar says.
#[derive(Debug, Clone)]
pub struct JarVersion {
    pub id: String,
    pub protocol: i32,
    pub world_version: i32,
}

/// Reads `version.json` from the server jar (system `unzip`).
pub fn jar_version(jar: &Path) -> Result<JarVersion, Error> {
    let out = Command::new("unzip")
        .arg("-p")
        .arg(jar)
        .arg("version.json")
        .output()
        .map_err(|e| Error::Tool(format!("unzip: {e}")))?;
    if !out.status.success() {
        return Err(Error::Tool(format!(
            "unzip {}: {}",
            jar.display(),
            out.status
        )));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::Json(format!("{}/version.json: {e}", jar.display())))?;
    let int = |k: &str| {
        v.get(k)
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| Error::Json(format!("{}/version.json: no {k}", jar.display())))
    };
    Ok(JarVersion {
        id: v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        protocol: int("protocol_version")?,
        world_version: int("world_version")?,
    })
}

/// Runs the generator in `dir` (where `server.jar` is) unless its output is
/// already complete. Returns the reports directory.
pub fn generate(dir: &Path, java: &str) -> Result<PathBuf, Error> {
    let out = dir.join("out");
    let reports = out.join("reports");
    if out.join(DONE_MARKER).is_file() {
        return Ok(reports);
    }
    let log = std::fs::File::create(dir.join("generator.log"))
        .map_err(|e| Error::io(&dir.join("generator.log"), e))?;
    let log_err = log.try_clone().map_err(|e| Error::io(dir, e))?;
    let status = Command::new(java)
        .current_dir(dir)
        .args([
            "-DbundlerMainClass=net.minecraft.data.Main",
            "-jar",
            "server.jar",
            "--reports",
            "--output",
            "out",
        ])
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .status()
        .map_err(|e| Error::Tool(format!("{java}: {e}")))?;
    if !status.success() {
        return Err(Error::Tool(format!(
            "generator in {} failed ({status}), see generator.log",
            dir.display()
        )));
    }
    for f in ["packets.json", "registries.json", "blocks.json"] {
        if !reports.join(f).is_file() {
            return Err(Error::Tool(format!(
                "generator in {} wrote no {f}",
                dir.display()
            )));
        }
    }
    std::fs::write(out.join(DONE_MARKER), b"").map_err(|e| Error::io(&out, e))?;
    Ok(reports)
}

fn read_json(path: &Path) -> Result<Value, Error> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_str(&text).map_err(|e| Error::Json(format!("{}: {e}", path.display())))
}

fn object<'a>(v: &'a Value, what: &str) -> Result<&'a serde_json::Map<String, Value>, Error> {
    v.as_object()
        .ok_or_else(|| Error::Json(format!("{what} is not an object")))
}

fn as_i64(v: &Value, what: &str) -> Result<i64, Error> {
    v.as_i64()
        .ok_or_else(|| Error::Json(format!("{what} is not an integer")))
}

/// Packets from `packets.json`, with aliases applied.
pub fn packets(json: &Value) -> Result<(Vec<PacketEntry>, Vec<PacketAlias>), Error> {
    let mut list = Vec::new();
    let mut aliases = Vec::new();
    for (phase_name, dirs) in object(json, "packets.json")? {
        let phase = Phase::from_report_name(phase_name)
            .ok_or_else(|| Error::Json(format!("unknown phase {phase_name}")))?;
        for (dir_name, packets) in object(dirs, phase_name)? {
            let direction = Direction::from_report_name(dir_name)
                .ok_or_else(|| Error::Json(format!("unknown direction {dir_name}")))?;
            for (name, info) in object(packets, dir_name)? {
                let id = info
                    .get("protocol_id")
                    .ok_or_else(|| Error::Json(format!("{name} without protocol_id")))?;
                let id = i32::try_from(as_i64(id, name)?)
                    .map_err(|_| Error::Json(format!("{name}: id out of range")))?;
                let short = pumbo_data::short_name(name);
                let current = ALIASES
                    .iter()
                    .find(|(p, d, old, _)| *p == phase_name && *d == dir_name && *old == short)
                    .map(|(_, _, _, new)| *new);
                if let Some(new) = current {
                    aliases.push(PacketAlias {
                        phase,
                        direction,
                        report_name: short.to_string(),
                        name: new.to_string(),
                    });
                }
                list.push(PacketEntry {
                    phase,
                    direction,
                    id,
                    name: current.unwrap_or(short).to_string(),
                });
            }
        }
    }
    Ok((list, aliases))
}

/// Static registries we keep, from `registries.json`. Entry IDs must be
/// exactly 0..n.
pub fn registries(json: &Value) -> Result<Vec<Registry>, Error> {
    let root = object(json, "registries.json")?;
    let mut out = Vec::new();
    for name in STATIC_REGISTRIES.iter().chain(LATER_REGISTRIES) {
        let Some(reg) = root.get(*name) else {
            if LATER_REGISTRIES.contains(name) {
                continue;
            }
            return Err(Error::Json(format!("registry {name} missing")));
        };
        let entries = object(
            reg.get("entries")
                .ok_or_else(|| Error::Json(format!("{name} without entries")))?,
            name,
        )?;
        let mut by_id: BTreeMap<i64, String> = BTreeMap::new();
        for (entry, info) in entries {
            let id = as_i64(
                info.get("protocol_id")
                    .ok_or_else(|| Error::Json(format!("{entry} without protocol_id")))?,
                entry,
            )?;
            if by_id.insert(id, entry.clone()).is_some() {
                return Err(Error::Json(format!("{name}: duplicate id {id}")));
            }
        }
        let contiguous = by_id
            .keys()
            .enumerate()
            .all(|(i, id)| i64::try_from(i).ok() == Some(*id));
        if !contiguous {
            return Err(Error::Json(format!("{name}: ids are not 0..n")));
        }
        out.push(Registry::new(name, by_id.into_values().collect()));
    }
    Ok(out)
}

/// All blocks in compact form from `blocks.json`. Fails if the states are not
/// the cartesian product of the properties sorted by name (then the compact
/// form would be wrong).
pub fn blocks(json: &Value) -> Result<Vec<Block>, Error> {
    let mut out = Vec::new();
    for (name, info) in object(json, "blocks.json")? {
        let mut properties: Vec<BlockProperty> = match info.get("properties") {
            None => Vec::new(),
            Some(p) => object(p, name)?
                .iter()
                .map(|(k, vals)| {
                    let values = vals
                        .as_array()
                        .ok_or_else(|| Error::Json(format!("{name}.{k}: not a list")))?
                        .iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| Error::Json(format!("{name}.{k}: not strings")))?;
                    Ok(BlockProperty {
                        name: k.clone(),
                        values,
                    })
                })
                .collect::<Result<_, Error>>()?,
        };
        properties.sort_by(|a, b| a.name.cmp(&b.name));
        let states = info
            .get("states")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Json(format!("{name}: no states")))?;
        let first = states
            .first()
            .and_then(|s| s.get("id"))
            .and_then(Value::as_i64)
            .and_then(|i| u32::try_from(i).ok())
            .ok_or_else(|| Error::Json(format!("{name}: no first state")))?;
        let mut block = Block {
            name: name.clone(),
            first_state: first,
            default_offset: 0,
            properties,
        };
        let mut default = None;
        if usize::try_from(block.state_count()).ok() != Some(states.len()) {
            return Err(Error::Json(format!("{name}: state count mismatch")));
        }
        for (offset, state) in states.iter().enumerate() {
            let offset =
                u32::try_from(offset).map_err(|_| Error::Json(format!("{name}: too many")))?;
            let id = state.get("id").and_then(Value::as_i64);
            if id != Some(i64::from(first) + i64::from(offset)) {
                return Err(Error::Json(format!("{name}: ids not consecutive")));
            }
            let expected = block
                .state_values(offset)
                .ok_or_else(|| Error::Json(format!("{name}: bad offset")))?;
            for (p, value) in block.properties.iter().zip(expected) {
                let got = state
                    .get("properties")
                    .and_then(|ps| ps.get(&p.name))
                    .and_then(Value::as_str);
                if got != Some(value) {
                    return Err(Error::Json(format!(
                        "{name}: state {offset} is not in sorted-property order"
                    )));
                }
            }
            if state.get("default").and_then(Value::as_bool) == Some(true) {
                default = Some(offset);
            }
        }
        block.default_offset = default.ok_or_else(|| Error::Json(format!("{name}: no default")))?;
        out.push(block);
    }
    Ok(out)
}

/// Tables of one release.
pub fn tables(reports: &Path, version: &JarVersion, server_sha1: &str) -> Result<Tables, Error> {
    let (packet_list, aliases) = packets(&read_json(&reports.join("packets.json"))?)?;
    let registry_list = registries(&read_json(&reports.join("registries.json"))?)?;
    let block_list = blocks(&read_json(&reports.join("blocks.json"))?)?;
    Ok(Tables::new(
        ProtocolVersion(version.protocol),
        vec![Release {
            name: version.id.clone(),
            world_version: version.world_version,
            server_sha1: server_sha1.to_string(),
        }],
        packet_list,
        aliases,
        registry_list,
        block_list,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_with_alias() {
        let json: Value = serde_json::from_str(
            r#"{"login":{"clientbound":{"minecraft:game_profile":{"protocol_id":2}}},
                "play":{"serverbound":{"minecraft:chat":{"protocol_id":7}}}}"#,
        )
        .unwrap();
        let (list, aliases) = packets(&json).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|p| p.name == "login_finished" && p.id == 2));
        assert_eq!(aliases.len(), 1);
    }

    #[test]
    fn blocks_compact_and_checked() {
        let json: Value = serde_json::from_str(
            r#"{"minecraft:chest":{"properties":{"waterlogged":["true","false"],"facing":["north","south"]},
                "states":[{"id":10,"properties":{"facing":"north","waterlogged":"true"}},
                          {"id":11,"default":true,"properties":{"facing":"north","waterlogged":"false"}},
                          {"id":12,"properties":{"facing":"south","waterlogged":"true"}},
                          {"id":13,"properties":{"facing":"south","waterlogged":"false"}}]}}"#,
        )
        .unwrap();
        let b = blocks(&json).unwrap();
        assert_eq!(b.first().unwrap().default_offset, 1);
        assert_eq!(
            b.first().unwrap().properties.first().unwrap().name,
            "facing"
        );

        let bad: Value = serde_json::from_str(
            r#"{"minecraft:chest":{"properties":{"facing":["north","south"],"waterlogged":["true","false"]},
                "states":[{"id":10,"properties":{"facing":"north","waterlogged":"true"}},
                          {"id":11,"default":true,"properties":{"facing":"south","waterlogged":"true"}},
                          {"id":12,"properties":{"facing":"north","waterlogged":"false"}},
                          {"id":13,"properties":{"facing":"south","waterlogged":"false"}}]}}"#,
        )
        .unwrap();
        assert!(blocks(&bad).is_err());
    }
}
