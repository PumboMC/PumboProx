//! The narrow interface from the host to the proxy core.
//!
//! Imports never wait for events to be dispatched (plan §4.4 item 1): player
//! commands go to the player's session queue and return at once, long
//! operations (`connect`, `reconnect`) are futures. The proxy implements this
//! trait; tests use a recording fake.

use futures::future::BoxFuture;
use pumbo_text::Component;

use crate::wit::players::ConnectError;
use crate::wit::types::{BossbarColor, BossbarOverlay, PlayerId, Property, TitleTimes};

/// A command for a player's session. Text is already resolved for this player.
#[derive(Debug, Clone)]
pub enum PlayerCommand {
    Message(Component),
    ActionBar(Component),
    Title {
        title: Component,
        subtitle: Component,
        times: (u32, u32, u32),
    },
    ClearTitle,
    Sound {
        sound: String,
        volume: f32,
        pitch: f32,
    },
    TabHeaderFooter {
        header: Component,
        footer: Component,
    },
    Kick(Component),
    /// Applies from the next forwarding (plan §4.3).
    SetProperty(Property),
    RemoveProperty(String),
    PluginMessage {
        channel: String,
        data: Vec<u8>,
        to_backend: bool,
    },
    Bossbar(BossbarCommand),
    /// The visible proxy commands of this player changed (permissions,
    /// context or registrations): rebuild the command tree.
    CommandsChanged,
    /// The virtual world (WIT `virtual`, plan §5).
    Virtual(pumbo_virtual::Command),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BossbarCommand {
    Show {
        bar: u64,
        title: Component,
        progress: f32,
        color: BossbarColor,
        overlay: BossbarOverlay,
    },
    Title {
        bar: u64,
        title: Component,
    },
    Progress {
        bar: u64,
        progress: f32,
    },
    Hide {
        bar: u64,
    },
}

impl From<TitleTimes> for (u32, u32, u32) {
    fn from(t: TitleTimes) -> Self {
        (t.fade_in, t.stay, t.fade_out)
    }
}

pub trait ProxyBridge: Send + Sync + 'static {
    /// Queues a command for the player's session; returns at once.
    fn send(&self, player: PlayerId, cmd: PlayerCommand);
    /// Moves the player to a server (plan §3.3); `on-server-connect` runs in
    /// the session task, never on the calling plugin's stack.
    fn connect(
        &self,
        player: PlayerId,
        server: String,
    ) -> BoxFuture<'static, Result<(), ConnectError>>;
    /// Reconnects to the current backend (plan §3.3).
    fn reconnect(&self, player: PlayerId) -> BoxFuture<'static, Result<(), ConnectError>>;
}

/// Bridge for tools without a proxy (`pumboprox describe`, `check-config`):
/// commands go nowhere and connections fail.
#[derive(Debug, Default)]
pub struct NoProxy;

impl ProxyBridge for NoProxy {
    fn send(&self, _: PlayerId, _: PlayerCommand) {}

    fn connect(&self, _: PlayerId, _: String) -> BoxFuture<'static, Result<(), ConnectError>> {
        Box::pin(async { Err(ConnectError::Cancelled) })
    }

    fn reconnect(&self, _: PlayerId) -> BoxFuture<'static, Result<(), ConnectError>> {
        Box::pin(async { Err(ConnectError::Cancelled) })
    }
}
