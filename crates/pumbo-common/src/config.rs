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
//! settings it had, and a start uses the last copy that loaded
//! ([`load_with_last_good`]).

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

/// Reads a config or message file (a message file is one in a `lang` folder),
/// writing `template` first when it does not exist. A file that cannot be read
/// yields the template and a warning.
///
/// What was written is also kept in `.generated/` next to the file, so a new
/// version of the plugin can tell whether the admin changed it. A file nobody
/// changed follows the new template (a config only when none of its values
/// changed, so the values in force never change by themselves); a changed
/// file stays, with a warning naming the options it lacks and the ones that
/// still have the previous built-in value. A deleted file comes back. A file
/// from before the copy existed counts as unchanged when a released version
/// wrote it as it is ([`RELEASED`]); any other one is compared with this
/// version from now on.
pub fn read_or_create(path: &str, template: &str) -> (String, Vec<Warning>) {
    read_or_create_known(path, template, RELEASED)
}

/// FNV-1a of every `config.yml` and `lang/<code>.yml` the Pumpkin plugins
/// wrote before `.generated/` existed (0.1.0-beta.1 up to this change).
const RELEASED: &[u64] = &[
    0x06e6_8758_cf74_4681,
    0x079b_4837_7ec4_8b98,
    0x1462_2bb4_e3b5_dbac,
    0x3814_daa8_c933_1e7a,
    0x3c81_3ba1_9a72_5a5e,
    0x5504_7e1e_3bde_9bc7,
    0x5b2f_b347_4427_0c1f,
    0x5f11_ece2_165b_2e9b,
    0x636a_f97b_de8a_108d,
    0x708f_bc8d_0b7b_4483,
    0x751d_90b5_2321_718f,
    0x795b_af07_3705_5983,
    0x8891_b608_b37d_1713,
    0x9a2a_8395_a13e_cdf2,
    0x9ae7_93ec_462c_e53f,
    0xa402_de1f_5f61_5cc0,
    0xaed7_8970_ece7_b193,
    0xb437_81eb_1b1a_1e48,
    0xc1a4_cb7d_06ff_e14a,
    0xc98f_a2ef_14f5_77a3,
    0xea9e_847f_f647_9420,
    0xf1b5_1abc_41b9_4883,
    0xf621_5158_2e88_ce2a,
];

/// 64-bit FNV-1a, stable across Rust versions (unlike `DefaultHasher`).
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn read_or_create_known(path: &str, template: &str, known: &[u64]) -> (String, Vec<Warning>) {
    let file = std::path::Path::new(path);
    let parent = file.parent().unwrap_or(std::path::Path::new(""));
    let copy = parent.join(".generated").join(file.file_name().unwrap_or_default());
    let lang = parent.file_name().is_some_and(|n| n == "lang");
    let write = |p: &std::path::Path| {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(p, template)
    };
    let save = || write(file).and_then(|()| write(&copy));
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let warning = save().err().map(|e| Warning::new(None, format!("cannot write {path}: {e}")));
            return (template.to_string(), warning.into_iter().collect());
        }
        Err(e) => {
            return (
                template.to_string(),
                vec![Warning::new(None, format!("cannot read {path}: {e}; using defaults"))],
            );
        }
    };
    let mut old = std::fs::read_to_string(&copy).ok();
    if old.is_none() && known.contains(&fnv(&text)) {
        old = Some(text.clone());
    }
    let (mut missing, mut stale) = (Vec::new(), Vec::new());
    if let (Ok(t), Ok(u)) = (parse_table(template), parse_table(&text)) {
        let o = old.as_deref().and_then(|o| parse_table(o).ok());
        compare(&t, o.as_ref(), &u, "", &mut missing, &mut stale);
    }
    let unchanged = old.as_deref() == Some(text.as_str());
    if text != template && unchanged && (lang || stale.is_empty()) {
        let what = if lang { "texts" } else { "template" };
        let note = match save() {
            Ok(()) => format!("updated {path} to the new built-in {what}"),
            Err(e) => format!("cannot write {path}: {e}"),
        };
        return (template.to_string(), vec![Warning::new(None, note)]);
    }
    // An unknown file without a copy is compared with this version from now on.
    if old.is_none() || (text == template && old.as_deref() != Some(template)) {
        let _ = write(&copy);
    }
    let (lacks, kept) = if lang {
        (
            "lacks texts of this version, the built-in ones apply",
            "still has the previous built-in texts (remove these keys to get the new ones) for",
        )
    } else {
        ("lacks options of this version, their defaults apply", "keeps the previous defaults (they apply) for")
    };
    let warnings = [(missing, lacks), (stale, kept)]
        .into_iter()
        .filter(|(names, _)| !names.is_empty())
        .map(|(names, what)| Warning::new(None, format!("{path} {what}: {}", names.join(", "))))
        .collect();
    (text, warnings)
}

/// Compares `text` with the `template` of this version: options it lacks (a
/// missing section is named once) go to `missing`, and options whose built-in
/// value changed from `old` while `text` still has the old one to `stale`.
/// Lists are values.
fn compare(
    t: &Table,
    old: Option<&Table>,
    text: &Table,
    path: &str,
    missing: &mut Vec<String>,
    stale: &mut Vec<String>,
) {
    for (key, tv) in t {
        let full = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
        let ov = old.and_then(|o| o.get(key));
        match (tv, text.get(key)) {
            (_, None) => missing.push(full),
            (Value::Object(ts), Some(Value::Object(us))) => {
                compare(ts, ov.and_then(Value::as_object), us, &full, missing, stale);
            }
            (_, Some(uv)) => {
                if ov.is_some_and(|ov| ov != tv && ov == uv) {
                    stale.push(full);
                }
            }
        }
    }
}

/// The `commands:` section: short commands for the plugin's main command and
/// what is switched off. Each plugin sets its own defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Commands {
    /// Short commands for the main command (`pp` for `/pumboperms`); the
    /// full name is always registered.
    pub aliases: Vec<String>,
    /// Commands or subcommands not to offer.
    pub disabled: Vec<String>,
}

impl Commands {
    /// Names as typed without `/`, in lower case; anything that cannot be a
    /// command name is dropped with a warning.
    pub fn validate(&mut self, check: &mut Check<'_>) {
        for (option, list) in [("commands.aliases", &mut self.aliases), ("commands.disabled", &mut self.disabled)] {
            for raw in std::mem::take(list) {
                let name = raw.trim().trim_start_matches('/').to_lowercase();
                if !crate::command::is_valid_node(&name) {
                    check.warn(option, format!("'{raw}' in '{option}' is not a command name, ignored"));
                } else if !list.contains(&name) {
                    list.push(name);
                }
            }
        }
    }

    /// The short commands to register for `full`: the full name never twice,
    /// and a name `taken` gives an owner for is skipped with a warning.
    pub fn aliases_to_register(
        &self,
        full: &str,
        taken: impl Fn(&str) -> Option<String>,
    ) -> (Vec<String>, Vec<Warning>) {
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        for alias in self.aliases.iter().filter(|a| *a != full) {
            match taken(alias) {
                Some(owner) => warnings.push(Warning::new(
                    Some("commands.aliases"),
                    format!("/{alias} belongs to {owner}, not registered; /{full} works"),
                )),
                None => out.push(alias.clone()),
            }
        }
        (out, warnings)
    }
}

/// The copy of the last `config.yml` that loaded, in the data folder.
pub const LAST_GOOD: &str = "config.last-good.yml";

/// Loads `config.yml` from `config_dir` at start (missing: the defaults). A
/// file that loads is copied to [`LAST_GOOD`] in `data_dir`. A file that is not
/// valid YAML (or cannot be read) loads that copy instead, with a fatal warning
/// for the log; without a copy the error is returned and the plugin must not
/// start, since the defaults may give rights the admin took away.
pub fn load_with_last_good<T: Settings>(config_dir: &str, data_dir: &str) -> Result<(T, Vec<Warning>), String> {
    let path = format!("{config_dir}/config.yml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {path}: {e}")),
    };
    let broken = match text {
        Ok(text) => {
            let (cfg, mut warnings) = load::<T>(&text);
            match warnings.iter().find(|w| w.fatal) {
                None => {
                    warnings.extend(keep_last_good(config_dir, data_dir));
                    return Ok((cfg, warnings));
                }
                Some(w) => w.message.clone(),
            }
        }
        Err(e) => e,
    };
    let copy = format!("{data_dir}/{LAST_GOOD}");
    let Ok(text) = std::fs::read_to_string(&copy) else {
        return Err(format!("{broken}; there is no {LAST_GOOD} to start from, fix config.yml"));
    };
    let (cfg, mut warnings) = load::<T>(&text);
    if warnings.iter().any(|w| w.fatal) {
        return Err(format!("{broken}; {LAST_GOOD} cannot be read either, fix config.yml"));
    }
    warnings.insert(0, Warning::fatal(format!("{broken}; started with the last good copy {LAST_GOOD}")));
    Ok((cfg, warnings))
}

/// Copies `config.yml` of `config_dir` to [`LAST_GOOD`] in `data_dir` (after a
/// load or reload that worked). Nothing to copy when the file is missing.
pub fn keep_last_good(config_dir: &str, data_dir: &str) -> Option<Warning> {
    let text = std::fs::read_to_string(format!("{config_dir}/config.yml")).ok()?;
    write_atomic(&format!("{data_dir}/{LAST_GOOD}"), text.as_bytes())
        .err()
        .map(|e| Warning::new(None, format!("cannot save {LAST_GOOD}: {e}")))
}

/// Writes through `<path>.tmp`, so a crash leaves the old file or the new one.
fn write_atomic(path: &str, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = format!("{path}.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
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
    #[expect(clippy::integer_division, reason = "a third of the key length, rounded down")]
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
        assert!(w.is_empty());
        std::fs::write(path, "a: 2\n").unwrap();
        assert_eq!(read_or_create(path, "a: 1\n").0, "a: 2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A plugin data folder with `lang/en.yml` written by a version whose
    /// texts were `old`.
    fn installed(name: &str, old: &str) -> (std::path::PathBuf, String) {
        let (dir, _) = start_dirs(name);
        let path = dir.join("lang/en.yml").to_str().unwrap().to_string();
        let (text, w) = read_or_create(&path, old);
        assert!(text == old && w.is_empty(), "{w:?}");
        (dir, path)
    }

    const OLD: &str = "prefix: \"&#f28c28PumboX &8» \"\nbye: \"Do not move\"\n";
    const NEW: &str = "prefix: \"&#f28c28Pumbo&#ffff55X &8» \"\nbye: \"Don't move\"\nhello: \"Hi\"\n";

    #[test]
    fn untouched_lang_file_follows_update() {
        let (dir, path) = installed("lang-update", OLD);
        let (text, w) = read_or_create(&path, NEW);
        assert_eq!(text, NEW);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), NEW);
        assert!(w.len() == 1 && w[0].message.contains("updated") && !w[0].fatal, "{w:?}");
        // the next start finds nothing to do
        assert!(read_or_create(&path, NEW).1.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edited_lang_file_stays_with_warning() {
        let (dir, path) = installed("lang-edited", OLD);
        let edited = "prefix: \"&#f28c28PumboX &8» \"\nbye: \"Stay still\"\n";
        std::fs::write(&path, edited).unwrap();
        let (text, w) = read_or_create(&path, NEW);
        assert_eq!(text, edited);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        let messages: Vec<&str> = w.iter().map(|w| w.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("lacks") && m.ends_with(": hello")), "{messages:?}");
        // `prefix` still has the old built-in text, `bye` is the admin's own
        assert!(messages.iter().any(|m| m.contains("remove these keys") && m.ends_with(": prefix")), "{messages:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn released_file_without_copy_follows_update() {
        assert_eq!(fnv("a"), 0xaf63_dc4c_8601_ec8c);
        let (dir, _) = start_dirs("lang-released");
        let path = dir.join("lang/en.yml").to_str().unwrap().to_string();
        std::fs::create_dir_all(dir.join("lang")).unwrap();
        std::fs::write(&path, OLD).unwrap();
        let (text, w) = read_or_create_known(&path, NEW, &[fnv(OLD)]);
        assert!(text == NEW && w.len() == 1 && w[0].message.contains("updated"), "{w:?}");
        assert_eq!(std::fs::read_to_string(dir.join("lang/.generated/en.yml")).unwrap(), NEW);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_file_without_copy_is_compared_from_now_on() {
        let (dir, _) = start_dirs("lang-unknown");
        let path = dir.join("lang/en.yml").to_str().unwrap().to_string();
        std::fs::create_dir_all(dir.join("lang")).unwrap();
        let own = "prefix: \"[X] \"\nbye: \"Don't move\"\nhello: \"Hi\"\n";
        std::fs::write(&path, own).unwrap();
        // the admin's own texts are not listed
        let (text, w) = read_or_create_known(&path, NEW, &[fnv(OLD)]);
        assert!(text == own && w.is_empty(), "{w:?}");
        let newer = NEW.replace("Don't move", "Stand still");
        let (text, w) = read_or_create_known(&path, &newer, &[fnv(OLD)]);
        assert!(text == own && w.len() == 1 && w[0].message.ends_with(": bye"), "{w:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleted_lang_file_returns() {
        let (dir, path) = installed("lang-deleted", OLD);
        std::fs::remove_file(&path).unwrap();
        let (text, w) = read_or_create(&path, NEW);
        assert!(text == NEW && w.is_empty(), "{w:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), NEW);
        // and counts as untouched for the version after
        let newer = format!("{NEW}extra: \"x\"\n");
        assert_eq!(read_or_create(&path, &newer).0, newer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn untouched_config_keeps_changed_defaults() {
        let (dir, _) = start_dirs("cfg-update");
        let path = dir.join("config.yml").to_str().unwrap().to_string();
        read_or_create(&path, "a: 1\n");
        // only an option added: the file follows
        assert_eq!(read_or_create(&path, "a: 1\nb: 2\n").0, "a: 1\nb: 2\n");
        // a default changed: the old value stays in force
        let (text, w) = read_or_create(&path, "a: 5\nb: 2\n");
        assert_eq!(text, "a: 1\nb: 2\n");
        assert!(w.len() == 1 && w[0].message.ends_with("keeps the previous defaults (they apply) for: a"), "{w:?}");
        // a config from before the copy existed is never replaced
        std::fs::remove_dir_all(dir.join(".generated")).unwrap();
        let (text, w) = read_or_create(&path, "a: 1\nb: 2\nc: 3\n");
        assert_eq!(text, "a: 1\nb: 2\n");
        assert!(w.len() == 1 && w[0].message.ends_with("lacks options of this version, their defaults apply: c"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn commands(yaml: &str) -> Commands {
        #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
        #[serde(default, rename_all = "kebab-case")]
        struct C {
            commands: Commands,
        }
        impl Settings for C {
            fn validate(&mut self, check: &mut Check<'_>) {
                self.commands.validate(check);
            }
        }
        let (c, w) = load::<C>(yaml);
        assert!(w.is_empty(), "{w:?}");
        c.commands
    }

    #[test]
    fn aliases_come_from_config() {
        let c = commands("commands:\n  aliases: [/Perms, p, perms, pumboperms]\n  disabled: [Editor]\n");
        assert_eq!(c.aliases_to_register("pumboperms", |_| None).0, vec!["perms", "p"]);
        assert_eq!(c.disabled, vec!["editor"]);
        let mut bad = Commands { aliases: vec!["two words".into(), "ok".into()], disabled: Vec::new() };
        let mut w = Vec::new();
        bad.validate(&mut Check::new(&mut w));
        assert_eq!((bad.aliases, options(&w)), (vec!["ok".to_string()], vec!["commands.aliases".to_string()]));
    }

    #[test]
    fn cleared_alias_list_registers_only_full_name() {
        let c = commands("commands:\n  aliases: []\n");
        let (aliases, w) = c.aliases_to_register("pumboperms", |_| None);
        assert!(aliases.is_empty() && w.is_empty(), "{aliases:?} {w:?}");
    }

    #[test]
    fn taken_alias_is_skipped_with_warning() {
        let c = commands("commands:\n  aliases: [pp, ban]\n");
        let (aliases, w) = c.aliases_to_register("pumboperms", |a| (a == "ban").then(|| "Pumpkin".to_string()));
        assert_eq!(aliases, vec!["pp"]);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].message, "/ban belongs to Pumpkin, not registered; /pumboperms works");
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

    fn start_dirs(name: &str) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("pumbo-common-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap().to_string();
        (dir, d)
    }

    #[test]
    fn fatal_config_at_start_uses_last_good_copy() {
        let (dir, d) = start_dirs("last-good");
        std::fs::write(dir.join("config.yml"), "language: pl\n").unwrap();
        let (c, w) = load_with_last_good::<Cfg>(&d, &d).unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.language, "pl");
        assert_eq!(std::fs::read_to_string(dir.join(LAST_GOOD)).unwrap(), "language: pl\n");
        std::fs::write(dir.join("config.yml"), "language: [pl\n").unwrap();
        let (c, w) = load_with_last_good::<Cfg>(&d, &d).unwrap();
        assert_eq!(c.language, "pl", "the copy, not the defaults");
        assert!(w.first().is_some_and(|w| w.fatal && w.message.contains(LAST_GOOD)), "{w:?}");
        assert_eq!(std::fs::read_to_string(dir.join(LAST_GOOD)).unwrap(), "language: pl\n", "copy kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fatal_config_without_copy_refuses_start() {
        let (dir, d) = start_dirs("no-copy");
        std::fs::write(dir.join("config.yml"), "language: [pl\n").unwrap();
        let err = load_with_last_good::<Cfg>(&d, &d).unwrap_err();
        assert!(err.contains("not valid YAML") && err.contains(LAST_GOOD), "{err}");
        // No file at all is the defaults, not an error.
        std::fs::remove_file(dir.join("config.yml")).unwrap();
        let (c, w) = load_with_last_good::<Cfg>(&d, &d).unwrap();
        assert!(w.is_empty() && c == Cfg::default(), "{w:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
