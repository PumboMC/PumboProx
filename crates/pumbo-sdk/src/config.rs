//! Plugin config (plan §6.6.1, §11 E5): `config.yml` in the plugin's config
//! directory plus overlays `servers/<server>.yml` and `groups/<group>.yml`,
//! applied over the global file in the order server > group (config
//! priority) > global. Mappings merge deeply, other values (lists included)
//! are replaced. YAML 1.2: only `true` and `false` are booleans, so `no` or
//! `off` stay text; errors name the file, line and column.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::bindings::pumbo::prox::types::{PlayerContext, PlayerId};

type Table = Map<String, Value>;

/// `general`, the same in every Pumbo plugin.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "kebab-case")]
pub struct General {
    /// Fallback language when the client's is not available.
    pub language: String,
    pub debug: bool,
}

impl Default for General {
    fn default() -> Self {
        General {
            language: "en".into(),
            debug: false,
        }
    }
}

/// `storage`: `redb` in 0.1; `sql` with `shared: true` later (plan §6.6.1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "kebab-case")]
pub struct Storage {
    pub backend: String,
    pub shared: bool,
}

impl Default for Storage {
    fn default() -> Self {
        Storage {
            backend: "redb".into(),
            shared: false,
        }
    }
}

/// `messages`: the plugin prefix used by the `<prefix>` tag.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "kebab-case")]
pub struct Messages {
    pub prefix: String,
}

/// Deep merge: mappings merge key by key, any other value replaces.
pub fn merge(base: &mut Table, over: &Table) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(Value::Object(b)), Value::Object(o)) => merge(b, o),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Reads YAML 1.2 text (`no`, `off`, `on` stay text); an error names the line
/// and column.
pub fn from_yaml<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    let options = serde_saphyr::options! { strict_booleans: true, with_snippet: false };
    serde_saphyr::from_str_with_options(text, options).map_err(|e| e.to_string())
}

#[derive(Debug, Default, Clone)]
struct Files {
    global: Table,
    servers: BTreeMap<String, Table>,
    groups: BTreeMap<String, Table>,
}

fn parse(name: &str, text: &str) -> Result<Table, String> {
    match from_yaml::<Value>(text).map_err(|e| format!("{name}: {e}"))? {
        Value::Object(t) => Ok(t),
        Value::Null => Ok(Table::new()),
        _ => Err(format!(
            "{name}: expected `key: value` lines at the top level"
        )),
    }
}

/// Logs a file of the format before YAML, which is not read any more.
fn warn_old(path: &str) {
    crate::log::warn(&format!(
        "found {path}, this version reads {}.yml - convert it (see README)",
        path.trim_end_matches(".toml")
    ));
}

fn read_dir_tables(dir: &str) -> Result<BTreeMap<String, Table>, String> {
    let mut out = BTreeMap::new();
    for file in crate::host::list_config_dir(dir) {
        if file.ends_with(".toml") {
            warn_old(&format!("{dir}/{file}"));
        }
        let Some(stem) = file.strip_suffix(".yml") else {
            continue;
        };
        let path = format!("{dir}/{file}");
        let text = crate::host::read_config_file(&path).unwrap_or_default();
        out.insert(stem.to_string(), parse(&path, &text)?);
    }
    Ok(out)
}

/// A typed config with overlays, cached per context.
pub struct Config<T> {
    files: RefCell<Option<Files>>,
    cache: RefCell<BTreeMap<String, Rc<T>>>,
}

impl<T> Default for Config<T> {
    fn default() -> Self {
        Config::new()
    }
}

impl<T> Config<T> {
    pub const fn new() -> Config<T> {
        Config {
            files: RefCell::new(None),
            cache: RefCell::new(BTreeMap::new()),
        }
    }
}

impl<T: serde::de::DeserializeOwned + Default> Config<T> {
    fn read() -> Result<Files, String> {
        let global = match crate::host::read_config_file("config.yml") {
            Some(text) => parse("config.yml", &text)?,
            None => {
                if crate::host::read_config_file("config.toml").is_some() {
                    warn_old("config.toml");
                }
                Table::new()
            }
        };
        let files = Files {
            global,
            servers: read_dir_tables("servers")?,
            groups: read_dir_tables("groups")?,
        };
        // Every file on its own over the global one must give a valid config.
        Self::build(&files.global, &[])?;
        for (kind, map) in [("servers", &files.servers), ("groups", &files.groups)] {
            for (name, t) in map {
                Self::build(&files.global, &[t]).map_err(|e| format!("{kind}/{name}.yml: {e}"))?;
            }
        }
        Ok(files)
    }

    fn build(global: &Table, overlays: &[&Table]) -> Result<T, String> {
        let mut t = global.clone();
        for o in overlays {
            merge(&mut t, o);
        }
        T::deserialize(Value::Object(t)).map_err(|e| e.to_string())
    }

    /// Re-reads every file; on an error the old config stays and the error
    /// names the file.
    pub fn reload(&self) -> Result<(), String> {
        let files = Self::read()?;
        *self.files.borrow_mut() = Some(files);
        self.cache.borrow_mut().clear();
        Ok(())
    }

    fn ensure_loaded(&self) {
        if self.files.borrow().is_none() {
            let files = Self::read().unwrap_or_else(|e| {
                crate::log::error(&format!("config: {e}; using defaults"));
                Files::default()
            });
            *self.files.borrow_mut() = Some(files);
        }
    }

    /// The global config.
    pub fn get(&self) -> Rc<T> {
        self.at(&PlayerContext {
            server: None,
            groups: Vec::new(),
        })
    }

    /// The config in a context: global, then group overlays from the lowest
    /// to the highest priority, then the server overlay.
    pub fn at(&self, ctx: &PlayerContext) -> Rc<T> {
        let key = format!(
            "{}|{}",
            ctx.server.as_deref().unwrap_or(""),
            ctx.groups.join(",")
        );
        if let Some(c) = self.cache.borrow().get(&key) {
            return Rc::clone(c);
        }
        self.ensure_loaded();
        let value = {
            let files = self.files.borrow();
            let files = files.as_ref();
            let empty = Table::new();
            let global = files.map_or(&empty, |f| &f.global);
            let mut overlays: Vec<&Table> = Vec::new();
            if let Some(f) = files {
                for g in ctx.groups.iter().rev() {
                    if let Some(t) = f.groups.get(g) {
                        overlays.push(t);
                    }
                }
                if let Some(t) = ctx.server.as_ref().and_then(|s| f.servers.get(s)) {
                    overlays.push(t);
                }
            }
            Self::build(global, &overlays).unwrap_or_default()
        };
        let value = Rc::new(value);
        self.cache.borrow_mut().insert(key, Rc::clone(&value));
        value
    }

    /// The config in the player's current context (global in the virtual
    /// world and before the first backend).
    pub fn for_player(&self, id: PlayerId) -> Rc<T> {
        match crate::players::get(id) {
            Some(p) if !p.in_virtual => self.at(&p.context),
            _ => self.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default)]
    struct Cfg {
        greeting: String,
        tags: Vec<String>,
        limits: Limits,
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default)]
    struct Limits {
        a: u32,
        b: u32,
    }

    fn ctx(server: &str, groups: &[&str]) -> PlayerContext {
        PlayerContext {
            server: Some(server.into()),
            groups: groups.iter().map(|g| g.to_string()).collect(),
        }
    }

    #[test]
    fn overlays_merge_in_order() {
        testing::reset();
        testing::config_file(
            "config.yml",
            "greeting: hi\ntags: [a]\nlimits:\n  a: 1\n  b: 2\n",
        );
        testing::config_file(
            "groups/survivals.yml",
            "greeting: group\nlimits:\n  a: 10\n",
        );
        testing::config_file("groups/all.yml", "greeting: all\ntags: [x, y]\n");
        testing::config_file("servers/survival.yml", "limits:\n  b: 20\n");
        let c: Config<Cfg> = Config::new();
        assert_eq!(c.get().greeting, "hi");
        // survivals before all in config order: survivals wins the conflict.
        let s = c.at(&ctx("survival", &["survivals", "all"]));
        assert_eq!(s.greeting, "group");
        assert_eq!(s.tags, vec!["x".to_string(), "y".to_string()]);
        assert_eq!(s.limits, Limits { a: 10, b: 20 });
        let lobby = c.at(&ctx("lobby", &["all"]));
        assert_eq!(lobby.greeting, "all");
        assert_eq!(lobby.limits, Limits { a: 1, b: 2 });

        // A broken overlay is named in the error and the old config stays:
        // a value of the wrong type...
        testing::config_file("servers/survival.yml", "limits:\n  b: many\n");
        let err = c.reload().unwrap_err();
        assert!(err.contains("servers/survival.yml"), "{err}");
        assert_eq!(c.at(&ctx("survival", &["survivals", "all"])).limits.b, 20);
        // ...and a file that is not valid YAML (bad indentation), with its line.
        testing::config_file("servers/survival.yml", "limits:\n    a: 1\n  b: 30\n");
        let err = c.reload().unwrap_err();
        assert!(
            err.contains("servers/survival.yml") && err.contains("line 3"),
            "{err}"
        );
        assert_eq!(c.at(&ctx("survival", &["survivals", "all"])).limits.b, 20);
        testing::config_file("servers/survival.yml", "limits:\n  b: 30\n");
        c.reload().unwrap();
        assert_eq!(c.at(&ctx("survival", &["survivals", "all"])).limits.b, 30);
        // `no` is text in YAML 1.2, not `false`.
        testing::config_file("config.yml", "greeting: no\n");
        c.reload().unwrap();
        assert_eq!(c.get().greeting, "no");
    }
}
