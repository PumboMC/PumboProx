//! Host imports of `pumbo:prox` (plan §4.3). Every import is one of three
//! kinds (plan §4.4 item 1): a read of host data, a command queued for the
//! proxy that returns at once, or a future. None of them dispatches events to
//! plugins and waits for them.

mod bars;
mod perms;
mod player;
mod world;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use pumbo_core::permissions::PermContext;
use pumbo_text::Component;
use wasmtime::component::{Accessor, HasSelf};

use crate::HostInner;
use crate::actor::{self, PluginSlot};
use crate::bridge::{BossbarCommand, PlayerCommand};
use crate::permissions;
use crate::runtime::PluginState;
use crate::text;
use crate::wit::types::{BossbarColor, BossbarOverlay, Context, PlayerId, QueryContext, Text};
use crate::wit::{
    admin, bus, commands, crypto, http, log, messaging, placeholders, scheduler, servers, services,
    types,
};

/// Plugin messages a plugin may not send: they belong to the proxy.
const RESERVED_CHANNELS: &[&str] = &["bungeecord:main", "velocity:player_info"];

/// Channels only the proxy speaks (`pumbo:bridge*` as a prefix).
fn reserved(channel: &str) -> bool {
    RESERVED_CHANNELS.contains(&channel) || channel.starts_with("pumbo:bridge")
}
const MAX_PLUGIN_MESSAGE: usize = 1024 * 1024;
const MAX_LOG_LINE: usize = 2048;
const MAX_SLEEP_MS: u64 = 3_600_000;
const MIN_EVERY_MS: u64 = 50;

static NEXT_BAR: AtomicU64 = AtomicU64::new(1);

/// The `bar` resource: the id of a bar whose state the plugin slot keeps.
pub struct BarHandle(pub u64);

/// The `world` resource of `virtual`.
pub struct WorldHandle(pub Arc<pumbo_virtual::World>);

/// The `map-image` resource of `virtual`.
pub struct MapHandle(pub pumbo_virtual::MapImage);

/// Teleport IDs the host hands out (`virtual.teleport` returns one at once).
static NEXT_TELEPORT: AtomicU64 = AtomicU64::new(1_000_000);
/// Inputs in the channel of one plugin. The batching task reads it all the
/// time and bounds what it keeps ([`crate::inputs`]); the session drops
/// inputs only when the channel itself is full
/// (`pumboprox_dropped_packets_total{kind="virtual_input"}`).
const INPUT_QUEUE: usize = 4096;
/// One `on-virtual-input` per tick at most.
const INPUT_TICK: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub(crate) struct BarState {
    pub title: Component,
    pub progress: f32,
    pub color: BossbarColor,
    pub overlay: BossbarOverlay,
    pub viewers: BTreeSet<PlayerId>,
}

/// Hides every bar of a plugin (its instance ended).
pub(crate) fn hide_all_bars(host: &HostInner, slot: &PluginSlot) {
    let bars: Vec<(u64, BarState)> = slot
        .bars
        .lock()
        .map(|mut b| b.drain().collect())
        .unwrap_or_default();
    for (id, bar) in bars {
        for p in bar.viewers {
            host.bridge
                .send(p, PlayerCommand::Bossbar(BossbarCommand::Hide { bar: id }));
        }
    }
}

pub(crate) fn forget_viewer(slot: &PluginSlot, player: PlayerId) {
    if let Ok(mut bars) = slot.bars.lock() {
        for b in bars.values_mut() {
            b.viewers.remove(&player);
        }
    }
}

impl PluginState {
    fn text(&self, t: &Text, player: Option<PlayerId>) -> Component {
        text::component(&self.host, t, player, Some(&self.slot))
    }

    fn send(&self, id: PlayerId, cmd: PlayerCommand) {
        self.host.bridge.send(id, cmd);
    }
}

/// Levels of a query context.
pub(crate) fn query_levels(
    host: &HostInner,
    player: Option<PlayerId>,
    at: &QueryContext,
) -> Vec<PermContext> {
    match at {
        QueryContext::Current => match player {
            Some(id) => host.player_levels(id),
            None => permissions::levels(None, &[]),
        },
        QueryContext::Global => permissions::levels(None, &[]),
        QueryContext::Group(g) => vec![PermContext::group(g), PermContext::Global],
        QueryContext::Server(s) => permissions::levels(Some(s), &host.cfg.groups_of(s)),
    }
}

pub(crate) fn perm_context(c: &Context) -> PermContext {
    match c {
        Context::Global => PermContext::Global,
        Context::Group(g) => PermContext::group(g),
        Context::Server(s) => PermContext::server(s),
    }
}

impl types::Host for PluginState {}
impl servers::Host for PluginState {
    fn all(&mut self) -> Vec<servers::ServerInfo> {
        self.touch();
        self.host.server_list()
    }
}

impl commands::Host for PluginState {
    fn register(&mut self, spec: commands::CommandSpec) -> Result<(), String> {
        self.touch();
        if self.host.commands.register(&self.slot, spec)? {
            self.host.broadcast(PlayerCommand::CommandsChanged);
        }
        Ok(())
    }
}

impl services::Host for PluginState {
    fn lookup(&mut self, service: String) -> Option<services::ServiceRef> {
        self.touch();
        self.host.service_lookup(&self.slot, &service)
    }

    fn set_available(&mut self, service: String, available: bool) -> Result<(), String> {
        self.touch();
        self.host
            .service_set_available(&self.slot, &service, available)
    }
}

impl services::HostWithStore<PluginState> for HasSelf<PluginState> {
    async fn call(
        accessor: &Accessor<PluginState, Self>,
        service: String,
        method: String,
        payload: Vec<u8>,
        opts: services::CallOptions,
    ) -> Result<Vec<u8>, services::ServiceError> {
        let (host, slot) = accessor.with(|mut a| {
            let s = a.get();
            s.waited();
            (Arc::clone(&s.host), Arc::clone(&s.slot))
        });
        host.service_call(&slot, service, method, payload, opts)
            .await
    }
}

impl bus::Host for PluginState {
    fn publish(&mut self, topic: String, payload: Vec<u8>) -> Result<(), String> {
        self.touch();
        self.host.publish(&self.slot, &topic, payload)
    }
}

impl admin::Host for PluginState {
    fn counter_add(
        &mut self,
        name: String,
        value: u64,
        labels: Vec<(String, String)>,
    ) -> Result<(), String> {
        self.touch();
        #[allow(clippy::cast_precision_loss)]
        self.host.metric(
            &self.slot,
            &name,
            admin::MetricKind::Counter,
            value as f64,
            labels,
        )
    }

    fn gauge_set(
        &mut self,
        name: String,
        value: f64,
        labels: Vec<(String, String)>,
    ) -> Result<(), String> {
        self.touch();
        self.host
            .metric(&self.slot, &name, admin::MetricKind::Gauge, value, labels)
    }

    fn histogram_record(
        &mut self,
        name: String,
        value: f64,
        labels: Vec<(String, String)>,
    ) -> Result<(), String> {
        self.touch();
        self.host.metric(
            &self.slot,
            &name,
            admin::MetricKind::Histogram,
            value,
            labels,
        )
    }
}

impl placeholders::Host for PluginState {
    fn set(
        &mut self,
        key: String,
        player: Option<PlayerId>,
        value: Text,
        ctx: Context,
    ) -> Result<(), String> {
        self.touch();
        self.host
            .placeholder_set(&self.slot, &key, player, value, ctx)
    }

    fn clear(&mut self, key: String, player: Option<PlayerId>, ctx: Context) {
        self.touch();
        self.host.placeholder_clear(&self.slot, &key, player, ctx);
    }

    fn invalidate(&mut self, key: String, player: Option<PlayerId>) {
        self.touch();
        self.host.placeholder_invalidate(&self.slot, &key, player);
    }
}

impl placeholders::HostWithStore<PluginState> for HasSelf<PluginState> {
    async fn resolve(
        accessor: &Accessor<PluginState, Self>,
        t: types::TextTemplate,
        player: Option<PlayerId>,
        at: QueryContext,
    ) -> Result<Text, placeholders::ResolveError> {
        let (host, slot) = accessor.with(|mut a| {
            let s = a.get();
            s.waited();
            (Arc::clone(&s.host), Arc::clone(&s.slot))
        });
        host.resolve_template(&t, player, &at, Some(&slot))
            .await
            .map(Text::Mini)
    }
}

impl scheduler::Host for PluginState {
    fn after(&mut self, ms: u64) -> u64 {
        self.touch();
        self.timer(ms, false)
    }

    fn every(&mut self, ms: u64) -> u64 {
        self.touch();
        self.timer(ms.max(MIN_EVERY_MS), true)
    }

    fn cancel(&mut self, timer: u64) {
        self.touch();
        if let Ok(mut t) = self.slot.timers.lock()
            && let Some(h) = t.remove(&timer)
        {
            h.abort();
        }
    }
}

impl PluginState {
    /// 0 means the timer was refused (limit per plugin).
    fn timer(&mut self, ms: u64, repeat: bool) -> u64 {
        let slot = Arc::clone(&self.slot);
        let Ok(mut timers) = slot.timers.lock() else {
            return 0;
        };
        if timers.len() >= self.host.cfg.plugins.timers_per_plugin {
            return 0;
        }
        let id = slot.next_id();
        let s = Arc::clone(&slot);
        let period = Duration::from_millis(ms.min(MAX_SLEEP_MS));
        let task = tokio::spawn(async move {
            let mut next = tokio::time::Instant::now() + period;
            loop {
                tokio::time::sleep_until(next).await;
                // A full mailbox skips a tick instead of queueing without bound.
                let _ = s.submit(actor::job(move |acc, g| {
                    Box::pin(async move {
                        let _ = g.call_on_timer(acc, id).await;
                    })
                }));
                if !repeat {
                    if let Ok(mut t) = s.timers.lock() {
                        t.remove(&id);
                    }
                    return;
                }
                next += period;
            }
        });
        timers.insert(id, task.abort_handle());
        id
    }
}

impl scheduler::HostWithStore<PluginState> for HasSelf<PluginState> {
    async fn sleep(accessor: &Accessor<PluginState, Self>, ms: u64) {
        accessor.with(|mut a| a.get().waited());
        if ms == 0 {
            tokio::task::yield_now().await;
        } else {
            tokio::time::sleep(Duration::from_millis(ms.min(MAX_SLEEP_MS))).await;
        }
    }
}

fn valid_channel(c: &str) -> bool {
    let Some((ns, path)) = c.split_once(':') else {
        return false;
    };
    c.len() <= 128
        && !ns.is_empty()
        && !path.is_empty()
        && c.bytes().all(|b| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'_' | b'-' | b'.' | b'/' | b':')
        })
}

impl messaging::Host for PluginState {
    fn subscribe(&mut self, channel: String) {
        self.touch();
        if valid_channel(&channel)
            && !reserved(&channel)
            && let Ok(mut c) = self.slot.channels.lock()
        {
            c.insert(channel);
        }
    }

    fn send(&mut self, id: PlayerId, channel: String, data: Vec<u8>, to: messaging::Side) -> bool {
        self.touch();
        if !valid_channel(&channel) || reserved(&channel) || data.len() > MAX_PLUGIN_MESSAGE {
            return false;
        }
        if self.host.player(id).is_none() {
            return false;
        }
        PluginState::send(
            self,
            id,
            PlayerCommand::PluginMessage {
                channel,
                data,
                to_backend: to == messaging::Side::Backend,
            },
        );
        true
    }
}

impl crypto::Host for PluginState {}

impl crypto::HostWithStore<PluginState> for HasSelf<PluginState> {
    async fn argon2id_hash(
        accessor: &Accessor<PluginState, Self>,
        password: String,
        params: crypto::Argon2Params,
    ) -> Result<String, String> {
        let host = accessor.with(|mut a| {
            let s = a.get();
            s.waited();
            Arc::clone(&s.host)
        });
        crate::crypto::argon2id_hash(&host.crypto, password, params).await
    }

    async fn argon2id_verify(
        accessor: &Accessor<PluginState, Self>,
        password: String,
        phc: String,
    ) -> bool {
        let host = accessor.with(|mut a| {
            let s = a.get();
            s.waited();
            Arc::clone(&s.host)
        });
        crate::crypto::argon2id_verify(&host.crypto, password, phc).await
    }

    async fn bcrypt_verify(
        accessor: &Accessor<PluginState, Self>,
        password: String,
        hash: String,
    ) -> bool {
        let host = accessor.with(|mut a| {
            let s = a.get();
            s.waited();
            Arc::clone(&s.host)
        });
        crate::crypto::bcrypt_verify(&host.crypto, password, hash).await
    }
}

impl log::Host for PluginState {
    fn write(&mut self, level: log::Level, message: String) {
        self.touch();
        let limit = self.host.cfg.plugins.log_lines_per_second;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut summary = None;
        if let Ok(mut l) = self.slot.log.lock() {
            if l.second != now {
                if l.dropped > 0 {
                    summary = Some(l.dropped);
                }
                l.second = now;
                l.count = 0;
                l.dropped = 0;
            }
            if l.count >= limit {
                l.dropped += 1;
                return;
            }
            l.count += 1;
        }
        let id = &self.slot.id;
        if let Some(n) = summary {
            tracing::warn!(plugin = %id, "{n} log lines dropped (limit {limit}/s)");
        }
        let mut msg = message;
        if msg.len() > MAX_LOG_LINE {
            let mut end = MAX_LOG_LINE;
            while !msg.is_char_boundary(end) {
                end -= 1;
            }
            msg.truncate(end);
        }
        match level {
            log::Level::Trace => tracing::trace!(plugin = %id, "{msg}"),
            log::Level::Debug => tracing::debug!(plugin = %id, "{msg}"),
            log::Level::Info => tracing::info!(plugin = %id, "{msg}"),
            log::Level::Warn => tracing::warn!(plugin = %id, "{msg}"),
            log::Level::Error => tracing::error!(plugin = %id, "{msg}"),
        }
    }
}

impl http::Host for PluginState {}

impl http::HostWithStore<PluginState> for HasSelf<PluginState> {
    async fn fetch(
        accessor: &Accessor<PluginState, Self>,
        req: http::Request,
    ) -> Result<http::Response, http::HttpError> {
        let (host, slot) = accessor.with(|mut a| {
            let s = a.get();
            s.waited();
            (Arc::clone(&s.host), Arc::clone(&s.slot))
        });
        host.http.fetch(&slot, req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels() {
        assert!(valid_channel("minecraft:brand"));
        assert!(valid_channel("pumbo:example/a.b"));
        assert!(!valid_channel("brand"));
        assert!(!valid_channel("Upper:case"));
        assert!(!valid_channel(":x"));
    }
}
