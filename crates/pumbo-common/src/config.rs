//! Plugin configuration in YAML (`config.yml` in the plugin data folder).
//!
//! Loading never fails and never panics: syntax errors, unknown options and bad
//! values become [`Warning`]s naming the option, and each affected option keeps
//! its default. A plugin describes its file with one struct:
//!
//! ```
//! use pumbo_common::config::{self, Check, Settings};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, PartialEq, Serialize, Deserialize)]
//! #[serde(default, rename_all = "kebab-case")]
//! struct MyConfig {
//!     language: String,
//!     max_warnings: u32,
//! }
//!
//! impl Default for MyConfig {
//!     fn default() -> Self {
//!         Self { language: "en".into(), max_warnings: 3 }
//!     }
//! }
//!
//! impl Settings for MyConfig {
//!     fn validate(&mut self, check: &mut Check<'_>) {
//!         check.clamp("max-warnings", &mut self.max_warnings, 1, 100);
//!     }
//! }
//!
//! let (cfg, warnings) = config::load::<MyConfig>("max-warnings: 0\ncolour: 1\n");
//! assert_eq!(cfg.max_warnings, 1);
//! assert_eq!(warnings.len(), 2); // out of range, unknown option
//! ```
//!
//! Rules for config structs: `#[serde(default, rename_all = "kebab-case")]` on
//! every struct, sections as nested structs, no `Option` fields (use an empty
//! string or 0 for "off"), enums as lowercase strings.
//!
//! YAML is read as YAML 1.2: only `true` and `false` are booleans, so `no`,
//! `off` or `on` stay text. A file that is not valid YAML gives a
//! [`Warning::fatal`] with the line and column; a reload then keeps the
//! settings it had.

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

type Table = Map<String, Value>;

/// A problem found while loading. The plugin logs it; nothing else happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Dotted option name (`captcha.attempts`), when the problem belongs to one.
    pub option: Option<String>,
    pub message: String,
    /// The whole file was rejected (not valid YAML): a reload keeps the
    /// current settings instead of falling back to the defaults.
    pub fatal: bool,
}

impl Warning {
    pub fn new(option: Option<&str>, message: impl Into<String>) -> Self {
        Self { option: option.map(str::to_string), message: message.into(), fatal: false }
    }

    /// A file that could not be read as YAML at all.
    pub fn fatal(message: impl Into<String>) -> Self {
        Self { option: None, message: message.into(), fatal: true }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// A configuration file type.
pub trait Settings: Serialize + DeserializeOwned + Default {
    /// Top-level sections renamed in later versions, as `(old, new)`. A file that
    /// still uses the old name keeps working; when it has both, the new one wins.
    const RENAMED_SECTIONS: &'static [(&'static str, &'static str)] = &[];

    /// Options that no longer do anything, as `(dotted name, why)`. They are
    /// dropped with a note instead of the "unknown option" warning.
    const REMOVED_OPTIONS: &'static [(&'static str, &'static str)] = &[];

    /// Sections whose keys are chosen by the user (maps such as message presets).
    /// Their entries are taken as written and checked one by one.
    const FREE_SECTIONS: &'static [&'static str] = &[];

    /// Fixes values that parse but would break the logic (ranges, rules between
    /// options). Report every correction through `check`.
    fn validate(&mut self, _check: &mut Check<'_>) {}
}

/// Reads YAML 1.2 text into a tree (`no`, `off`, `on` are text, only `true`
/// and `false` are booleans). An error names the line and column.
pub fn parse_yaml(text: &str) -> Result<Value, String> {
    let options = serde_saphyr::options! { strict_booleans: true, with_snippet: false };
    serde_saphyr::from_str_with_options(text, options).map_err(|e| e.to_string())
}

/// Reads a file of `key: value` lines (an empty file is an empty table).
fn parse_table(text: &str) -> Result<Table, String> {
    match parse_yaml(text)? {
        Value::Object(t) => Ok(t),
        Value::Null => Ok(Table::new()),
        _ => Err("expected `option: value` lines at the top level".into()),
    }
}

/// Parses a configuration file, see the module docs.
pub fn load<T: Settings>(text: &str) -> (T, Vec<Warning>) {
    let mut warnings = Vec::new();
    let defaults = T::default();
    let Ok(Value::Object(default_table)) = serde_json::to_value(&defaults) else {
        warnings.push(Warning::new(None, "internal: the default configuration cannot be written as a table"));
        return (defaults, warnings);
    };
    let user = parse_table(text).unwrap_or_else(|e| {
        warnings.push(Warning::fatal(format!("config.yml is not valid YAML: {e}")));
        Table::new()
    });
    let user = drop_removed(rename_sections(user, T::RENAMED_SECTIONS), T::REMOVED_OPTIONS, &mut warnings);
    let mut merged = default_table.clone();
    let mut ctx = Merge { root: &default_table, free: T::FREE_SECTIONS, check: leaf_ok::<T>, warnings: &mut warnings };
    ctx.merge(&mut merged, &default_table, &user, "");
    let mut cfg = match serde_json::from_value::<T>(Value::Object(merged)) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(Warning::new(None, format!("config could not be applied, using defaults: {e}")));
            defaults
        }
    };
    cfg.validate(&mut Check::new(&mut warnings));
    (cfg, warnings)
}

/// Files of the old TOML format left in `dir` (`config.toml`, `lang/*.toml`):
/// they are not read any more, so each one gets a warning instead of being
/// silently ignored.
pub fn old_files(dir: &str) -> Vec<Warning> {
    let mut found = Vec::new();
    if std::path::Path::new(&format!("{dir}/config.toml")).is_file() {
        found.push("config".to_string());
    }
    if let Ok(entries) = std::fs::read_dir(format!("{dir}/lang")) {
        let mut langs: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".toml").map(|c| format!("lang/{c}")))
            .collect();
        langs.sort();
        found.extend(langs);
    }
    found
        .into_iter()
        .map(|f| Warning::new(None, format!("found {f}.toml, this version reads {f}.yml - convert it (see README)")))
        .collect()
}

/// Reads a config or message file, writing `template` first when it does not
/// exist. A file that cannot be read yields the template and a warning.
pub fn read_or_create(path: &str, template: &str) -> (String, Option<Warning>) {
    match std::fs::read_to_string(path) {
        Ok(text) => (text, None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = std::path::Path::new(path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let warning = std::fs::write(path, template)
                .err()
                .map(|e| Warning::new(None, format!("cannot write {path}: {e}; using built-in defaults")));
            (template.to_string(), warning)
        }
        Err(e) => (template.to_string(), Some(Warning::new(None, format!("cannot read {path}: {e}; using defaults")))),
    }
}

/// Collects corrections made by [`Settings::validate`].
pub struct Check<'a> {
    warnings: &'a mut Vec<Warning>,
}

impl<'a> Check<'a> {
    pub fn new(warnings: &'a mut Vec<Warning>) -> Self {
        Self { warnings }
    }

    /// Keeps `value` within `min..=max`, moving it to the nearest bound (a NaN to
    /// `min`) with a warning.
    pub fn clamp<N: PartialOrd + Copy + fmt::Display>(&mut self, option: &str, value: &mut N, min: N, max: N) {
        if *value >= min && *value <= max {
            return;
        }
        let fixed = if *value > max { max } else { min };
        self.warnings.push(Warning::new(
            Some(option),
            format!("value {} for option '{option}' is out of range {min}..={max}, using {fixed}", *value),
        ));
        *value = fixed;
    }

    /// Resets `value` to `default` with a warning unless `valid(value)` holds.
    pub fn ensure<V: fmt::Debug>(&mut self, option: &str, value: &mut V, default: V, valid: impl FnOnce(&V) -> bool) {
        if !valid(value) {
            self.invalid(option, value, &default);
            *value = default;
        }
    }

    /// Records that `value` was replaced by `default`.
    pub fn invalid(&mut self, option: &str, value: &dyn fmt::Debug, default: &dyn fmt::Debug) {
        self.warnings.push(Warning::new(
            Some(option),
            format!("invalid value {value:?} for option '{option}', using default {default:?}"),
        ));
    }

    /// Any other note about an option.
    pub fn warn(&mut self, option: &str, message: impl Into<String>) {
        self.warnings.push(Warning::new(Some(option), message));
    }
}

fn rename_sections(mut user: Table, renamed: &[(&str, &str)]) -> Table {
    for (old, new) in renamed {
        if let Some(value) = user.remove(*old) {
            user.entry(*new).or_insert(value);
        }
    }
    user
}

fn drop_removed(mut user: Table, removed: &[(&str, &str)], w: &mut Vec<Warning>) -> Table {
    for (path, why) in removed {
        let parts: Vec<&str> = path.split('.').collect();
        if remove_path(&mut user, &parts) {
            w.push(Warning::new(Some(path), format!("option '{path}' is no longer used ({why}); it can be removed")));
        }
    }
    user
}

fn remove_path(table: &mut Table, path: &[&str]) -> bool {
    match path {
        [key] => table.remove(*key).is_some(),
        [first, rest @ ..] => match table.get_mut(*first) {
            Some(Value::Object(t)) => remove_path(t, rest),
            _ => false,
        },
        [] => false,
    }
}

struct Merge<'a> {
    root: &'a Table,
    free: &'a [&'a str],
    check: fn(&Table, &str, &str, &Value) -> bool,
    warnings: &'a mut Vec<Warning>,
}

impl Merge<'_> {
    /// Copies user values over the defaults, key by key. A value of the wrong type
    /// (or one that does not deserialize on its own) keeps the default.
    fn merge(&mut self, target: &mut Table, defaults: &Table, user: &Table, path: &str) {
        let free = self.free.contains(&path);
        for (key, value) in user {
            let full = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
            let def = match defaults.get(key) {
                Some(d) => d,
                None if free => {
                    if (self.check)(self.root, path, key, value) {
                        target.insert(key.clone(), value.clone());
                    } else {
                        self.warnings
                            .push(Warning::new(Some(&full), format!("invalid value {value} for '{full}' ignored")));
                    }
                    continue;
                }
                None => {
                    let hint =
                        closest(self.root, "", key).map(|p| format!(" (did you mean '{p}'?)")).unwrap_or_default();
                    self.warnings.push(Warning::new(Some(&full), format!("unknown option '{full}' ignored{hint}")));
                    continue;
                }
            };
            match (def, value) {
                (Value::Object(dt), Value::Object(ut)) => {
                    let mut sub = dt.clone();
                    self.merge(&mut sub, dt, ut, &full);
                    target.insert(key.clone(), Value::Object(sub));
                }
                (Value::Object(_), other) => {
                    self.warnings.push(Warning::new(
                        Some(&full),
                        format!("option '{full}' must be a section, got {other}; using defaults"),
                    ));
                }
                // Same kind of value (numbers of any kind count as one); whether it
                // fits (an integer for a whole number, an enum name) decides `check`.
                (d, u)
                    if std::mem::discriminant(d) == std::mem::discriminant(u)
                        && (self.check)(self.root, path, key, u) =>
                {
                    target.insert(key.clone(), u.clone());
                }
                (d, u) => self.warnings.push(Warning::new(
                    Some(&full),
                    format!("invalid value {u} for option '{full}', using default {d}"),
                )),
            }
        }
    }
}

/// The known option whose name is closest to `key` (at most two letters off,
/// fewer for short names),
/// anywhere in `table`: a typo, or a line indented under the wrong section.
fn closest(table: &Table, path: &str, key: &str) -> Option<String> {
    fn walk(table: &Table, path: &str, key: &str, best: &mut Option<(usize, String)>) {
        for (name, value) in table {
            let full = if path.is_empty() { name.clone() } else { format!("{path}.{name}") };
            let d = distance(name, key);
            if d <= 2.min(key.chars().count() / 3) && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                *best = Some((d, full.clone()));
            }
            if let Value::Object(sub) = value {
                walk(sub, &full, key, best);
            }
        }
    }
    let mut best = None;
    walk(table, path, key, &mut best);
    best.map(|(_, p)| p)
}

/// Levenshtein distance between two option names.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let above = row.get(j + 1).copied().unwrap_or(0);
            let left = row.get(j).copied().unwrap_or(0);
            let next = (prev + usize::from(ca != *cb)).min(above + 1).min(left + 1);
            prev = above;
            if let Some(cell) = row.get_mut(j + 1) {
                *cell = next;
            }
        }
    }
    row.last().copied().unwrap_or(0)
}

/// Checks one value by deserializing the defaults with only that value replaced.
fn leaf_ok<T: Settings>(root: &Table, path: &str, key: &str, value: &Value) -> bool {
    let mut root = root.clone();
    let mut cursor = &mut root;
    if !path.is_empty() {
        for part in path.split('.') {
            match cursor.get_mut(part) {
                Some(Value::Object(t)) => cursor = t,
                _ => return false,
            }
        }
    }
    cursor.insert(key.to_string(), value.clone());
    serde_json::from_value::<T>(Value::Object(root)).is_ok()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum Mode {
        Fast,
        Safe,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(default, rename_all = "kebab-case")]
    struct Cfg {
        mode: Mode,
        language: String,
        world: World,
        limits: Limits,
        presets: BTreeMap<String, u32>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(default, rename_all = "kebab-case")]
    struct World {
        name: String,
        height: f64,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(default, rename_all = "kebab-case")]
    struct Limits {
        attempts: u32,
        timeout_seconds: u32,
        blocked: Vec<String>,
    }

    impl Default for Cfg {
        fn default() -> Self {
            let presets = BTreeMap::from([("spam".to_string(), 10)]);
            Self {
                mode: Mode::Safe,
                language: "en".into(),
                world: World::default(),
                limits: Limits::default(),
                presets,
            }
        }
    }

    impl Default for World {
        fn default() -> Self {
            Self { name: "pumbo".into(), height: 512.0 }
        }
    }

    impl Default for Limits {
        fn default() -> Self {
            Self { attempts: 3, timeout_seconds: 60, blocked: Vec::new() }
        }
    }

    impl Settings for Cfg {
        const RENAMED_SECTIONS: &'static [(&'static str, &'static str)] = &[("limbo", "world")];
        const REMOVED_OPTIONS: &'static [(&'static str, &'static str)] = &[("world.main-world", "not needed")];
        const FREE_SECTIONS: &'static [&'static str] = &["presets"];

        fn validate(&mut self, c: &mut Check<'_>) {
            c.clamp("limits.attempts", &mut self.limits.attempts, 1, 100);
            c.ensure("world.height", &mut self.world.height, 512.0, |h| h.is_finite() && *h > 0.0);
            c.ensure("language", &mut self.language, "en".into(), |l| !l.trim().is_empty());
        }
    }

    fn options(w: &[Warning]) -> Vec<String> {
        w.iter().filter_map(|w| w.option.clone()).collect()
    }

    #[test]
    fn empty_file_gives_defaults() {
        let (c, w) = load::<Cfg>("");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c, Cfg::default());
    }

    #[test]
    fn bad_values_fall_back_per_option() {
        let text = "mode: weird\nlanguage: pl\nbogus: 1\nlimits:\n  attempts: 0\n  timeout-seconds: x\n  blocked: [a, 2]\nworld:\n  height: -1\n";
        let (c, w) = load::<Cfg>(text);
        assert_eq!(c.mode, Mode::Safe);
        assert_eq!(c.language, "pl");
        assert_eq!(c.limits.attempts, 1);
        assert_eq!(c.limits.timeout_seconds, 60);
        assert!(c.limits.blocked.is_empty());
        assert!((c.world.height - 512.0).abs() < f64::EPSILON);
        let mut names = options(&w);
        names.sort();
        assert_eq!(
            names,
            vec!["bogus", "limits.attempts", "limits.blocked", "limits.timeout-seconds", "mode", "world.height"]
        );
        // every warning names its option and the value used instead
        assert!(w.iter().any(|w| w.message.contains("'limits.attempts'") && w.message.contains("using 1")));
        assert!(w.iter().any(|w| w.message.contains("'mode'") && w.message.contains("\"safe\"")));
    }

    #[test]
    fn broken_yaml_gives_defaults_and_a_fatal_warning() {
        // `mode` indented by one space, neither under `world` nor at the top
        let (c, w) = load::<Cfg>("world:\n  name: x\n mode: fast\n");
        assert_eq!(c, Cfg::default());
        assert_eq!(w.len(), 1);
        assert!(w[0].option.is_none() && w[0].fatal);
        assert!(w[0].message.contains("line 3, column 2"), "{}", w[0].message);
        let (_, w) = load::<Cfg>("- a\n- b\n");
        assert!(w.len() == 1 && w[0].fatal, "{w:?}");
    }

    #[test]
    fn yaml_1_2_words_stay_text() {
        let (c, w) = load::<Cfg>("language: no\nworld:\n  name: off\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!((c.language.as_str(), c.world.name.as_str()), ("no", "off"));
        // ...so they are not booleans either
        let (_, w) = load::<Cfg>("mode: yes\n");
        assert_eq!(options(&w), vec!["mode"]);
    }

    #[test]
    fn unknown_options_get_a_hint() {
        // a typo, and an option indented under the wrong section
        let (_, w) = load::<Cfg>("languag: pl\nattempts: 5\nzzz: 1\n");
        let messages: Vec<&str> = w.iter().map(|w| w.message.as_str()).collect();
        assert!(messages.contains(&"unknown option 'languag' ignored (did you mean 'language'?)"), "{messages:?}");
        assert!(
            messages.contains(&"unknown option 'attempts' ignored (did you mean 'limits.attempts'?)"),
            "{messages:?}"
        );
        assert!(messages.contains(&"unknown option 'zzz' ignored"), "{messages:?}");
    }

    #[test]
    fn integer_for_float_and_negative_for_unsigned() {
        let (c, w) = load::<Cfg>("world:\n  height: 600\nlimits:\n  timeout-seconds: -5\n");
        assert!((c.world.height - 600.0).abs() < f64::EPSILON);
        assert_eq!(c.limits.timeout_seconds, 60);
        assert_eq!(options(&w), vec!["limits.timeout-seconds"]);
    }

    #[test]
    fn section_given_as_value() {
        let (c, w) = load::<Cfg>("world: 5\n");
        assert_eq!(c.world, World::default());
        assert_eq!(options(&w), vec!["world"]);
    }

    #[test]
    fn renamed_section_and_removed_option() {
        let (c, w) = load::<Cfg>("limbo:\n  name: old\n  main-world: x\n");
        assert_eq!(c.world.name, "old");
        assert_eq!(w.len(), 1);
        assert!(w[0].message.contains("world.main-world") && w[0].message.contains("no longer used"), "{w:?}");
        // the new name wins when both are present
        let (c, w) = load::<Cfg>("limbo:\n  name: old\nworld:\n  height: 100\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.world.name, "pumbo");
        assert!((c.world.height - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn free_sections_take_user_keys() {
        let (c, w) = load::<Cfg>("presets:\n  hacking: 30\n  bad: x\n");
        assert_eq!(c.presets.get("hacking"), Some(&30));
        assert_eq!(c.presets.get("spam"), Some(&10));
        assert!(!c.presets.contains_key("bad"));
        assert_eq!(options(&w), vec!["presets.bad"]);
        let (c, _) = load::<Cfg>("presets:\n  spam: 5\n");
        assert_eq!(c.presets.get("spam"), Some(&5));
    }

    #[test]
    fn check_helpers() {
        let mut w = Vec::new();
        let mut c = Check::new(&mut w);
        let mut x = f64::NAN;
        c.clamp("x", &mut x, 1.0, 2.0);
        let mut y = 50u32;
        c.clamp("y", &mut y, 1, 10);
        c.warn("z", "note");
        assert!((x - 1.0).abs() < f64::EPSILON);
        assert_eq!(y, 10);
        assert_eq!(options(&w), vec!["x", "y", "z"]);
    }

    #[test]
    fn read_or_create_writes_the_template() {
        let dir = std::env::temp_dir().join(format!("pumbo-common-cfg-{}", std::process::id()));
        let path = dir.join("sub").join("config.yml");
        let path = path.to_str().unwrap();
        let (text, w) = read_or_create(path, "a: 1\n");
        assert_eq!(text, "a: 1\n");
        assert!(w.is_none());
        std::fs::write(path, "a: 2\n").unwrap();
        assert_eq!(read_or_create(path, "a: 1\n").0, "a: 2\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn old_toml_files_are_reported() {
        let dir = std::env::temp_dir().join(format!("pumbo-common-old-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lang")).unwrap();
        let d = dir.to_str().unwrap();
        assert!(old_files(d).is_empty());
        std::fs::write(dir.join("config.toml"), "").unwrap();
        std::fs::write(dir.join("lang/pl.toml"), "").unwrap();
        std::fs::write(dir.join("lang/pl.yml"), "").unwrap();
        let w: Vec<String> = old_files(d).into_iter().map(|w| w.message).collect();
        assert_eq!(
            w,
            vec![
                "found config.toml, this version reads config.yml - convert it (see README)",
                "found lang/pl.toml, this version reads lang/pl.yml - convert it (see README)",
            ]
        );
        for f in ["config.toml", "lang/pl.toml", "lang/pl.yml"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        let _ = std::fs::remove_dir(dir.join("lang"));
        let _ = std::fs::remove_dir(&dir);
    }
}
