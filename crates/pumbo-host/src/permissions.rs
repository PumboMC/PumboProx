//! Permissions as host data (plan §4.4 item 4, §5.8.4). A check reads the
//! table and never calls a plugin; resolution lives in
//! `pumbo_contracts::permissions`.
//!
//! Once the permission provider (PumboPerms) has loaded a player, its set
//! replaces the file for that player (PumboPerms spec §16: the provider took
//! the file over at its first start). While the provider is down, players
//! online keep its last set and `on-load-failure` decides new logins.
//! Session nodes (`permissions.set`) sit on top of both until the player
//! leaves.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::RwLock;

pub use pumbo_contracts::permissions::{Decision, Layer, Layers, levels, resolve};
use pumbo_core::permissions::{PermContext, PermissionEntry, PermissionSource, Subject};
use serde::Deserialize;
use uuid::Uuid;

use crate::config::HostConfig;
use crate::wit::types::PlayerId;

/// Permissions of one online player.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlayerPerms {
    pub file: Vec<PermissionEntry>,
    pub provider: Vec<PermissionEntry>,
    /// Nodes from `permissions.set`, kept until the player leaves.
    pub session: Vec<PermissionEntry>,
    /// The provider loaded this player: its set replaces the file.
    pub loaded: bool,
    /// Past the point of loading (`load-at`): loaded again when the
    /// provider comes back.
    pub eligible: bool,
    /// Bumped by every `permissions.replace`: a load started before it is
    /// older than the set the provider pushed since.
    pub epoch: u64,
}

impl PlayerPerms {
    /// The layers that decide: session nodes, then the provider's set alone
    /// once it is loaded, otherwise the file.
    pub fn layers(&self) -> Layers<'_> {
        Layers {
            session: &self.session,
            file: if self.loaded { &[] } else { &self.file },
            provider: &self.provider,
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
        resolve(online.get(&id)?.layers(), node, levels)
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

    /// The provider pushed a new set (`permissions.replace`).
    pub fn replace_provider_layer(&self, id: PlayerId, entries: Vec<PermissionEntry>) -> bool {
        self.with(id, |p| {
            p.epoch += 1;
            p.provider = entries;
            p.loaded = true;
        })
        .is_some()
    }

    /// The set of a load started at `epoch`; false when a newer push came
    /// in between (or the player left).
    pub fn load_provider_layer(
        &self,
        id: PlayerId,
        epoch: u64,
        entries: Vec<PermissionEntry>,
    ) -> bool {
        self.with(id, |p| {
            if p.epoch == epoch {
                p.provider = entries;
                p.loaded = true;
            }
            p.epoch == epoch
        })
        .unwrap_or(false)
    }

    pub fn epoch(&self, id: PlayerId) -> Option<u64> {
        self.online.read().ok()?.get(&id).map(|p| p.epoch)
    }

    fn with<T>(&self, id: PlayerId, f: impl FnOnce(&mut PlayerPerms) -> T) -> Option<T> {
        self.online.write().ok()?.get_mut(&id).map(f)
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
        p.session.retain(|e| !(e.node == node && e.context == ctx));
        if let Some(value) = value {
            p.session.push(PermissionEntry {
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
        self.players(|p| p.eligible)
    }

    /// Players past the point of loading without a set from the provider.
    pub fn unloaded(&self) -> Vec<PlayerId> {
        self.players(|p| p.eligible && !p.loaded)
    }

    fn players(&self, f: impl Fn(&PlayerPerms) -> bool) -> Vec<PlayerId> {
        self.online
            .read()
            .map(|o| o.iter().filter(|(_, p)| f(p)).map(|(id, _)| *id).collect())
            .unwrap_or_default()
    }

    /// Every player back on the file; only for a proxy without a provider.
    /// Returns the players whose permissions change.
    #[cfg(test)]
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

    /// Warns (at most once a minute) that the provider is down: players
    /// online keep its last set, `on-load-failure` decides new logins.
    pub(crate) fn warn_provider_down(&self, why: &str) {
        let Some(provider) = self.perm_provider.as_deref() else {
            return;
        };
        if let Ok(mut last) = self.provider_down_logged.lock()
            && last.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60))
        {
            *last = Some(std::time::Instant::now());
            let logins = match self.cfg.permissions.on_load_failure {
                crate::config::OnLoadFailure::Deny => "new logins are refused".to_string(),
                crate::config::OnLoadFailure::FileOnly => {
                    format!("new logins get {}", self.cfg.permissions.file.display())
                }
            };
            tracing::warn!(
                "permission provider {provider} {why}: players online keep their permissions, {logins}; fix it from the proxy console (/prox plugins reload {provider}, or permissions.provider: file)"
            );
        }
    }

    /// `on-load-failure` for a player the provider cannot load.
    fn load_failed(&self) -> Result<(), pumbo_text::Component> {
        match self.cfg.permissions.on_load_failure {
            crate::config::OnLoadFailure::Deny => {
                Err(self.message(&self.cfg.plugins.messages.permissions_unavailable))
            }
            crate::config::OnLoadFailure::FileOnly => Ok(()),
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

    /// Loads the provider layer of a player (plan §5.8.4, `load-at`). When
    /// the provider is down or fails, `on-load-failure` decides: `deny`
    /// keeps the player out, `file-only` lets them in on the file.
    pub(crate) async fn load_provider_permissions(
        &self,
        id: crate::wit::types::PlayerId,
    ) -> Result<(), pumbo_text::Component> {
        let Some(provider) = self.perm_provider.as_deref() else {
            return Ok(());
        };
        self.perms.mark_eligible(id);
        let Some(slot) = self.provider_slot() else {
            self.warn_provider_down("is not running");
            return self.load_failed();
        };
        let (Some(info), Some(epoch)) = (self.player(id), self.perms.epoch(id)) else {
            return Err(self.message(&self.cfg.plugins.messages.permissions_unavailable));
        };
        let who = crate::subject(&info);
        let source = PluginSource { host: self, slot };
        match source.load(&who).await {
            Ok(entries) => {
                // A set pushed while this load ran is newer.
                if self.perms.load_provider_layer(id, epoch, entries) {
                    self.forget_offline(Some(who.uuid));
                    self.bridge
                        .send(id, crate::bridge::PlayerCommand::CommandsChanged);
                }
                Ok(())
            }
            Err(e) => {
                tracing::warn!(player = id, provider = %provider, "permissions not loaded: {e}");
                self.load_failed()
            }
        }
    }

    /// Loads again the players past the point of loading that have no set
    /// from the provider (it was down or failed for them), while it runs.
    pub(crate) async fn retry_unloaded(&self) {
        if self
            .provider_slot()
            .is_none_or(|s| s.status() != crate::actor::Status::Running)
        {
            return;
        }
        for id in self.perms.unloaded() {
            if let Err(t) = self.load_provider_permissions(id).await {
                tracing::warn!(
                    player = id,
                    "permissions not loaded again: {}",
                    t.plain_text()
                );
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

    use PermContext::Global;

    fn file_layer(entries: &[PermissionEntry]) -> Layers<'_> {
        Layers {
            file: entries,
            ..Layers::default()
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
            resolve(file_layer(&entries), node, &levels(Some(server), &groups)).map(|d| d.value)
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
        let r = resolve(
            file_layer(&entries),
            "pumbo.proxy.alert",
            &levels(None, &[]),
        )
        .map(|d| d.value);
        assert_eq!(r, Some(true));
        let r = resolve(
            file_layer(&entries),
            "pumbo.proxy.server",
            &levels(None, &[]),
        )
        .map(|d| d.value);
        assert_eq!(r, Some(true));

        // Unlisted players get the default group.
        let other = Subject {
            uuid: Uuid::nil(),
            name: "someone".into(),
        };
        let entries = src.entries_for(&other);
        let r = resolve(
            file_layer(&entries),
            "pumbo.proxy.server",
            &levels(None, &[]),
        )
        .map(|d| d.value);
        assert_eq!(r, Some(true));
    }

    #[test]
    fn file_export_for_the_bridge() {
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

    fn one_online(id: PlayerId) -> Permissions {
        let p = Permissions::default();
        p.online.write().unwrap().insert(id, PlayerPerms::default());
        p
    }

    fn value(p: &Permissions, id: PlayerId, node: &str) -> Option<bool> {
        p.decide(id, node, &levels(None, &[])).map(|d| d.value)
    }

    #[test]
    fn session_node_survives_provider_replace() {
        let p = one_online(1);
        p.set_node(1, "minigame.spectate", Some(true), Global);
        p.replace_provider_layer(1, vec![e("pumbo.proxy.server", true, Global)]);
        assert_eq!(value(&p, 1, "minigame.spectate"), Some(true));
    }

    #[test]
    fn session_node_survives_provider_drop() {
        let p = one_online(1);
        p.replace_provider_layer(1, vec![e("pumbo.proxy.server", true, Global)]);
        p.set_node(1, "minigame.spectate", Some(true), Global);
        p.drop_provider();
        assert_eq!(value(&p, 1, "minigame.spectate"), Some(true));
    }

    #[test]
    fn clearing_session_node_keeps_provider_entry() {
        let p = one_online(1);
        p.replace_provider_layer(1, vec![e("minigame.spectate", false, Global)]);
        p.set_node(1, "minigame.spectate", Some(true), Global);
        p.set_node(1, "minigame.spectate", None, Global);
        assert_eq!(value(&p, 1, "minigame.spectate"), Some(false));
    }

    #[test]
    fn server_context_ignores_case() {
        let cfg = HostConfig::parse(
            "servers:\n  Survival: { address: x }\nserver-group:\n  - name: Games\n    servers: [Arena]\n",
        )
        .unwrap();
        let text = r#"
players:
  069a79f4-44e9-4726-a5be-fca90e38aaf5:
    server:
      survival: { permissions: [pumbo.proxy.glist, "minecraft:command.help"] }
    group:
      GAMES: { permissions: ["-pumbo.proxy.server"] }
"#;
        let (src, warnings) = FileSource::parse(text, &cfg).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let notch = Subject {
            uuid: Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap(),
            name: "Notch".into(),
        };
        let entries = src.entries_for(&notch);
        let at = |server: &str| levels(Some(server), &cfg.groups_of(server));
        let has = |node: &str, server: &str| {
            resolve(file_layer(&entries), node, &at(server)).map(|d| d.value)
        };
        assert_eq!(has("pumbo.proxy.glist", "Survival"), Some(true));
        assert_eq!(has("pumbo.proxy.server", "arena"), Some(false));
        let x = src.export("Survival", &[]);
        assert_eq!(
            x["users"][0]["data"]["permissions"][0]["node"],
            "minecraft:command.help"
        );

        let p = one_online(1);
        let ctx =
            crate::imports::perm_context(&crate::wit::types::Context::Server("SURVIVAL".into()));
        p.replace_provider_layer(1, vec![e("pumbo.proxy.alert", true, ctx)]);
        assert_eq!(
            p.decide(1, "pumbo.proxy.alert", &at("Survival"))
                .map(|d| d.value),
            Some(true)
        );
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

    /// The host side of a change at network scale: 20 groups of 50 nodes
    /// with entries on 10 servers in each player's set, 300 players online,
    /// 3000 players in `permissions.yml`.
    #[test]
    #[ignore = "release benchmark"]
    fn host_lookups_at_scale() {
        use std::time::{Duration, Instant};
        let mut set = Vec::new();
        for g in 0..20 {
            set.push(e(&format!("group.g{g}"), true, Global));
            for i in 0..50 {
                let ctx = match i % 5 {
                    0 => PermContext::server(&format!("s{}", (g + i) % 10)),
                    _ => Global,
                };
                set.push(e(&format!("pumbo.g{g}.n{i}"), i % 3 != 0, ctx));
            }
            set.push(e(&format!("pumbog{g}:*"), true, Global));
        }
        let p = Permissions::default();
        for id in 0..300 {
            p.online.write().unwrap().insert(id, PlayerPerms::default());
            p.replace_provider_layer(id, set.clone());
        }
        let at = levels(Some("s3"), &["games".into()]);

        // Worst case: no entry anywhere, every level scans the whole set.
        let t = Instant::now();
        for i in 0..10_000u64 {
            assert!(p.decide(i % 300, "pumbo.none.here", &at).is_none());
        }
        let check = t.elapsed() / 10_000;

        let catalog: Vec<String> = (0..200)
            .map(|i| format!("pumbog{}:node{i}", i % 20))
            .collect();
        let t = Instant::now();
        let online = p.online.read().unwrap();
        for id in 0..300 {
            let set =
                pumbo_contracts::permissions::backend_set(online[&id].layers(), &catalog, &at);
            assert!(!set.is_empty());
        }
        drop(online);
        let sets = t.elapsed();

        let mut yaml =
            String::from("groups:\n  vip: { permissions: [pumbo.proxy.glist] }\nplayers:\n");
        for i in 0..3000u128 {
            yaml.push_str(&format!("  {}: {{ groups: [vip] }}\n", Uuid::from_u128(i)));
        }
        let (src, _) = FileSource::parse(&yaml, &cfg()).unwrap();
        let who = Subject {
            uuid: Uuid::from_u128(2999),
            name: "Last".into(),
        };
        let t = Instant::now();
        for _ in 0..100 {
            assert!(!src.entries_for(&who).is_empty());
        }
        let join = t.elapsed() / 100;

        eprintln!(
            "[measure] set of {} entries: check {check:?} (no entry), backend sets for 300 players {sets:?}, file layer at join {join:?}",
            set.len()
        );
        assert!(check < Duration::from_micros(50), "{check:?}");
        // The budget of one push to 300 players.
        assert!(sets < Duration::from_millis(50), "{sets:?}");
        assert!(join < Duration::from_millis(1), "{join:?}");
    }
}
