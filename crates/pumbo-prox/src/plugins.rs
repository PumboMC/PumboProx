//! The plugin host in the proxy (E5): started from the host sections of
//! `pumboprox.yml`, asked at login (`on-pre-login`, profile patches,
//! `on-login`, gates), told about server changes and the end of a session,
//! and asked first about player and console commands it may own.
//!
//! The play relay asks it about chat, backend commands, client plugin
//! messages, `server-connect` and `server-kicked`, and about proxy command
//! permissions (`pumbo.proxy.<command>`).
//!
//! Commands of plugins to sessions use the session queue of E4: messages,
//! `connect` and `reconnect`. Not wired yet (the session queue has no
//! variant): action bar, titles, sounds, Tab header, kick, boss bars, plugin
//! messages, profile properties and command tree updates; the status event.

// Refusals carry a text component once per login.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use futures::future::BoxFuture;
use pumbo_core::profile::{GameProfile, Property};
use pumbo_host::wit::events::{
    BackendCommandEvent, BackendCommandReply, ChatReply, ConnectEvent, KickedEvent, PreLoginEvent,
};
use pumbo_host::wit::players::ConnectError;
use pumbo_host::wit::servers::ServerInfo;
use pumbo_host::wit::types::{self as w, PlayerId};
use pumbo_host::{
    BossbarCommand, CommandOutcome, CommandSender, GateOutcome, Host, HostConfig, PlayerCommand,
    PreLogin, ProxyBridge,
};
pub use pumbo_host::{ConnectDecision, KickDecision};
use pumbo_text::Component;
use uuid::Uuid;

use crate::server::{Proxy, SessionCmd};

/// Boss bar ids of the host as UUIDs of the protocol.
fn bossbar(b: BossbarCommand) -> SessionCmd {
    use crate::world::BossbarOp;
    let (bar, op) = match b {
        BossbarCommand::Show {
            bar,
            title,
            progress,
            color,
            overlay,
        } => (
            bar,
            BossbarOp::Show {
                title,
                progress,
                color: color as i32,
                overlay: overlay as i32,
            },
        ),
        BossbarCommand::Title { bar, title } => (bar, BossbarOp::Title(title)),
        BossbarCommand::Progress { bar, progress } => (bar, BossbarOp::Progress(progress)),
        BossbarCommand::Hide { bar } => (bar, BossbarOp::Hide),
    };
    SessionCmd::Bossbar {
        id: Uuid::from_u64_pair(0x7075_6d62_6f62_6172, bar),
        op: Box::new(op),
    }
}

/// The proxy side of [`ProxyBridge`]: plugin commands to session queues.
#[derive(Default)]
struct SessionBridge {
    proxy: OnceLock<Weak<Proxy>>,
    uuids: Mutex<HashMap<PlayerId, Uuid>>,
}

impl SessionBridge {
    fn target(&self, player: PlayerId) -> Option<(Arc<Proxy>, Uuid)> {
        let proxy = self.proxy.get()?.upgrade()?;
        let uuid = *self.uuids.lock().ok()?.get(&player)?;
        Some((proxy, uuid))
    }
}

impl ProxyBridge for SessionBridge {
    fn send(&self, player: PlayerId, cmd: PlayerCommand) {
        let Some((proxy, uuid)) = self.target(player) else {
            return;
        };
        let cmd = match cmd {
            PlayerCommand::Message(c) => SessionCmd::Message(Box::new(c)),
            PlayerCommand::ActionBar(c) => SessionCmd::ActionBar(Box::new(c)),
            PlayerCommand::Title {
                title,
                subtitle,
                times,
            } => SessionCmd::Title {
                title: Box::new(title),
                subtitle: Box::new(subtitle),
                times: (
                    i32::try_from(times.0).unwrap_or(i32::MAX),
                    i32::try_from(times.1).unwrap_or(i32::MAX),
                    i32::try_from(times.2).unwrap_or(i32::MAX),
                ),
            },
            PlayerCommand::ClearTitle => SessionCmd::ClearTitle,
            // ponytail: sounds play in the virtual world only (the proxy knows
            // no position on a server); `sound_entity` on servers when needed.
            PlayerCommand::Sound {
                sound,
                volume,
                pitch,
            } => SessionCmd::Virtual(Box::new(pumbo_virtual::Command::Sound {
                name: sound,
                volume,
                pitch,
            })),
            PlayerCommand::TabHeaderFooter { header, footer } => SessionCmd::TabList {
                header: Box::new(header),
                footer: Box::new(footer),
            },
            PlayerCommand::Kick(c) => SessionCmd::Kick(Box::new(c)),
            PlayerCommand::SetProperty(p) => SessionCmd::Property {
                name: p.name.clone(),
                value: Some(Property {
                    name: p.name,
                    value: p.value,
                    signature: p.signature,
                }),
            },
            PlayerCommand::RemoveProperty(name) => SessionCmd::Property { name, value: None },
            PlayerCommand::PluginMessage {
                channel,
                data,
                to_backend,
            } => SessionCmd::PluginMessage {
                channel,
                data,
                to_backend,
            },
            PlayerCommand::Bossbar(b) => bossbar(b),
            PlayerCommand::CommandsChanged => SessionCmd::CommandsChanged,
            PlayerCommand::Virtual(v) => SessionCmd::Virtual(Box::new(v)),
        };
        proxy.send_to(uuid, cmd);
    }

    fn connect(
        &self,
        player: PlayerId,
        server: String,
    ) -> BoxFuture<'static, Result<(), ConnectError>> {
        let queued = self.target(player).is_some_and(|(p, u)| {
            p.send_to(
                u,
                SessionCmd::Connect {
                    server,
                    quiet: false,
                },
            )
        });
        // ponytail: queued, not finished; the session reports failures to the
        // player. A result after the switch needs a reply channel in SessionCmd.
        Box::pin(async move {
            if queued {
                Ok(())
            } else {
                Err(ConnectError::Cancelled)
            }
        })
    }

    fn reconnect(&self, player: PlayerId) -> BoxFuture<'static, Result<(), ConnectError>> {
        let queued = self
            .target(player)
            .is_some_and(|(p, u)| p.send_to(u, SessionCmd::Reconnect));
        Box::pin(async move {
            if queued {
                Ok(())
            } else {
                Err(ConnectError::Cancelled)
            }
        })
    }
}

pub struct Plugins {
    pub host: Host,
    bridge: Arc<SessionBridge>,
    ids: Mutex<HashMap<Uuid, PlayerId>>,
    next: AtomicU64,
}

impl std::fmt::Debug for Plugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugins")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

fn wit_profile(p: &GameProfile) -> w::Profile {
    w::Profile {
        id: pumbo_host::wit_uuid(p.id),
        name: p.name.clone(),
        properties: p
            .properties
            .iter()
            .map(|q| w::Property {
                name: q.name.clone(),
                value: q.value.clone(),
                signature: q.signature.clone(),
            })
            .collect(),
    }
}

fn core_profile(p: &w::Profile) -> GameProfile {
    GameProfile {
        id: pumbo_host::uuid_of(&p.id),
        name: p.name.clone(),
        properties: p
            .properties
            .iter()
            .map(|q| Property {
                name: q.name.clone(),
                value: q.value.clone(),
                signature: q.signature.clone(),
            })
            .collect(),
    }
}

/// Connection data of a login for plugins.
pub fn connection(peer: SocketAddr, virtual_host: &str, protocol: i32) -> w::Connection {
    w::Connection {
        address: peer.ip().to_string(),
        port: peer.port(),
        virtual_host: virtual_host.to_string(),
        protocol,
        original_protocol: None,
        route: w::Route::Direct,
    }
}

impl Plugins {
    /// Starts the host when the plugin directory exists; `None` otherwise.
    pub async fn start(
        config_text: &str,
        proxy: &Arc<Proxy>,
        natives: Vec<pumbo_host::Native>,
    ) -> Result<Option<Arc<Plugins>>, String> {
        let cfg = HostConfig::parse(config_text).map_err(|e| e.to_string())?;
        if !cfg.plugins.dir.is_dir() {
            return Ok(None);
        }
        let bridge = Arc::new(SessionBridge::default());
        let _ = bridge.proxy.set(Arc::downgrade(proxy));
        let host = Host::start_with(cfg, bridge.clone(), natives)
            .await
            .map_err(|e| e.to_string())?;
        let rt = proxy.runtime();
        host.set_max_players(rt.config.status.max_players);
        host.set_servers(
            rt.backends
                .iter()
                .map(|b| ServerInfo {
                    name: b.name.clone(),
                    address: b.address.clone(),
                    protocol: b.protocol.map(|p| p.0),
                    online: b.online,
                    players: b.players,
                    groups: Vec::new(),
                    enforces_secure_chat: None,
                })
                .collect(),
        );
        host.wait_started(std::time::Duration::from_secs(30))
            .await
            .map_err(|e| e.to_string())?;
        for slot in host.plugins() {
            tracing::info!(plugin = %slot.id, status = ?slot.status(), "plugin loaded");
        }
        Ok(Some(Arc::new(Plugins {
            host,
            bridge,
            ids: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        })))
    }

    /// After `hello`: closed logins and `on-pre-login`. `Ok(Some(online))`
    /// overrides the online-mode decision of the proxy (D-E3-1).
    pub async fn pre_login(
        &self,
        conn: w::Connection,
        name: &str,
    ) -> Result<Option<bool>, Component> {
        let e = PreLoginEvent {
            connection: conn,
            name: name.to_string(),
            claimed_uuid: None,
        };
        match self.host.on_pre_login(e).await {
            PreLogin::Allow => Ok(None),
            PreLogin::ForceOnline => Ok(Some(true)),
            PreLogin::ForceOffline => Ok(Some(false)),
            PreLogin::Deny(t) => Err(t),
        }
    }

    /// After authentication: profile patches and `on-login`. Returns the
    /// final profile; on a refusal the host forgets the player. The gates
    /// run later, with the client in configuration ([`Plugins::gates`]), so
    /// a gate that holds can put it in a virtual world (§5.2).
    pub async fn joined(
        &self,
        profile: GameProfile,
        online: bool,
        conn: w::Connection,
    ) -> Result<GameProfile, Component> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let info = w::PlayerInfo {
            id,
            profile: wit_profile(&profile),
            online_mode: online,
            connection: conn,
            brand: None,
            settings: None,
            server: None,
            context: w::PlayerContext {
                server: None,
                groups: Vec::new(),
            },
            in_virtual: false,
        };
        self.host.player_joined(info);
        let result = async {
            let patched = core_profile(&self.host.on_profile(id).await?);
            self.host.on_login(id).await?;
            Ok(patched)
        }
        .await;
        let final_uuid = result.as_ref().map(|p| p.id);
        let registered = match final_uuid {
            Ok(u) => self
                .ids
                .lock()
                .map(|mut ids| match ids.entry(u) {
                    std::collections::hash_map::Entry::Occupied(_) => false,
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(id);
                        if let Ok(mut back) = self.bridge.uuids.lock() {
                            back.insert(id, u);
                        }
                        true
                    }
                })
                .unwrap_or(false),
            Err(_) => false,
        };
        if !registered {
            self.host.player_left(id).await;
        }
        match result {
            Ok(p) if registered => Ok(p),
            Ok(_) => Err(Component::text("You are already connected to this proxy.")),
            Err(t) => Err(t),
        }
    }

    /// The gates in order of priority (§5.6); `Err` is the kick text. A
    /// player held by a gate waits here until it is released.
    pub async fn gates(&self, uuid: Uuid) -> Result<(), Component> {
        let Some(id) = self.id(uuid) else {
            return Ok(());
        };
        match self.host.run_gates(id).await {
            GateOutcome::Pass => Ok(()),
            GateOutcome::Deny(t) => Err(t),
        }
    }

    /// The client's settings (`client_information`) for `player-info`, so
    /// plugins see the locale, view distance, main hand… (§5.2).
    pub fn settings(&self, uuid: Uuid, c: &pumbo_protocol::packets::common::ClientInformation) {
        let Some(id) = self.id(uuid) else { return };
        let settings = w::ClientSettings {
            locale: c.locale.clone(),
            view_distance: u8::try_from(c.view_distance).unwrap_or(0),
            chat_mode: u8::try_from(c.chat_mode).unwrap_or(0),
            chat_colors: c.chat_colors,
            skin_parts: c.skin_parts,
            main_hand_right: c.main_hand == 1,
            text_filtering: c.text_filtering,
            server_listing: c.server_listing,
        };
        self.host.player_update(id, |p| p.settings = Some(settings));
    }

    /// Keep-alive round trip for `%player_ping%`.
    pub fn ping(&self, uuid: Uuid, ms: u32) {
        if let Some(id) = self.id(uuid) {
            self.host.set_ping(id, ms);
        }
    }

    /// The client's brand for `player-info`.
    pub fn brand(&self, uuid: Uuid, brand: &str) {
        let Some(id) = self.id(uuid) else { return };
        let brand = brand.to_string();
        self.host.player_update(id, |p| p.brand = Some(brand));
    }

    /// A MiniMessage template with push and cached placeholders only (no
    /// waiting, §5.8.3), for one player or globally.
    pub fn render(&self, uuid: Option<Uuid>, mini: &str) -> Component {
        let id = uuid.and_then(|u| self.id(u));
        self.host.render_now(mini, &[], id)
    }

    /// `on-status`: what plugins make of the server list answer.
    pub async fn status(
        &self,
        conn: w::Connection,
        motd: &Component,
        online: u32,
        max: u32,
        favicon: bool,
    ) -> pumbo_host::StatusOutcome {
        let e = pumbo_host::wit::events::StatusEvent {
            connection: conn,
            motd: w::Text::Json(motd.to_json(pumbo_text::TextFormat::V770)),
            online,
            max,
            favicon,
        };
        self.host.on_status(e).await
    }

    /// The session of this UUID ended.
    pub fn left(&self, uuid: Uuid) {
        let id = self.ids.lock().ok().and_then(|mut ids| ids.remove(&uuid));
        if let Some(id) = id {
            if let Ok(mut back) = self.bridge.uuids.lock() {
                back.remove(&id);
            }
            let host = self.host.clone();
            tokio::spawn(async move { host.player_left(id).await });
        }
    }

    fn id(&self, uuid: Uuid) -> Option<PlayerId> {
        self.ids.lock().ok()?.get(&uuid).copied()
    }

    /// The player joined a backend (`None` between servers is ignored).
    pub fn server_changed(&self, uuid: Uuid, server: Option<&str>) {
        if let (Some(id), Some(s)) = (self.id(uuid), server) {
            self.host
                .player_server_changed(id, Some(s.to_string()), false);
        }
    }

    /// A command line of a player: `Some(lines to show)` when the host or a
    /// plugin owns it (it never goes to the backend), `None` otherwise.
    pub fn command(&self, uuid: Uuid, line: &str) -> Option<Vec<Component>> {
        let id = self.id(uuid)?;
        outcome(self.host.dispatch_command(CommandSender::Player(id), line))
    }

    /// `/prox plugins …` and `/prox debug …` (the host's proxy admin
    /// commands); `None`: the console.
    pub fn admin(&self, uuid: Option<Uuid>, args: &[String]) -> Vec<Component> {
        let sender = match uuid {
            Some(u) => match self.id(u) {
                Some(id) => CommandSender::Player(id),
                None => return Vec::new(),
            },
            None => CommandSender::Console,
        };
        outcome(self.host.proxy_admin(sender, args)).unwrap_or_default()
    }

    /// A console line (`/pumbo …` and plugin commands).
    pub fn console(&self, line: &str) -> Option<Vec<Component>> {
        outcome(self.host.dispatch_command(CommandSender::Console, line))
    }
}

/// Events of the play relay (E4 hook points).
impl Plugins {
    /// `on-chat`: `true` cancels the message. `replace` works only in the
    /// virtual world (§2.8) and counts as `pass` on a backend.
    pub async fn chat(&self, uuid: Uuid, message: &str) -> bool {
        let Some(id) = self.id(uuid) else {
            return false;
        };
        match self.host.on_chat(id, message.to_string()).await {
            ChatReply::Cancel => true,
            ChatReply::Pass => false,
            ChatReply::Replace(_) => {
                tracing::warn!("chat replace from a plugin ignored on a backend (§2.8)");
                false
            }
        }
    }

    /// `on-backend-command` for commands in a `command-filter`: `true` cancels.
    pub async fn backend_command(
        &self,
        uuid: Uuid,
        server: Option<&str>,
        line: &str,
        signed: bool,
    ) -> bool {
        let Some(id) = self.id(uuid) else {
            return false;
        };
        let e = BackendCommandEvent {
            player: id,
            server: server.unwrap_or_default().to_string(),
            line: line.to_string(),
            signed,
        };
        self.host.on_backend_command(e).await == BackendCommandReply::Cancel
    }

    /// `on-plugin-message` from the client: `false` drops it.
    pub async fn plugin_message(&self, uuid: Uuid, channel: &str, data: &[u8]) -> bool {
        if !self.host.listens_channel(channel) {
            return true;
        }
        let Some(id) = self.id(uuid) else {
            return true;
        };
        self.host
            .on_plugin_message(id, channel.to_string(), data.to_vec())
            .await
    }

    /// `on-server-connect` before the proxy logs in to `target`.
    pub async fn server_connect(&self, uuid: Uuid, target: &str, reason: &str) -> ConnectDecision {
        let Some(id) = self.id(uuid) else {
            return ConnectDecision::Allow;
        };
        let e = ConnectEvent {
            player: id,
            target: target.to_string(),
            reason: reason.to_string(),
        };
        self.host.on_server_connect(e).await
    }

    /// `on-server-kicked` for a kick from `server`.
    pub async fn server_kicked(
        &self,
        uuid: Uuid,
        server: &str,
        reason: &Component,
    ) -> KickDecision {
        let Some(id) = self.id(uuid) else {
            return KickDecision::Keep;
        };
        let e = KickedEvent {
            player: id,
            server: server.to_string(),
            reason: w::Text::Json(reason.to_json(pumbo_text::TextFormat::V770)),
            during_connect: false,
        };
        self.host.on_server_kicked(e).await
    }

    /// Permission of a player from the host (`None` = no entry).
    /// PumboBridge `perm-set` for an online player (`None`: not known here).
    pub fn backend_permissions(
        &self,
        uuid: Uuid,
        server: &str,
        catalog: &[String],
    ) -> Option<std::collections::BTreeMap<String, bool>> {
        Some(
            self.host
                .backend_permissions(self.id(uuid)?, server, catalog),
        )
    }

    pub fn set_player_value(&self, uuid: Uuid, key: &str, value: String) {
        if let Some(id) = self.id(uuid) {
            self.host.set_player_value(id, key, Some(value));
        }
    }

    pub fn permission(&self, uuid: Uuid, node: &str, server: Option<&str>) -> Option<bool> {
        self.host.permission_decision(self.id(uuid)?, node, server)
    }

    /// Plugin commands a player may use now (for the command tree): each
    /// name and alias with the subcommands to list after it.
    pub fn visible_commands(&self, uuid: Uuid) -> Vec<(String, Vec<String>)> {
        self.id(uuid)
            .map(|id| {
                self.host
                    .visible_commands(id)
                    .into_iter()
                    .flat_map(|c| {
                        let subs = c.subcommands;
                        std::iter::once(c.name)
                            .chain(c.aliases)
                            .map(move |n| (n, subs.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn outcome(o: CommandOutcome) -> Option<Vec<Component>> {
    match o {
        CommandOutcome::NotOurs => None,
        CommandOutcome::Handled => Some(Vec::new()),
        CommandOutcome::Reply(lines) => Some(lines),
        CommandOutcome::Refused(t) => Some(vec![t]),
    }
}
