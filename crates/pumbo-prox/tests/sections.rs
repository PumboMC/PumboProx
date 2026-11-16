//! The sections of pumboprox.yml the proxy and the plugin host register:
//! each once, and closed unless named below.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use pumbo_prox::config::Config;
use pumbo_prox::modules;
use serde_json::{Value, json};

/// Sections with keys of the network's own (plugin ids, style tags, host
/// names), with a module's keys next to the typed ones, and the version.
const OPEN: &[&str] = &[
    "plugins",
    "style",
    "forced-hosts",
    "config-version",
    "authentication",
    "forwarding",
    "routing",
    "storage",
    "listener",
];

#[test]
fn no_section_is_registered_twice() {
    let mut keys: Vec<&str> = modules::sections().iter().map(|s| s.key).collect();
    keys.sort_unstable();
    let all = keys.len();
    keys.dedup();
    assert_eq!(keys.len(), all, "{keys:?}");
}

#[test]
fn the_proxy_registers_only_fields_of_its_config() {
    for s in pumbo_prox::config::sections() {
        // A field of Config takes the value or refuses it; any other key
        // lands in `sections`.
        if let Ok(cfg) = serde_json::from_value::<Config>(json!({ s.key: "x" })) {
            assert!(
                !cfg.sections.contains_key(s.key),
                "{} is no field of Config",
                s.key
            );
        }
    }
}

/// A mapping that takes no other keys, or a map or list of such entries.
fn closed(schema: &Value) -> bool {
    match schema.get("additionalProperties") {
        Some(Value::Bool(false)) => true,
        Some(entry) if entry.is_object() => closed(entry),
        _ => schema.get("items").is_some_and(closed),
    }
}

#[test]
fn every_section_is_closed_but_the_open_ones() {
    for s in modules::sections() {
        assert_eq!(
            closed(&s.schema),
            !OPEN.contains(&s.key),
            "{}: {}",
            s.key,
            s.schema
        );
    }
}
