//! `permissions.yml`: the file source of the permission table.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use pumbo_core::permissions::{PermContext, PermissionEntry, PermissionSource, Subject};
use serde::Deserialize;
use uuid::Uuid;

use crate::config::HostConfig;

/// `permissions.yml` (plan §5.8.4), flattened to entries with contexts.
#[derive(Debug, Default, Clone)]
pub struct FileSource {
    groups: BTreeMap<String, Section>,
    players: Vec<(Key, Section)>,
    /// `server-group` from the proxy config: group → servers.
    server_groups: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Uuid(Uuid),
    Name(String),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    inherits: Vec<String>,
    /// Only for the reader of the file (and the PumboPerms export).
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    server: BTreeMap<String, Section>,
    #[serde(default)]
    group: BTreeMap<String, Section>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDoc {
    #[serde(default)]
    groups: BTreeMap<String, Section>,
    #[serde(default)]
    players: BTreeMap<String, Section>,
}

/// Group every player has, if the file defines it.
const DEFAULT_GROUP: &str = "default";
const MAX_INHERIT_DEPTH: usize = 16;

impl FileSource {
    /// Parses the file; returns warnings (unknown servers and groups in
    /// contexts are warnings, not errors, plan §5.8.4).
    pub fn parse(text: &str, cfg: &HostConfig) -> Result<(FileSource, Vec<String>), String> {
        let doc: FileDoc = pumbo_core::yaml::from_str(text)?;
        let mut players = Vec::new();
        for (k, v) in doc.players {
            let key = match Uuid::parse_str(&k) {
                Ok(u) => Key::Uuid(u),
                Err(_) => Key::Name(k.to_ascii_lowercase()),
            };
            players.push((key, v));
        }
        let src = FileSource {
            groups: doc.groups,
            players,
            server_groups: cfg
                .server_groups
                .iter()
                .map(|g| {
                    let servers = g.servers.iter().map(|s| s.to_ascii_lowercase()).collect();
                    (g.name.to_ascii_lowercase(), servers)
                })
                .collect(),
        };
        let mut warnings = Vec::new();
        let known_groups: BTreeSet<&str> =
            src.server_groups.iter().map(|(g, _)| g.as_str()).collect();
        let known_servers: BTreeSet<String> =
            cfg.servers.keys().map(|s| s.to_ascii_lowercase()).collect();
        let mut check = |what: &str, s: &Section| {
            for (srv, sub) in &s.server {
                if !known_servers.is_empty() && !known_servers.contains(&srv.to_ascii_lowercase()) {
                    warnings.push(format!("{what}: unknown server \"{srv}\""));
                }
                if !sub.server.is_empty() || !sub.group.is_empty() {
                    warnings.push(format!(
                        "{what}: nested contexts under server \"{srv}\" are ignored"
                    ));
                }
            }
            for g in s.group.keys() {
                if !known_groups.contains(g.to_ascii_lowercase().as_str()) {
                    warnings.push(format!("{what}: unknown server group \"{g}\""));
                }
            }
            for g in s.groups.iter().chain(&s.inherits) {
                if !src.groups.contains_key(g) {
                    warnings.push(format!("{what}: unknown permission group \"{g}\""));
                }
            }
        };
        for (n, g) in &src.groups {
            check(&format!("group {n}"), g);
        }
        for (k, p) in &src.players {
            check(&format!("player {k:?}"), p);
        }
        Ok((src, warnings))
    }

    pub fn load(path: &Path, cfg: &HostConfig) -> Result<(FileSource, Vec<String>), String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, cfg).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let old = path.with_extension("toml");
                let warnings = if old.is_file() {
                    vec![format!(
                        "found {}, this version reads {} - convert it to YAML with the same keys (see https://github.com/PumboMC/PumboProx)",
                        old.display(),
                        path.display()
                    )]
                } else {
                    Vec::new()
                };
                Ok((FileSource::default(), warnings))
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Flattened entries of a subject in every context.
    pub fn entries_for(&self, who: &Subject) -> Vec<PermissionEntry> {
        let name = who.name.to_ascii_lowercase();
        let mut out = Vec::new();
        let mut groups_seen = false;
        for (key, section) in &self.players {
            let hit = match key {
                Key::Uuid(u) => *u == who.uuid,
                Key::Name(n) => *n == name,
            };
            if hit {
                groups_seen |= !section.groups.is_empty();
                self.flatten(section, &PermContext::Global, 0, &mut out);
            }
        }
        if !groups_seen && let Some(g) = self.groups.get(DEFAULT_GROUP) {
            self.flatten(g, &PermContext::Global, 0, &mut out);
        }
        out
    }

    /// Adds the entries of a section applied in context `at`. Own entries
    /// come first; an inherited entry is skipped when the section already has
    /// the same node in the same context (override).
    fn flatten(&self, s: &Section, at: &PermContext, depth: usize, out: &mut Vec<PermissionEntry>) {
        if depth > MAX_INHERIT_DEPTH {
            return;
        }
        let mut own = Vec::new();
        let push_nodes = |nodes: &[String], ctx: &PermContext, own: &mut Vec<PermissionEntry>| {
            for raw in nodes {
                let (value, node) = match raw.strip_prefix('-') {
                    Some(n) => (false, n),
                    None => (true, raw.as_str()),
                };
                own.push(PermissionEntry {
                    node: node.to_ascii_lowercase(),
                    value,
                    context: ctx.clone(),
                });
            }
        };
        push_nodes(&s.permissions, at, &mut own);
        for (srv, sub) in &s.server {
            for ctx in self.narrow(at, &PermContext::server(srv)) {
                push_nodes(&sub.permissions, &ctx, &mut own);
            }
        }
        for (g, sub) in &s.group {
            for ctx in self.narrow(at, &PermContext::group(g)) {
                push_nodes(&sub.permissions, &ctx, &mut own);
            }
        }
        let mut inherited = Vec::new();
        let mut members: Vec<(String, PermContext)> = s
            .groups
            .iter()
            .chain(&s.inherits)
            .map(|g| (g.clone(), at.clone()))
            .collect();
        for (srv, sub) in &s.server {
            for ctx in self.narrow(at, &PermContext::server(srv)) {
                members.extend(sub.groups.iter().map(|g| (g.clone(), ctx.clone())));
            }
        }
        for (grp, sub) in &s.group {
            for ctx in self.narrow(at, &PermContext::group(grp)) {
                members.extend(sub.groups.iter().map(|g| (g.clone(), ctx.clone())));
            }
        }
        for (g, ctx) in members {
            if let Some(section) = self.groups.get(&g) {
                self.flatten(section, &ctx, depth + 1, &mut inherited);
            }
        }
        inherited.retain(|e| {
            !own.iter()
                .any(|o| o.node == e.node && o.context == e.context)
        });
        out.extend(own);
        out.extend(inherited);
    }
}

impl FileSource {
    /// The file in one server's context as a PumboPerms export (JSON, format
    /// `pumboperms` 1, PumboBridge spec §5.3): what applies on `server` is
    /// merged into global entries, the more specific context winning (server,
    /// then `groups` in config order, then global). Proxy command nodes
    /// (`pumbo.…`) stay out; players named only by nickname too (PumboPerms
    /// keys users by UUID).
    pub fn export(&self, server: &str, groups: &[String]) -> serde_json::Value {
        let data = |s: &Section| {
            let mut nodes: BTreeMap<String, bool> = BTreeMap::new();
            let mut parents: Vec<String> = Vec::new();
            let mut apply = |sec: &Section| {
                for raw in &sec.permissions {
                    let (value, node) = match raw.strip_prefix('-') {
                        Some(n) => (false, n),
                        None => (true, raw.as_str()),
                    };
                    let node = node.to_ascii_lowercase();
                    if !node.starts_with("pumbo.") {
                        nodes.insert(node, value);
                    }
                }
                for g in sec.groups.iter().chain(&sec.inherits) {
                    if !parents.contains(g) {
                        parents.push(g.clone());
                    }
                }
            };
            apply(s);
            for g in groups.iter().rev() {
                named(&s.group, g).for_each(&mut apply);
            }
            named(&s.server, server).for_each(&mut apply);
            serde_json::json!({
                "permissions": nodes.into_iter().map(|(node, value)| serde_json::json!({"node": node, "value": value})).collect::<Vec<_>>(),
                "parents": parents.into_iter().map(|group| serde_json::json!({"group": group})).collect::<Vec<_>>(),
            })
        };
        let groups_json: Vec<_> = self
            .groups
            .iter()
            .map(|(name, s)| serde_json::json!({"name": name, "data": data(s)}))
            .collect();
        let users: Vec<_> = self
            .players
            .iter()
            .filter_map(|(k, s)| match k {
                Key::Uuid(u) => Some(serde_json::json!({
                    "uuid": u.hyphenated().to_string(),
                    "name": s.name.clone().unwrap_or_default(),
                    "data": data(s),
                })),
                Key::Name(_) => None,
            })
            .collect();
        serde_json::json!({"format": "pumboperms", "version": 1, "groups": groups_json, "users": users})
    }

    /// The whole file as a PumboPerms export with its contexts (`server=`,
    /// `group=`) and players named by nickname under `by-name`: what the
    /// permission provider takes over (method `file`, PumboPerms spec §16).
    pub fn snapshot(&self) -> serde_json::Value {
        use serde_json::json;
        let data = |s: &Section| {
            let mut permissions = Vec::new();
            let mut parents = Vec::new();
            let mut add = |sec: &Section, context: serde_json::Value| {
                for raw in &sec.permissions {
                    let (value, node) = match raw.strip_prefix('-') {
                        Some(n) => (false, n),
                        None => (true, raw.as_str()),
                    };
                    permissions.push(json!({"node": node.to_ascii_lowercase(), "value": value, "context": context}));
                }
                for g in sec.groups.iter().chain(&sec.inherits) {
                    parents.push(json!({"group": g.to_ascii_lowercase(), "context": context}));
                }
            };
            add(s, json!({}));
            for (srv, sub) in &s.server {
                add(sub, json!({"server": srv.to_ascii_lowercase()}));
            }
            for (g, sub) in &s.group {
                add(sub, json!({"group": g.to_ascii_lowercase()}));
            }
            json!({"permissions": permissions, "parents": parents})
        };
        let groups: Vec<_> = self
            .groups
            .iter()
            .map(|(name, s)| json!({"name": name.to_ascii_lowercase(), "data": data(s)}))
            .collect();
        let mut users = Vec::new();
        let mut by_name = Vec::new();
        for (k, s) in &self.players {
            match k {
                Key::Uuid(u) => users.push(json!({
                    "uuid": u.hyphenated().to_string(),
                    "name": s.name.clone().unwrap_or_default(),
                    "data": data(s),
                })),
                Key::Name(n) => by_name.push(json!({"name": n, "data": data(s)})),
            }
        }
        json!({"format": "pumboperms", "version": 1, "groups": groups, "users": users, "by-name": by_name})
    }

    fn in_group(&self, server: &str, group: &str) -> bool {
        self.server_groups
            .iter()
            .any(|(g, servers)| g == group && servers.iter().any(|s| s == server))
    }

    /// Intersection of a membership context with an entry context, as the
    /// contexts where both hold.
    fn narrow(&self, outer: &PermContext, inner: &PermContext) -> Vec<PermContext> {
        match (outer, inner) {
            (PermContext::Global, c) | (c, PermContext::Global) => vec![c.clone()],
            (a, b) if a == b => vec![a.clone()],
            (PermContext::Server(_), PermContext::Server(_)) => Vec::new(),
            (PermContext::Server(s), PermContext::Group(g))
            | (PermContext::Group(g), PermContext::Server(s)) => {
                if self.in_group(s, g) {
                    vec![PermContext::Server(s.clone())]
                } else {
                    Vec::new()
                }
            }
            (PermContext::Group(a), PermContext::Group(b)) => self
                .server_groups
                .iter()
                .filter(|(g, _)| g == a)
                .flat_map(|(_, servers)| servers.iter())
                .filter(|s| self.in_group(s, b))
                .map(|s| PermContext::Server(s.clone()))
                .collect(),
        }
    }
}

/// Sections of a context map whose name matches regardless of case.
fn named<'a>(m: &'a BTreeMap<String, Section>, name: &'a str) -> impl Iterator<Item = &'a Section> {
    m.iter()
        .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// The `file` source (the host reads it synchronously at join, through
/// `entries_for`; the trait is the shape for further native sources).
impl PermissionSource for FileSource {
    fn name(&self) -> &str {
        "file"
    }

    fn load<'a>(
        &'a self,
        who: &'a Subject,
    ) -> pumbo_core::BoxFuture<'a, Result<Vec<PermissionEntry>, String>> {
        let r = self.entries_for(who);
        Box::pin(async move { Ok(r) })
    }
}
