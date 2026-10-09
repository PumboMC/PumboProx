//! Plugin manifest (plan §4.1, §5.8, §6.6): built into the `.wasm`
//! (`pumbo_sdk::embed!`) or `<id>.yml` next to it, and the default config
//! and language files the plugin carries (D-STD-1, D-STD-2).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::actor::PluginSlot;

/// WIT package version the host speaks; manifests declare `api: "0.1"`.
pub const API: &str = "0.1";

/// Custom sections `pumbo_sdk::embed!` puts into a plugin's `.wasm`.
pub const MANIFEST_SECTION: &str = "pumbo-manifest";
pub const CONFIG_SECTION: &str = "pumbo-config";

/// A custom section of a `.wasm`, also inside its core modules (where
/// `link_section` puts it); read without compiling or running anything.
pub fn embedded<'a>(wasm: &'a [u8], name: &str) -> Option<&'a [u8]> {
    wasmparser::Parser::new(0)
        .parse_all(wasm)
        .find_map(|p| match p {
            Ok(wasmparser::Payload::CustomSection(s)) if s.name() == name => Some(s.data()),
            _ => None,
        })
}

/// Before an instance of new code (start, reload): the manifest of the file
/// now (`<id>.yml` next to it, else the built-in one) against the one the
/// proxy started with. One that does not read or names another plugin
/// refuses the load; other changes are logged and wait for a restart, the
/// running manifest stays (D-STD-3).
pub(crate) fn check_update(slot: &PluginSlot) -> Result<(), String> {
    let Some(dir) = slot.wasm.parent().filter(|_| slot.wasm.exists()) else {
        return Err(
            "not found (a file under another name loads at the next start of the proxy)".into(),
        );
    };
    let file = dir.join(format!("{}.yml", slot.id));
    let text = if file.is_file() {
        std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?
    } else {
        let bytes = std::fs::read(&slot.wasm).map_err(|e| e.to_string())?;
        let built_in = embedded(&bytes, MANIFEST_SECTION).ok_or("no manifest built in")?;
        String::from_utf8(built_in.to_vec()).map_err(|_| "the built-in manifest is not UTF-8")?
    };
    let m = Manifest::parse(&text).map_err(|e| format!("manifest: {e}"))?;
    if m.id != slot.id {
        return Err(format!("the file now holds plugin {}", m.id));
    }
    let changes = changes(&slot.manifest_text, &text).join(", ");
    if !changes.is_empty() {
        tracing::warn!(
            plugin = %slot.id,
            "the manifest of the new file differs: {changes}; the code is reloaded, the manifest is not - restart the proxy to apply the new manifest"
        );
    }
    if let Ok(mut r) = slot.restart.lock() {
        *r = (!changes.is_empty()).then_some(changes);
    }
    Ok(())
}

/// Top-level keys whose values differ (`version 0.1.0 -> 0.2.0` for plain
/// values, the key alone for lists and maps).
fn changes(old: &str, new: &str) -> Vec<String> {
    let read = |t: &str| match pumbo_core::yaml::from_str::<Value>(t) {
        Ok(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    let (old, new) = (read(old), read(new));
    let keys: std::collections::BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    let plain = |v: Option<&Value>| match v {
        Some(Value::String(s)) => Some(s.clone()),
        Some(v @ (Value::Number(_) | Value::Bool(_))) => Some(v.to_string()),
        _ => None,
    };
    keys.into_iter()
        .filter(|k| old.get(*k) != new.get(*k))
        .map(|k| match (plain(old.get(k)), plain(new.get(k))) {
            (Some(a), Some(b)) => format!("{k} {a} -> {b}"),
            _ => k.clone(),
        })
        .collect()
}

/// Prefix of the sections with language files (`pumbo-lang:lang/pl.yml`).
pub const LANG_SECTION: &str = "pumbo-lang:";

/// Before an instance starts: the default config and language files built
/// into the plugin become `<dir>/config.yml` and `<dir>/lang/<code>.yml`
/// when they are missing (like `saveDefaultConfig` in Paper and the language
/// files of the Pumpkin versions). A copy of what was written goes to
/// `<data>/.generated/` (D-STD-7): a file nobody changed since then follows
/// a new version of the plugin (a config only when none of its values
/// changed); a changed file stays, and options or texts it lacks or still
/// has from the previous version are named in the log.
pub(crate) fn default_files(id: &str, wasm: &Path, dir: &Path, data: &Path) {
    let Ok(bytes) = std::fs::read(wasm) else {
        return;
    };
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        let Ok(wasmparser::Payload::CustomSection(s)) = payload else {
            continue;
        };
        let (rel, kind) = if s.name() == CONFIG_SECTION {
            ("config.yml".to_string(), Kind::Config)
        } else if let Some(file) = s.name().strip_prefix(LANG_SECTION).and_then(lang_file) {
            (format!("lang/{file}"), Kind::Lang)
        } else {
            continue;
        };
        let base = data.join(".generated").join(&rel);
        default_file(id, &dir.join(&rel), &base, s.data(), kind);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Config,
    Lang,
}

/// `lang/pl.yml` → `pl.yml`: the name of a language file the host writes
/// (the section name comes from the plugin, so no other path).
fn lang_file(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let code = name.strip_suffix(".yml")?;
    let ok = !code.is_empty()
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then_some(name)
}

fn default_file(id: &str, path: &Path, base: &Path, template: &[u8], kind: Kind) {
    let write = |p: &Path| {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(p, template)
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(template));
    match created {
        Ok(()) => {
            let _ = write(base);
            tracing::info!(plugin = %id, "created {}", path.display());
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let template = String::from_utf8_lossy(template);
            // What the host wrote last time; none for files of older hosts,
            // whose language files are compared as they are.
            let old = std::fs::read_to_string(base).ok();
            let since = match kind {
                Kind::Lang => Some(old.as_deref().unwrap_or(&text)),
                Kind::Config => old.as_deref(),
            };
            let stale = since
                .map(|o| stale_options(&template, o, &text))
                .unwrap_or_default();
            let unchanged = old.as_deref() == Some(text.as_str());
            if text != template && unchanged && (kind == Kind::Lang || stale.is_empty()) {
                match write(path).and_then(|()| write(base)) {
                    Ok(()) => tracing::info!(
                        plugin = %id,
                        "updated {} to the new built-in {}",
                        path.display(),
                        if kind == Kind::Lang { "texts" } else { "template" }
                    ),
                    Err(e) => tracing::warn!(plugin = %id, "cannot write {}: {e}", path.display()),
                }
                return;
            }
            if text == template && old.as_deref() != Some(&*template) {
                let _ = write(base);
            }
            let (lacks, kept) = match kind {
                Kind::Config => (
                    "options of this version, their defaults apply",
                    "keeps the previous defaults (they apply) for",
                ),
                Kind::Lang => (
                    "texts of this version, the built-in ones apply",
                    "differs from the new built-in texts (delete the file to get them) for",
                ),
            };
            let missing = missing_options(&template, &text);
            if !missing.is_empty() {
                tracing::warn!(
                    plugin = %id,
                    "{} lacks {lacks}: {}",
                    path.display(),
                    missing.join(", ")
                );
            }
            if !stale.is_empty() {
                tracing::warn!(
                    plugin = %id,
                    "{} {kept}: {}",
                    path.display(),
                    stale.join(", ")
                );
            }
        }
        Err(e) => tracing::warn!(plugin = %id, "cannot write {}: {e}", path.display()),
    }
}

/// Dotted names of the options whose built-in value changed from `old` to
/// `template` while `text` still has the old one. Lists are values.
fn stale_options(template: &str, old: &str, text: &str) -> Vec<String> {
    fn leaves(v: &Value, path: &str, out: &mut BTreeMap<String, Value>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    let full = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    leaves(v, &full, out);
                }
            }
            v => {
                out.insert(path.to_string(), v.clone());
            }
        }
    }
    let read = |t: &str| {
        let mut out = BTreeMap::new();
        if let Ok(v) = pumbo_core::yaml::from_str::<Value>(t) {
            leaves(&v, "", &mut out);
        }
        out
    };
    let (t, o, u) = (read(template), read(old), read(text));
    t.into_iter()
        .filter(|(k, v)| o.get(k).is_some_and(|ov| ov != v && u.get(k) == Some(ov)))
        .map(|(k, _)| k)
        .collect()
}

/// Dotted names of the options of `template` that `text` lacks; a missing
/// section is named once. Lists are values. Text that is not valid YAML
/// gives none: the plugin reports it with the line.
fn missing_options(template: &str, text: &str) -> Vec<String> {
    fn walk(t: &Value, u: &Value, path: &str, out: &mut Vec<String>) {
        let empty = serde_json::Map::new();
        let Value::Object(t) = t else {
            return;
        };
        let u = match u {
            Value::Object(u) => u,
            Value::Null => &empty,
            _ => return,
        };
        for (key, tv) in t {
            let full = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            match u.get(key) {
                Some(uv) => walk(tv, uv, &full, out),
                None => out.push(full),
            }
        }
    }
    let read = pumbo_core::yaml::from_str::<Value>;
    let (Ok(t), Ok(u)) = (read(template), read(text)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(&t, &u, "", &mut out);
    out
}

/// Kinds of events, used for deadlines, `on-failure` and subscriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    Handshake,
    Status,
    PreLogin,
    Profile,
    Login,
    Gate,
    ServerConnect,
    ServerConnected,
    ServerKicked,
    Disconnect,
    Chat,
    BackendCommand,
    PluginMessage,
    ContextChanged,
    /// Implied by registered commands; accepted for readability.
    Command,
    /// Implied by the scheduler; accepted for readability.
    Timer,
    /// Virtual world input (E6); accepted, not delivered yet.
    VirtualInput,
    /// The following are implied by other manifest keys.
    ServiceCall,
    ServiceChanged,
    Placeholder,
    PermissionLoad,
    BusEvent,
    Admin,
}

impl EventKind {
    pub fn default_timeout_ms(self) -> u64 {
        match self {
            EventKind::Handshake | EventKind::Status => 100,
            EventKind::PreLogin | EventKind::Login | EventKind::Profile => 5000,
            EventKind::Chat | EventKind::BackendCommand | EventKind::PluginMessage => 200,
            EventKind::ServerConnect | EventKind::ServerKicked => 2000,
            EventKind::PermissionLoad => 2000,
            EventKind::Placeholder => 50,
            // Gates have their own `gate-timeout` (plan §5.6).
            EventKind::Gate => 300_000,
            _ => 5000,
        }
    }

    /// Default `on-failure` (plan §4.4 item 5).
    pub fn default_on_failure(self) -> OnFailure {
        match self {
            EventKind::Gate | EventKind::PreLogin | EventKind::Login | EventKind::Profile => {
                OnFailure::Deny
            }
            _ => OnFailure::Allow,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnFailure {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GateDecl {
    pub name: String,
    #[serde(default)]
    pub priority: i32,
    pub on_failure: Option<OnFailure>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ProfileDecl {
    #[serde(default)]
    pub priority: i32,
    pub on_failure: Option<OnFailure>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Provide {
    pub service: String,
    pub version: String,
    pub max_timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Use {
    pub service: String,
    pub version: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyScope {
    Player,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum KeyMode {
    #[default]
    Push,
    Pull,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct KeyDecl {
    pub name: String,
    pub scope: KeyScope,
    #[serde(default)]
    pub mode: KeyMode,
    #[serde(default)]
    pub public: bool,
    pub fallback: Option<String>,
    pub ttl_ms: Option<u64>,
    /// Pull deadline (default from `placeholders.pull-timeout-ms`, at most 250 ms).
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PlaceholderDecl {
    pub namespace: String,
    #[serde(default)]
    pub keys: Vec<KeyDecl>,
    #[serde(default)]
    pub suggested_aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionDecl {
    pub node: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub api: String,
    #[serde(default)]
    pub events: Vec<EventKind>,
    /// Hosts for outgoing HTTP.
    #[serde(default)]
    pub http: Vec<String>,
    pub gate: Option<GateDecl>,
    pub profile: Option<ProfileDecl>,
    /// `on-failure` per event kind with a result.
    #[serde(default)]
    pub on_failure: BTreeMap<EventKind, OnFailure>,
    /// Reserved as soon as the manifest loads, even if the plugin is dead (plan §2.8).
    #[serde(default)]
    pub sensitive_commands: Vec<String>,
    /// Backend commands this plugin sees in `on-backend-command`; `"*"` for all.
    #[serde(default)]
    pub command_filter: Vec<String>,
    /// Profile properties the plugin may change (e.g. `textures`).
    #[serde(default)]
    pub profile_properties: Vec<String>,
    #[serde(default)]
    pub provides: Vec<Provide>,
    #[serde(default)]
    pub uses: Vec<Use>,
    pub placeholders: Option<PlaceholderDecl>,
    #[serde(default)]
    pub permission_provider: bool,
    #[serde(default)]
    pub permissions_write: bool,
    /// Name under `/pumbo <short-name>`.
    pub short_name: Option<String>,
    /// A short root command for `/pumbo<short-name>` (`pf` for `/pumbofilter`).
    pub short_alias: Option<String>,
    #[serde(default)]
    pub publishes: Vec<String>,
    #[serde(default)]
    pub subscribes: Vec<String>,
    /// Permission nodes of the plugin (`pumbo.<plugin>.<action>`).
    #[serde(default)]
    pub permissions: Vec<PermissionDecl>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Not valid YAML, or a value of the wrong type (with line and column).
    #[error("{0}")]
    Yaml(String),
    #[error("{0}")]
    Invalid(String),
}

/// `name@major.minor` or `major.minor`.
pub fn parse_version(v: &str) -> Option<(u16, u16)> {
    let (major, minor) = v.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Splits `pumbo:player-punished@1.0` into name and version.
pub fn split_topic(t: &str) -> Option<(&str, u16, u16)> {
    let (name, ver) = t.rsplit_once('@')?;
    let (major, minor) = parse_version(ver)?;
    is_service_name(name).then_some((name, major, minor))
}

fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
}

/// `<namespace>:<name>` with lowercase letters, digits and `-`.
pub fn is_service_name(s: &str) -> bool {
    let Some((ns, name)) = s.split_once(':') else {
        return false;
    };
    let ok = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    ok(ns) && ok(name)
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        Manifest::parse_partial(text).map_err(|(_, e)| e)
    }

    /// [`Manifest::parse`] that also returns a manifest which reads but is
    /// invalid: its id and gate still tell whether it is required.
    pub(crate) fn parse_partial(text: &str) -> Result<Manifest, (Option<Manifest>, ManifestError)> {
        let m: Manifest =
            pumbo_core::yaml::from_str(text).map_err(|e| (None, ManifestError::Yaml(e)))?;
        match m.validate() {
            Ok(()) => Ok(m),
            Err(e) => Err((Some(m), e)),
        }
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let bad = |m: String| Err(ManifestError::Invalid(m));
        if !is_id(&self.id) || self.id == "data" || self.id == "proxy" {
            return bad(format!("invalid plugin id \"{}\"", self.id));
        }
        if self.api != API {
            return bad(format!(
                "{} {} is built for plugin API {}, this proxy provides {API}: use a build of the plugin for this proxy version",
                self.id, self.version, self.api
            ));
        }
        if self.gate.is_some() != self.events.contains(&EventKind::Gate) {
            return bad("`gate` and the \"gate\" event go together".into());
        }
        if self.profile.is_some() != self.events.contains(&EventKind::Profile) {
            return bad("`profile` and the \"profile\" event go together".into());
        }
        for p in &self.provides {
            if !is_service_name(&p.service) || parse_version(&p.version).is_none() {
                return bad(format!(
                    "invalid provided service {}@{}",
                    p.service, p.version
                ));
            }
        }
        for u in &self.uses {
            if !is_service_name(&u.service) || parse_version(&u.version).is_none() {
                return bad(format!("invalid used service {}@{}", u.service, u.version));
            }
        }
        for t in self.publishes.iter().chain(&self.subscribes) {
            if split_topic(t).is_none() {
                return bad(format!(
                    "invalid topic \"{t}\" (expected namespace:name@1.0)"
                ));
            }
        }
        if let Some(p) = &self.placeholders {
            let ns_ok = !p.namespace.is_empty()
                && p.namespace
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
            if !ns_ok {
                return bad(format!(
                    "placeholder namespace \"{}\" must be [a-z0-9]+",
                    p.namespace
                ));
            }
            for k in &p.keys {
                let key_ok = !k.name.is_empty()
                    && k.name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
                if !key_ok {
                    return bad(format!("placeholder key \"{}\" must be [a-z0-9_]+", k.name));
                }
            }
        }
        if let Some(s) = &self.short_name
            && (!is_id(s) || s == "proxy")
        {
            return bad(format!("invalid short-name \"{s}\""));
        }
        if let Some(a) = &self.short_alias
            && (self.short_name.is_none()
                || !is_id(a)
                || a.starts_with("pumbo")
                || crate::commands::RESERVED.contains(&a.as_str()))
        {
            return bad(format!(
                "invalid short-alias \"{a}\" (needs short-name, not a proxy command)"
            ));
        }
        Ok(())
    }

    pub fn listens(&self, kind: EventKind) -> bool {
        self.events.contains(&kind)
    }

    /// `on-failure` for an event kind: manifest, else the default; the proxy
    /// config may only raise it to `deny`.
    pub fn on_failure(&self, kind: EventKind, raise: Option<OnFailure>) -> OnFailure {
        let declared = match kind {
            EventKind::Gate => self.gate.as_ref().and_then(|g| g.on_failure),
            EventKind::Profile => self.profile.as_ref().and_then(|p| p.on_failure),
            _ => None,
        }
        .or_else(|| self.on_failure.get(&kind).copied())
        .unwrap_or_else(|| kind.default_on_failure());
        declared.max(raise.unwrap_or(OnFailure::Allow))
    }

    /// Lowercase names of the sensitive commands.
    pub fn sensitive(&self) -> impl Iterator<Item = String> + '_ {
        self.sensitive_commands
            .iter()
            .map(|c| c.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTH: &str = r#"
id: pumbo-auth
version: 0.1.0
api: "0.1"
events: [pre-login, profile, gate, command, virtual-input, timer, disconnect]
http: [api.mojang.com, api.minecraftservices.com]
gate: { name: auth, priority: 200, on-failure: deny }
profile: { priority: 100, on-failure: deny }
sensitive-commands: [login, L, register]
provides: [{ service: "pumboauth:accounts", version: "1.0", max-timeout-ms: 500 }]
uses: [{ service: "pumbobans:punish", version: "1.0" }]
publishes: ["pumbo:player-authenticated@1.0"]
short-name: auth
short-alias: pa
on-failure: { chat: deny }

placeholders:
  namespace: pumboauth
  keys:
    - { name: premium, scope: player, mode: push, public: true, fallback: "?" }
    - { name: registered, scope: player, mode: pull, public: true, ttl-ms: 60000 }
  suggested-aliases: { premium: pumboauth_premium }
"#;

    #[test]
    fn parses_the_plan_example() {
        let m = Manifest::parse(AUTH).unwrap();
        assert_eq!(m.id, "pumbo-auth");
        assert_eq!(m.gate.as_ref().unwrap().priority, 200);
        assert_eq!(
            m.sensitive().collect::<Vec<_>>(),
            ["login", "l", "register"]
        );
        let ph = m.placeholders.as_ref().unwrap();
        assert_eq!(ph.keys[1].mode, KeyMode::Pull);
        assert_eq!(
            split_topic(&m.publishes[0]),
            Some(("pumbo:player-authenticated", 1, 0))
        );
    }

    #[test]
    fn on_failure_defaults_and_raising() {
        let m = Manifest::parse(AUTH).unwrap();
        assert_eq!(m.on_failure(EventKind::Gate, None), OnFailure::Deny);
        assert_eq!(m.on_failure(EventKind::PreLogin, None), OnFailure::Deny);
        assert_eq!(
            m.on_failure(EventKind::ServerConnect, None),
            OnFailure::Allow
        );
        assert_eq!(m.on_failure(EventKind::Chat, None), OnFailure::Deny);
        // The proxy config raises, never lowers.
        assert_eq!(
            m.on_failure(EventKind::ServerConnect, Some(OnFailure::Deny)),
            OnFailure::Deny
        );
        assert_eq!(
            m.on_failure(EventKind::Gate, Some(OnFailure::Allow)),
            OnFailure::Deny
        );
    }

    #[test]
    fn rejects_bad_manifests() {
        for (needle, replacement) in [
            ("id: pumbo-auth", "id: Pumbo Auth"),
            ("api: \"0.1\"", "api: \"0.2\""),
            ("short-name: auth", "short-name: proxy"),
            ("short-alias: pa", "short-alias: server"),
            ("short-alias: pa", "short-alias: pumbox"),
            ("namespace: pumboauth", "namespace: pumbo_auth"),
            ("@1.0\"]", "\"]"),
            ("gate: { name", "gatex: { name"),
            // `version` is text even when it looks like a number
            ("version: 0.1.0", "version: [0, 1]"),
        ] {
            let text = AUTH.replace(needle, replacement);
            assert!(Manifest::parse(&text).is_err(), "{needle} -> {replacement}");
        }
        let no_gate_event = AUTH.replace("gate, ", "");
        assert!(Manifest::parse(&no_gate_event).is_err());
    }

    #[test]
    fn manifest_changes() {
        let new = AUTH
            .replace("version: 0.1.0", "version: 0.2.0")
            .replace(
                "sensitive-commands: [login, L, register]",
                "sensitive-commands: [login]",
            )
            .replace("short-name: auth\n", "");
        assert_eq!(
            changes(AUTH, &new),
            ["sensitive-commands", "short-name", "version 0.1.0 -> 0.2.0"]
        );
        assert!(changes(AUTH, AUTH).is_empty());
    }

    #[test]
    fn language_file_names() {
        assert_eq!(lang_file("lang/pl.yml"), Some("pl.yml"));
        assert_eq!(lang_file("assets/lang/en_us.yml"), Some("en_us.yml"));
        for bad in [
            "../../x.toml",
            "lang/.yml",
            "lang/a.b.yml",
            "..\\x.yml",
            "pl",
        ] {
            assert_eq!(lang_file(bad), None, "{bad}");
        }
    }

    #[test]
    fn missing_options_of_a_newer_version() {
        let template = "# header\na: 1\nb:\n  c: 2 # note\n  d: [x]\ne:\n  f: 3\nlist: [1]\n";
        assert!(missing_options(template, template).is_empty());
        assert_eq!(
            missing_options(template, "a: 5\nb:\n  c: 7\nlist: []\nown: 1\n"),
            ["b.d", "e"]
        );
        assert_eq!(missing_options(template, ""), ["a", "b", "e", "list"]);
        // Broken YAML is the plugin's to report, with the line.
        assert!(missing_options(template, "a: [\n").is_empty());
    }

    #[test]
    fn options_left_at_the_previous_default() {
        let old = "a: 1\nb:\n  c: x\n  d: [1]\ne: 5\n";
        let new = "a: 2\nb:\n  c: y\n  d: [1, 2]\ne: 5\nf: 1\n";
        assert_eq!(stale_options(new, old, old), ["a", "b.c", "b.d"]);
        // Changed by the admin, or gone from the file: not stale.
        assert_eq!(stale_options(new, old, "a: 9\nb:\n  c: x\n"), ["b.c"]);
        assert!(stale_options(new, new, old).is_empty());
        assert!(stale_options(new, "a: [\n", old).is_empty());
    }
}
