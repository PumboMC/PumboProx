//! A fake host for `cargo test` without WASM (plan §4.6): player table,
//! recorded commands, permissions, services, placeholders, bus, config
//! files and the virtual world (calls recorded, [`virtual_calls`]). State is per thread, so every test has its own host; call
//! [`reset`] at the start of a test.
//!
//! ```ignore
//! pumbo_sdk::testing::reset();
//! let p = pumbo_sdk::testing::add_player("Steve", Some("lobby"));
//! let plugin = MyPlugin::default();
//! pumbo_sdk::testing::block_on(plugin.on_command(command(p, "hello", &[])));
//! assert_eq!(pumbo_sdk::testing::messages(p), ["Hello!"]);
//! ```

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use crate::bindings::exports::pumbo::prox::events::{CommandEvent, ServiceCall};
use crate::bindings::pumbo::prox::commands::CommandSpec;
use crate::bindings::pumbo::prox::http::{HttpError, Request, Response};
use crate::bindings::pumbo::prox::log::Level;
use crate::bindings::pumbo::prox::messaging::Side;
use crate::bindings::pumbo::prox::permissions::PermissionSet;
use crate::bindings::pumbo::prox::servers::ServerInfo;
use crate::bindings::pumbo::prox::services::{CallOptions, CallReject, ServiceError, ServiceRef};
use crate::bindings::pumbo::prox::types::{
    ClientSettings, Connection, Context, PlayerContext, PlayerId, PlayerInfo, Profile, Property,
    QueryContext, Route, Text, TitleTimes, Uuid,
};
use crate::bindings::pumbo::prox::virtual_::{BlockPos, GameMode, Hand, Position, WorldOptions};

/// What the plugin sent to a player.
#[derive(Debug, Clone, PartialEq)]
pub enum Sent {
    Message(Text),
    ActionBar(Text),
    Title(Text, Text),
    ClearTitle,
    Sound(String),
    TabHeaderFooter(Text, Text),
    Kick(Text),
    SetProperty(Property),
    RemoveProperty(String),
    PluginMessage(String, Vec<u8>, bool),
    BarShown(u64),
    BarHidden(u64),
}

/// What the plugin asked of the virtual world for a player.
#[derive(Debug, Clone, PartialEq)]
pub enum Virtual {
    /// Into the world with this id ([`backend::virtual_::World::id`]).
    Enter {
        world: u64,
        at: Position,
        commands: Vec<String>,
    },
    /// With the ID the confirmation would carry.
    Teleport(Position, u32),
    ShowMap(Hand),
    ClearInventory,
    Xp(f32, i32),
    Time(i64),
    GameMode(GameMode),
    Flying(bool, bool),
    Release,
}

type ServiceHandler = Box<dyn Fn(&str, Vec<u8>) -> Result<Vec<u8>, ServiceError>>;
type HttpHandler = Box<dyn Fn(Request) -> Result<Response, HttpError>>;

#[derive(Default)]
pub struct MockHost {
    pub players: BTreeMap<PlayerId, PlayerInfo>,
    pub servers: Vec<ServerInfo>,
    pub sent: Vec<(PlayerId, Sent)>,
    pub commands: Vec<CommandSpec>,
    /// (player, node) → value; other nodes are false.
    pub permissions: BTreeMap<(PlayerId, String), bool>,
    pub replaced: Vec<(PlayerId, PermissionSet)>,
    services: BTreeMap<String, (ServiceRef, ServiceHandler)>,
    pub service_calls: Vec<(String, String)>,
    pub unavailable: BTreeSet<String>,
    pub published: Vec<(String, Vec<u8>)>,
    /// Push values: (key, player, context) → value.
    pub placeholders: BTreeMap<(String, Option<PlayerId>, String), Text>,
    pub timers: BTreeMap<u64, (u64, bool)>,
    pub logs: Vec<(Level, String)>,
    pub metrics: Vec<(String, f64)>,
    /// Files of the config directory by relative path (`config.yml`,
    /// `servers/survival.yml`, `lang/pl.yml`).
    pub config_files: BTreeMap<String, String>,
    pub released: Vec<PlayerId>,
    pub virtual_calls: Vec<(PlayerId, Virtual)>,
    pub channels: BTreeSet<String>,
    http: Option<HttpHandler>,
    next_id: u64,
}

thread_local! {
    static HOST: RefCell<MockHost> = RefCell::new(MockHost::default());
}

pub fn reset() {
    HOST.with(|h| *h.borrow_mut() = MockHost::default());
}

/// Access to the fake host. Do not call host functions inside `f`.
pub fn with<R>(f: impl FnOnce(&mut MockHost) -> R) -> R {
    HOST.with(|h| f(&mut h.borrow_mut()))
}

/// Runs an async handler to completion.
pub fn block_on<F: Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

fn next_id() -> u64 {
    with(|h| {
        h.next_id += 1;
        h.next_id
    })
}

pub fn settings(locale: &str) -> ClientSettings {
    ClientSettings {
        locale: locale.into(),
        view_distance: 10,
        chat_mode: 0,
        chat_colors: true,
        skin_parts: 0x7f,
        main_hand_right: true,
        text_filtering: false,
        server_listing: true,
    }
}

/// Adds an online player and returns its id.
pub fn add_player(name: &str, server: Option<&str>) -> PlayerId {
    let id = next_id();
    let info = PlayerInfo {
        id,
        profile: Profile {
            id: Uuid { high: id, low: id },
            name: name.into(),
            properties: Vec::new(),
        },
        online_mode: false,
        connection: Connection {
            address: "127.0.0.1".into(),
            port: 50000,
            virtual_host: "localhost".into(),
            protocol: 777,
            original_protocol: None,
            route: Route::Direct,
        },
        brand: Some("vanilla".into()),
        settings: None,
        server: server.map(str::to_string),
        context: PlayerContext {
            server: server.map(str::to_string),
            groups: Vec::new(),
        },
        in_virtual: false,
    };
    with(|h| h.players.insert(id, info));
    id
}

pub fn grant(id: PlayerId, node: &str) {
    with(|h| h.permissions.insert((id, node.to_string()), true));
}

pub fn config_file(path: &str, text: &str) {
    with(|h| h.config_files.insert(path.to_string(), text.to_string()));
}

/// A provider of a service: `handler(method, payload)`.
pub fn provide(
    service: &str,
    major: u16,
    minor: u16,
    handler: impl Fn(&str, Vec<u8>) -> Result<Vec<u8>, ServiceError> + 'static,
) {
    let r = ServiceRef {
        name: service.into(),
        major,
        minor,
    };
    with(|h| {
        h.services
            .insert(service.to_string(), (r, Box::new(handler)))
    });
}

pub fn on_http(handler: impl Fn(Request) -> Result<Response, HttpError> + 'static) {
    with(|h| h.http = Some(Box::new(handler)));
}

/// What a player got, in order.
pub fn sent_to(id: PlayerId) -> Vec<Sent> {
    with(|h| {
        h.sent
            .iter()
            .filter(|(p, _)| *p == id)
            .map(|(_, s)| s.clone())
            .collect()
    })
}

/// What the plugin asked of the virtual world for a player, in order.
pub fn virtual_calls(id: PlayerId) -> Vec<Virtual> {
    with(|h| {
        h.virtual_calls
            .iter()
            .filter(|(p, _)| *p == id)
            .map(|(_, v)| v.clone())
            .collect()
    })
}

/// Plain view of the messages a player got (templates with arguments filled in).
pub fn messages(id: PlayerId) -> Vec<String> {
    sent_to(id)
        .into_iter()
        .filter_map(|s| match s {
            Sent::Message(t) => Some(render(&t)),
            _ => None,
        })
        .collect()
}

/// Text as the fake host shows it: templates with arguments, tags as written.
pub fn render(t: &Text) -> String {
    match t {
        Text::Plain(s) | Text::Legacy(s) | Text::Mini(s) | Text::Json(s) => s.clone(),
        Text::Template(t) => {
            let mut s = t.mini.clone();
            for (k, v) in &t.args {
                s = s.replace(&format!("{{{k}}}"), v);
            }
            s
        }
    }
}

/// A command event as the host would send it.
pub fn command(player: PlayerId, name: &str, args: &[&str]) -> CommandEvent {
    CommandEvent {
        player: Some(player),
        name: name.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
    }
}

/// A service call as the host would send it.
pub fn service_call(service: &str, major: u16, method: &str, payload: Vec<u8>) -> ServiceCall {
    ServiceCall {
        service: service.into(),
        major,
        minor: 0,
        method: method.into(),
        payload,
        caller: "test".into(),
        player: None,
        ctx: 1,
        deadline_ms: 1000,
    }
}

fn record(id: PlayerId, s: Sent) {
    with(|h| h.sent.push((id, s)));
}

fn ctx_key(c: &Context) -> String {
    match c {
        Context::Global => "global".into(),
        Context::Group(g) => format!("group={g}"),
        Context::Server(s) => format!("server={s}"),
    }
}

/// The same functions as the WASM bindings, backed by [`MockHost`].
pub mod backend {
    use super::*;

    pub mod players {
        use super::*;
        pub use crate::bindings::pumbo::prox::players::ConnectError;

        pub fn all() -> Vec<PlayerInfo> {
            with(|h| h.players.values().cloned().collect())
        }

        pub fn get(id: PlayerId) -> Option<PlayerInfo> {
            with(|h| h.players.get(&id).cloned())
        }

        pub fn find(name: &str) -> Option<PlayerInfo> {
            with(|h| {
                h.players
                    .values()
                    .find(|p| p.profile.name.eq_ignore_ascii_case(name))
                    .cloned()
            })
        }

        pub fn send_message(id: PlayerId, msg: &Text) {
            record(id, Sent::Message(msg.clone()));
        }

        pub fn send_action_bar(id: PlayerId, msg: &Text) {
            record(id, Sent::ActionBar(msg.clone()));
        }

        pub fn send_title(id: PlayerId, title: &Text, subtitle: &Text, _: TitleTimes) {
            record(id, Sent::Title(title.clone(), subtitle.clone()));
        }

        pub fn clear_title(id: PlayerId) {
            record(id, Sent::ClearTitle);
        }

        pub fn play_sound(id: PlayerId, sound: &str, _: f32, _: f32) {
            record(id, Sent::Sound(sound.into()));
        }

        pub fn tab_header_footer(id: PlayerId, header: &Text, footer: &Text) {
            record(id, Sent::TabHeaderFooter(header.clone(), footer.clone()));
        }

        pub fn kick(id: PlayerId, reason: &Text) {
            record(id, Sent::Kick(reason.clone()));
        }

        pub async fn connect(id: PlayerId, server: String) -> Result<(), ConnectError> {
            let known =
                with(|h| h.servers.is_empty() || h.servers.iter().any(|s| s.name == server));
            if !known {
                return Err(ConnectError::UnknownServer);
            }
            with(|h| match h.players.get_mut(&id) {
                Some(p) => {
                    p.server = Some(server.clone());
                    p.context.server = Some(server);
                    Ok(())
                }
                None => Err(ConnectError::Cancelled),
            })
        }

        pub async fn reconnect(id: PlayerId) -> Result<(), ConnectError> {
            with(|h| {
                if h.players.contains_key(&id) {
                    Ok(())
                } else {
                    Err(ConnectError::Cancelled)
                }
            })
        }

        pub fn set_property(id: PlayerId, prop: &Property) -> Result<(), String> {
            record(id, Sent::SetProperty(prop.clone()));
            Ok(())
        }

        pub fn remove_property(id: PlayerId, name: &str) -> Result<(), String> {
            record(id, Sent::RemoveProperty(name.into()));
            Ok(())
        }
    }

    pub mod bossbar {
        use super::*;
        use crate::bindings::pumbo::prox::types::{BossbarColor, BossbarOverlay};

        pub struct Bar {
            pub id: u64,
        }

        impl Bar {
            pub fn new(_: &Text, _: f32, _: BossbarColor, _: BossbarOverlay) -> Bar {
                Bar { id: next_id() }
            }

            pub fn set_title(&self, _: &Text) {}

            pub fn set_progress(&self, _: f32) {}

            pub fn show(&self, id: PlayerId) {
                record(id, Sent::BarShown(self.id));
            }

            pub fn hide(&self, id: PlayerId) {
                record(id, Sent::BarHidden(self.id));
            }
        }
    }

    pub mod servers {
        use super::*;

        pub fn all() -> Vec<ServerInfo> {
            with(|h| h.servers.clone())
        }
    }

    pub mod commands {
        use super::*;

        pub fn register(spec: &CommandSpec) -> Result<(), String> {
            with(|h| {
                if h.commands.iter().any(|c| c.name == spec.name) {
                    return Err(format!("command \"{}\" is registered", spec.name));
                }
                h.commands.push(spec.clone());
                Ok(())
            })
        }
    }

    pub mod gates {
        use super::*;

        pub fn release(id: PlayerId) -> Result<(), String> {
            with(|h| h.released.push(id));
            Ok(())
        }
    }

    /// The virtual world: calls are recorded per player, `enter` and
    /// `release` set `in_virtual` of the player.
    pub mod virtual_ {
        use super::*;

        /// A world of the fake host: its options and the blocks set.
        pub struct World {
            pub id: u64,
            pub options: WorldOptions,
            blocks: RefCell<Vec<(BlockPos, String)>>,
        }

        impl World {
            pub fn new(options: WorldOptions) -> World {
                World {
                    id: next_id(),
                    options,
                    blocks: RefCell::default(),
                }
            }

            pub fn set_block(&self, pos: BlockPos, block: &str) -> Result<(), String> {
                let mut b = self.blocks.borrow_mut();
                b.retain(|(p, _)| *p != pos);
                b.push((pos, block.to_string()));
                Ok(())
            }

            /// Places nothing: the fake host has no data files.
            pub fn load_structure(&self, _: &str, _: BlockPos) -> Result<u32, String> {
                Ok(0)
            }

            pub fn blocks(&self) -> Vec<(BlockPos, String)> {
                self.blocks.borrow().clone()
            }
        }

        pub struct MapImage {
            pub pixels: Vec<u8>,
        }

        impl MapImage {
            pub fn new(pixels: &[u8]) -> MapImage {
                MapImage {
                    pixels: pixels.to_vec(),
                }
            }
        }

        fn call(id: PlayerId, v: Virtual) {
            with(|h| h.virtual_calls.push((id, v)));
        }

        /// Sets `in_virtual` of a known player.
        fn set_in(id: PlayerId, inside: bool, v: Virtual) -> Result<(), String> {
            with(|h| {
                let p = h.players.get_mut(&id).ok_or("unknown player")?;
                p.in_virtual = inside;
                h.virtual_calls.push((id, v));
                Ok(())
            })
        }

        pub fn enter(
            id: PlayerId,
            w: &World,
            at: Position,
            commands: &[String],
        ) -> Result<(), String> {
            let world = w.id;
            let commands = commands.to_vec();
            set_in(
                id,
                true,
                Virtual::Enter {
                    world,
                    at,
                    commands,
                },
            )
        }

        pub fn teleport(id: PlayerId, at: Position) -> u32 {
            let t = u32::try_from(next_id()).unwrap_or(u32::MAX);
            call(id, Virtual::Teleport(at, t));
            t
        }

        pub fn show_map(id: PlayerId, _: &MapImage, hand: Hand) {
            call(id, Virtual::ShowMap(hand));
        }

        pub fn clear_inventory(id: PlayerId) {
            call(id, Virtual::ClearInventory);
        }

        pub fn set_xp(id: PlayerId, bar: f32, level: i32) {
            call(id, Virtual::Xp(bar, level));
        }

        pub fn set_time(id: PlayerId, ticks: i64) {
            call(id, Virtual::Time(ticks));
        }

        pub fn set_game_mode(id: PlayerId, mode: GameMode) {
            call(id, Virtual::GameMode(mode));
        }

        pub fn set_flying(id: PlayerId, allow: bool, flying: bool) {
            call(id, Virtual::Flying(allow, flying));
        }

        pub fn release(id: PlayerId) -> Result<(), String> {
            set_in(id, false, Virtual::Release)
        }
    }

    pub mod services {
        use super::*;

        pub async fn call(
            service: String,
            method: String,
            payload: Vec<u8>,
            _: CallOptions,
        ) -> Result<Vec<u8>, ServiceError> {
            if payload.len() > 256 * 1024 {
                return Err(ServiceError::TooLarge);
            }
            if with(|h| h.unavailable.contains(&service)) {
                return Err(ServiceError::Unavailable);
            }
            with(|h| h.service_calls.push((service.clone(), method.clone())));
            // The handler runs outside the borrow so it may use the fake host.
            let handler = with(|h| h.services.remove(&service));
            let Some((r, handler)) = handler else {
                return Err(ServiceError::Unavailable);
            };
            let out = handler(&method, payload);
            with(|h| h.services.insert(service, (r, handler)));
            out
        }

        pub fn lookup(service: &str) -> Option<ServiceRef> {
            with(|h| {
                if h.unavailable.contains(service) {
                    return None;
                }
                h.services.get(service).map(|(r, _)| r.clone())
            })
        }

        pub fn set_available(service: &str, available: bool) -> Result<(), String> {
            with(|h| {
                if available {
                    h.unavailable.remove(service);
                } else {
                    h.unavailable.insert(service.to_string());
                }
            });
            Ok(())
        }
    }

    pub mod permissions {
        use super::*;

        pub fn has(id: PlayerId, node: &str, _: &QueryContext) -> bool {
            with(|h| {
                h.permissions
                    .get(&(id, node.to_string()))
                    .copied()
                    .unwrap_or(false)
            })
        }

        pub async fn has_offline(
            id: Uuid,
            node: String,
            _: QueryContext,
        ) -> Result<bool, ServiceError> {
            with(|h| {
                let p = h
                    .players
                    .values()
                    .find(|p| p.profile.id == id)
                    .map(|p| p.id);
                Ok(p.and_then(|p| h.permissions.get(&(p, node)).copied())
                    .unwrap_or(false))
            })
        }

        pub fn set(
            id: PlayerId,
            node: &str,
            value: Option<bool>,
            _: &Context,
        ) -> Result<(), String> {
            with(|h| match value {
                Some(v) => h.permissions.insert((id, node.to_string()), v),
                None => h.permissions.remove(&(id, node.to_string())),
            });
            Ok(())
        }

        pub fn replace(id: PlayerId, entries: &PermissionSet) -> Result<(), String> {
            with(|h| h.replaced.push((id, entries.clone())));
            Ok(())
        }
    }

    pub mod bus {
        use super::*;

        pub fn publish(topic: &str, payload: &[u8]) -> Result<(), String> {
            if payload.len() > 64 * 1024 {
                return Err("payload over 64 KB".into());
            }
            with(|h| h.published.push((topic.to_string(), payload.to_vec())));
            Ok(())
        }
    }

    pub mod admin {
        use super::*;

        fn push(name: &str, v: f64) -> Result<(), String> {
            with(|h| h.metrics.push((name.to_string(), v)));
            Ok(())
        }

        pub fn counter_add(name: &str, value: u64, _: &[(String, String)]) -> Result<(), String> {
            #[allow(clippy::cast_precision_loss)]
            push(name, value as f64)
        }

        pub fn gauge_set(name: &str, value: f64, _: &[(String, String)]) -> Result<(), String> {
            push(name, value)
        }

        pub fn histogram_record(
            name: &str,
            value: f64,
            _: &[(String, String)],
        ) -> Result<(), String> {
            push(name, value)
        }
    }

    pub mod placeholders {
        use super::*;
        use crate::bindings::pumbo::prox::placeholders::ResolveError;
        use crate::bindings::pumbo::prox::types::TextTemplate;

        pub fn set(
            key: &str,
            player: Option<PlayerId>,
            value: &Text,
            ctx: &Context,
        ) -> Result<(), String> {
            with(|h| {
                h.placeholders
                    .insert((key.to_string(), player, ctx_key(ctx)), value.clone())
            });
            Ok(())
        }

        pub fn clear(key: &str, player: Option<PlayerId>, ctx: &Context) {
            with(|h| {
                h.placeholders
                    .remove(&(key.to_string(), player, ctx_key(ctx)))
            });
        }

        pub fn invalidate(_: &str, _: Option<PlayerId>) {}

        /// Arguments only; the fake host knows no placeholders.
        pub async fn resolve(
            t: TextTemplate,
            _: Option<PlayerId>,
            _: QueryContext,
        ) -> Result<Text, ResolveError> {
            Ok(Text::Mini(render(&Text::Template(t))))
        }
    }

    pub mod scheduler {
        use super::*;

        pub fn after(ms: u64) -> u64 {
            let id = next_id();
            with(|h| h.timers.insert(id, (ms, false)));
            id
        }

        pub fn every(ms: u64) -> u64 {
            let id = next_id();
            with(|h| h.timers.insert(id, (ms, true)));
            id
        }

        pub fn cancel(timer: u64) {
            with(|h| h.timers.remove(&timer));
        }

        /// Returns at once.
        pub async fn sleep(_: u64) {}
    }

    pub mod messaging {
        use super::*;

        pub fn subscribe(channel: &str) {
            with(|h| h.channels.insert(channel.to_string()));
        }

        pub fn send(id: PlayerId, channel: &str, data: &[u8], to: Side) -> bool {
            record(
                id,
                Sent::PluginMessage(channel.into(), data.to_vec(), to == Side::Backend),
            );
            true
        }
    }

    pub mod crypto {
        use crate::bindings::pumbo::prox::crypto::Argon2Params;

        /// Not a real hash: `fake$<password>`.
        pub async fn argon2id_hash(password: String, _: Argon2Params) -> Result<String, String> {
            Ok(format!("fake${password}"))
        }

        pub async fn argon2id_verify(password: String, phc: String) -> bool {
            phc == format!("fake${password}")
        }

        pub async fn bcrypt_verify(password: String, hash: String) -> bool {
            hash == format!("fake${password}")
        }
    }

    pub mod log {
        use super::*;

        pub fn write(level: Level, message: &str) {
            with(|h| h.logs.push((level, message.to_string())));
        }
    }

    pub mod http {
        use super::*;

        pub async fn fetch(req: Request) -> Result<Response, HttpError> {
            let handler = with(|h| h.http.take());
            let Some(handler) = handler else {
                return Err(HttpError::NotAllowed);
            };
            let out = handler(req);
            with(|h| h.http = Some(handler));
            out
        }
    }
}

/// Default reply of an unknown service method (for provider tests).
pub fn unknown_method() -> CallReject {
    CallReject::UnknownMethod
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_world as v;

    /// A gate written as for the host, run against the fake virtual world.
    #[test]
    fn virtual_world_calls_are_recorded() {
        reset();
        let id = add_player("Steve", None);
        let w = v::World::new(v::WorldOptions {
            time: 6000,
            light: 15,
            game_mode: v::GameMode::Adventure,
            view_distance: 2,
        });
        w.set_block(v::BlockPos { x: 0, y: 64, z: 0 }, "minecraft:barrier")
            .unwrap();
        let at = v::Position {
            x: 0.5,
            y: 65.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
        };
        v::enter(id, &w, at, &["login".to_string()]).unwrap();
        assert!(crate::players::get(id).unwrap().in_virtual);
        let t = v::teleport(id, at);
        v::show_map(id, &v::MapImage::new(&[0; 16]), v::Hand::Main);
        v::release(id).unwrap();
        assert!(!crate::players::get(id).unwrap().in_virtual);
        assert_eq!(
            virtual_calls(id),
            [
                Virtual::Enter {
                    world: w.id,
                    at,
                    commands: vec!["login".into()],
                },
                Virtual::Teleport(at, t),
                Virtual::ShowMap(v::Hand::Main),
                Virtual::Release,
            ]
        );
        assert_eq!(w.blocks().len(), 1);
        assert!(v::enter(id + 1, &w, at, &[]).is_err());
    }
}
