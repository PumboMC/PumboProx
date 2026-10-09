//! Events from the proxy core to plugins (plan §4.2) and how their answers
//! combine. Every event with a result has a deadline; a failure (deadline,
//! trap, unavailable plugin) applies the plugin's `on-failure` rule.
//!
//! Scope of plugins enabled per server (plan §11, E5): events of a player on
//! a server go only to plugins enabled there; events without a server
//! (handshake, status, pre-login, profile, login, gates, the virtual world
//! before a backend) go to every listener; `server-connect` goes to plugins
//! enabled on the target, `server-kicked` to those enabled on the kicking
//! server, `context-changed` to those enabled on the old or the new server.

use std::sync::Arc;
use std::time::Duration;

use pumbo_core::permissions::PermissionEntry;
use pumbo_text::Component;
use tokio::sync::oneshot;

use crate::actor::{self, CallError, PluginSlot, Status};
use crate::commands::{self, CommandOutcome, CommandSender};
use crate::imports::perm_context;
use crate::manifest::{EventKind, OnFailure};
use crate::text;
use crate::wit::events::{
    BackendCommandEvent, BackendCommandReply, ChatReply, ConnectEvent, ConnectReply, GateReply,
    KickedEvent, KickedReply, PreLoginEvent, PreLoginReply, PropertyChange, StatusEvent,
    StatusReply,
};
use crate::wit::permissions::PermissionSet;
use crate::wit::types::{Connection, PlayerId, Profile, Verdict};
use crate::{Host, HostInner};

/// Result of the gates for one player.
#[derive(Debug, Clone, PartialEq)]
pub enum GateOutcome {
    Pass,
    Deny(Component),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreLogin {
    Allow,
    Deny(Component),
    ForceOnline,
    ForceOffline,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectDecision {
    Allow,
    Deny(Component),
    Redirect(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum KickDecision {
    /// The proxy's default handling (plan §3.2).
    Keep,
    Disconnect(Component),
    Redirect(String, Component),
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatusOutcome {
    Keep,
    Change {
        motd: Component,
        online: u32,
        max: u32,
        favicon: bool,
    },
    /// No answer to the ping.
    Cancel,
}

impl HostInner {
    fn fails_closed(&self, slot: &PluginSlot, kind: EventKind) -> bool {
        slot.manifest
            .on_failure(kind, self.cfg.plugins.on_failure.get(&kind).copied())
            == OnFailure::Deny
    }

    /// How long an event waits for a restarting plugin.
    fn wait(&self, kind: EventKind) -> Duration {
        self.cfg
            .plugins
            .timeout(kind)
            .min(Duration::from_millis(self.cfg.services.restart_wait_ms))
    }

    fn report(&self, slot: &PluginSlot, kind: EventKind, e: &CallError) {
        tracing::warn!(plugin = %slot.id, event = ?kind, "event failed: {e}");
    }

    fn deny_text(
        &self,
        slot: &PluginSlot,
        t: &crate::wit::types::Text,
        player: Option<PlayerId>,
    ) -> Component {
        text::component(self, t, player, Some(slot))
    }

    fn failure_message(&self) -> Component {
        self.message(&self.cfg.plugins.messages.gate_failed)
    }

    /// Sends a notification without waiting; the result is ignored.
    pub(crate) fn notify<F>(&self, slot: &PluginSlot, f: F)
    where
        F: for<'a> FnOnce(
                &'a wasmtime::component::Accessor<crate::runtime::PluginState>,
                &'a crate::wit_bindings::exports::pumbo::prox::events::Guest,
            ) -> futures::future::BoxFuture<'a, ()>
            + Send
            + 'static,
    {
        if slot.status() == Status::Running {
            let _ = slot.submit(actor::job(f));
        }
    }

    pub(crate) fn broadcast(&self, cmd: crate::bridge::PlayerCommand) {
        let ids: Vec<PlayerId> = self
            .players
            .read()
            .map(|p| p.keys().copied().collect())
            .unwrap_or_default();
        for id in ids {
            self.bridge.send(id, cmd.clone());
        }
    }

    pub(crate) async fn notify_disconnect(&self, id: PlayerId) {
        for slot in self.listeners(EventKind::Disconnect) {
            if self.in_scope(slot, id) {
                self.notify(slot, move |acc, g| {
                    Box::pin(async move {
                        let _ = g.call_on_disconnect(acc, id).await;
                    })
                });
            }
        }
    }
}

/// Provider entries from a WIT permission set.
pub(crate) fn provider_entries(set: PermissionSet) -> Vec<PermissionEntry> {
    set.entries
        .into_iter()
        .map(|e| PermissionEntry {
            node: e.node.to_ascii_lowercase(),
            value: e.value,
            context: perm_context(&e.context),
        })
        .collect()
}

fn sorted_by_priority<'a>(
    slots: impl Iterator<Item = &'a Arc<PluginSlot>>,
    priority: impl Fn(&PluginSlot) -> i32,
) -> Vec<&'a Arc<PluginSlot>> {
    let mut v: Vec<_> = slots.collect();
    v.sort_by_key(|s| (priority(s), s.id.clone()));
    v
}

fn valid_name(n: &str) -> bool {
    (1..=16).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Host {
    /// After the handshake and native limits: the first `deny` wins.
    pub async fn on_handshake(&self, c: Connection) -> Result<(), Component> {
        let inner = &self.inner;
        for slot in inner.listeners(EventKind::Handshake) {
            let conn = c.clone();
            let r = slot
                .call(
                    inner.wait(EventKind::Handshake),
                    inner.cfg.plugins.timeout(EventKind::Handshake),
                    move |acc, g| Box::pin(async move { g.call_on_handshake(acc, conn).await }),
                )
                .await;
            match r {
                Ok(Verdict::Allow) => {}
                Ok(Verdict::Deny(t)) => return Err(inner.deny_text(slot, &t, None)),
                Err(e) => {
                    inner.report(slot, EventKind::Handshake, &e);
                    if inner.fails_closed(slot, EventKind::Handshake) {
                        return Err(Component::default());
                    }
                }
            }
        }
        Ok(())
    }

    /// Server list ping: a chain, each plugin sees the previous result.
    pub async fn on_status(&self, mut e: StatusEvent) -> StatusOutcome {
        let inner = &self.inner;
        let mut changed = false;
        for slot in inner.listeners(EventKind::Status) {
            let ev = e.clone();
            let r = slot
                .call(
                    inner.wait(EventKind::Status),
                    inner.cfg.plugins.timeout(EventKind::Status),
                    move |acc, g| Box::pin(async move { g.call_on_status(acc, ev).await }),
                )
                .await;
            match r {
                Ok(StatusReply::Keep) => {}
                Ok(StatusReply::Change(next)) => {
                    e = StatusEvent {
                        connection: e.connection,
                        ..next
                    };
                    changed = true;
                }
                Ok(StatusReply::Cancel) => return StatusOutcome::Cancel,
                Err(err) => inner.report(slot, EventKind::Status, &err),
            }
        }
        if !changed {
            return StatusOutcome::Keep;
        }
        StatusOutcome::Change {
            motd: text::component(inner, &e.motd, None, None),
            online: e.online,
            max: e.max,
            favicon: e.favicon,
        }
    }

    /// After `hello`, before encryption. Every listener gets the event in
    /// turn (plugin id order); `deny` ends the login at once, of
    /// `force-online` and `force-offline` the first one counts and a
    /// contradicting later one is logged (D-E7-2).
    pub async fn on_pre_login(&self, e: PreLoginEvent) -> PreLogin {
        let inner = &self.inner;
        if let Err(msg) = self.login_open() {
            return PreLogin::Deny(msg);
        }
        let mut forced: Option<(PreLogin, String)> = None;
        for slot in inner.listeners(EventKind::PreLogin) {
            let ev = e.clone();
            let r = slot
                .call(
                    inner.wait(EventKind::PreLogin),
                    inner.cfg.plugins.timeout(EventKind::PreLogin),
                    move |acc, g| Box::pin(async move { g.call_on_pre_login(acc, ev).await }),
                )
                .await;
            match r {
                Ok(PreLoginReply::Allow) => {}
                Ok(PreLoginReply::Deny(t)) => {
                    return PreLogin::Deny(inner.deny_text(slot, &t, None));
                }
                Ok(reply @ (PreLoginReply::ForceOnline | PreLoginReply::ForceOffline)) => {
                    let want = if reply == PreLoginReply::ForceOnline {
                        PreLogin::ForceOnline
                    } else {
                        PreLogin::ForceOffline
                    };
                    match &forced {
                        None => forced = Some((want, slot.id.clone())),
                        Some((first, by)) if *first != want => tracing::warn!(
                            plugin = %slot.id,
                            "pre-login: {want:?} ignored, {by} answered {first:?} first"
                        ),
                        Some(_) => {}
                    }
                }
                Err(err) => {
                    inner.report(slot, EventKind::PreLogin, &err);
                    if inner.fails_closed(slot, EventKind::PreLogin) {
                        return PreLogin::Deny(inner.failure_message());
                    }
                }
            }
        }
        forced.map_or(PreLogin::Allow, |(f, _)| f)
    }

    /// Profile patches by `profile.priority` (plan §4.2): every plugin sees
    /// the result of the earlier ones. Returns the final profile, which the
    /// host also stores.
    pub async fn on_profile(&self, id: PlayerId) -> Result<Profile, Component> {
        let inner = &self.inner;
        let slots = sorted_by_priority(inner.listeners(EventKind::Profile), |s| {
            s.manifest.profile.as_ref().map_or(0, |p| p.priority)
        });
        for slot in slots {
            let Some(info) = inner.player(id) else {
                return Err(inner.failure_message());
            };
            let premium_locked =
                info.online_mode && !inner.cfg.plugins.allow_premium_profile_changes;
            let r = slot
                .call(
                    inner.wait(EventKind::Profile),
                    inner.cfg.plugins.timeout(EventKind::Profile),
                    move |acc, g| Box::pin(async move { g.call_on_profile(acc, info).await }),
                )
                .await;
            let patch = match r {
                Ok(p) => p,
                Err(err) => {
                    inner.report(slot, EventKind::Profile, &err);
                    if inner.fails_closed(slot, EventKind::Profile) {
                        return Err(inner.failure_message());
                    }
                    continue;
                }
            };
            let touches =
                patch.id.is_some() || patch.name.is_some() || !patch.properties.is_empty();
            if touches && premium_locked {
                tracing::warn!(plugin = %slot.id, player = id, "profile patch of a premium player ignored");
                continue;
            }
            let allowed = &slot.manifest.profile_properties;
            if let Ok(mut players) = inner.players.write()
                && let Some(p) = players.get_mut(&id)
            {
                if let Some(u) = patch.id {
                    p.profile.id = u;
                }
                if let Some(n) = patch.name {
                    if valid_name(&n) {
                        p.profile.name = n;
                    } else {
                        tracing::warn!(plugin = %slot.id, "invalid name in profile patch ignored");
                    }
                }
                for change in patch.properties {
                    let name = match &change {
                        PropertyChange::Set(prop) => prop.name.clone(),
                        PropertyChange::Remove(n) => n.clone(),
                    };
                    if !allowed.contains(&name) {
                        tracing::warn!(plugin = %slot.id, "property {name} not in profile-properties, ignored");
                        continue;
                    }
                    p.profile.properties.retain(|q| q.name != name);
                    if let PropertyChange::Set(prop) = change {
                        p.profile.properties.push(prop);
                    }
                }
            }
        }
        let info = inner.player(id).ok_or_else(|| inner.failure_message())?;
        // The file layer may depend on the patched UUID or name.
        inner.perms.load_file_layer(id, &crate::subject(&info));
        Ok(info.profile)
    }

    /// The player is logged in to the proxy: the first `deny` wins.
    pub async fn on_login(&self, id: PlayerId) -> Result<(), Component> {
        let inner = &self.inner;
        if inner.cfg.permissions.load_at == crate::config::LoadAt::AfterProfile {
            inner.load_provider_permissions(id).await?;
        }
        for slot in inner.listeners(EventKind::Login) {
            let Some(info) = inner.player(id) else {
                return Err(inner.failure_message());
            };
            let r = slot
                .call(
                    inner.wait(EventKind::Login),
                    inner.cfg.plugins.timeout(EventKind::Login),
                    move |acc, g| Box::pin(async move { g.call_on_login(acc, info).await }),
                )
                .await;
            match r {
                Ok(Verdict::Allow) => {}
                Ok(Verdict::Deny(t)) => return Err(inner.deny_text(slot, &t, Some(id))),
                Err(err) => {
                    inner.report(slot, EventKind::Login, &err);
                    if inner.fails_closed(slot, EventKind::Login) {
                        return Err(inner.failure_message());
                    }
                }
            }
        }
        Ok(())
    }

    /// Gates by priority (plan §5.6): `pass` goes on, `deny` kicks, `hold`
    /// waits for `gates.release` until the gate timeout. A gate that dies
    /// while holding a player denies, never passes.
    pub async fn run_gates(&self, id: PlayerId) -> GateOutcome {
        let inner = &self.inner;
        if let Err(msg) = self.login_open() {
            return GateOutcome::Deny(msg);
        }
        let gates = sorted_by_priority(inner.listeners(EventKind::Gate), |s| {
            s.manifest.gate.as_ref().map_or(0, |g| g.priority)
        });
        for slot in gates {
            let name = slot
                .manifest
                .gate
                .as_ref()
                .map(|g| g.name.clone())
                .unwrap_or_default();
            let timeout = inner.cfg.plugins.gate_timeout(&name);
            let started = tokio::time::Instant::now();
            let Some(info) = inner.player(id) else {
                return GateOutcome::Deny(inner.failure_message());
            };
            let (tx, released) = oneshot::channel();
            let key = (id, slot.id.clone());
            if let Ok(mut h) = inner.held.lock() {
                h.insert(key.clone(), tx);
            }
            let _cleanup = HeldGuard { host: inner, key };
            let wait = Duration::from_millis(inner.cfg.plugins.gate_reload_wait_ms);
            let r = slot
                .call(wait, timeout, move |acc, g| {
                    Box::pin(async move { g.call_on_gate(acc, info).await })
                })
                .await;
            match r {
                Ok(GateReply::Pass) => {}
                Ok(GateReply::Deny(t)) => {
                    return GateOutcome::Deny(inner.deny_text(slot, &t, Some(id)));
                }
                Ok(GateReply::Hold) => {
                    let generation = slot.generation.load(std::sync::atomic::Ordering::SeqCst);
                    let left = timeout.saturating_sub(started.elapsed());
                    let mut status = slot.subscribe_status();
                    let died = async {
                        loop {
                            if status.changed().await.is_err() {
                                return;
                            }
                            let gen_now = slot.generation.load(std::sync::atomic::Ordering::SeqCst);
                            if *status.borrow() != Status::Running || gen_now != generation {
                                return;
                            }
                        }
                    };
                    tokio::select! {
                        r = released => if r.is_err() {
                            return GateOutcome::Deny(inner.failure_message());
                        },
                        () = died => {
                            tracing::warn!(plugin = %slot.id, player = id, "gate died while holding a player");
                            return GateOutcome::Deny(inner.failure_message());
                        }
                        () = tokio::time::sleep(left) => {
                            return GateOutcome::Deny(inner.failure_message());
                        }
                    }
                }
                Err(err) => {
                    inner.report(slot, EventKind::Gate, &err);
                    if inner.fails_closed(slot, EventKind::Gate) {
                        return GateOutcome::Deny(inner.failure_message());
                    }
                }
            }
        }
        // After the last gate, so bots refused by a gate never reach the provider.
        if inner.cfg.permissions.load_at == crate::config::LoadAt::AfterGates
            && let Err(t) = inner.load_provider_permissions(id).await
        {
            return GateOutcome::Deny(t);
        }
        GateOutcome::Pass
    }

    /// Before connecting to a backend; plugins enabled on the target decide,
    /// the first answer other than `allow` wins.
    pub async fn on_server_connect(&self, e: ConnectEvent) -> ConnectDecision {
        let inner = &self.inner;
        for slot in inner.listeners(EventKind::ServerConnect) {
            if !inner.scope_allows_server(slot, &e.target) {
                continue;
            }
            let ev = e.clone();
            let r = slot
                .call(
                    inner.wait(EventKind::ServerConnect),
                    inner.cfg.plugins.timeout(EventKind::ServerConnect),
                    move |acc, g| Box::pin(async move { g.call_on_server_connect(acc, ev).await }),
                )
                .await;
            match r {
                Ok(ConnectReply::Allow) => {}
                Ok(ConnectReply::Deny(t)) => {
                    return ConnectDecision::Deny(inner.deny_text(slot, &t, Some(e.player)));
                }
                Ok(ConnectReply::Redirect(s)) => return ConnectDecision::Redirect(s),
                Err(err) => {
                    inner.report(slot, EventKind::ServerConnect, &err);
                    if inner.fails_closed(slot, EventKind::ServerConnect) {
                        return ConnectDecision::Deny(inner.failure_message());
                    }
                }
            }
        }
        ConnectDecision::Allow
    }

    /// The player is now on `server` (or in the virtual world with `None`):
    /// updates the context, notifies plugins and asks the proxy to rebuild
    /// the command tree.
    pub fn player_server_changed(&self, id: PlayerId, server: Option<String>, in_virtual: bool) {
        let inner = &self.inner;
        let context = inner.context_of(server.as_deref());
        let mut previous = None;
        if let Ok(mut players) = inner.players.write()
            && let Some(p) = players.get_mut(&id)
        {
            previous = Some((p.server.clone(), p.context.clone(), p.in_virtual));
            p.server = server.clone();
            p.context = context.clone();
            p.in_virtual = in_virtual;
        }
        let Some((prev_server, prev_context, prev_virtual)) = previous else {
            return;
        };
        inner.context_changed(id);
        if let Some(s) = server.clone() {
            for slot in inner.listeners(EventKind::ServerConnected) {
                if !inner.scope_allows_server(slot, &s) {
                    continue;
                }
                let (s, prev) = (s.clone(), prev_server.clone());
                inner.notify(slot, move |acc, g| {
                    Box::pin(async move {
                        let _ = g.call_on_server_connected(acc, id, s, prev).await;
                    })
                });
            }
        }
        let had_context = prev_server.is_some() && !prev_virtual;
        for slot in inner.listeners(EventKind::ContextChanged) {
            let old_in = prev_server
                .as_deref()
                .is_none_or(|s| inner.scope_allows_server(slot, s));
            let new_in = server
                .as_deref()
                .is_none_or(|s| inner.scope_allows_server(slot, s));
            if !(old_in || new_in) {
                continue;
            }
            let now = context.clone();
            let prev = had_context.then(|| prev_context.clone());
            inner.notify(slot, move |acc, g| {
                Box::pin(async move {
                    let _ = g.call_on_context_changed(acc, id, now, prev).await;
                })
            });
        }
        inner
            .bridge
            .send(id, crate::bridge::PlayerCommand::CommandsChanged);
    }

    /// Kick from a backend: plugins enabled on that server decide; the first
    /// answer other than `keep` wins.
    pub async fn on_server_kicked(&self, e: KickedEvent) -> KickDecision {
        let inner = &self.inner;
        for slot in inner.listeners(EventKind::ServerKicked) {
            if !inner.scope_allows_server(slot, &e.server) {
                continue;
            }
            let ev = e.clone();
            let r = slot
                .call(
                    inner.wait(EventKind::ServerKicked),
                    inner.cfg.plugins.timeout(EventKind::ServerKicked),
                    move |acc, g| Box::pin(async move { g.call_on_server_kicked(acc, ev).await }),
                )
                .await;
            match r {
                Ok(KickedReply::Keep) => {}
                Ok(KickedReply::Disconnect(t)) => {
                    return KickDecision::Disconnect(inner.deny_text(slot, &t, Some(e.player)));
                }
                Ok(KickedReply::Redirect((s, t))) => {
                    return KickDecision::Redirect(s, inner.deny_text(slot, &t, Some(e.player)));
                }
                Err(err) => inner.report(slot, EventKind::ServerKicked, &err),
            }
        }
        KickDecision::Keep
    }

    /// Chat message: a chain; `replace` changes the text for the next plugin,
    /// `cancel` stops (plan §2.8 limits `replace` to the virtual world).
    pub async fn on_chat(&self, id: PlayerId, message: String) -> ChatReply {
        let inner = &self.inner;
        let mut current = message.clone();
        for slot in inner.listeners(EventKind::Chat) {
            if !inner.in_scope(slot, id) {
                continue;
            }
            let m = current.clone();
            let r = slot
                .call(
                    inner.wait(EventKind::Chat),
                    inner.cfg.plugins.timeout(EventKind::Chat),
                    move |acc, g| Box::pin(async move { g.call_on_chat(acc, id, m).await }),
                )
                .await;
            match r {
                Ok(ChatReply::Pass) => {}
                Ok(ChatReply::Cancel) => return ChatReply::Cancel,
                Ok(ChatReply::Replace(s)) => current = s,
                Err(err) => {
                    inner.report(slot, EventKind::Chat, &err);
                    if inner.fails_closed(slot, EventKind::Chat) {
                        return ChatReply::Cancel;
                    }
                }
            }
        }
        if current == message {
            ChatReply::Pass
        } else {
            ChatReply::Replace(current)
        }
    }

    /// Backend command from a `command-filter` list (plan §2.8).
    pub async fn on_backend_command(&self, e: BackendCommandEvent) -> BackendCommandReply {
        let inner = &self.inner;
        let (root, _) = commands::split(&e.line);
        for slot in inner.listeners(EventKind::BackendCommand) {
            let subscribed = slot
                .manifest
                .command_filter
                .iter()
                .any(|c| c == "*" || c.eq_ignore_ascii_case(&root));
            if !subscribed || !inner.in_scope(slot, e.player) {
                continue;
            }
            let ev = e.clone();
            let r = slot
                .call(
                    inner.wait(EventKind::BackendCommand),
                    inner.cfg.plugins.timeout(EventKind::BackendCommand),
                    move |acc, g| Box::pin(async move { g.call_on_backend_command(acc, ev).await }),
                )
                .await;
            match r {
                Ok(BackendCommandReply::Pass) => {}
                Ok(BackendCommandReply::Cancel) => return BackendCommandReply::Cancel,
                Err(err) => {
                    inner.report(slot, EventKind::BackendCommand, &err);
                    if inner.fails_closed(slot, EventKind::BackendCommand) {
                        return BackendCommandReply::Cancel;
                    }
                }
            }
        }
        BackendCommandReply::Pass
    }

    /// Plugin message on a subscribed channel: any `false` drops it.
    pub async fn on_plugin_message(&self, id: PlayerId, channel: String, data: Vec<u8>) -> bool {
        let inner = &self.inner;
        for slot in inner.listeners(EventKind::PluginMessage) {
            let subscribed = slot
                .channels
                .lock()
                .map(|c| c.contains(&channel))
                .unwrap_or(false);
            if !subscribed || !inner.in_scope(slot, id) {
                continue;
            }
            let (c, d) = (channel.clone(), data.clone());
            let r = slot
                .call(
                    inner.wait(EventKind::PluginMessage),
                    inner.cfg.plugins.timeout(EventKind::PluginMessage),
                    move |acc, g| {
                        Box::pin(async move { g.call_on_plugin_message(acc, id, c, d).await })
                    },
                )
                .await;
            match r {
                Ok(true) => {}
                Ok(false) => return false,
                Err(err) => {
                    inner.report(slot, EventKind::PluginMessage, &err);
                    if inner.fails_closed(slot, EventKind::PluginMessage) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// A command line from a player or the console (plan §2.8).
    pub fn dispatch_command(&self, sender: CommandSender, line: &str) -> CommandOutcome {
        commands::dispatch(&self.inner, sender, line)
    }
}

/// Removes the held entry when the gate step ends, however it ends.
struct HeldGuard<'a> {
    host: &'a HostInner,
    key: (PlayerId, String),
}

impl Drop for HeldGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut h) = self.host.held.lock() {
            h.remove(&self.key);
        }
    }
}
