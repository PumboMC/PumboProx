//! `bossbar`: bars a plugin shows to players.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use wasmtime::component::Resource;

use super::{BarHandle, BarState, NEXT_BAR};
use crate::bridge::{BossbarCommand, PlayerCommand};
use crate::runtime::PluginState;
use crate::wit::bossbar;
use crate::wit::types::{BossbarColor, BossbarOverlay, PlayerId, Text};

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
