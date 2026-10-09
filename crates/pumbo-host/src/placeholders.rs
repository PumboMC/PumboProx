//! Placeholders (plan §5.8.3): namespaces and aliases, push values with
//! contexts, pull values with a cache, built-in `player`, `proxy`, `server`.
//!
//! Hot paths (status, Tab, messages queued by plugins) read push values and
//! the cache only; a cold or stale pull entry is refreshed in the background.
//! `resolve` (async) waits for cold pull values up to their deadline.
//! Templates are rendered in one pass; values are never scanned again.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pumbo_core::permissions::PermContext;
use pumbo_text::template::{
    Placeholder, PlaceholderContext, Template, escape_mini, legacy_to_mini,
};
use tokio::time::Instant;

use crate::HostInner;
use crate::actor::{PluginSlot, Status};
use crate::config::Unresolved;
use crate::manifest::{KeyDecl, KeyMode, KeyScope};
use crate::permissions::levels;
use crate::wit::placeholders::{PlaceholderRequest, ResolveError};
use crate::wit::types::{Context, PlayerContext, PlayerId, QueryContext, Text, TextTemplate};

pub const BUILTIN: &[&str] = &["player", "proxy", "server"];
/// Push value limit.
pub const MAX_VALUE: usize = 1024;
const ERROR_TTL: Duration = Duration::from_secs(1);
const MAX_PULL_TIMEOUT_MS: u64 = 250;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    ns: String,
    key: String,
    arg: Option<String>,
    player: Option<PlayerId>,
    server: Option<String>,
    groups: Vec<String>,
    /// From `@context`: kept when the player changes servers.
    explicit: bool,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    value: Option<String>,
    at: Instant,
    ttl: Duration,
    refreshing: bool,
}

type PushKey = (String, String, Option<PlayerId>);

#[derive(Debug, Default)]
pub(crate) struct Placeholders {
    /// Namespace → owning plugin (built-in namespaces are not here).
    owners: BTreeMap<String, String>,
    keys: BTreeMap<(String, String), KeyDecl>,
    aliases: BTreeMap<String, String>,
    push: Mutex<HashMap<PushKey, Vec<(PermContext, String)>>>,
    cache: Mutex<HashMap<CacheKey, CacheEntry>>,
    pub max_players: AtomicU32,
    pub pings: Mutex<HashMap<PlayerId, u32>>,
    /// Values from PumboBridge: (server, key) and (player, key).
    pub server_values: Mutex<HashMap<(String, String), String>>,
    pub player_values: Mutex<HashMap<(PlayerId, String), String>>,
}

impl Placeholders {
    pub fn new(
        plugins: &BTreeMap<String, Arc<PluginSlot>>,
        cfg: &crate::config::HostConfig,
    ) -> Result<Placeholders, String> {
        let mut owners = BTreeMap::new();
        let mut keys = BTreeMap::new();
        let mut suggested: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for slot in plugins.values() {
            let Some(decl) = &slot.manifest.placeholders else {
                continue;
            };
            if BUILTIN.contains(&decl.namespace.as_str()) {
                return Err(format!(
                    "{}: placeholder namespace {} is built in",
                    slot.id, decl.namespace
                ));
            }
            if let Some(other) = owners.insert(decl.namespace.clone(), slot.id.clone()) {
                return Err(format!(
                    "placeholder namespace {} declared by {other} and {}",
                    decl.namespace, slot.id
                ));
            }
            for k in &decl.keys {
                keys.insert((decl.namespace.clone(), k.name.clone()), k.clone());
            }
            for (alias, target) in &decl.suggested_aliases {
                suggested
                    .entry(alias.clone())
                    .or_default()
                    .push((slot.id.clone(), target.clone()));
            }
        }
        let mut aliases = cfg.placeholders.aliases.clone();
        for (alias, list) in suggested {
            if aliases.contains_key(&alias) {
                continue;
            }
            match list.as_slice() {
                [(_, target)] => {
                    aliases.insert(alias, target.clone());
                }
                _ => tracing::warn!(
                    "placeholder alias {alias} suggested by {}: not enabled",
                    list.iter()
                        .map(|(p, _)| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
        Ok(Placeholders {
            owners,
            keys,
            aliases,
            ..Placeholders::default()
        })
    }

    pub fn player_left(&self, id: PlayerId) {
        if let Ok(mut p) = self.push.lock() {
            p.retain(|(_, _, pl), _| *pl != Some(id));
        }
        if let Ok(mut c) = self.cache.lock() {
            c.retain(|k, _| k.player != Some(id));
        }
        if let Ok(mut p) = self.pings.lock() {
            p.remove(&id);
        }
        if let Ok(mut p) = self.player_values.lock() {
            p.retain(|(who, _), _| *who != id);
        }
    }

    /// The player changed servers: entries resolved in the current context go.
    pub fn context_changed(&self, id: PlayerId) {
        if let Ok(mut c) = self.cache.lock() {
            c.retain(|k, _| k.player != Some(id) || k.explicit);
        }
    }
}

fn perm_ctx(c: &Context) -> PermContext {
    match c {
        Context::Global => PermContext::Global,
        Context::Group(g) => PermContext::Group(g.clone()),
        Context::Server(s) => PermContext::Server(s.clone()),
    }
}

/// A value as MiniMessage that shows what the provider meant.
fn value_mini(t: &Text) -> String {
    match t {
        Text::Plain(s) => escape_mini(s),
        Text::Mini(s) => s.clone(),
        Text::Legacy(s) => legacy_to_mini(s),
        Text::Json(s) => escape_mini(
            &pumbo_text::Component::from_json(s)
                .map(|c| c.plain_text())
                .unwrap_or_default(),
        ),
        // A template as a value is not resolved again (one pass).
        Text::Template(t) => t.mini.clone(),
    }
}

/// Where a placeholder resolves after aliases.
struct Target {
    ns: String,
    key: String,
    arg: Option<String>,
    context: PlayerContext,
    explicit: bool,
}

/// The pull request of a cold cache entry.
struct Cold {
    provider: String,
    key: CacheKey,
    req: PlaceholderRequest,
    ttl: Duration,
    timeout: Duration,
}

impl HostInner {
    fn placeholder_target(&self, p: &Placeholder, player: Option<PlayerId>) -> Option<Target> {
        let ph = &self.placeholders;
        let mut p = p.clone();
        let known = |q: &Placeholder| {
            q.split()
                .is_some_and(|(ns, _)| BUILTIN.contains(&ns) || ph.owners.contains_key(ns))
        };
        if !known(&p) {
            let target = ph.aliases.get(&p.name)?;
            let t = Template::parse(&format!("%{target}%")).ok()?;
            let a = t.placeholders().next()?.clone();
            p = Placeholder {
                name: a.name,
                arg: p.arg.or(a.arg),
                context: p.context.or(a.context),
            };
        }
        let (ns, key) = p.split()?;
        let explicit = p.context.is_some();
        let context = match &p.context {
            Some(PlaceholderContext::Global) => PlayerContext {
                server: None,
                groups: Vec::new(),
            },
            Some(PlaceholderContext::Group(g)) => PlayerContext {
                server: None,
                groups: vec![g.clone()],
            },
            Some(PlaceholderContext::Server(s)) => self.context_of(Some(s)),
            None => match player.and_then(|id| self.player(id)) {
                Some(info) if !info.in_virtual => info.context,
                _ => PlayerContext {
                    server: None,
                    groups: Vec::new(),
                },
            },
        };
        Some(Target {
            ns: ns.to_string(),
            key: key.to_string(),
            arg: p.arg.clone(),
            context,
            explicit,
        })
    }

    fn builtin(&self, t: &Target, player: Option<PlayerId>) -> Option<String> {
        let info = player.and_then(|id| self.player(id));
        let arg = t.arg.as_deref();
        let v = match (t.ns.as_str(), t.key.as_str()) {
            ("player", "name") => info?.profile.name,
            ("player", "uuid") => crate::uuid_of(&info?.profile.id).hyphenated().to_string(),
            ("player", "server") => info?.server.unwrap_or_default(),
            ("player", "version") => info?.connection.protocol.to_string(),
            ("player", "ping") => self
                .placeholders
                .pings
                .lock()
                .ok()?
                .get(&player?)?
                .to_string(),
            ("proxy", "online") => self.players.read().ok()?.len().to_string(),
            ("proxy", "max") => self
                .placeholders
                .max_players
                .load(Ordering::Relaxed)
                .to_string(),
            ("server", "online") => self.servers.read().ok()?.get(arg?)?.players.to_string(),
            ("server", "status") => {
                let up = self.servers.read().ok()?.get(arg?)?.online;
                (if up { "online" } else { "offline" }).to_string()
            }
            // From PumboBridge; `?` without a bridge on that server.
            ("server", k @ ("tps" | "mspt")) => self
                .placeholders
                .server_values
                .lock()
                .ok()?
                .get(&(arg?.to_string(), k.to_string()))
                .cloned()
                .unwrap_or_else(|| "?".into()),
            ("player", k @ ("health" | "food" | "level" | "world" | "gamemode")) => self
                .placeholders
                .player_values
                .lock()
                .ok()?
                .get(&(player?, k.to_string()))
                .cloned()
                .unwrap_or_else(|| "?".into()),
            _ => return None,
        };
        // Built-in values are data (names, servers): always literal.
        Some(escape_mini(&v))
    }

    /// Visibility and the declaration of a plugin key.
    fn plugin_key(&self, t: &Target, caller: Option<&PluginSlot>) -> Option<(String, KeyDecl)> {
        let owner = self.placeholders.owners.get(&t.ns)?;
        let decl = self.placeholders.keys.get(&(t.ns.clone(), t.key.clone()))?;
        let visible = decl.public || caller.is_none_or(|c| &c.id == owner);
        visible.then(|| (owner.clone(), decl.clone()))
    }

    fn sanitize(&self, ns: &str, v: String) -> String {
        if self.cfg.placeholders.rich.get(ns).copied().unwrap_or(false) {
            v
        } else {
            pumbo_text::strip_events(&v)
        }
    }

    /// The value now (push, cache), or a cold pull to fetch.
    /// `wait`: the caller will wait for cold entries, so an entry whose
    /// first fetch is still running counts as cold too.
    fn lookup(
        &self,
        p: &Placeholder,
        player: Option<PlayerId>,
        caller: Option<&PluginSlot>,
        wait: bool,
    ) -> (Option<String>, Option<Cold>) {
        let Some(t) = self.placeholder_target(p, player) else {
            return (None, None);
        };
        if BUILTIN.contains(&t.ns.as_str()) {
            return (self.builtin(&t, player), None);
        }
        let Some((owner, decl)) = self.plugin_key(&t, caller) else {
            return (None, None);
        };
        let fallback = decl.fallback.clone();
        let who = match decl.scope {
            KeyScope::Player => match player {
                Some(id) => Some(id),
                None => return (fallback, None),
            },
            KeyScope::Global => None,
        };
        let lv = levels(t.context.server.as_deref(), &t.context.groups);
        match decl.mode {
            KeyMode::Push => {
                let v = self.placeholders.push.lock().ok().and_then(|push| {
                    let entries = push.get(&(t.ns.clone(), t.key.clone(), who))?;
                    lv.iter()
                        .find_map(|l| entries.iter().find(|(c, _)| c == l).map(|(_, v)| v.clone()))
                });
                (v.map(|v| self.sanitize(&t.ns, v)).or(fallback), None)
            }
            KeyMode::Pull => {
                let key = CacheKey {
                    ns: t.ns.clone(),
                    key: t.key.clone(),
                    arg: t.arg.clone(),
                    player: who,
                    server: t.context.server.clone(),
                    groups: t.context.groups.clone(),
                    explicit: t.explicit,
                };
                let ttl = Duration::from_millis(decl.ttl_ms.unwrap_or(5000).clamp(100, 600_000));
                let timeout = Duration::from_millis(
                    decl.timeout_ms
                        .unwrap_or(self.cfg.placeholders.pull_timeout_ms)
                        .min(MAX_PULL_TIMEOUT_MS),
                );
                let mut cold = None;
                let mut value = None;
                if let Ok(mut cache) = self.placeholders.cache.lock() {
                    match cache.get_mut(&key) {
                        Some(e) => {
                            value = e.value.clone();
                            if e.at.elapsed() >= e.ttl && !e.refreshing {
                                e.refreshing = true;
                                cold = Some(());
                            } else if wait && e.refreshing && e.value.is_none() {
                                cold = Some(());
                            }
                        }
                        None => {
                            cache.insert(
                                key.clone(),
                                CacheEntry {
                                    value: None,
                                    at: Instant::now(),
                                    ttl: Duration::ZERO,
                                    refreshing: true,
                                },
                            );
                            cold = Some(());
                        }
                    }
                }
                let cold = cold.map(|()| Cold {
                    provider: owner,
                    req: PlaceholderRequest {
                        key: t.key.clone(),
                        arg: t.arg.clone(),
                        player: who,
                        context: t.context.clone(),
                    },
                    key,
                    ttl,
                    timeout,
                });
                (value.map(|v| self.sanitize(&t.ns, v)).or(fallback), cold)
            }
        }
    }

    /// For rendering without waiting: push and cached values; cold pull
    /// entries are refreshed in the background.
    pub(crate) fn placeholder_now(
        &self,
        p: &Placeholder,
        player: Option<PlayerId>,
        caller: Option<&PluginSlot>,
    ) -> Option<String> {
        let (v, cold) = self.lookup(p, player, caller, false);
        if let Some(c) = cold
            && let Some(me) = self.me.get().and_then(std::sync::Weak::upgrade)
        {
            tokio::spawn(async move { me.fetch_pull(vec![c]).await });
        }
        v
    }

    /// Asks providers for cold entries, one call per provider, and fills the
    /// cache (errors and timeouts are remembered for 1 s).
    async fn fetch_pull(&self, cold: Vec<Cold>) {
        let mut by_provider: BTreeMap<String, Vec<Cold>> = BTreeMap::new();
        for c in cold {
            by_provider.entry(c.provider.clone()).or_default().push(c);
        }
        let calls = by_provider.into_iter().map(|(provider, list)| async move {
            let slot = self.plugins.get(&provider).cloned();
            let timeout = list
                .iter()
                .map(|c| c.timeout)
                .max()
                .unwrap_or(Duration::from_millis(50));
            let reqs: Vec<PlaceholderRequest> = list.iter().map(|c| c.req.clone()).collect();
            let n = reqs.len();
            let answers = if let Some(slot) = slot.filter(|s| s.status() == Status::Running) {
                slot.call(Duration::ZERO, timeout, move |acc, g| {
                    Box::pin(async move { g.call_on_placeholder(acc, reqs).await })
                })
                .await
                .ok()
                .filter(|a| a.len() == n)
            } else {
                None
            };
            if let Ok(mut cache) = self.placeholders.cache.lock() {
                for (i, c) in list.into_iter().enumerate() {
                    let v = answers.as_ref().and_then(|a| a.get(i).cloned().flatten());
                    let ttl = if answers.is_some() { c.ttl } else { ERROR_TTL };
                    let value = v.as_ref().map(value_mini).filter(|s| s.len() <= MAX_VALUE);
                    cache.insert(
                        c.key,
                        CacheEntry {
                            value,
                            at: Instant::now(),
                            ttl,
                            refreshing: false,
                        },
                    );
                }
                let limit = self.cfg.placeholders.cache_entries.max(1);
                if cache.len() > limit {
                    // ponytail: drops the oldest tenth, an LRU if eviction shows up in profiles.
                    let mut ages: Vec<(Instant, CacheKey)> =
                        cache.iter().map(|(k, e)| (e.at, k.clone())).collect();
                    ages.sort_by_key(|(t, _)| *t);
                    for (_, k) in ages.into_iter().take(limit / 10 + 1) {
                        cache.remove(&k);
                    }
                }
            }
        });
        futures::future::join_all(calls).await;
    }

    /// Renders a template for a recipient, waiting for cold pull values up to
    /// their deadline. `caller`: the plugin that asks (`None` = proxy config).
    pub(crate) async fn resolve_template(
        &self,
        t: &TextTemplate,
        player: Option<PlayerId>,
        at: &QueryContext,
        caller: Option<&PluginSlot>,
    ) -> Result<String, ResolveError> {
        let tpl = Template::parse(&t.mini).map_err(|e| match e {
            pumbo_text::template::TemplateError::TooLarge => ResolveError::TooLarge,
            pumbo_text::template::TemplateError::TooMany => ResolveError::TooMany,
        })?;
        // An explicit query context applies to placeholders without `@`.
        let with_ctx = |p: &Placeholder| -> Placeholder {
            let mut p = p.clone();
            if p.context.is_none() {
                p.context = match at {
                    QueryContext::Current => None,
                    QueryContext::Global => Some(PlaceholderContext::Global),
                    QueryContext::Group(g) => Some(PlaceholderContext::Group(g.clone())),
                    QueryContext::Server(s) => Some(PlaceholderContext::Server(s.clone())),
                };
            }
            p
        };
        let cold: Vec<Cold> = tpl
            .placeholders()
            .filter_map(|p| self.lookup(&with_ctx(p), player, caller, true).1)
            .collect();
        if !cold.is_empty() {
            self.fetch_pull(cold).await;
        }
        let keep = self.cfg.placeholders.unresolved == Unresolved::Keep;
        let out = tpl.render(&t.args, keep, |p| {
            self.lookup(&with_ctx(p), player, caller, false).0
        });
        if out.len() > crate::text::MAX_TEXT {
            return Err(ResolveError::TooLarge);
        }
        Ok(out)
    }

    fn own_key(&self, slot: &PluginSlot, key: &str) -> Result<(String, KeyDecl), String> {
        let decl = slot
            .manifest
            .placeholders
            .as_ref()
            .ok_or("the manifest declares no placeholders")?;
        let k = decl
            .keys
            .iter()
            .find(|k| k.name == key)
            .ok_or_else(|| format!("placeholder key {key} is not in the manifest"))?;
        Ok((decl.namespace.clone(), k.clone()))
    }

    pub(crate) fn placeholder_set(
        &self,
        slot: &PluginSlot,
        key: &str,
        player: Option<PlayerId>,
        value: Text,
        ctx: Context,
    ) -> Result<(), String> {
        let (ns, decl) = self.own_key(slot, key)?;
        if decl.mode != KeyMode::Push {
            return Err(format!("{key} is a pull key"));
        }
        match (decl.scope, player) {
            (KeyScope::Player, None) => return Err(format!("{key} needs a player")),
            (KeyScope::Global, Some(_)) => return Err(format!("{key} is global")),
            _ => {}
        }
        let v = value_mini(&value);
        if v.len() > MAX_VALUE {
            return Err(format!("value over {MAX_VALUE} bytes"));
        }
        let c = perm_ctx(&ctx);
        if let Ok(mut push) = self.placeholders.push.lock() {
            let entries = push.entry((ns, key.to_string(), player)).or_default();
            entries.retain(|(e, _)| *e != c);
            entries.push((c, v));
        }
        Ok(())
    }

    pub(crate) fn placeholder_clear(
        &self,
        slot: &PluginSlot,
        key: &str,
        player: Option<PlayerId>,
        ctx: Context,
    ) {
        let Ok((ns, _)) = self.own_key(slot, key) else {
            return;
        };
        let c = perm_ctx(&ctx);
        if let Ok(mut push) = self.placeholders.push.lock()
            && let Some(entries) = push.get_mut(&(ns, key.to_string(), player))
        {
            entries.retain(|(e, _)| *e != c);
        }
    }

    pub(crate) fn placeholder_invalidate(
        &self,
        slot: &PluginSlot,
        key: &str,
        player: Option<PlayerId>,
    ) {
        let Ok((ns, _)) = self.own_key(slot, key) else {
            return;
        };
        if let Ok(mut cache) = self.placeholders.cache.lock() {
            cache.retain(|k, _| {
                !(k.ns == ns && k.key == key && player.is_none_or(|p| k.player == Some(p)))
            });
        }
    }
}
