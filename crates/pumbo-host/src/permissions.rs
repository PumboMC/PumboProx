//! Permissions as host data (plan §4.4 item 4, §5.8.4). A check reads the
//! table and never calls a plugin.
//!
//! Resolution: levels server > group (config order) > global; the first level
//! with a matching entry decides. Within a level: an exact node before the
//! longest matching `a.b.*`, before `*`; on a tie the file wins over the
//! provider (the administrator has the last word) and `false` wins over `true`.
//! Resolution order modelled on the public LuckPerms documentation (MIT).
//!
//! Once the permission provider (PumboPerms) has loaded a player, its set
//! replaces the file for that player (PumboPerms spec §16: the provider took
//! the file over at its first start). While the provider is not running, the
//! file decides again.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::RwLock;

use pumbo_core::permissions::{PermContext, PermissionEntry, PermissionSource, Subject};
use serde::Deserialize;
use uuid::Uuid;

use crate::config::HostConfig;
use crate::wit::types::PlayerId;

/// Where an entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    File,
    Provider,
}

/// Levels of a query, highest priority first.
pub fn levels(server: Option<&str>, groups: &[String]) -> Vec<PermContext> {
    let mut out = Vec::with_capacity(groups.len() + 2);
    if let Some(s) = server {
        out.push(PermContext::Server(s.to_string()));
    }
    out.extend(groups.iter().map(|g| PermContext::Group(g.clone())));
    out.push(PermContext::Global);
    out
}

/// How specific an entry is for `node`; `None` if it does not match.
fn specificity(entry: &str, node: &str) -> Option<usize> {
    if entry == node {
        return Some(usize::MAX);
    }
    if entry == "*" {
        return Some(0);
    }
    // `minecraft:*`: everything in a namespace.
    if let Some(ns) = entry.strip_suffix(":*") {
        let rest = node.strip_prefix(ns)?;
        return rest.starts_with(':').then_some(ns.len() + 1);
    }
    let prefix = entry.strip_suffix(".*")?;
    let rest = node.strip_prefix(prefix)?;
    rest.starts_with('.').then_some(prefix.len() + 1)
}

/// The decision and the entry that made it (for `/pumbo proxy perms check`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub value: bool,
    pub entry: PermissionEntry,
    pub layer: Layer,
}

pub fn resolve(
    file: &[PermissionEntry],
    provider: &[PermissionEntry],
    node: &str,
    levels: &[PermContext],
) -> Option<Decision> {
    let node = node.to_ascii_lowercase();
    for level in levels {
        let mut best: Option<(usize, Layer, &PermissionEntry)> = None;
        let candidates = file
            .iter()
            .map(|e| (Layer::File, e))
            .chain(provider.iter().map(|e| (Layer::Provider, e)));
        for (layer, e) in candidates {
            if e.context != *level {
                continue;
            }
            let Some(spec) = specificity(&e.node, &node) else {
                continue;
            };
            let better = match best {
                None => true,
                Some((bs, bl, be)) => {
                    spec > bs
                        || (spec == bs
                            && ((layer == Layer::File && bl == Layer::Provider)
                                || (layer == bl && !e.value && be.value)))
                }
            };
            if better {
                best = Some((spec, layer, e));
            }
        }
        if let Some((_, layer, e)) = best {
            return Some(Decision {
                value: e.value,
                entry: e.clone(),
                layer,
            });
        }
    }
    None
}

/// Permissions of one online player.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlayerPerms {
    pub file: Vec<PermissionEntry>,
    pub provider: Vec<PermissionEntry>,
    /// The provider loaded this player: its set replaces the file.
    pub loaded: bool,
    /// Past the point of loading (`load-at`): loaded again when the
    /// provider comes back.
    pub eligible: bool,
}

impl PlayerPerms {
    /// The layers that decide: the provider's set alone once it is loaded,
    /// otherwise the file (with session changes from `permissions.set`).
    pub fn layers(&self) -> (&[PermissionEntry], &[PermissionEntry]) {
        if self.loaded {
            (&[], &self.provider)
        } else {
            (&self.file, &self.provider)
        }
    }
}

/// The host table.
#[derive(Debug, Default)]
pub(crate) struct Permissions {
    pub file: RwLock<FileSource>,
    pub online: RwLock<HashMap<PlayerId, PlayerPerms>>,
}

impl Permissions {
    pub fn decide(&self, id: PlayerId, node: &str, levels: &[PermContext]) -> Option<Decision> {
        let online = self.online.read().ok()?;
        let (file, provider) = online.get(&id)?.layers();
        resolve(file, provider, node, levels)
    }

    pub fn load_file_layer(&self, id: PlayerId, who: &Subject) {
        let entries = self
            .file
            .read()
            .map(|f| f.entries_for(who))
            .unwrap_or_default();
        if let Ok(mut o) = self.online.write() {
            o.entry(id).or_default().file = entries;
        }
    }

    pub fn replace_provider_layer(&self, id: PlayerId, entries: Vec<PermissionEntry>) -> bool {
        match self.online.write() {
            Ok(mut o) => match o.get_mut(&id) {
                Some(p) => {
                    p.provider = entries;
                    p.loaded = true;
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }

    /// One node for the session (`permissions.set`); `None` removes it.
    pub fn set_node(
        &self,
        id: PlayerId,
        node: &str,
        value: Option<bool>,
        ctx: PermContext,
    ) -> bool {
        let Ok(mut o) = self.online.write() else {
            return false;
        };
        let Some(p) = o.get_mut(&id) else {
            return false;
        };
        let node = node.to_ascii_lowercase();
        p.provider.retain(|e| !(e.node == node && e.context == ctx));
        if let Some(value) = value {
            p.provider.push(PermissionEntry {
                node,
                value,
                context: ctx,
            });
        }
        true
    }

    /// Marks the player as due for the provider; false for an unknown one.
    pub fn mark_eligible(&self, id: PlayerId) -> bool {
        self.online
            .write()
            .ok()
            .and_then(|mut o| o.get_mut(&id).map(|p| p.eligible = true))
            .is_some()
    }

    pub fn eligible(&self) -> Vec<PlayerId> {
        self.online
            .read()
            .map(|o| {
                o.iter()
                    .filter(|(_, p)| p.eligible)
                    .map(|(id, _)| *id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The provider went down: every player is back on the file. Returns
    /// the players whose permissions change.
    pub fn drop_provider(&self) -> Vec<PlayerId> {
        let Ok(mut o) = self.online.write() else {
            return Vec::new();
        };
        o.iter_mut()
            .filter(|(_, p)| p.loaded)
            .map(|(id, p)| {
                p.loaded = false;
                p.provider.clear();
                *id
            })
            .collect()
    }

    pub fn remove(&self, id: PlayerId) {
        if let Ok(mut o) = self.online.write() {
            o.remove(&id);
        }
    }
}

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
                .map(|g| (g.name.clone(), g.servers.clone()))
                .collect(),
        };
        let mut warnings = Vec::new();
        let known_groups: BTreeSet<&str> =
            cfg.server_groups.iter().map(|g| g.name.as_str()).collect();
        let known_servers: BTreeSet<&str> = cfg.servers.keys().map(String::as_str).collect();
        let mut check = |what: &str, s: &Section| {
            for (srv, sub) in &s.server {
                if !known_servers.is_empty() && !known_servers.contains(srv.as_str()) {
                    warnings.push(format!("{what}: unknown server \"{srv}\""));
                }
                if !sub.server.is_empty() || !sub.group.is_empty() {
                    warnings.push(format!(
                        "{what}: nested contexts under server \"{srv}\" are ignored"
                    ));
                }
            }
            for g in s.group.keys() {
                if !known_groups.contains(g.as_str()) {
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
            for ctx in self.narrow(at, &PermContext::Server(srv.clone())) {
                push_nodes(&sub.permissions, &ctx, &mut own);
            }
        }
        for (g, sub) in &s.group {
            for ctx in self.narrow(at, &PermContext::Group(g.clone())) {
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
            for ctx in self.narrow(at, &PermContext::Server(srv.clone())) {
                members.extend(sub.groups.iter().map(|g| (g.clone(), ctx.clone())));
            }
        }
        for (grp, sub) in &s.group {
            for ctx in self.narrow(at, &PermContext::Group(grp.clone())) {
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
                if let Some(sub) = s.group.get(g) {
                    apply(sub);
                }
            }
            if let Some(sub) = s.server.get(server) {
                apply(sub);
            }
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

/// The `plugin` source: `on-permission-load` of the provider (plan §5.8.4).
pub(crate) struct PluginSource<'h> {
    pub host: &'h crate::HostInner,
    pub slot: std::sync::Arc<crate::actor::PluginSlot>,
}

impl std::fmt::Debug for PluginSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginSource")
            .field("plugin", &self.slot.id)
            .finish()
    }
}

impl PermissionSource for PluginSource<'_> {
    fn name(&self) -> &str {
        &self.slot.id
    }

    fn load<'a>(
        &'a self,
        who: &'a Subject,
    ) -> pumbo_core::BoxFuture<'a, Result<Vec<PermissionEntry>, String>> {
        Box::pin(async move {
            let host = self.host;
            let info = host
                .players
                .read()
                .ok()
                .and_then(|p| {
                    p.values()
                        .find(|i| crate::uuid_of(&i.profile.id) == who.uuid)
                        .cloned()
                })
                .ok_or("player not online")?;
            let cfg = &host.cfg;
            let wait = std::time::Duration::from_millis(cfg.services.restart_wait_ms);
            let deadline = std::time::Duration::from_millis(cfg.permissions.load_timeout_ms);
            let set = self
                .slot
                .call(wait, deadline, move |acc, g| {
                    Box::pin(async move { g.call_on_permission_load(acc, info).await })
                })
                .await
                .map_err(|e| e.to_string())??;
            Ok(crate::dispatch::provider_entries(set))
        })
    }
}

impl crate::HostInner {
    pub(crate) fn is_permission_provider(&self, slot: &crate::actor::PluginSlot) -> bool {
        self.perm_provider.as_deref() == Some(slot.id.as_str())
    }

    /// The provider while it runs or is about to run again; `None`: the
    /// file decides.
    pub(crate) fn provider_slot(&self) -> Option<std::sync::Arc<crate::actor::PluginSlot>> {
        let slot = self.plugins.get(self.perm_provider.as_deref()?)?;
        slot.status()
            .is_coming()
            .then(|| std::sync::Arc::clone(slot))
    }

    /// Warns (at most once a minute) that the file decides while the
    /// provider is down.
    pub(crate) fn warn_file_fallback(&self, why: &str) {
        let Some(provider) = self.perm_provider.as_deref() else {
            return;
        };
        if let Ok(mut last) = self.provider_down_logged.lock()
            && last.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60))
        {
            *last = Some(std::time::Instant::now());
            tracing::warn!(
                "permission provider {provider} {why}: permissions from {} until it is back",
                self.cfg.permissions.file.display()
            );
        }
    }

    /// Calls a method of the provider's `pumbo:permissions` service.
    pub(crate) async fn call_provider<A: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        req: &impl serde::Serialize,
        timeout_ms: u32,
    ) -> Result<A, String> {
        let mut payload = Vec::new();
        ciborium::into_writer(req, &mut payload).map_err(|e| e.to_string())?;
        let c = pumbo_contracts::PERMISSIONS;
        let opts = crate::wit::services::CallOptions {
            timeout_ms: Some(timeout_ms),
            player: None,
            ctx: None,
        };
        let out = self
            .call_service(
                crate::services::HOST_CALLER,
                c.name,
                c.major,
                c.minor,
                method.into(),
                payload,
                opts,
            )
            .await
            .map_err(|e| format!("{e:?}"))?;
        ciborium::from_reader(out.as_slice()).map_err(|e| format!("answer: {e}"))
    }

    /// The provider (re)started: it gets `permissions.yml` to take over,
    /// then the players online are loaded from it again.
    pub(crate) async fn provider_started(&self) {
        let data = self
            .perms
            .file
            .read()
            .map(|f| f.snapshot().to_string())
            .unwrap_or_default();
        let file = pumbo_contracts::PermissionsFile {
            fingerprint: fnv_hex(&data),
            data,
        };
        if let Err(e) = self
            .call_provider::<bool>(pumbo_contracts::METHOD_FILE, &file, 10_000)
            .await
        {
            tracing::warn!("permissions.yml not handed to the permission provider: {e}");
        }
        for id in self.perms.eligible() {
            if let Err(t) = self.load_provider_permissions(id).await {
                tracing::warn!(player = id, "permissions not reloaded: {}", t.plain_text());
            }
        }
    }

    /// Loads the provider layer of a player (plan §5.8.4, `load-at`);
    /// with `on-load-failure: deny` a failure keeps the player out. Without
    /// a running provider the file decides.
    pub(crate) async fn load_provider_permissions(
        &self,
        id: crate::wit::types::PlayerId,
    ) -> Result<(), pumbo_text::Component> {
        let Some(provider) = self.perm_provider.as_deref() else {
            return Ok(());
        };
        self.perms.mark_eligible(id);
        let Some(slot) = self.provider_slot() else {
            self.warn_file_fallback("is not running");
            return Ok(());
        };
        let Some(info) = self.player(id) else {
            return Err(self.message(&self.cfg.plugins.messages.permissions_unavailable));
        };
        let who = crate::subject(&info);
        let source = PluginSource { host: self, slot };
        match source.load(&who).await {
            Ok(entries) => {
                self.perms.replace_provider_layer(id, entries);
                self.bridge
                    .send(id, crate::bridge::PlayerCommand::CommandsChanged);
                Ok(())
            }
            Err(e) => {
                tracing::warn!(player = id, provider = %provider, "permissions not loaded: {e}");
                match self.cfg.permissions.on_load_failure {
                    crate::config::OnLoadFailure::Deny => {
                        Err(self.message(&self.cfg.plugins.messages.permissions_unavailable))
                    }
                    crate::config::OnLoadFailure::FileOnly => Ok(()),
                }
            }
        }
    }
}

/// FNV-1a 64 of a text, hex: tells a changed `permissions.yml` from the
/// same one across restarts.
fn fnv_hex(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(node: &str, value: bool, ctx: PermContext) -> PermissionEntry {
        PermissionEntry {
            node: node.into(),
            value,
            context: ctx,
        }
    }

    fn srv(s: &str) -> PermContext {
        PermContext::Server(s.into())
    }

    fn grp(s: &str) -> PermContext {
        PermContext::Group(s.into())
    }

    use PermContext::Global;

    /// The case table from plan §5.8.4.
    #[test]
    fn resolution_cases() {
        let lv = levels(Some("survival"), &["survivals".into(), "all".into()]);
        let lobby = levels(Some("lobby"), &["all".into()]);
        type Case<'a> = (
            &'a str,
            Vec<PermissionEntry>,
            Vec<PermissionEntry>,
            &'a str,
            &'a [PermContext],
            Option<bool>,
        );
        let cases: Vec<Case> = vec![
            ("nothing", vec![], vec![], "a.b", &lv, None),
            (
                "global",
                vec![e("a.b", true, Global)],
                vec![],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "server beats global",
                vec![e("a.b", true, Global), e("a.b", false, srv("survival"))],
                vec![],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "other server ignored",
                vec![e("a.b", false, srv("lobby")), e("a.b", true, Global)],
                vec![],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "group beats global",
                vec![e("a.b", true, Global), e("a.b", false, grp("survivals"))],
                vec![],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "first group wins",
                vec![
                    e("a.b", true, grp("all")),
                    e("a.b", false, grp("survivals")),
                ],
                vec![],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "server beats group",
                vec![
                    e("a.b", false, grp("survivals")),
                    e("a.b", true, srv("survival")),
                ],
                vec![],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "exact beats wildcard",
                vec![e("a.*", false, Global), e("a.b", true, Global)],
                vec![],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "longest wildcard",
                vec![e("a.*", false, Global), e("a.b.*", true, Global)],
                vec![],
                "a.b.c",
                &lv,
                Some(true),
            ),
            (
                "star",
                vec![e("*", true, Global)],
                vec![],
                "x.y",
                &lv,
                Some(true),
            ),
            (
                "wildcard does not match its base",
                vec![e("a.b.*", true, Global)],
                vec![],
                "a.b",
                &lv,
                None,
            ),
            (
                "false wins a tie",
                vec![e("a.b", true, Global), e("a.b", false, Global)],
                vec![],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "file wins over provider",
                vec![e("a.b", true, Global)],
                vec![e("a.b", false, Global)],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "provider fills in",
                vec![],
                vec![e("a.b", true, srv("survival"))],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "level before layer",
                vec![e("a.b", true, Global)],
                vec![e("a.b", false, srv("survival"))],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "lobby sees global",
                vec![e("a.b", true, Global)],
                vec![e("a.b", false, srv("survival"))],
                "a.b",
                &lobby,
                Some(true),
            ),
            (
                "case-insensitive",
                vec![e("a.b", true, Global)],
                vec![],
                "A.B",
                &lv,
                Some(true),
            ),
        ];
        for (name, file, provider, node, levels, want) in cases {
            let got = resolve(&file, &provider, node, levels).map(|d| d.value);
            assert_eq!(got, want, "{name}");
        }
    }

    const FILE: &str = r#"
groups:
  default:
    permissions: [pumbo.proxy.server]
    group:
      minigames: { permissions: ["-pumbo.proxy.server"] }
  vip:
    inherits: [default]
    permissions: [pumbo.proxy.glist, "-pumbo.proxy.alert"]
    server:
      survival: { permissions: [pumbo.skins.change] }
  mod:
    inherits: [vip]
    permissions: [pumbo.proxy.alert]

players:
  069a79f4-44e9-4726-a5be-fca90e38aaf5:
    name: Notch
    groups: [default]
    server:
      survival: { groups: [vip] }
  jeb_:
    groups: [mod]
"#;

    fn cfg() -> HostConfig {
        HostConfig::parse(
            r#"
servers:
  survival: { address: x }
  lobby: { address: y }
server-group:
  - name: minigames
    servers: [bedwars]
"#,
        )
        .unwrap()
    }

    #[test]
    fn file_source_flattening() {
        let (src, warnings) = FileSource::parse(FILE, &cfg()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let notch = Subject {
            uuid: Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap(),
            name: "Notch".into(),
        };
        let entries = src.entries_for(&notch);
        let has = |node: &str, server: &str, groups: &[&str]| {
            let groups: Vec<String> = groups.iter().map(|g| g.to_string()).collect();
            resolve(&entries, &[], node, &levels(Some(server), &groups)).map(|d| d.value)
        };
        assert_eq!(has("pumbo.proxy.server", "lobby", &[]), Some(true));
        assert_eq!(
            has("pumbo.proxy.server", "bedwars", &["minigames"]),
            Some(false)
        );
        // VIP only on survival.
        assert_eq!(has("pumbo.proxy.glist", "survival", &[]), Some(true));
        assert_eq!(has("pumbo.proxy.glist", "lobby", &[]), None);
        assert_eq!(has("pumbo.skins.change", "survival", &[]), Some(true));
        // default's `group.minigames` entry inherited through vip on survival
        // does not apply: survival is not in minigames.
        assert_eq!(has("pumbo.proxy.server", "survival", &[]), Some(true));

        // Inherited `-pumbo.proxy.alert` of vip is overridden by mod's own entry.
        let jeb = Subject {
            uuid: Uuid::nil(),
            name: "Jeb_".into(),
        };
        let entries = src.entries_for(&jeb);
        let r = resolve(&entries, &[], "pumbo.proxy.alert", &levels(None, &[])).map(|d| d.value);
        assert_eq!(r, Some(true));
        let r = resolve(&entries, &[], "pumbo.proxy.server", &levels(None, &[])).map(|d| d.value);
        assert_eq!(r, Some(true));

        // Unlisted players get the default group.
        let other = Subject {
            uuid: Uuid::nil(),
            name: "someone".into(),
        };
        let entries = src.entries_for(&other);
        let r = resolve(&entries, &[], "pumbo.proxy.server", &levels(None, &[])).map(|d| d.value);
        assert_eq!(r, Some(true));
    }

    #[test]
    fn namespace_wildcard_and_bridge_export() {
        let all = [e("minecraft:*", true, PermContext::Global)];
        let lv = levels(Some("survival"), &[]);
        assert_eq!(
            resolve(&all, &[], "minecraft:command.gamemode", &lv).map(|d| d.value),
            Some(true)
        );
        assert_eq!(resolve(&all, &[], "minecraftx:a", &lv), None);
        let text = r#"
groups:
  default:
    permissions: [pumbo.proxy.server, "minecraft:command.help"]
  builder:
    inherits: [default]
    permissions: ["-minecraft:command.gamemode"]
    server:
      survival: { permissions: ["minecraft:command.gamemode"] }
players:
  069a79f4-44e9-4726-a5be-fca90e38aaf5:
    name: Notch
    server:
      survival: { groups: [builder] }
  Jeb_:
    groups: [builder]
"#;
        let (src, _) = FileSource::parse(text, &cfg()).unwrap();
        let x = src.export("survival", &[]);
        assert_eq!(x["format"], "pumboperms");
        let builder = &x["groups"][0];
        assert_eq!(builder["name"], "builder");
        assert_eq!(
            builder["data"]["permissions"][0],
            serde_json::json!({"node": "minecraft:command.gamemode", "value": true}),
            "the server context wins over global"
        );
        assert_eq!(builder["data"]["parents"][0]["group"], "default");
        assert_eq!(
            x["groups"][1]["data"]["permissions"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "pumbo.* nodes stay on the proxy"
        );
        let users = x["users"].as_array().unwrap();
        assert_eq!(users.len(), 1, "players by nickname are left out");
        assert_eq!(users[0]["data"]["parents"][0]["group"], "builder");
        assert!(
            src.export("lobby", &[])["users"][0]["data"]["parents"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    /// A provider set of PumboPerms size (100 nodes in global and in three
    /// server contexts): a check is a scan of the player's set in memory, no
    /// plugin call.
    #[test]
    fn checks_from_a_provider_set_are_fast() {
        let mut set = Vec::new();
        for ctx in [Global, srv("lobby"), srv("survival"), srv("arena")] {
            for i in 0..100 {
                set.push(e(&format!("pumbo.node{i}.sub"), i % 2 == 0, ctx.clone()));
            }
            set.push(e("pumbo.*", true, ctx.clone()));
        }
        let lv = levels(Some("survival"), &["games".into()]);
        let t = std::time::Instant::now();
        let mut yes = 0;
        for i in 0..100_000 {
            let node = format!("pumbo.node{}.sub", i % 150);
            yes += usize::from(resolve(&[], &set, &node, &lv).is_some_and(|d| d.value));
        }
        let took = t.elapsed();
        eprintln!(
            "[measure] 100000 checks, set of {} entries: {took:?}",
            set.len()
        );
        assert!(yes > 0);
        assert!(took < std::time::Duration::from_secs(10), "{took:?}");
    }

    #[test]
    fn snapshot_for_the_provider_keeps_contexts_and_names() {
        let (src, _) = FileSource::parse(FILE, &cfg()).unwrap();
        let x = src.snapshot();
        assert_eq!(x["format"], "pumboperms");
        let vip = x["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == "vip")
            .unwrap();
        let perms = vip["data"]["permissions"].as_array().unwrap();
        assert!(perms.contains(
            &serde_json::json!({"node": "pumbo.proxy.alert", "value": false, "context": {}})
        ));
        assert!(perms.contains(
            &serde_json::json!({"node": "pumbo.skins.change", "value": true, "context": {"server": "survival"}})
        ));
        assert_eq!(vip["data"]["parents"][0]["group"], "default");
        let notch = &x["users"][0];
        assert_eq!(notch["name"], "Notch");
        assert_eq!(
            notch["data"]["parents"],
            serde_json::json!([{"group": "default", "context": {}}, {"group": "vip", "context": {"server": "survival"}}])
        );
        assert_eq!(x["by-name"][0]["name"], "jeb_");
        assert_eq!(x["by-name"][0]["data"]["parents"][0]["group"], "mod");
        assert_eq!(fnv_hex("a"), fnv_hex("a"));
        assert_ne!(fnv_hex("a"), fnv_hex("b"));
    }

    #[test]
    fn unknown_contexts_are_warnings() {
        let text = FILE.replace(
            "  mod:\n",
            "  x:\n    server: { nowhere: { permissions: [a] } }\n    group: { nogroup: { permissions: [b] } }\n    inherits: [ghost]\n  mod:\n",
        );
        let (_, warnings) = FileSource::parse(&text, &cfg()).unwrap();
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(FileSource::parse("groups:\n  x:\n    bogus: 1\n", &cfg()).is_err());
        // Not valid YAML: the error names the line.
        let err = FileSource::parse("groups:\n  x:\n    permissions: [a\n", &cfg()).unwrap_err();
        assert!(err.contains("line"), "{err}");
    }
}
