//! The [`Host`] handle: start, plugin lifecycle, state pushed by the proxy
//! core and queries.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use pumbo_text::{Component, StyleSheet};
use tokio::sync::Semaphore;

use crate::actor::{PluginSlot, Status};
use crate::bridge::{PlayerCommand, ProxyBridge};
use crate::commands::{CommandOutcome, CommandSender, VisibleCommand};
use crate::config::HostConfig;
use crate::load::{
    check_failures, check_unique, load_manifests, permission_provider, required, required_error,
};
use crate::manifest::EventKind;
use crate::services::Native;
use crate::wit::servers::ServerInfo;
use crate::wit::types::{PlayerId, PlayerInfo};
use crate::{
    Host, HostError, HostInner, PROVIDER_RETRY, actor, admin, commands, config, http, imports,
    permissions, placeholders, runtime, services, subject, text, uuid_of, wit,
};

impl Host {
    /// Loads manifests from the plugin directory and starts every plugin.
    /// Returns once the actors run; `wait_started` waits for their `init`.
    pub async fn start(cfg: HostConfig, bridge: Arc<dyn ProxyBridge>) -> Result<Host, HostError> {
        Host::build(cfg, bridge, Vec::new(), true).await
    }

    /// [`Host::start`] with services the host provides itself.
    pub async fn start_with(
        cfg: HostConfig,
        bridge: Arc<dyn ProxyBridge>,
        natives: Vec<Native>,
    ) -> Result<Host, HostError> {
        Host::build(cfg, bridge, natives, true).await
    }

    /// Loads manifests without starting plugins (for tools).
    pub(crate) async fn build(
        cfg: HostConfig,
        bridge: Arc<dyn ProxyBridge>,
        natives: Vec<Native>,
        spawn: bool,
    ) -> Result<Host, HostError> {
        let runtime = runtime::Runtime::new().map_err(|e| HostError::Runtime(format!("{e:#}")))?;
        let (file, warnings) = permissions::FileSource::load(&cfg.permissions.file, &cfg)
            .map_err(HostError::Config)?;
        for w in warnings {
            tracing::warn!("permissions: {w}");
        }
        let (manifests, failed) = load_manifests(&cfg.plugins.dir)?;
        let mut plugins = BTreeMap::new();
        let mut receivers = Vec::new();
        for (manifest, text, wasm) in manifests {
            let scope = cfg
                .plugins
                .scopes
                .get(&manifest.id)
                .cloned()
                .unwrap_or_default();
            if manifest.permissions_write
                && !scope.grants.contains(&config::Grant::PermissionsWrite)
            {
                tracing::warn!(
                    plugin = %manifest.id,
                    "asks for permissions-write; to allow it, add it to plugins.{}.grants in the proxy config and restart the proxy",
                    manifest.id
                );
            }
            let (slot, rx) = PluginSlot::new(manifest, text, wasm, scope, cfg.plugins.mailbox);
            receivers.push((Arc::clone(&slot), rx));
            plugins.insert(slot.id.clone(), slot);
        }
        let commands = commands::Commands::new(&plugins).map_err(HostError::Config)?;
        check_unique(&plugins)?;
        let perm_provider = permission_provider(&cfg, &plugins, &failed)?;
        let services = services::Services::new(
            &plugins,
            &cfg.services.providers,
            natives,
            perm_provider.as_deref(),
        )
        .map_err(HostError::Config)?;
        let placeholders =
            placeholders::Placeholders::new(&plugins, &cfg).map_err(HostError::Config)?;
        check_failures(&cfg.plugins, &plugins, &failed)?;
        let blocked = services.blocked(&plugins);
        let styles = StyleSheet::new(cfg.style.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let mut http = http::Http::new(cfg.plugins.http_allow_private);
        http.allow_plain = cfg.plugins.http_allow_plain_for_tests;
        let inner = Arc::new(HostInner {
            runtime,
            bridge,
            styles,
            plugins,
            players: RwLock::new(HashMap::new()),
            servers: RwLock::new(BTreeMap::new()),
            commands,
            perms: permissions::Permissions {
                file: RwLock::new(file),
                ..Default::default()
            },
            held: Mutex::new(HashMap::new()),
            http,
            crypto: Semaphore::new(4),
            services,
            placeholders,
            metrics: admin::Metrics::default(),
            me: OnceLock::new(),
            login_closed_logged: Mutex::new(None),
            required_gates: RwLock::new(cfg.plugins.required_gates.clone()),
            failed,
            perm_provider,
            provider_down_logged: Mutex::new(None),
            offline: !spawn,
            cfg,
        });
        let _ = inner.me.set(Arc::downgrade(&inner));
        if spawn && inner.perm_provider.is_some() {
            let me = Arc::downgrade(&inner);
            tokio::spawn(async move {
                let mut every = tokio::time::interval(PROVIDER_RETRY);
                every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    every.tick().await;
                    let Some(inner) = me.upgrade() else { return };
                    inner.retry_unloaded().await;
                }
            });
        }
        if spawn
            && let Some(slot) = inner
                .perm_provider
                .as_ref()
                .and_then(|p| inner.plugins.get(p))
        {
            let me = Arc::downgrade(&inner);
            let mut status = slot.subscribe_status();
            // Warns when the provider goes down for real: a reload or a
            // restart after a trap brings it back (logins wait for it), and a
            // stop is the proxy shutting down.
            tokio::spawn(async move {
                let mut was_down = false;
                while status.changed().await.is_ok() {
                    let now = status.borrow_and_update().clone();
                    let down = !now.is_coming() && now != Status::Stopped;
                    if down && !was_down {
                        let Some(inner) = me.upgrade() else { return };
                        if let Ok(mut last) = inner.provider_down_logged.lock() {
                            *last = None;
                        }
                        inner.warn_provider_down("is down");
                    }
                    was_down = down;
                }
            });
        }
        if spawn {
            for (slot, rx) in receivers {
                if let Some((_, why)) = blocked.iter().find(|(id, _)| *id == slot.id) {
                    tracing::error!(plugin = %slot.id, "not started: {why}");
                    slot.set_status(Status::Disabled(why.clone()));
                    continue;
                }
                if slot.status() == Status::Missing {
                    tracing::error!(plugin = %slot.id, "{} is missing", slot.wasm.display());
                }
                actor::spawn(Arc::clone(&inner), slot, rx);
            }
        }
        Ok(Host { inner })
    }

    /// Waits until no plugin is in `Starting` (at most `timeout`). A required
    /// gate or plugin that failed to load (its `.wasm` does not compile) is
    /// an error: the proxy does not start (fail-closed).
    pub async fn wait_started(&self, timeout: Duration) -> Result<(), HostError> {
        for slot in self.inner.plugins.values() {
            let mut rx = slot.subscribe_status();
            let _ = tokio::time::timeout(timeout, rx.wait_for(|s| *s != Status::Starting)).await;
            if let Status::Failed(why) = slot.status()
                && required(&self.inner.cfg.plugins, &slot.manifest)
            {
                return Err(required_error(&slot.id, &why));
            }
        }
        Ok(())
    }

    pub fn plugin(&self, id: &str) -> Option<Arc<PluginSlot>> {
        self.inner.plugins.get(id).cloned()
    }

    pub fn plugins(&self) -> Vec<Arc<PluginSlot>> {
        self.inner.plugins.values().cloned().collect()
    }

    /// Reloads a plugin from its file, or starts an unloaded, disabled or
    /// missing one (`/pumbo proxy plugin reload|load <id>`).
    pub async fn reload_plugin(&self, id: &str) -> Result<(), String> {
        let slot = self
            .plugin(id)
            .ok_or_else(|| format!("unknown plugin {id}"))?;
        slot.reload().await
    }

    /// Stops every plugin (shutdown export, bounded).
    pub async fn shutdown(&self) {
        for slot in self.inner.plugins.values() {
            slot.stop();
        }
        for slot in self.inner.plugins.values() {
            let mut rx = slot.subscribe_status();
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                rx.wait_for(|s| {
                    matches!(s, Status::Stopped | Status::Missing | Status::Disabled(_))
                }),
            )
            .await;
        }
    }

    /// `Ok` when every required gate and plugin is loaded (plan §4.4 item 8a);
    /// otherwise the message for the player. Status and ping work either way.
    pub fn login_open(&self) -> Result<(), Component> {
        let inner = &self.inner;
        let cfg = &inner.cfg.plugins;
        let mut missing = Vec::new();
        for gate in &self.required_gates() {
            let up = inner.plugins.values().any(|s| {
                s.manifest.gate.as_ref().is_some_and(|g| &g.name == gate) && s.status().is_coming()
            });
            if !up {
                missing.push(format!("gate {gate}"));
            }
        }
        for id in &cfg.required_plugins {
            if !inner
                .plugins
                .get(id)
                .is_some_and(|s| s.status().is_coming())
            {
                missing.push(format!("plugin {id}"));
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        if let Ok(mut last) = inner.login_closed_logged.lock()
            && last.is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
        {
            *last = Some(Instant::now());
            tracing::error!("logins closed, missing: {}", missing.join(", "));
        }
        Err(inner.message(&inner.cfg.plugins.messages.login_unavailable))
    }

    /// The proxy's admin commands from `/prox`: `plugin reload|load|unload
    /// <id>`, `perms list|check …`, `services`.
    pub fn proxy_admin(&self, sender: CommandSender, args: &[String]) -> CommandOutcome {
        admin::proxy_admin(&self.inner, sender, args)
    }

    /// [`Host::proxy_admin`] with the proxy's permission check, which falls
    /// back to `commands.operators` like the other `/prox` commands.
    pub fn proxy_admin_with(
        &self,
        sender: CommandSender,
        args: &[String],
        can: &dyn Fn(&str) -> bool,
    ) -> CommandOutcome {
        admin::proxy_command(&self.inner, sender, args, can)
    }

    /// `plugins.required-gates` as it is now.
    pub fn required_gates(&self) -> Vec<String> {
        self.inner
            .required_gates
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// New `plugins.required-gates` (a config reload): logins close at once
    /// while one of them is missing.
    /// The host sections the host started with.
    pub fn config(&self) -> &HostConfig {
        &self.inner.cfg
    }

    pub fn set_required_gates(&self, gates: Vec<String>) {
        if let Ok(mut g) = self.inner.required_gates.write() {
            *g = gates;
        }
    }

    // --- state pushed by the proxy core ---

    /// A player logged in to the proxy (before gates). The host fills in the
    /// context from `server` and loads the file permission layer.
    pub fn player_joined(&self, mut info: PlayerInfo) {
        let inner = &self.inner;
        info.context = inner.context_of(info.server.as_deref());
        let who = subject(&info);
        inner.perms.load_file_layer(info.id, &who);
        if let Ok(mut p) = inner.players.write() {
            p.insert(info.id, info);
        }
    }

    /// Changes stored player data (brand, settings, profile, virtual flag).
    /// Server changes go through [`Host::player_server_changed`].
    pub fn player_update(&self, id: PlayerId, f: impl FnOnce(&mut PlayerInfo)) {
        if let Ok(mut p) = self.inner.players.write()
            && let Some(info) = p.get_mut(&id)
        {
            let server = info.server.clone();
            f(info);
            info.server = server;
        }
    }

    pub fn player(&self, id: PlayerId) -> Option<PlayerInfo> {
        self.inner.player(id)
    }

    /// The player left: `on-disconnect` to plugins, then cleanup.
    pub async fn player_left(&self, id: PlayerId) {
        self.inner.notify_disconnect(id).await;
        let inner = &self.inner;
        inner.perms.remove(id);
        if let Ok(mut h) = inner.held.lock() {
            h.retain(|(p, _), _| *p != id);
        }
        for slot in inner.plugins.values() {
            imports::forget_viewer(slot, id);
        }
        inner.placeholders.player_left(id);
        if let Ok(mut p) = inner.players.write() {
            p.remove(&id);
        }
    }

    /// Servers known to the proxy (with ping state); groups come from the host config.
    pub fn set_servers(&self, servers: Vec<ServerInfo>) {
        let inner = &self.inner;
        if let Ok(mut s) = inner.servers.write() {
            s.clear();
            for mut info in servers {
                info.groups = inner.cfg.groups_of(&info.name);
                s.insert(info.name.clone(), info);
            }
        }
    }

    /// Permission check in the player's current context (or explicit).
    pub fn has_permission(&self, id: PlayerId, node: &str, server: Option<&str>) -> bool {
        let inner = &self.inner;
        let lv = match server {
            Some(s) => permissions::levels(Some(s), &inner.cfg.groups_of(s)),
            None => inner.player_levels(id),
        };
        inner.has(id, node, &lv)
    }

    /// The permission decision of the host, `None` without a matching entry
    /// (the proxy keeps its own default then).
    pub fn permission_decision(
        &self,
        id: PlayerId,
        node: &str,
        server: Option<&str>,
    ) -> Option<bool> {
        let inner = &self.inner;
        let lv = match server {
            Some(s) => permissions::levels(Some(s), &inner.cfg.groups_of(s)),
            None => inner.player_levels(id),
        };
        inner.perms.decide(id, node, &lv).map(|d| d.value)
    }

    /// Like [`Host::permission_decision`], for nodes no wildcard may grant:
    /// only an entry for exactly `node` grants it (`*` and `a.*` can still
    /// deny it).
    pub fn exact_permission_decision(
        &self,
        id: PlayerId,
        node: &str,
        server: Option<&str>,
    ) -> Option<bool> {
        let inner = &self.inner;
        let lv = match server {
            Some(s) => permissions::levels(Some(s), &inner.cfg.groups_of(s)),
            None => inner.player_levels(id),
        };
        inner.perms.decide_exact(id, node, &lv).map(|d| d.value)
    }

    /// Server groups of a server (`[[server-group]]`, config order).
    pub fn groups_of(&self, server: &str) -> Vec<String> {
        self.inner.cfg.groups_of(server)
    }

    /// Decisions for a player on a backend (PumboBridge `perm-set`, spec
    /// §5.1): every node of `catalog`, every namespaced node and wildcard
    /// (`ns:…`, `ns:*`, `ns:a.*`) and every `group.<name>` of the player's
    /// entries, resolved in that server's context and filtered so Pumpkin
    /// reads them the same way (`pumbo_contracts::permissions::backend_set`).
    pub fn backend_permissions(
        &self,
        id: PlayerId,
        server: &str,
        catalog: &[String],
    ) -> BTreeMap<String, bool> {
        let inner = &self.inner;
        let lv = permissions::levels(Some(server), &inner.cfg.groups_of(server));
        let Ok(online) = inner.perms.online.read() else {
            return BTreeMap::new();
        };
        let Some(p) = online.get(&id) else {
            return BTreeMap::new();
        };
        pumbo_contracts::permissions::backend_set(p.layers(), catalog, &lv)
    }

    /// The permission table for a server behind the proxy as a PumboPerms
    /// export (JSON text, PumboBridge `perms-export`). With a permission
    /// provider in the config it is only the flag `proxy-rules` (whatever
    /// state the provider is in): the proxy rules that server and players get
    /// their sets through the bridge. Without one it is `permissions.yml` in
    /// that server's context.
    pub async fn permissions_export(&self, server: &str) -> String {
        let inner = &self.inner;
        let groups = inner.cfg.groups_of(server);
        if inner.perm_provider.is_some() {
            let mut x = permissions::FileSource::default().export(server, &groups);
            if let Some(o) = x.as_object_mut() {
                o.insert("proxy-rules".into(), serde_json::Value::Bool(true));
            }
            return x.to_string();
        }
        inner
            .perms
            .file
            .read()
            .map(|f| f.export(server, &groups).to_string())
            .unwrap_or_default()
    }

    /// Whether `permissions.yml` reads (`/prox reload` checks it before it
    /// applies the new config).
    pub fn check_permissions(&self) -> Result<(), String> {
        let inner = &self.inner;
        permissions::FileSource::load(&inner.cfg.permissions.file, &inner.cfg).map(|_| ())
    }

    /// Reads `permissions.yml` again (`/prox reload`): players online get
    /// the new file layer, backends a new export and a running provider the
    /// file unless it has accepted this one already.
    pub fn reload_permissions(&self) -> Result<(), String> {
        let inner = &self.inner;
        let (file, warnings) =
            permissions::FileSource::load(&inner.cfg.permissions.file, &inner.cfg)?;
        for w in warnings {
            tracing::warn!("permissions: {w}");
        }
        if let Ok(mut f) = inner.perms.file.write() {
            *f = file;
        }
        let online: Vec<PlayerInfo> = inner
            .players
            .read()
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default();
        for info in online {
            inner.perms.load_file_layer(info.id, &subject(&info));
            inner.bridge.send(info.id, PlayerCommand::CommandsChanged);
        }
        inner.export_changed();
        // A provider that is not running gets the file when it starts.
        if inner
            .provider_slot()
            .is_some_and(|s| s.status() == Status::Running)
        {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move { inner.hand_file(false).await });
        }
        Ok(())
    }

    /// What the permission provider shows for an online player on `server`
    /// (PumboBridge carries it with the player's `perm-set`); `Ok(None)`
    /// without a provider, `Err` when the provider is down or does not
    /// answer (the last info stays).
    pub async fn permissions_info(
        &self,
        id: PlayerId,
        server: &str,
    ) -> Result<Option<String>, String> {
        let inner = &self.inner;
        if inner.perm_provider.is_none() {
            return Ok(None);
        }
        inner.provider_slot().ok_or("provider is down")?;
        let Some(player) = inner.player(id) else {
            return Ok(None);
        };
        let uuid = uuid_of(&player.profile.id);
        let req = pumbo_contracts::PermissionsInfoRequest {
            uuid: uuid.simple().to_string(),
            server: server.to_string(),
            groups: inner.cfg.groups_of(server),
        };
        match inner
            .call_provider::<pumbo_contracts::PermissionsInfo>(
                pumbo_contracts::METHOD_INFO,
                &req,
                1000,
            )
            .await
        {
            Ok(x) => Ok(x.data),
            Err(e) => {
                tracing::debug!(player = id, "no permission info for {server}: {e}");
                Err(e)
            }
        }
    }

    /// Publishes a host topic (`pumbo:bridge-event`, …) to its subscribers.
    pub fn publish(&self, topic: &pumbo_contracts::Contract, payload: Vec<u8>) {
        if let Err(e) = self.inner.publish_native(topic, payload) {
            tracing::warn!("bus {}: {e}", topic.name);
        }
    }

    /// `%server_<key>:<server>%` from PumboBridge (`None` clears).
    pub fn set_server_value(&self, server: &str, key: &str, value: Option<String>) {
        if let Ok(mut m) = self.inner.placeholders.server_values.lock() {
            let k = (server.to_string(), key.to_string());
            match value {
                Some(v) => m.insert(k, v),
                None => m.remove(&k),
            };
        }
    }

    /// `%player_<key>%` from PumboBridge (`None` clears).
    pub fn set_player_value(&self, id: PlayerId, key: &str, value: Option<String>) {
        if let Ok(mut m) = self.inner.placeholders.player_values.lock() {
            let k = (id, key.to_string());
            match value {
                Some(v) => m.insert(k, v),
                None => m.remove(&k),
            };
        }
    }

    /// A sensitive command name from a manifest (never sent to a backend).
    pub fn is_sensitive_command(&self, name: &str) -> bool {
        self.inner.commands.is_sensitive(&name.to_ascii_lowercase())
    }

    /// Whether any plugin may want plugin messages on this channel (cheap
    /// check before `on_plugin_message`).
    pub fn listens_channel(&self, channel: &str) -> bool {
        self.inner.listeners(EventKind::PluginMessage).any(|s| {
            s.channels
                .lock()
                .map(|c| c.contains(channel))
                .unwrap_or(false)
        })
    }

    /// Commands of the proxy and plugins visible to a player now (for the
    /// command tree).
    pub fn visible_commands(&self, id: PlayerId) -> Vec<VisibleCommand> {
        self.inner.commands.visible(&self.inner, id)
    }

    /// Tab completion of a plugin command past its name (`pp user St`,
    /// without the slash) by the plugin's `suggest` export. The answers
    /// replace the last word; `None`: the proxy decides.
    pub fn suggest_command(
        &self,
        id: PlayerId,
        line: &str,
    ) -> impl std::future::Future<Output = Option<Vec<String>>> + Send + 'static {
        commands::suggest(Arc::clone(&self.inner), id, line.to_string())
    }
}

/// Shared services and descriptions (part B of E5).
impl Host {
    /// Plugin metrics for the proxy's Prometheus endpoint (plan §2.11).
    pub fn render_metrics(&self) -> String {
        self.inner.render_metrics()
    }

    /// The description of a running plugin as JSON (plan §6.6.5).
    pub fn describe_json(&self, id: &str) -> Option<serde_json::Value> {
        let slot = self.plugin(id)?;
        let d = slot.description()?;
        Some(admin::description_json(&slot, &d))
    }

    /// The global config of a plugin with `x-pumbo-secret` fields hidden.
    pub fn config_view(&self, id: &str) -> Result<serde_json::Value, String> {
        let slot = self
            .plugin(id)
            .ok_or_else(|| format!("unknown plugin {id}"))?;
        self.inner.config_view(&slot)
    }

    /// Validates the config files of a plugin (and its overlays) against
    /// the schema of its description.
    pub fn validate_config(&self, id: &str) -> Result<(), String> {
        let slot = self
            .plugin(id)
            .ok_or_else(|| format!("unknown plugin {id}"))?;
        self.inner.validate_config(&slot)
    }

    /// `%proxy_max%`.
    pub fn set_max_players(&self, n: u32) {
        self.inner
            .placeholders
            .max_players
            .store(n, std::sync::atomic::Ordering::Relaxed);
    }

    /// `%player_ping%`.
    pub fn set_ping(&self, id: PlayerId, ms: u32) {
        if let Ok(mut p) = self.inner.placeholders.pings.lock() {
            p.insert(id, ms);
        }
    }

    /// A template of the proxy config for a recipient (kicks, `/alert`),
    /// waiting for pull values up to their deadline.
    pub async fn render(
        &self,
        mini: &str,
        args: &[(String, String)],
        player: Option<PlayerId>,
    ) -> Component {
        let t = wit::types::TextTemplate {
            mini: mini.to_string(),
            args: args.to_vec(),
        };
        let out = self
            .inner
            .resolve_template(&t, player, &wit::types::QueryContext::Current, None)
            .await
            .unwrap_or_else(|e| pumbo_text::template::escape_mini(&format!("({e:?})")));
        self.inner.message(&out)
    }

    /// The same without waiting (status, Tab): push and cached values only.
    pub fn render_now(
        &self,
        mini: &str,
        args: &[(String, String)],
        player: Option<PlayerId>,
    ) -> Component {
        let t = wit::types::TextTemplate {
            mini: mini.to_string(),
            args: args.to_vec(),
        };
        self.inner
            .message(&text::render_cached(&self.inner, &t, player, None))
    }
}
