//! Host imports of `pumbo:prox` (plan §4.3). Every import is one of three
//! kinds (plan §4.4 item 1): a read of host data, a command queued for the
//! proxy that returns at once, or a future. None of them dispatches events to
//! plugins and waits for them.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pumbo_core::permissions::{PermContext, Subject};
use pumbo_text::Component;
use wasmtime::component::{Accessor, HasSelf, Resource};

use crate::HostInner;
use crate::actor::{self, PluginSlot};
use crate::bridge::{BossbarCommand, PlayerCommand};
use crate::permissions;
use crate::runtime::PluginState;
use crate::text;
use crate::wit::types::{BossbarColor, BossbarOverlay, Context, PlayerId, QueryContext, Text};
use crate::wit::virtual_ as wv;
use crate::wit::{
    admin, bossbar, bus, commands, crypto, gates, http, log, messaging, permissions as wperm,
    placeholders, players, scheduler, servers, services, types,
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
/// Inputs waiting per plugin before they are dropped.
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
        QueryContext::Group(g) => vec![PermContext::Group(g.clone()), PermContext::Global],
        QueryContext::Server(s) => permissions::levels(Some(s), &host.cfg.groups_of(s)),
    }
}

pub(crate) fn perm_context(c: &Context) -> PermContext {
    match c {
        Context::Global => PermContext::Global,
        Context::Group(g) => PermContext::Group(g.clone()),
        Context::Server(s) => PermContext::Server(s.clone()),
    }
}

impl types::Host for PluginState {}

impl players::Host for PluginState {
    fn all(&mut self) -> Vec<types::PlayerInfo> {
        self.touch();
        self.host
            .players
            .read()
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default()
    }

    fn get(&mut self, id: PlayerId) -> Option<types::PlayerInfo> {
        self.touch();
        self.host.player(id)
    }

    fn find(&mut self, name: String) -> Option<types::PlayerInfo> {
        self.touch();
        self.host
            .players
            .read()
            .ok()?
            .values()
            .find(|p| p.profile.name.eq_ignore_ascii_case(&name))
            .cloned()
    }

    fn send_message(&mut self, id: PlayerId, msg: Text) {
        self.touch();
        let c = self.text(&msg, Some(id));
        self.send(id, PlayerCommand::Message(c));
    }

    fn send_action_bar(&mut self, id: PlayerId, msg: Text) {
        self.touch();
        let c = self.text(&msg, Some(id));
        self.send(id, PlayerCommand::ActionBar(c));
    }

    fn send_title(&mut self, id: PlayerId, title: Text, subtitle: Text, times: types::TitleTimes) {
        self.touch();
        let title = self.text(&title, Some(id));
        let subtitle = self.text(&subtitle, Some(id));
        self.send(
            id,
            PlayerCommand::Title {
                title,
                subtitle,
                times: times.into(),
            },
        );
    }

    fn clear_title(&mut self, id: PlayerId) {
        self.touch();
        self.send(id, PlayerCommand::ClearTitle);
    }

    fn play_sound(&mut self, id: PlayerId, sound: String, volume: f32, pitch: f32) {
        self.touch();
        if sound.len() <= 256 {
            self.send(
                id,
                PlayerCommand::Sound {
                    sound,
                    volume,
                    pitch,
                },
            );
        }
    }

    fn tab_header_footer(&mut self, id: PlayerId, header: Text, footer: Text) {
        self.touch();
        let header = self.text(&header, Some(id));
        let footer = self.text(&footer, Some(id));
        self.send(id, PlayerCommand::TabHeaderFooter { header, footer });
    }

    fn kick(&mut self, id: PlayerId, reason: Text) {
        self.touch();
        let c = self.text(&reason, Some(id));
        tracing::info!(plugin = %self.slot.id, player = id, "kick: {}", c.plain_text());
        self.send(id, PlayerCommand::Kick(c));
    }

    fn set_property(&mut self, id: PlayerId, prop: types::Property) -> Result<(), String> {
        self.touch();
        self.check_property(id, &prop.name)?;
        if prop.name.len() > 64
            || prop.value.len() > 32 * 1024
            || prop.signature.as_ref().is_some_and(|s| s.len() > 4096)
        {
            return Err("property too large".into());
        }
        if let Ok(mut players) = self.host.players.write()
            && let Some(p) = players.get_mut(&id)
        {
            p.profile.properties.retain(|q| q.name != prop.name);
            p.profile.properties.push(prop.clone());
        }
        self.send(id, PlayerCommand::SetProperty(prop));
        Ok(())
    }

    fn remove_property(&mut self, id: PlayerId, name: String) -> Result<(), String> {
        self.touch();
        self.check_property(id, &name)?;
        if let Ok(mut players) = self.host.players.write()
            && let Some(p) = players.get_mut(&id)
        {
            p.profile.properties.retain(|q| q.name != name);
        }
        self.send(id, PlayerCommand::RemoveProperty(name));
        Ok(())
    }
}

impl PluginState {
    fn check_property(&self, id: PlayerId, name: &str) -> Result<(), String> {
        if !self
            .slot
            .manifest
            .profile_properties
            .iter()
            .any(|p| p == name)
        {
            return Err(format!(
                "property \"{name}\" is not in profile-properties of the manifest"
            ));
        }
        let player = self.host.player(id).ok_or("unknown player")?;
        if player.online_mode && !self.host.cfg.plugins.allow_premium_profile_changes {
            return Err("profiles of premium players cannot be changed".into());
        }
        Ok(())
    }
}

impl players::HostWithStore<PluginState> for HasSelf<PluginState> {
    async fn connect(
        accessor: &Accessor<PluginState, Self>,
        id: PlayerId,
        server: String,
    ) -> Result<(), players::ConnectError> {
        let bridge = accessor.with(|mut a| {
            let s = a.get();
            s.touch();
            Arc::clone(&s.host.bridge)
        });
        bridge.connect(id, server).await
    }

    async fn reconnect(
        accessor: &Accessor<PluginState, Self>,
        id: PlayerId,
    ) -> Result<(), players::ConnectError> {
        let bridge = accessor.with(|mut a| {
            let s = a.get();
            s.touch();
            Arc::clone(&s.host.bridge)
        });
        bridge.reconnect(id).await
    }
}

impl bossbar::HostBar for PluginState {
    fn new(
        &mut self,
        title: Text,
        progress: f32,
        color: BossbarColor,
        overlay: BossbarOverlay,
    ) -> Resource<BarHandle> {
        self.touch();
        let id = NEXT_BAR.fetch_add(1, Ordering::Relaxed);
        let state = BarState {
            title: self.text(&title, None),
            progress: progress.clamp(0.0, 1.0),
            color,
            overlay,
            viewers: BTreeSet::new(),
        };
        if let Ok(mut b) = self.slot.bars.lock() {
            b.insert(id, state);
        }
        // The resource table only holds the id; a failed push leaks nothing
        // visible, the bar simply has no viewers.
        self.table
            .push(BarHandle(id))
            .unwrap_or_else(|_| Resource::new_own(u32::MAX))
    }

    fn set_title(&mut self, bar: Resource<BarHandle>, title: Text) {
        self.touch();
        let Some(id) = self.bar_id(&bar) else { return };
        let title = self.text(&title, None);
        let viewers = self.update_bar(id, |b| b.title = title.clone());
        for p in viewers {
            self.send(
                p,
                PlayerCommand::Bossbar(BossbarCommand::Title {
                    bar: id,
                    title: title.clone(),
                }),
            );
        }
    }

    fn set_progress(&mut self, bar: Resource<BarHandle>, progress: f32) {
        self.touch();
        let Some(id) = self.bar_id(&bar) else { return };
        let progress = progress.clamp(0.0, 1.0);
        for p in self.update_bar(id, |b| b.progress = progress) {
            self.send(
                p,
                PlayerCommand::Bossbar(BossbarCommand::Progress { bar: id, progress }),
            );
        }
    }

    fn show(&mut self, bar: Resource<BarHandle>, player: PlayerId) {
        self.touch();
        let Some(id) = self.bar_id(&bar) else { return };
        let mut shown = None;
        let _ = self.update_bar(id, |b| {
            if b.viewers.insert(player) {
                shown = Some(b.clone());
            }
        });
        if let Some(b) = shown {
            self.send(
                player,
                PlayerCommand::Bossbar(BossbarCommand::Show {
                    bar: id,
                    title: b.title,
                    progress: b.progress,
                    color: b.color,
                    overlay: b.overlay,
                }),
            );
        }
    }

    fn hide(&mut self, bar: Resource<BarHandle>, player: PlayerId) {
        self.touch();
        let Some(id) = self.bar_id(&bar) else { return };
        let mut hidden = false;
        let _ = self.update_bar(id, |b| hidden = b.viewers.remove(&player));
        if hidden {
            self.send(
                player,
                PlayerCommand::Bossbar(BossbarCommand::Hide { bar: id }),
            );
        }
    }

    fn drop(&mut self, bar: Resource<BarHandle>) -> wasmtime::Result<()> {
        self.touch();
        let handle = self.table.delete(bar)?;
        let state = self
            .slot
            .bars
            .lock()
            .ok()
            .and_then(|mut b| b.remove(&handle.0));
        if let Some(state) = state {
            for p in state.viewers {
                self.send(
                    p,
                    PlayerCommand::Bossbar(BossbarCommand::Hide { bar: handle.0 }),
                );
            }
        }
        Ok(())
    }
}

impl PluginState {
    fn bar_id(&self, bar: &Resource<BarHandle>) -> Option<u64> {
        self.table.get(bar).ok().map(|b| b.0)
    }

    /// Applies `f` to a bar and returns its viewers.
    fn update_bar(&self, id: u64, f: impl FnOnce(&mut BarState)) -> Vec<PlayerId> {
        let Ok(mut bars) = self.slot.bars.lock() else {
            return Vec::new();
        };
        match bars.get_mut(&id) {
            Some(b) => {
                f(b);
                b.viewers.iter().copied().collect()
            }
            None => Vec::new(),
        }
    }
}

impl bossbar::Host for PluginState {}

impl servers::Host for PluginState {
    fn all(&mut self) -> Vec<servers::ServerInfo> {
        self.touch();
        self.host
            .servers
            .read()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default()
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

fn game_mode(m: wv::GameMode) -> pumbo_virtual::GameMode {
    match m {
        wv::GameMode::Survival => pumbo_virtual::GameMode::Survival,
        wv::GameMode::Creative => pumbo_virtual::GameMode::Creative,
        wv::GameMode::Adventure => pumbo_virtual::GameMode::Adventure,
        wv::GameMode::Spectator => pumbo_virtual::GameMode::Spectator,
    }
}

fn position(p: wv::Position) -> pumbo_virtual::Position {
    pumbo_virtual::Position {
        x: p.x,
        y: p.y,
        z: p.z,
        yaw: p.yaw,
        pitch: p.pitch,
    }
}

fn wit_position(p: pumbo_virtual::Position) -> wv::Position {
    wv::Position {
        x: p.x,
        y: p.y,
        z: p.z,
        yaw: p.yaw,
        pitch: p.pitch,
    }
}

fn wit_input(i: pumbo_virtual::PlayerInput) -> wv::Input {
    use pumbo_virtual::Input as I;
    let id = i.player;
    match i.input {
        I::Moved {
            position,
            on_ground,
        } => wv::Input::Moved((id, wit_position(position), on_ground)),
        I::TeleportConfirmed(t) => wv::Input::TeleportConfirmed((id, t as u32)),
        I::Loaded => wv::Input::Loaded(id),
        I::Chat(m) => wv::Input::Chat((id, m)),
        I::Command(c) => wv::Input::Command((id, c)),
        I::Settings => wv::Input::Settings(id),
        I::Brand(b) => wv::Input::Brand((id, b)),
        I::PluginMessage(c, d) => wv::Input::PluginMessage((id, c, d)),
        I::KeepaliveRtt(r) => wv::Input::KeepaliveRtt((id, r)),
        I::Left => wv::Input::Left(id),
    }
}

impl PluginState {
    fn virtual_cmd(&self, id: PlayerId, cmd: pumbo_virtual::Command) {
        self.send(id, PlayerCommand::Virtual(cmd));
    }

    /// The input channel of this plugin; the first call starts the task that
    /// hands batches to `on-virtual-input`, one per tick.
    fn inputs(&self) -> tokio::sync::mpsc::Sender<pumbo_virtual::PlayerInput> {
        let Ok(mut slot_inputs) = self.slot.inputs.lock() else {
            return tokio::sync::mpsc::channel(1).0;
        };
        if let Some(tx) = slot_inputs.as_ref() {
            return tx.clone();
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel(INPUT_QUEUE);
        let host = Arc::downgrade(&self.host);
        let slot = Arc::downgrade(&self.slot);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(INPUT_TICK);
            let mut batch = Vec::new();
            loop {
                tokio::select! {
                    i = rx.recv() => match i {
                        Some(i) => batch.push(i),
                        None => return,
                    },
                    _ = tick.tick() => {
                        if batch.is_empty() {
                            continue;
                        }
                        let (Some(host), Some(slot)) = (host.upgrade(), slot.upgrade()) else {
                            return;
                        };
                        let items: Vec<wv::Input> = batch.drain(..).map(wit_input).collect();
                        if slot.manifest.listens(crate::manifest::EventKind::VirtualInput) {
                            host.notify(&slot, move |acc, g| {
                                Box::pin(async move {
                                    let _ = g.call_on_virtual_input(acc, items).await;
                                })
                            });
                        }
                    }
                }
            }
        });
        *slot_inputs = Some(tx.clone());
        tx
    }

    fn held_here(&self, id: PlayerId) -> Option<tokio::sync::oneshot::Sender<()>> {
        self.host
            .held
            .lock()
            .ok()
            .and_then(|mut h| h.remove(&(id, self.slot.id.clone())))
    }
}

impl wv::HostWorld for PluginState {
    fn new(&mut self, opts: wv::WorldOptions) -> Resource<WorldHandle> {
        self.touch();
        let world = pumbo_virtual::World::new(pumbo_virtual::WorldOptions {
            time: opts.time,
            light: opts.light.min(15),
            game_mode: game_mode(opts.game_mode),
            view_distance: opts.view_distance.clamp(1, 16),
        });
        self.table
            .push(WorldHandle(Arc::new(world)))
            .unwrap_or_else(|_| Resource::new_own(u32::MAX))
    }

    fn set_block(
        &mut self,
        w: Resource<WorldHandle>,
        pos: wv::BlockPos,
        block: String,
    ) -> Result<(), String> {
        self.touch();
        let world = self.table.get(&w).map_err(|e| e.to_string())?;
        world
            .0
            .set_block(
                pumbo_virtual::BlockPos {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                },
                &block,
            )
            .map_err(|e| e.to_string())
    }

    fn load_structure(
        &mut self,
        w: Resource<WorldHandle>,
        path: String,
        at: wv::BlockPos,
    ) -> Result<u32, String> {
        self.touch();
        // Only inside the plugin's data directory.
        let rel = std::path::Path::new(&path);
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("a path inside the plugin's data directory".into());
        }
        let file = self.host.cfg.plugins.data_dir(&self.slot.id).join(rel);
        let data = std::fs::read(&file).map_err(|e| format!("{path}: {e}"))?;
        let world = self.table.get(&w).map_err(|e| e.to_string())?;
        let placed = world
            .0
            .load_structure(
                &data,
                pumbo_virtual::BlockPos {
                    x: at.x,
                    y: at.y,
                    z: at.z,
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(u32::try_from(placed).unwrap_or(u32::MAX))
    }

    fn drop(&mut self, w: Resource<WorldHandle>) -> wasmtime::Result<()> {
        self.table.delete(w)?;
        Ok(())
    }
}

impl wv::HostMapImage for PluginState {
    fn new(&mut self, mut pixels: Vec<u8>) -> Resource<MapHandle> {
        self.touch();
        pixels.resize(pumbo_virtual::map::SIDE * pumbo_virtual::map::SIDE, 0);
        match pumbo_virtual::MapImage::new(&pixels) {
            Ok(img) => self
                .table
                .push(MapHandle(img))
                .unwrap_or_else(|_| Resource::new_own(u32::MAX)),
            Err(_) => Resource::new_own(u32::MAX),
        }
    }

    fn drop(&mut self, m: Resource<MapHandle>) -> wasmtime::Result<()> {
        self.table.delete(m)?;
        Ok(())
    }
}

impl wv::Host for PluginState {
    fn enter(
        &mut self,
        id: PlayerId,
        w: Resource<WorldHandle>,
        at: wv::Position,
        commands: Vec<String>,
    ) -> Result<(), String> {
        self.touch();
        if self.host.player(id).is_none() {
            return Err("unknown player".into());
        }
        let world = self.table.get(&w).map_err(|e| e.to_string())?.0.clone();
        let inputs = self.inputs();
        self.virtual_cmd(
            id,
            pumbo_virtual::Command::Enter {
                world,
                at: position(at),
                commands,
                tag: id,
                inputs,
            },
        );
        // A context change (plan §5.8.4): placeholders and commands follow.
        crate::Host {
            inner: self.host.clone(),
        }
        .player_server_changed(id, None, true);
        Ok(())
    }

    fn teleport(&mut self, id: PlayerId, at: wv::Position) -> u32 {
        self.touch();
        let t = (NEXT_TELEPORT.fetch_add(1, Ordering::Relaxed) % i32::MAX as u64) as u32;
        self.virtual_cmd(
            id,
            pumbo_virtual::Command::Teleport {
                at: position(at),
                id: Some(t as i32),
            },
        );
        t
    }

    fn show_map(&mut self, id: PlayerId, img: Resource<MapHandle>, hand: wv::Hand) {
        self.touch();
        let Ok(image) = self.table.get(&img).map(|m| m.0.clone()) else {
            return;
        };
        let hand = match hand {
            wv::Hand::Main => pumbo_virtual::Hand::Main,
            wv::Hand::Off => pumbo_virtual::Hand::Off,
        };
        self.virtual_cmd(id, pumbo_virtual::Command::ShowMap(image, hand));
    }

    fn clear_inventory(&mut self, id: PlayerId) {
        self.touch();
        self.virtual_cmd(id, pumbo_virtual::Command::ClearInventory);
    }

    fn set_xp(&mut self, id: PlayerId, bar: f32, level: i32) {
        self.touch();
        self.virtual_cmd(id, pumbo_virtual::Command::Xp { bar, level });
    }

    fn set_time(&mut self, id: PlayerId, ticks: i64) {
        self.touch();
        self.virtual_cmd(id, pumbo_virtual::Command::Time(ticks));
    }

    fn set_game_mode(&mut self, id: PlayerId, mode: wv::GameMode) {
        self.touch();
        self.virtual_cmd(id, pumbo_virtual::Command::GameMode(game_mode(mode)));
    }

    fn set_flying(&mut self, id: PlayerId, allow: bool, flying: bool) {
        self.touch();
        self.virtual_cmd(id, pumbo_virtual::Command::Flying { allow, flying });
    }

    fn release(&mut self, id: PlayerId) -> Result<(), String> {
        self.touch();
        // Held by this plugin's gate: the next gate (or a server) follows.
        if let Some(tx) = self.held_here(id) {
            let _ = tx.send(());
            return Ok(());
        }
        if self.host.player(id).is_none() {
            return Err("unknown player".into());
        }
        self.virtual_cmd(id, pumbo_virtual::Command::Release);
        Ok(())
    }
}

impl gates::Host for PluginState {
    fn release(&mut self, id: PlayerId) -> Result<(), String> {
        self.touch();
        let tx = self
            .host
            .held
            .lock()
            .ok()
            .and_then(|mut h| h.remove(&(id, self.slot.id.clone())));
        match tx {
            Some(tx) => {
                let _ = tx.send(());
                Ok(())
            }
            None => Err("player is not held by this gate".into()),
        }
    }
}

impl wperm::Host for PluginState {
    fn has(&mut self, id: PlayerId, node: String, at: QueryContext) -> bool {
        self.touch();
        let lv = query_levels(&self.host, Some(id), &at);
        self.host.has(id, &node, &lv)
    }

    fn set(
        &mut self,
        id: PlayerId,
        node: String,
        value: Option<bool>,
        ctx: Context,
    ) -> Result<(), String> {
        self.touch();
        if !self.slot.manifest.permissions_write {
            return Err("the manifest has no permissions-write".into());
        }
        if !self
            .host
            .perms
            .set_node(id, &node, value, perm_context(&ctx))
        {
            return Err("unknown player".into());
        }
        self.send(id, PlayerCommand::CommandsChanged);
        Ok(())
    }

    fn replace(&mut self, id: PlayerId, set: wperm::PermissionSet) -> Result<(), String> {
        self.touch();
        if !self.host.is_permission_provider(&self.slot) {
            return Err(
                "only the configured permission provider may replace permission sets".into(),
            );
        }
        let entries = crate::dispatch::provider_entries(set);
        if !self.host.perms.replace_provider_layer(id, entries) {
            return Err("unknown player".into());
        }
        self.send(id, PlayerCommand::CommandsChanged);
        Ok(())
    }
}

impl wperm::HostWithStore<PluginState> for HasSelf<PluginState> {
    async fn has_offline(
        accessor: &Accessor<PluginState, Self>,
        id: types::Uuid,
        node: String,
        at: QueryContext,
    ) -> Result<bool, services::ServiceError> {
        let (host, slot) = accessor.with(|mut a| {
            let s = a.get();
            s.touch();
            (Arc::clone(&s.host), Arc::clone(&s.slot))
        });
        let at = match at {
            QueryContext::Current => QueryContext::Global,
            other => other,
        };
        // The provider decides while it runs (it took the file over); the
        // file when it is not there or does not answer.
        if host.provider_slot().is_some() {
            match host.offline_from_provider(&slot, id, &node, &at).await {
                Ok(v) => return Ok(v.unwrap_or_else(|| host.declared_default(&node))),
                Err(e) => host.warn_file_fallback(&format!("does not answer ({e:?})")),
            }
        }
        let lv = query_levels(&host, None, &at);
        let who = Subject {
            uuid: crate::uuid_of(&id),
            name: String::new(),
        };
        let file = host
            .perms
            .file
            .read()
            .map(|f| f.entries_for(&who))
            .unwrap_or_default();
        match permissions::resolve(&file, &[], &node, &lv) {
            Some(d) => Ok(d.value),
            None => Ok(host.declared_default(&node)),
        }
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
            s.touch();
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
            s.touch();
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
        accessor.with(|mut a| a.get().touch());
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
            s.touch();
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
            s.touch();
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
            s.touch();
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
            s.touch();
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
