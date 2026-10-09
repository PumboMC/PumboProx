//! Service registry (plan §5.8.2) and events between plugins (`bus`, §6.6.2).
//!
//! A call is a future in the caller's task; the provider gets it as a new
//! task in its own instance through its mailbox. No lock is held while a
//! plugin runs, so a cycle A→B→A is just more tasks, bounded by the chain
//! depth, deadlines and queue limits.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

use crate::HostInner;
use crate::actor::{self, CallError, PluginSlot, Status};
use crate::manifest::{Use, parse_version, split_topic};
use crate::wit::events::ServiceCall;
use crate::wit::services::{CallOptions, CallReject, ServiceError, ServiceRef};

/// Payload limit of a call, both ways.
pub const MAX_PAYLOAD: usize = 256 * 1024;
/// Payload limit of a bus event.
pub const MAX_EVENT: usize = 64 * 1024;
/// Caller id of calls the host makes itself.
pub const HOST_CALLER: &str = "proxy";

/// A service the host provides itself (e.g. `pumbo:bridge`, answered by the
/// proxy's bridge module). Calls get the caller's plugin id; the host
/// applies the caller's deadline.
pub trait NativeService: Send + Sync + 'static {
    fn call(
        &self,
        caller: String,
        method: String,
        payload: Vec<u8>,
    ) -> futures::future::BoxFuture<'static, Result<Vec<u8>, ServiceError>>;

    /// MiniMessage lines of its `/pumbo <name> …` command (`console`: plain
    /// columns for the console; `secrets`: the caller may see keys).
    fn admin(&self, _args: &[String], _console: bool, _secrets: bool) -> Vec<String> {
        Vec::new()
    }
}

/// A native provider registered at start.
#[derive(Clone)]
pub struct Native {
    pub service: &'static str,
    pub major: u16,
    pub minor: u16,
    pub provider: Arc<dyn NativeService>,
}

impl std::fmt::Debug for Native {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Native({}@{}.{})", self.service, self.major, self.minor)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Provider {
    pub plugin: String,
    pub minor: u16,
    pub max_timeout_ms: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct Chain {
    depth: u32,
    deadline: Instant,
}

type OfflineKey = (uuid::Uuid, String, String);

#[derive(Debug, Default)]
pub(crate) struct Services {
    /// (service, major) → provider.
    providers: BTreeMap<(String, u16), Provider>,
    /// Services the host provides itself (provider plugin = [`HOST_CALLER`]).
    natives: BTreeMap<String, Native>,
    /// Services switched off by their provider.
    off: Mutex<std::collections::BTreeSet<String>>,
    chains: Mutex<HashMap<u64, Chain>>,
    next_ctx: AtomicU64,
    /// Calls queued or running per (caller, provider).
    pairs: Mutex<HashMap<(String, String), usize>>,
    /// Calls queued or running per provider.
    pending: Mutex<HashMap<String, usize>>,
    /// Bus events queued or running per subscriber.
    bus_pending: Mutex<HashMap<String, usize>>,
    pub bus_dropped: AtomicU64,
    /// `has-offline` answers of the provider: (uuid, node, context) → (time, value).
    offline: Mutex<HashMap<OfflineKey, (Instant, Option<bool>)>>,
}

impl Services {
    /// Registry from the manifests; several providers of one service need a
    /// choice in `services.providers`.
    pub fn new(
        plugins: &BTreeMap<String, Arc<PluginSlot>>,
        choice: &BTreeMap<String, String>,
        natives: Vec<Native>,
    ) -> Result<Services, String> {
        let mut candidates: BTreeMap<(String, u16), Vec<Provider>> = BTreeMap::new();
        for n in &natives {
            candidates
                .entry((n.service.to_string(), n.major))
                .or_default()
                .push(Provider {
                    plugin: HOST_CALLER.to_string(),
                    minor: n.minor,
                    max_timeout_ms: None,
                });
        }
        for slot in plugins.values() {
            for p in &slot.manifest.provides {
                let Some((major, minor)) = parse_version(&p.version) else {
                    continue;
                };
                if let Some((ns, _)) = p.service.split_once(':')
                    && ns == "pumbo"
                {
                    let Some(c) = pumbo_contracts::SERVICES
                        .iter()
                        .find(|c| c.name == p.service)
                    else {
                        return Err(format!(
                            "{}: the pumbo: namespace is reserved, {} is not a pumbo: service",
                            slot.id, p.service
                        ));
                    };
                    let allowed = match c.owner {
                        Some(owner) => owner == slot.id,
                        None => slot.manifest.permission_provider,
                    };
                    if !allowed {
                        return Err(format!("{} may not provide {}", slot.id, p.service));
                    }
                }
                candidates
                    .entry((p.service.clone(), major))
                    .or_default()
                    .push(Provider {
                        plugin: slot.id.clone(),
                        minor,
                        max_timeout_ms: p.max_timeout_ms,
                    });
            }
        }
        let mut providers = BTreeMap::new();
        for ((name, major), list) in candidates {
            let chosen = if list.len() == 1 {
                list.into_iter().next()
            } else {
                match choice.get(&name) {
                    Some(id) => list.into_iter().find(|p| &p.plugin == id),
                    None => {
                        let ids: Vec<&str> = list.iter().map(|p| p.plugin.as_str()).collect();
                        return Err(format!(
                            "service {name}@{major} is provided by {}; choose one in [services.providers]",
                            ids.join(", ")
                        ));
                    }
                }
            };
            match chosen {
                Some(p) => {
                    providers.insert((name, major), p);
                }
                None => {
                    return Err(format!(
                        "[services.providers] {name}: the chosen plugin does not provide it"
                    ));
                }
            }
        }
        Ok(Services {
            providers,
            natives: natives
                .into_iter()
                .map(|n| (n.service.to_string(), n))
                .collect(),
            next_ctx: AtomicU64::new(1),
            ..Services::default()
        })
    }

    pub fn provider(&self, service: &str, major: u16) -> Option<&Provider> {
        self.providers.get(&(service.to_string(), major))
    }

    pub fn native(&self, service: &str) -> Option<&Arc<dyn NativeService>> {
        self.natives.get(service).map(|n| &n.provider)
    }

    /// Why a consumer cannot use a service, if it cannot (version check).
    pub fn check_use(&self, u: &Use) -> Result<&Provider, String> {
        let (major, minor) = parse_version(&u.version).ok_or("invalid version")?;
        let p = self
            .provider(&u.service, major)
            .ok_or_else(|| format!("no provider of {}@{major}", u.service))?;
        if p.minor < minor {
            return Err(format!(
                "{} provides {}@{major}.{}, {} needed",
                p.plugin, u.service, p.minor, u.version
            ));
        }
        Ok(p)
    }

    /// Consumers whose required services are missing or too old.
    pub fn blocked(&self, plugins: &BTreeMap<String, Arc<PluginSlot>>) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for slot in plugins.values() {
            for u in slot.manifest.uses.iter().filter(|u| u.required) {
                if let Err(e) = self.check_use(u) {
                    out.push((slot.id.clone(), format!("required service missing: {e}")));
                }
            }
        }
        out
    }

    fn is_off(&self, service: &str) -> bool {
        self.off
            .lock()
            .map(|o| o.contains(service))
            .unwrap_or(false)
    }

    fn chain(&self, ctx: Option<u64>) -> Option<Chain> {
        let id = ctx?;
        self.chains.lock().ok()?.get(&id).copied()
    }
}

/// Releases the counters of one call and its chain context when the
/// provider's task ends (or never runs).
struct CallGuard {
    host: Arc<HostInner>,
    provider: String,
    pair: (String, String),
    ctx: u64,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let s = &self.host.services;
        if let Ok(mut c) = s.chains.lock() {
            c.remove(&self.ctx);
        }
        if let Ok(mut p) = s.pairs.lock()
            && let Some(n) = p.get_mut(&self.pair)
        {
            *n = n.saturating_sub(1);
        }
        if let Ok(mut p) = s.pending.lock()
            && let Some(n) = p.get_mut(&self.provider)
        {
            *n = n.saturating_sub(1);
        }
    }
}

struct BusGuard {
    host: Arc<HostInner>,
    subscriber: String,
}

impl Drop for BusGuard {
    fn drop(&mut self) {
        if let Ok(mut p) = self.host.services.bus_pending.lock()
            && let Some(n) = p.get_mut(&self.subscriber)
        {
            *n = n.saturating_sub(1);
        }
    }
}

fn ctx_string(at: &crate::wit::types::QueryContext) -> String {
    use crate::wit::types::QueryContext as Q;
    match at {
        Q::Current | Q::Global => "global".into(),
        Q::Group(g) => format!("group={g}"),
        Q::Server(s) => format!("server={s}"),
    }
}

impl HostInner {
    fn self_arc(&self) -> Option<Arc<HostInner>> {
        self.me.get().and_then(std::sync::Weak::upgrade)
    }

    fn uses_of<'a>(&self, slot: &'a PluginSlot, service: &str) -> Option<&'a Use> {
        slot.manifest.uses.iter().find(|u| u.service == service)
    }

    pub(crate) fn service_lookup(&self, slot: &PluginSlot, service: &str) -> Option<ServiceRef> {
        let u = self.uses_of(slot, service)?;
        let p = self.services.check_use(u).ok()?;
        let up = p.plugin == HOST_CALLER
            || self
                .plugins
                .get(&p.plugin)
                .is_some_and(|s| s.status() == Status::Running);
        if !up || self.services.is_off(service) {
            return None;
        }
        let (major, _) = parse_version(&u.version)?;
        Some(ServiceRef {
            name: service.to_string(),
            major,
            minor: p.minor,
        })
    }

    pub(crate) fn service_set_available(
        &self,
        slot: &PluginSlot,
        service: &str,
        available: bool,
    ) -> Result<(), String> {
        if !slot.manifest.provides.iter().any(|p| p.service == service) {
            return Err(format!("{service} is not in provides of the manifest"));
        }
        let changed = match self.services.off.lock() {
            Ok(mut off) => {
                if available {
                    off.remove(service)
                } else {
                    off.insert(service.to_string())
                }
            }
            Err(_) => false,
        };
        if changed {
            self.notify_service(service, false);
        }
        Ok(())
    }

    /// `on-service-changed` to every running consumer of a service.
    /// `force_down`: the provider's instance just ended.
    pub(crate) fn notify_service(&self, service: &str, force_down: bool) {
        for slot in self.plugins.values() {
            if !slot.manifest.uses.iter().any(|u| u.service == service) {
                continue;
            }
            let now = if force_down {
                None
            } else {
                self.service_lookup(slot, service)
            };
            let name = service.to_string();
            self.notify(slot, move |acc, g| {
                Box::pin(async move {
                    let _ = g.call_on_service_changed(acc, name, now).await;
                })
            });
        }
    }

    /// The services of a plugin went up or down with its instance.
    pub(crate) fn services_of_changed(&self, slot: &PluginSlot, up: bool) {
        let mut names: Vec<&str> = slot
            .manifest
            .provides
            .iter()
            .map(|p| p.service.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        for n in names {
            self.notify_service(n, !up);
        }
    }

    pub(crate) async fn service_call(
        &self,
        caller: &PluginSlot,
        service: String,
        method: String,
        payload: Vec<u8>,
        opts: CallOptions,
    ) -> Result<Vec<u8>, ServiceError> {
        let Some(u) = self.uses_of(caller, &service) else {
            return Err(ServiceError::NotDeclared);
        };
        let (major, minor) = parse_version(&u.version).ok_or(ServiceError::Incompatible)?;
        self.call_service(&caller.id, &service, major, minor, method, payload, opts)
            .await
    }

    /// A call by a plugin or by the host itself (`caller` = [`HOST_CALLER`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_service(
        &self,
        caller: &str,
        service: &str,
        major: u16,
        minor: u16,
        method: String,
        payload: Vec<u8>,
        opts: CallOptions,
    ) -> Result<Vec<u8>, ServiceError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(ServiceError::TooLarge);
        }
        let s = &self.services;
        let Some(p) = s.provider(service, major).cloned() else {
            return Err(ServiceError::Unavailable);
        };
        if p.minor < minor {
            return Err(ServiceError::Incompatible);
        }
        if p.plugin == HOST_CALLER {
            let Some(native) = s.native(service).cloned() else {
                return Err(ServiceError::Unavailable);
            };
            let wait = Duration::from_millis(u64::from(
                opts.timeout_ms
                    .unwrap_or(self.cfg.services.default_timeout_ms),
            ));
            let call = native.call(caller.to_string(), method, payload);
            return match tokio::time::timeout(wait, call).await {
                Ok(Ok(out)) if out.len() > MAX_PAYLOAD => Err(ServiceError::TooLarge),
                Ok(r) => r,
                Err(_) => Err(ServiceError::Timeout),
            };
        }
        let Some(slot) = self.plugins.get(&p.plugin).cloned() else {
            return Err(ServiceError::Unavailable);
        };
        if s.is_off(service) || !slot.status().is_coming() {
            return Err(ServiceError::Unavailable);
        }
        let chain = s.chain(opts.ctx);
        let depth = chain.map_or(1, |c| c.depth + 1);
        if depth > self.cfg.services.max_depth {
            return Err(ServiceError::TooDeep);
        }
        let now = Instant::now();
        let mut deadline = Duration::from_millis(u64::from(
            opts.timeout_ms
                .unwrap_or(self.cfg.services.default_timeout_ms),
        ));
        if let Some(max) = p.max_timeout_ms {
            deadline = deadline.min(Duration::from_millis(u64::from(max)));
        }
        if let Some(c) = chain {
            deadline = deadline.min(c.deadline.saturating_duration_since(now));
        }
        if deadline.is_zero() {
            return Err(ServiceError::Timeout);
        }
        let Some(me) = self.self_arc() else {
            return Err(ServiceError::Unavailable);
        };
        let pair = (caller.to_string(), p.plugin.clone());
        {
            let Ok(mut pending) = s.pending.lock() else {
                return Err(ServiceError::Overloaded);
            };
            let Ok(mut pairs) = s.pairs.lock() else {
                return Err(ServiceError::Overloaded);
            };
            let n = pending.entry(p.plugin.clone()).or_insert(0);
            let m = pairs.entry(pair.clone()).or_insert(0);
            if *n >= self.cfg.services.queue_per_provider
                || *m >= self.cfg.services.in_flight_per_pair
            {
                return Err(ServiceError::Overloaded);
            }
            *n += 1;
            *m += 1;
        }
        let ctx = s.next_ctx.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut c) = s.chains.lock() {
            c.insert(
                ctx,
                Chain {
                    depth,
                    deadline: now + deadline,
                },
            );
        }
        let guard = CallGuard {
            host: me,
            provider: p.plugin.clone(),
            pair,
            ctx,
        };
        let call = ServiceCall {
            service: service.to_string(),
            major,
            minor,
            method,
            payload,
            caller: caller.to_string(),
            player: opts.player,
            ctx,
            deadline_ms: u32::try_from(deadline.as_millis()).unwrap_or(u32::MAX),
        };
        let wait = deadline.min(Duration::from_millis(self.cfg.services.restart_wait_ms));
        let r = slot
            .call(wait, deadline, move |acc, g| {
                Box::pin(async move {
                    let _guard = guard;
                    g.call_on_service_call(acc, call).await
                })
            })
            .await;
        match r {
            Ok(Ok(out)) if out.len() > MAX_PAYLOAD => Err(ServiceError::TooLarge),
            Ok(Ok(out)) => Ok(out),
            Ok(Err(CallReject::UnknownMethod)) => Err(ServiceError::UnknownMethod),
            Ok(Err(CallReject::Rejected(e))) => Err(ServiceError::Rejected(e)),
            Err(CallError::Timeout) => Err(ServiceError::Timeout),
            Err(CallError::Overloaded) => Err(ServiceError::Overloaded),
            Err(CallError::Unavailable) => Err(ServiceError::Unavailable),
            Err(CallError::Failed(_)) => Err(ServiceError::ProviderFailed),
        }
    }

    /// `has-offline` through the provider's `pumbo:permissions` (plan §5.8.4),
    /// cached; `Ok(None)` = no entry or no provider.
    pub(crate) async fn offline_from_provider(
        &self,
        _caller: &PluginSlot,
        id: crate::wit::types::Uuid,
        node: &str,
        at: &crate::wit::types::QueryContext,
    ) -> Result<Option<bool>, ServiceError> {
        if self.perm_provider.is_none() {
            return Ok(None);
        }
        let uuid = crate::uuid_of(&id);
        let context = ctx_string(at);
        let key = (uuid, node.to_ascii_lowercase(), context.clone());
        let ttl = Duration::from_millis(self.cfg.permissions.offline_cache_ms);
        if let Ok(c) = self.services.offline.lock()
            && let Some((t, v)) = c.get(&key)
            && t.elapsed() < ttl
        {
            return Ok(*v);
        }
        let req = pumbo_contracts::CheckOffline {
            uuid: uuid.simple().to_string(),
            node: key.1.clone(),
            context,
        };
        let mut payload = Vec::new();
        ciborium::into_writer(&req, &mut payload).map_err(|_| ServiceError::TooLarge)?;
        let c = pumbo_contracts::PERMISSIONS;
        let out = self
            .call_service(
                HOST_CALLER,
                c.name,
                c.major,
                c.minor,
                pumbo_contracts::METHOD_CHECK_OFFLINE.into(),
                payload,
                CallOptions {
                    timeout_ms: Some(self.cfg.permissions.offline_timeout_ms),
                    player: None,
                    ctx: None,
                },
            )
            .await?;
        let answer: pumbo_contracts::CheckOfflineAnswer = ciborium::from_reader(out.as_slice())
            .map_err(|e| ServiceError::Rejected(format!("answer: {e}")))?;
        if let Ok(mut cache) = self.services.offline.lock() {
            if cache.len() > 10_000 {
                cache.clear();
            }
            cache.insert(key, (Instant::now(), answer.value));
        }
        Ok(answer.value)
    }

    /// `bus.publish`: returns at once; delivery to every subscriber's mailbox
    /// in publish order (plan §6.6.2).
    pub(crate) fn publish(
        &self,
        slot: &PluginSlot,
        topic: &str,
        payload: Vec<u8>,
    ) -> Result<(), String> {
        let (name, major, minor) =
            split_topic(topic).ok_or("topic must look like namespace:name@1.0")?;
        let declared = slot
            .manifest
            .publishes
            .iter()
            .filter_map(|t| split_topic(t))
            .any(|(n, mj, mn)| n == name && mj == major && mn >= minor);
        if !declared {
            return Err(format!("{topic} is not in publishes of the manifest"));
        }
        if name.starts_with("pumbo:") {
            let c = pumbo_contracts::topic(name)
                .ok_or_else(|| format!("{name} is not a pumbo: topic"))?;
            if c.owner != Some(slot.id.as_str()) {
                return Err(format!(
                    "only {} may publish {name}",
                    c.owner.unwrap_or("nobody")
                ));
            }
        }
        self.deliver(name, major, minor, &slot.id, payload)
    }

    /// A bus event of the host itself (`publisher` = [`HOST_CALLER`]).
    pub(crate) fn publish_native(
        &self,
        topic: &pumbo_contracts::Contract,
        payload: Vec<u8>,
    ) -> Result<(), String> {
        self.deliver(topic.name, topic.major, topic.minor, HOST_CALLER, payload)
    }

    fn deliver(
        &self,
        name: &str,
        major: u16,
        minor: u16,
        publisher: &str,
        payload: Vec<u8>,
    ) -> Result<(), String> {
        if payload.len() > MAX_EVENT {
            return Err(format!("payload over {MAX_EVENT} bytes"));
        }
        let Some(me) = self.self_arc() else {
            return Err("host stopped".into());
        };
        let payload = Arc::new(payload);
        for sub in self.plugins.values() {
            let wants = sub
                .manifest
                .subscribes
                .iter()
                .filter_map(|t| split_topic(t))
                .any(|(n, mj, _)| n == name && mj == major);
            if !wants || sub.status() != Status::Running {
                continue;
            }
            let full = {
                let Ok(mut pending) = self.services.bus_pending.lock() else {
                    continue;
                };
                let n = pending.entry(sub.id.clone()).or_insert(0);
                if *n >= self.cfg.services.bus_queue {
                    true
                } else {
                    *n += 1;
                    false
                }
            };
            if full {
                self.services.bus_dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let guard = BusGuard {
                host: Arc::clone(&me),
                subscriber: sub.id.clone(),
            };
            let (t, p, publisher) = (
                name.to_string(),
                Arc::clone(&payload),
                publisher.to_string(),
            );
            let job = actor::job(move |acc, g| {
                Box::pin(async move {
                    let _guard = guard;
                    let _ = g
                        .call_on_bus_event(acc, t, major, minor, publisher, p.to_vec())
                        .await;
                })
            });
            if sub.submit(job).is_err() {
                self.services.bus_dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }
}
