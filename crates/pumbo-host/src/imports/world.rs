//! `virtual`: the virtual world and its inputs.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use wasmtime::component::Resource;

use super::{INPUT_QUEUE, INPUT_TICK, MapHandle, NEXT_TELEPORT, WorldHandle};
use crate::HostInner;
use crate::actor::PluginSlot;
use crate::bridge::PlayerCommand;
use crate::runtime::PluginState;
use crate::wit::types::PlayerId;
use crate::wit::virtual_ as wv;

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

/// Hands the next batch of inputs to `on-virtual-input`; when the plugin
/// cannot take it, its priority inputs wait for the next tick.
fn deliver_inputs(host: &HostInner, slot: &PluginSlot, queue: &mut crate::inputs::InputQueue) {
    if !slot.status().is_coming() {
        queue.clear();
        return;
    }
    let batch = queue.take();
    if !slot
        .manifest
        .listens(crate::manifest::EventKind::VirtualInput)
    {
        return;
    }
    let keep: Vec<pumbo_virtual::PlayerInput> = batch
        .iter()
        .filter(|i| crate::inputs::priority(&i.input))
        .cloned()
        .collect();
    let low = batch.len() - keep.len();
    let items: Vec<wv::Input> = batch.into_iter().map(wit_input).collect();
    let queued = host.notify(slot, move |acc, g| {
        Box::pin(async move {
            let _ = g.call_on_virtual_input(acc, items).await;
        })
    });
    if !queued {
        queue.dropped += low as u64;
        queue.give_back(keep);
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
            let mut queue = crate::inputs::InputQueue::default();
            loop {
                tokio::select! {
                    i = rx.recv() => match i {
                        Some(i) => queue.push(i),
                        None => return,
                    },
                    _ = tick.tick() => {
                        if queue.is_empty() && queue.dropped == 0 {
                            continue;
                        }
                        let (Some(host), Some(slot)) = (host.upgrade(), slot.upgrade()) else {
                            return;
                        };
                        deliver_inputs(&host, &slot, &mut queue);
                        let dropped = std::mem::take(&mut queue.dropped);
                        slot.inputs_dropped.fetch_add(dropped, Ordering::Relaxed);
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
        // Resolved like the plugin's own sandbox: the plugin can make links
        // there, but none that leads out (SEC-02). The world decompresses
        // at most 16 MiB of a structure, so a larger file is no structure.
        let data = crate::manifest::DataDir::open(&self.host.cfg.plugins.data_dir(&self.slot.id))
            .and_then(|d| d.read(rel, 16 << 20))
            .map_err(|e| format!("{path}: {e}"))?;
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
