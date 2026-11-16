//! The sections of pumboprox.yml the proxy and the plugin host register:
//! each once, closed unless named below, and no key that changes who may do
//! what marked as delegable.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use pumbo_core::section::{DELEGABLE, SectionInfo, marked_paths};
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

/// Config keys that change who may do what, which code runs and what the
/// proxy trusts, or where it listens and connects: none may be delegable.
const TRUST_KEYS: &[&str] = &[
    // operators
    "commands.operators",
    // grants
    "plugins.*.grants",
    // the permission provider
    "permissions.provider",
    "permissions.load-at",
    "permissions.file",
    "permissions.on-load-failure",
    "services.providers",
    // gates and identity
    "plugins.required-gates",
    "plugins.required-plugins",
    "login.online-mode",
    "authentication",
    "plugins.allow-premium-profile-changes",
    "commands.sensitive",
    // keys and verification
    "managed-servers.allow-unverified",
    "managed-servers.download-from-game",
    // code
    "plugins.dir",
    "managed-servers.dir",
    "managed-servers.versions-dir",
    // addresses and secrets; the bridge with its switch
    "listener",
    "bridge",
    "servers",
    "routing",
    "forced-hosts",
    "forwarding",
    "logging.metrics-bind",
    "logging.metrics-allow-remote",
    "logging.metrics-token-file",
    "plugins.http-allow-private",
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

/// Follows `key` through the schemas: a part must be a property and `*` an
/// entry of a map or an item of a list; a mapping open to other keys ends
/// the walk. The schema of `plugins` takes any key (plugin ids), so a key
/// under it is checked against what the host reads instead.
fn resolve(sections: &[SectionInfo], key: &str) -> Result<(), String> {
    if let Some(rest) = key.strip_prefix("plugins.") {
        return plugins_key(rest);
    }
    let mut parts = key.split('.');
    let first = parts.next().unwrap_or_default();
    let mut schema = &sections
        .iter()
        .find(|s| s.key == first)
        .ok_or("no such section")?
        .schema;
    for part in parts {
        let entry = ["additionalProperties", "items"]
            .iter()
            .find_map(|k| schema.get(*k).filter(|s| s.is_object()));
        let open = schema.get("items").is_none()
            && matches!(
                schema.get("additionalProperties"),
                None | Some(Value::Bool(true))
            );
        let property = schema.get("properties").and_then(|p| p.get(part));
        schema = match (part, entry, property) {
            ("*", Some(entry), _) => entry,
            (_, _, Some(property)) if part != "*" => property,
            _ if open => return Ok(()),
            _ => return Err(format!("no {part} here")),
        };
    }
    Ok(())
}

/// A key under `plugins`: a setting of the plugin host, or after `*` (a
/// plugin id) a setting of one plugin.
fn plugins_key(key: &str) -> Result<(), String> {
    match key.split_once('.') {
        Some(("*", setting)) => pumbo_core::section::fields::<pumbo_host::config::PluginScope>()
            .contains(&setting)
            .then_some(())
            .ok_or_else(|| format!("a plugin has no {setting}")),
        Some(_) => Err(format!("no {key} under plugins")),
        // A setting takes `{}` or refuses it; any other key is a plugin id.
        None => match serde_json::from_value::<pumbo_host::HostConfig>(
            json!({ "plugins": { key: {} } }),
        ) {
            Ok(cfg) if cfg.plugins.scopes.contains_key(key) => Err(format!("plugins has no {key}")),
            _ => Ok(()),
        },
    }
}

/// A marked path meets a key of `TRUST_KEYS`: one is a prefix of the other,
/// part by part. A `*` in the marked path (every entry or item) matches any
/// part; a `*` in the key matches only a `*`, never a named setting.
fn overlaps(marked: &str, key: &str) -> bool {
    marked
        .split('.')
        .zip(key.split('.'))
        .all(|(m, k)| m == k || m == "*")
}

/// What lets a key that widens trust be delegated, or what of `TRUST_KEYS`
/// the schemas do not have; empty when all is well.
fn trust_problems(sections: &[SectionInfo]) -> Vec<String> {
    let mut problems: Vec<String> = TRUST_KEYS
        .iter()
        .filter_map(|key| resolve(sections, key).err().map(|e| format!("{key}: {e}")))
        .collect();
    for s in sections {
        let paths = match marked_paths(&s.schema) {
            Ok(paths) => paths,
            Err(e) => {
                problems.push(format!("{}: {e}", s.key));
                continue;
            }
        };
        for p in paths {
            let path = if p.is_empty() {
                s.key.to_string()
            } else {
                format!("{}.{p}", s.key)
            };
            for key in TRUST_KEYS.iter().filter(|k| overlaps(&path, k)) {
                problems.push(format!("{path} is delegable, but {key} widens trust"));
            }
        }
    }
    problems
}

#[test]
fn no_key_that_widens_trust_is_delegable() {
    let problems = trust_problems(&modules::sections());
    assert!(problems.is_empty(), "{problems:#?}");
}

/// The registered sections with a mark at `pointer` in the schema of `key`.
fn marked(key: &str, pointer: &str) -> Vec<SectionInfo> {
    let mut sections = modules::sections();
    let s = sections.iter_mut().find(|s| s.key == key).unwrap();
    let at = s
        .schema
        .pointer_mut(pointer)
        .unwrap()
        .as_object_mut()
        .unwrap();
    at.insert(DELEGABLE.into(), true.into());
    sections
}

#[test]
fn the_trust_check_finds_every_mark() {
    let finds = |key: &str, pointer: &str, want: &str| {
        let problems = trust_problems(&marked(key, pointer));
        assert!(
            problems.iter().any(|p| p == want),
            "{key}{pointer}: {problems:#?}"
        );
    };
    // A section, the parent of a listed key, a map's entries, a list's items.
    finds("bridge", "", "bridge is delegable, but bridge widens trust");
    finds(
        "logging",
        "",
        "logging is delegable, but logging.metrics-bind widens trust",
    );
    finds(
        "servers",
        "/additionalProperties",
        "servers.* is delegable, but servers widens trust",
    );
    finds(
        "listener",
        "/items",
        "listener.* is delegable, but listener widens trust",
    );
    // An ordinary key may carry the mark.
    assert_eq!(
        trust_problems(&marked("messages", "/properties/no-server")),
        Vec::<String>::new()
    );
    // A mark where the walk does not look is a problem of its own.
    let mut hidden = modules::sections();
    let tab = hidden.iter_mut().find(|s| s.key == "tab").unwrap();
    tab.schema["patternProperties"] = json!({ "^x": { DELEGABLE: true } });
    let messages = hidden.iter_mut().find(|s| s.key == "messages").unwrap();
    messages.schema["$defs"] = json!({ "Text": { DELEGABLE: true } });
    messages.schema["properties"]["no-server"] = json!({ "$ref": "#/$defs/Text" });
    let problems = trust_problems(&hidden);
    assert_eq!(problems.len(), 2, "{problems:#?}");
    for section in ["tab: ", "messages: "] {
        assert!(
            problems.iter().any(|p| p.starts_with(section)),
            "{problems:#?}"
        );
    }
    // Entries of `plugins` (plugin ids) reach `plugins.*.grants`, and the
    // `*` of that key stands for them only, not for a named setting.
    let mut plugins = modules::sections();
    let s = plugins.iter_mut().find(|s| s.key == "plugins").unwrap();
    s.schema["additionalProperties"] = json!({ DELEGABLE: true });
    let problems = trust_problems(&plugins);
    assert!(
        problems.contains(&"plugins.* is delegable, but plugins.*.grants widens trust".into()),
        "{problems:#?}"
    );
    let mut named = modules::sections();
    let s = named.iter_mut().find(|s| s.key == "plugins").unwrap();
    s.schema["properties"] = json!({ "messages": { DELEGABLE: true } });
    assert_eq!(trust_problems(&named), Vec::<String>::new());
    // A listed key must exist: through the schemas, and under `plugins`
    // among the settings the host reads.
    let sections = modules::sections();
    assert!(resolve(&sections, "permissions.on-load-failur").is_err());
    assert!(resolve(&sections, "servers.lobby").is_err());
    assert!(resolve(&sections, "servers.*.address").is_ok());
    assert!(resolve(&sections, "plugins.required-gates").is_ok());
    assert!(resolve(&sections, "plugins.required-gate").is_err());
    assert!(resolve(&sections, "plugins.*.grants").is_ok());
    assert!(resolve(&sections, "plugins.*.grant").is_err());
    assert!(resolve(&sections, "plugins.dir.x").is_err());
}
