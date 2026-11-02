//! Keys of `pumboprox.yml` that no part of the proxy reads, and the line of
//! a key in the text. Serde skips an unknown key without a word, so a typo
//! (`limits.max-player`) would keep the default; these checks name it.
//!
//! The known keys come from the structs that read the file (the proxy's
//! [`Config`], the host's [`pumbo_host::HostConfig`]), so a new field counts
//! without an edit here. Maps whose keys the owner names (`servers.<name>`,
//! `forced-hosts`, `plugins.<id>`, `style`, ...) are never checked, and a
//! module without a key list here is never flagged.

use serde::de::{self, DeserializeOwned, Deserializer, Visitor};
use serde_json::Value;

use crate::config::Config;

/// One step of a key path: a mapping key or a list index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(usize),
}

impl From<&str> for Seg {
    fn from(k: &str) -> Self {
        Seg::Key(k.to_string())
    }
}

/// `limits.max-players`, `listener[0].bind`.
pub fn show(path: &[Seg]) -> String {
    let mut out = String::new();
    for s in path {
        match s {
            Seg::Key(k) if out.is_empty() => out.push_str(k),
            Seg::Key(k) => {
                out.push('.');
                out.push_str(k);
            }
            Seg::Index(i) => out.push_str(&format!("[{i}]")),
        }
    }
    out
}

/// A key no reader of the file knows, with the keys known in its place.
#[derive(Debug)]
pub struct Unknown {
    pub path: Vec<Seg>,
    pub known: Vec<String>,
}

impl Unknown {
    /// `unknown key limits.max-player (did you mean max-players?); it is ignored`.
    pub fn message(&self) -> String {
        let name = match self.path.last() {
            Some(Seg::Key(k)) => k.as_str(),
            _ => "",
        };
        let hint = closest(name, &self.known)
            .map(|k| format!(" (did you mean {k}?)"))
            .unwrap_or_default();
        format!("unknown key {}{hint}; it is ignored", show(&self.path))
    }
}

/// The field names a struct's `Deserialize` asks for, renames applied. A
/// struct with a flattened field asks for a map and gives none.
fn fields<T: DeserializeOwned>() -> &'static [&'static str] {
    struct Probe<'a>(&'a mut &'static [&'static str]);
    impl<'de> Deserializer<'de> for Probe<'_> {
        type Error = de::value::Error;
        fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
            Err(de::Error::custom("not a struct"))
        }
        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Self::Error> {
            *self.0 = fields;
            Err(de::Error::custom("fields read"))
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple
            tuple_struct map enum identifier ignored_any
        }
    }
    let mut out: &'static [&'static str] = &[];
    let _ = T::deserialize(Probe(&mut out));
    out
}

/// Sections read by a plain struct (no flattened field), by their key.
fn plain_sections() -> Vec<(&'static str, &'static [&'static str])> {
    use crate::config::*;
    use pumbo_host::config as host;
    vec![
        ("status", fields::<StatusConfig>()),
        ("login", fields::<LoginConfig>()),
        ("translation", fields::<TranslationConfig>()),
        ("limits", fields::<LimitsConfig>()),
        ("logging", fields::<LoggingConfig>()),
        ("switching", fields::<SwitchingConfig>()),
        ("commands", fields::<CommandsConfig>()),
        ("plugin-messages", fields::<PluginMessagesConfig>()),
        ("bungeecord-channel", fields::<BungeeCordConfig>()),
        ("messages", fields::<MessagesConfig>()),
        ("virtual", fields::<VirtualConfig>()),
        ("tab", fields::<TabConfig>()),
        ("pumbox", fields::<PumboxConfig>()),
        ("managed-servers", fields::<pumbo_servers::Config>()),
        ("services", fields::<host::ServicesConfig>()),
        ("permissions", fields::<host::PermissionsConfig>()),
        ("placeholders", fields::<host::PlaceholdersConfig>()),
    ]
}

/// The typed keys of the sections that pass the rest to a module; only for
/// the hint, the check itself uses what the struct left over.
const LISTENER: &[&str] = &[
    "bind",
    "transport",
    "proxy-protocol",
    "trusted-proxies",
    "allow-any-proxy",
];

/// Keys a built-in module reads from its section; `None` for a module
/// without a list here, whose keys are never flagged.
fn module_keys(section: &str, module: &str) -> Option<&'static [&'static str]> {
    Some(match (section, module) {
        ("listener", "java-tcp") => &[],
        ("forwarding", "modern") => &["secret-file", "secret-env"],
        ("forwarding", "none") => &[],
        ("routing", "try") => &["try", "forced-hosts"],
        ("authentication", "mojang") => &["timeout-ms", "session-server", "profile-lookup"],
        ("storage", "memory") => &[],
        _ => return None,
    })
}

/// Keys of `value` (the whole file) that neither the proxy nor the plugin
/// host reads; `cfg` is the same file, typed.
pub fn unknown(value: &Value, cfg: &Config) -> Vec<Unknown> {
    let mut out = Vec::new();
    let Some(top) = value.as_object() else {
        return out;
    };
    let plain = plain_sections();
    let host = fields::<pumbo_host::HostConfig>();
    let chain = &cfg.translation.chain;
    // Top level: what `Config` left over, minus the host's and the
    // translation providers' sections.
    let mut known_top: Vec<String> = top
        .keys()
        .filter(|k| !cfg.sections.contains_key(*k))
        .cloned()
        .chain(plain.iter().map(|(k, _)| k.to_string()))
        .chain(host.iter().map(|k| k.to_string()))
        .chain(chain.iter().cloned())
        .collect();
    known_top.sort();
    known_top.dedup();
    for k in cfg.sections.keys() {
        if !host.contains(&k.as_str()) && !chain.contains(k) {
            out.push(Unknown {
                path: vec![k.as_str().into()],
                known: known_top.clone(),
            });
        }
    }
    let mut check = |path: Vec<Seg>, map: Option<&Value>, known: &[&str]| {
        for k in map
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|m| m.keys())
        {
            if !known.contains(&k.as_str()) {
                let mut p = path.clone();
                p.push(k.as_str().into());
                out.push(Unknown {
                    path: p,
                    known: known.iter().map(|s| s.to_string()).collect(),
                });
            }
        }
    };
    for (name, known) in &plain {
        check(vec![(*name).into()], top.get(*name), known);
    }
    if let Some(Value::Object(servers)) = top.get("servers") {
        let known = fields::<crate::config::ServerConfig>();
        for (name, s) in servers {
            check(vec!["servers".into(), name.as_str().into()], Some(s), known);
        }
    }
    if let Some(Value::Array(groups)) = top.get("server-group") {
        let known = fields::<pumbo_host::config::ServerGroup>();
        for (i, g) in groups.iter().enumerate() {
            check(vec!["server-group".into(), Seg::Index(i)], Some(g), known);
        }
    }
    // Sections that pass their rest to a module: what the struct left over
    // against the module's list.
    let mut rest = |path: Vec<Seg>, typed: &[&str], left: &serde_json::Map<String, Value>, keys| {
        let Some(keys) = keys else { return };
        let known: Vec<&str> = typed.iter().chain(keys).copied().collect();
        let left = Value::Object(left.clone());
        check(path, Some(&left), &known);
    };
    for (i, l) in cfg.listener.iter().enumerate() {
        let path = vec!["listener".into(), Seg::Index(i)];
        rest(
            path,
            LISTENER,
            &l.rest,
            module_keys("listener", &l.transport),
        );
    }
    let f = &cfg.forwarding;
    rest(
        vec!["forwarding".into()],
        &["mode"],
        &f.rest,
        module_keys("forwarding", &f.mode),
    );
    let r = &cfg.routing;
    rest(
        vec!["routing".into()],
        &["selector"],
        &r.rest,
        module_keys("routing", &r.selector),
    );
    let a = &cfg.authentication;
    let keys = module_keys("authentication", &a.service);
    rest(vec!["authentication".into()], &["service"], &a.rest, keys);
    let s = &cfg.storage;
    rest(
        vec!["storage".into()],
        &["backend"],
        &s.rest,
        module_keys("storage", &s.backend),
    );
    out
}

/// The closest of `known` to `key` when it is a likely typo.
fn closest<'a>(key: &str, known: &'a [String]) -> Option<&'a str> {
    known
        .iter()
        .map(|k| (distance(key, k), k))
        .filter(|(d, k)| *d <= 2.max(key.len() / 4) && *d < key.len().max(k.len()))
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k.as_str())
}

/// Levenshtein distance by characters.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let above = row.get(j + 1).copied().unwrap_or(0);
            let left = row.get(j).copied().unwrap_or(0);
            let next = (diagonal + usize::from(ca != *cb))
                .min(above + 1)
                .min(left + 1);
            diagonal = above;
            if let Some(cell) = row.get_mut(j + 1) {
                *cell = next;
            }
        }
    }
    row.last().copied().unwrap_or(0)
}

/// The 1-based line of `path` in the YAML text, or of its deepest parent
/// written in block style (a key inside `{ ... }` gives the line of the
/// mapping). `None` when not even the first key is there.
pub fn line_of(text: &str, path: &[Seg]) -> Option<usize> {
    use crate::route::{indent_of, split_key};
    // A list item `- key: v` reads as `  key: v`, one level deeper.
    let lines: Vec<(String, bool)> = text
        .lines()
        .map(|l| {
            let t = l.trim_start();
            match t.strip_prefix("- ").or((t == "-").then_some("")) {
                Some(rest) => {
                    let lead = l.get(..l.len() - t.len()).unwrap_or_default();
                    (format!("{lead}  {rest}"), true)
                }
                None => (l.to_string(), false),
            }
        })
        .collect();
    let content = |i: usize| {
        lines
            .get(i)
            .filter(|(l, _)| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
    };
    // The search region and the line found so far.
    let (mut from, mut to, mut found) = (0, lines.len(), None);
    for seg in path {
        let Some(indent) = (from..to).find_map(|i| content(i).map(|(l, _)| indent_of(l))) else {
            break;
        };
        let hit = match seg {
            Seg::Key(k) => (from..to).find(|&i| {
                content(i).is_some_and(|(l, _)| {
                    indent_of(l) == indent && split_key(l).is_some_and(|(key, _, _)| &key == k)
                })
            }),
            Seg::Index(n) => (from..to)
                .filter(|&i| content(i).is_some_and(|(l, dash)| *dash && indent_of(l) == indent))
                .nth(*n),
        };
        let Some(at) = hit else { break };
        found = Some(at + 1);
        // The block under it: until a line at a smaller indent, or at the
        // same one (for an item: the next item).
        let end = (at + 1..to)
            .find(|&i| {
                content(i).is_some_and(|(l, dash)| match seg {
                    Seg::Key(_) => indent_of(l) <= indent,
                    Seg::Index(_) => indent_of(l) < indent || (*dash && indent_of(l) == indent),
                })
            })
            .unwrap_or(to);
        match seg {
            // An item's first key is on its dash line.
            Seg::Index(_) => from = at,
            Seg::Key(_) => from = at + 1,
        }
        to = end;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &[&str]) -> Vec<Seg> {
        p.iter()
            .map(|s| match s.parse() {
                Ok(i) => Seg::Index(i),
                Err(_) => Seg::Key(s.to_string()),
            })
            .collect()
    }

    #[test]
    fn lines_of_keys() {
        let text = "# head\nlimits:\n  # note\n  max-players: 1\n\nlisteners: x\nlistener:\n  - bind: a\n    transport: t\n  - bind: b\n    proxy-protocol: true\nstatus: { motd: x }\n";
        assert_eq!(line_of(text, &path(&["limits", "max-players"])), Some(4));
        assert_eq!(
            line_of(text, &path(&["listener", "0", "transport"])),
            Some(9)
        );
        assert_eq!(line_of(text, &path(&["listener", "1", "bind"])), Some(10));
        assert_eq!(
            line_of(text, &path(&["listener", "1", "proxy-protocol"])),
            Some(11)
        );
        // Inside a flow mapping: the line of the mapping.
        assert_eq!(line_of(text, &path(&["status", "motd"])), Some(12));
        assert_eq!(line_of(text, &path(&["nothing"])), None);
    }

    #[test]
    fn typos_get_a_hint() {
        let known = ["max-players", "max-in-gates"].map(String::from);
        assert_eq!(closest("max-player", &known), Some("max-players"));
        assert_eq!(closest("totally-else", &known), None);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("", "ab"), 2);
    }

    #[test]
    fn struct_fields_are_read() {
        assert!(fields::<crate::config::LimitsConfig>().contains(&"max-players"));
        // Flattened structs give none; their keys come from what they leave.
        assert!(fields::<Config>().is_empty());
        let l: crate::config::ListenerConfig = serde_json::from_value(serde_json::json!({
            "bind": "x", "transport": "t", "proxy-protocol": false,
            "trusted-proxies": [], "allow-any-proxy": false,
        }))
        .unwrap();
        assert!(l.rest.is_empty(), "{:?}", l.rest);
        assert_eq!(LISTENER.len(), 5);
    }
}
