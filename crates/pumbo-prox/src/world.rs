//! The virtual world in a session (PumboAPI, §5): commands to the session,
//! inputs from it, and the test gate.
//!
//! Whoever drives a player in a virtual world (the plugin host, the test
//! gate) sends [`VirtualCmd`]s through the session queue ([`Proxy::send_to`])
//! and gets the player's [`Input`]s on the channel it passed with
//! [`VirtualCmd::Enter`]. A full channel drops inputs (counted).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pumbo_text::Component;
use pumbo_virtual::physics::{FallCheck, FallParams, FallStatus};
use pumbo_virtual::{BlockPos, Hand, Input, MapImage, Position, World, WorldOptions};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::server::{Proxy, SessionCmd};
use crate::status::parse_text;

pub use pumbo_virtual::{Command as VirtualCmd, PlayerInput};

/// Boss bar operations of the proxy (WIT `bossbar`).
#[derive(Debug, Clone, PartialEq)]
pub enum BossbarOp {
    /// Show (or show again); colour and overlay are the vanilla IDs.
    Show {
        title: Component,
        progress: f32,
        color: i32,
        overlay: i32,
    },
    Progress(f32),
    Title(Component),
    Hide,
}

fn virtual_cmd(proxy: &Proxy, id: Uuid, cmd: VirtualCmd) -> bool {
    proxy.send_to(id, SessionCmd::Virtual(Box::new(cmd)))
}

// ---------------------------------------------------------------- test gate

const GATE_COMMAND: &str = "gate";
const SPAWN: (f64, f64, f64) = (0.5, 80.0, 0.5);
const PLATFORM_Y: i32 = 64;
const SECOND_SPAWN: (f64, f64, f64) = (0.5, 110.0, 0.5);
/// Map colours: base colour × 4 + shade 2 (white snow, red).
pub const MAP_WHITE: u8 = 8 * 4 + 2;
pub const MAP_RED: u8 = 28 * 4 + 2;

fn worlds() -> &'static (Arc<World>, Arc<World>) {
    static WORLDS: OnceLock<(Arc<World>, Arc<World>)> = OnceLock::new();
    WORLDS.get_or_init(|| {
        let first = World::new(WorldOptions::default());
        let second = World::new(WorldOptions::default());
        if let Err(e) = first
            .fill_layer((-2, -2), (2, 2), PLATFORM_Y, "minecraft:smooth_stone")
            .and_then(|()| second.fill_layer((-1, -1), (1, 1), 100, "minecraft:glass"))
        {
            warn!("test gate world: {e}");
        }
        (Arc::new(first), Arc::new(second))
    })
}

/// The test map: red border and diagonals on white.
pub fn test_map_pixels() -> Vec<u8> {
    let n = pumbo_virtual::map::SIDE;
    (0..n * n)
        .map(|i| {
            let (x, z) = (i % n, i / n);
            if x < 4 || z < 4 || x >= n - 4 || z >= n - 4 || x == z || x + z == n - 1 {
                MAP_RED
            } else {
                MAP_WHITE
            }
        })
        .collect()
}

fn at((x, y, z): (f64, f64, f64)) -> Position {
    Position::at(x, y, z)
}

/// Sends a player into the test gate world: right after login (`release`
/// then picks a server) or from a server (it goes back there). Tests only
/// (`virtual.test-gate`).
pub fn enter_test_gate(proxy: &Arc<Proxy>, id: Uuid) {
    let seconds = proxy.runtime().config.virtual_world.test_gate_seconds;
    tokio::spawn(test_gate(proxy.clone(), id, seconds));
}

/// The test gate: a 5×5 platform at y = 64, a fall from y = 80 checked with
/// the vanilla curve, title, bossbar and experience bar counting down, a map
/// in hand once the world shows, chat and commands answered, then release.
/// `/gate release` releases at once, `/gate world` changes the world.
async fn test_gate(proxy: Arc<Proxy>, id: Uuid, seconds: u64) {
    let (first, second) = worlds();
    let (tx, mut rx) = mpsc::channel(256);
    let entered = virtual_cmd(
        &proxy,
        id,
        VirtualCmd::Enter {
            world: first.clone(),
            at: at(SPAWN),
            commands: vec![GATE_COMMAND.into()],
            tag: 0,
            inputs: tx,
        },
    );
    if !entered {
        return;
    }
    let say = |text: &str| proxy.send_to(id, SessionCmd::Message(Box::new(parse_text(text))));
    let bar = Uuid::from_u128(0x7075_6d62_6f67_6174_6500_0000_0000_0001);
    let started = Instant::now();
    let end = (seconds > 0).then(|| started + Duration::from_secs(seconds));
    let mut fall = Some(FallCheck::new(
        SPAWN,
        FallParams {
            ticks: 10,
            max_y_difference: 0.01,
            max_y_errors: 3,
            max_xz_errors: 3,
        },
    ));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            input = rx.recv() => {
                let Some(PlayerInput { input, .. }) = input else { return };
                match input {
                    Input::Loaded => {
                        proxy.send_to(id, SessionCmd::Title {
                            title: Box::new(parse_text("&6PumboProx")),
                            subtitle: Box::new(parse_text("&7virtual world test")),
                            times: (10, 60, 10),
                        });
                        proxy.send_to(id, SessionCmd::Bossbar {
                            id: bar,
                            op: Box::new(BossbarOp::Show {
                                title: parse_text("&eTest gate"),
                                progress: 1.0,
                                color: 4,
                                overlay: 0,
                            }),
                        });
                        say("&7Test gate: &f/gate release&7, &f/gate world&7, &f/gate map&7.");
                        if let Ok(image) = MapImage::new(&test_map_pixels()) {
                            virtual_cmd(&proxy, id, VirtualCmd::ShowMap(image, Hand::Main));
                        }
                        virtual_cmd(&proxy, id, VirtualCmd::Sound {
                            name: "minecraft:block.note_block.pling".into(),
                            volume: 1.0,
                            pitch: 1.0,
                        });
                    }
                    Input::Moved { position, on_ground } => {
                        if let Some(check) = fall.as_mut() {
                            let status = check.on_move(position.x, position.y, position.z);
                            if on_ground || !matches!(status, FallStatus::Waiting | FallStatus::InProgress) {
                                say(&format!("&7fall check: {:?} ({})", status, check.summary()));
                                fall = None;
                            }
                        }
                    }
                    Input::Command(line) => {
                        let arg = line.split(' ').nth(1).unwrap_or_default();
                        match (line.split(' ').next(), arg) {
                            (Some(GATE_COMMAND), "release") => {
                                virtual_cmd(&proxy, id, VirtualCmd::Release);
                                return;
                            }
                            (Some(GATE_COMMAND), "world") => {
                                let (tx, new_rx) = mpsc::channel(256);
                                rx = new_rx;
                                virtual_cmd(&proxy, id, VirtualCmd::Enter {
                                    world: second.clone(),
                                    at: at(SECOND_SPAWN),
                                    commands: vec![GATE_COMMAND.into()],
                                    tag: 0,
                                    inputs: tx,
                                });
                                say("&7world 2");
                            }
                            (Some(GATE_COMMAND), "map") => {
                                if let Ok(image) = MapImage::new(&test_map_pixels()) {
                                    virtual_cmd(&proxy, id, VirtualCmd::ShowMap(image, Hand::Main));
                                }
                            }
                            // Arguments are never echoed: a real gate gets passwords here.
                            (root, _) => {
                                say(&format!(
                                    "&7command /{} ({} characters)",
                                    root.unwrap_or_default(),
                                    line.chars().count()
                                ));
                            }
                        }
                    }
                    Input::Chat(text) => {
                        proxy.send_to(id, SessionCmd::Message(Box::new(
                            Component::text("chat: ").append(Component::text(text)),
                        )));
                    }
                    Input::Left => return,
                    other => debug!("test gate: {other:?}"),
                }
            }
            _ = tick.tick() => {
                if let Some(end) = end {
                    let total = end.saturating_duration_since(started).as_secs_f32().max(1.0);
                    let left = end.saturating_duration_since(Instant::now()).as_secs_f32();
                    let progress = (left / total).clamp(0.0, 1.0);
                    proxy.send_to(id, SessionCmd::Bossbar { id: bar, op: Box::new(BossbarOp::Progress(progress)) });
                    virtual_cmd(&proxy, id, VirtualCmd::Xp { bar: progress, level: left.ceil() as i32 });
                }
            }
            () = sleep_until(end.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600))) => {
                if end.is_some() {
                    virtual_cmd(&proxy, id, VirtualCmd::Release);
                    return;
                }
            }
        }
    }
}

/// The platform's top, where a player lands after the fall (for tests).
pub const LANDING_Y: f64 = PLATFORM_Y as f64 + 1.0;

/// The block under the spawn (for tests).
pub const PLATFORM: BlockPos = BlockPos {
    x: 0,
    y: PLATFORM_Y,
    z: 0,
};
