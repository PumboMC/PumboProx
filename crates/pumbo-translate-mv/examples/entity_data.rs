//! Generates `data/entity_data.txt`: for every older client protocol, the
//! client's serializer ID per server serializer and the client's field index
//! per server field of every entity type. Ported from the `build.rs` of
//! pumpkin-java-multiversion (MIT OR Apache-2.0).
//!
//! `cargo run -p pumbo-translate-mv --example entity_data -- <old> <current> > data/entity_data.txt`
//!
//! `<old>`: a directory with `tracked_data/<folder>_tracked_data.json` and
//! `meta_data_type/<folder>_meta_data_type.json` per older version (the
//! `assets/` directory of pumpkin-java-multiversion); `<current>`: a directory
//! with the server version's `tracked_data.json` and `meta_data_type.json`
//! (`assets/` of Pumpkin). The files come from Pumpkin's data extractor.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

/// Older versions, newest first: (folder, protocol, Mojang field names).
const VERSIONS: &[(&str, i32, bool)] = &[
    ("26_2", 776, true),
    ("26_1", 775, true),
    ("1_21_11", 774, false),
    ("1_21_9", 773, false),
    ("1_21_7", 772, false),
    ("1_21_6", 771, false),
    ("1_21_5", 770, false),
    ("1_21_4", 769, false),
    ("1_21_2", 768, false),
    ("1_21", 767, false),
];

/// Older serializer names with the same wire format, as the server names them.
const SERIALIZER_RENAMES: &[(&str, &str)] = &[
    ("integer", "int"),
    ("text_component", "component"),
    ("optional_text_component", "optional_component"),
    ("rotation", "rotations"),
    ("facing", "direction"),
    ("lazy_entity_reference", "optional_living_entity_reference"),
    ("optional_uuid", "optional_living_entity_reference"),
    ("particle_list", "particles"),
    ("optional_int", "optional_unsigned_int"),
    ("entity_pose", "pose"),
    ("oxidation_level", "weathering_copper_state"),
    ("vector_3f", "vector3"),
    ("vector3f", "vector3"),
    ("quaternion_f", "quaternion"),
    ("quaternionf", "quaternion"),
    ("profile", "resolvable_profile"),
    ("arm", "humanoid_arm"),
];

/// Fields 26.1 has and 1.21.11 lacks, besides those with serializers it lacks.
const FIELDS_ADDED_IN_26_1: &[&str] = &["AGE_LOCKED", "DATA_VILLAGER_DATA_FINALIZED"];

/// Base `Entity` fields, the same in every version.
const BASE_FIELDS: usize = 8;

struct Field {
    name: String,
    id: u8,
    serializer: String,
}

fn serializer_name(name: &str) -> String {
    SERIALIZER_RENAMES
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| new)
        .to_string()
}

fn load_tracked(path: &Path) -> HashMap<String, Vec<Field>> {
    let json: HashMap<String, HashMap<String, Value>> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    json.into_iter()
        .map(|(entity, fields)| {
            let mut fields: Vec<_> = fields
                .into_iter()
                .map(|(name, f)| Field {
                    name,
                    id: f["id"].as_u64().unwrap() as u8,
                    serializer: serializer_name(f["type"].as_str().unwrap()),
                })
                .collect();
            fields.sort_by_key(|f| f.id);
            (entity, fields)
        })
        .collect()
}

fn load_serializers(path: &Path) -> HashMap<String, i64> {
    let json: HashMap<String, i64> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    json.into_iter()
        .map(|(n, id)| (serializer_name(&n), id))
        .collect()
}

/// Field IDs of `older` by field ID of `newer`, for one entity.
fn step_fields(
    newer: &[Field],
    older: &[Field],
    same_names: bool,
    older_serializers: &HashMap<String, i64>,
    entity: &str,
) -> HashMap<u8, u8> {
    if same_names {
        return newer
            .iter()
            .filter_map(|n| {
                let o = older
                    .iter()
                    .find(|o| o.name == n.name && o.serializer == n.serializer)?;
                Some((n.id, o.id))
            })
            .collect();
    }
    // Mojang to Yarn names (26.1 to 1.21.11): only added fields differ.
    let kept: Vec<_> = newer
        .iter()
        .filter(|n| {
            !FIELDS_ADDED_IN_26_1.contains(&n.name.as_str())
                && older_serializers.contains_key(&n.serializer)
        })
        .collect();
    assert!(
        kept.len() == older.len()
            && kept
                .iter()
                .zip(older)
                .all(|(n, o)| n.serializer == o.serializer),
        "entity data of {entity} does not line up between 26.1 and 1.21.11"
    );
    kept.iter().zip(older).map(|(n, o)| (n.id, o.id)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (old, current) = (Path::new(&args[0]), Path::new(&args[1]));
    let mut newer = load_tracked(&current.join("tracked_data.json"));
    let current_serializers = load_serializers(&current.join("meta_data_type.json"));
    let mut names: Vec<(&String, &i64)> = current_serializers.iter().collect();
    names.sort_by_key(|(_, id)| **id);

    let mut entities: Vec<String> = newer.keys().cloned().collect();
    entities.sort();
    // Server field ID → field ID in the version processed last.
    let mut state: HashMap<String, Vec<Option<u8>>> = entities
        .iter()
        .map(|e| {
            let len = newer[e].last().map_or(0, |f| usize::from(f.id) + 1);
            (e.clone(), (0..len).map(|id| Some(id as u8)).collect())
        })
        .collect();

    let mut out = String::from(
        "# Generated by examples/entity_data.rs from Pumpkin's extracted tracked data (see docs).\n",
    );
    let _ = writeln!(
        out,
        "server {}",
        names
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut newer_mojang = true;
    for &(folder, protocol, mojang) in VERSIONS {
        let older = load_tracked(&old.join(format!("tracked_data/{folder}_tracked_data.json")));
        let older_serializers =
            load_serializers(&old.join(format!("meta_data_type/{folder}_meta_data_type.json")));
        for entity in &entities {
            let ids = state.get_mut(entity).unwrap();
            match (newer.get(entity), older.get(entity)) {
                (Some(n), Some(o)) => {
                    let step =
                        step_fields(n, o, newer_mojang == mojang, &older_serializers, entity);
                    for id in ids.iter_mut() {
                        *id = id.and_then(|id| step.get(&id).copied());
                    }
                }
                // The client spawns another entity type: only the base fields carry over.
                _ => {
                    for (i, id) in ids.iter_mut().enumerate() {
                        *id = id.filter(|_| i < BASE_FIELDS);
                    }
                }
            }
        }
        let _ = writeln!(out, "protocol {protocol}");
        let serializers: Vec<String> = names
            .iter()
            .map(|(n, _)| older_serializers.get(*n).map_or("-".into(), i64::to_string))
            .collect();
        let _ = writeln!(out, "serializers {}", serializers.join(" "));
        for entity in &entities {
            let ids: Vec<String> = state[entity]
                .iter()
                .map(|id| id.map_or("-".into(), |v| v.to_string()))
                .collect();
            let _ = writeln!(out, "entity {entity} {}", ids.join(" "));
        }
        newer = older;
        newer_mojang = mojang;
    }
    print!("{out}");
}
