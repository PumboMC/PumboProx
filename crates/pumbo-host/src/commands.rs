//! Proxy commands of plugins (plan §2.8, §4.3): registration, sensitive
//! names reserved from manifests, dispatch with permissions and scope.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use pumbo_text::Component;

use crate::HostInner;
use crate::actor::{PluginSlot, Status};
use crate::wit::commands::{CommandSpec, PermissionState};
use crate::wit::events::CommandEvent;
use crate::wit::types::PlayerId;

/// Built-in proxy commands (E4) and the umbrella root; plugins cannot take them.
pub const RESERVED: &[&str] = &["pumbo", "prox", "server", "glist", "send", "find", "alert"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSender {
    Console,
    Player(PlayerId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandOutcome {
    /// Not a proxy command: the proxy forwards it to the backend.
    NotOurs,
    /// Taken by the host or a plugin.
    Handled,
    /// Answer of the host for the sender (e.g. `/pumbo`).
    Reply(Vec<Component>),
    /// A proxy command that cannot run now; nothing goes to the backend.
    Refused(Component),
}

/// A command a player may use, for the command tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleCommand {
    pub name: String,
    pub aliases: Vec<String>,
    pub usage: String,
    pub plugin: String,
    /// Subcommands the player may use, as literals after the name; any other
    /// words still fit the greedy argument.
    pub subcommands: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub plugin: String,
    pub spec: CommandSpec,
}

#[derive(Debug, Default)]
struct Inner {
    /// Name or alias → command.
    roots: BTreeMap<String, Arc<Entry>>,
    /// Umbrella commands: (short-name, name) → command.
    umbrella: BTreeMap<(String, String), Arc<Entry>>,
}

#[derive(Debug)]
pub(crate) struct Commands {
    inner: RwLock<Inner>,
    /// Sensitive names from manifests → plugin; never forwarded to a backend.
    sensitive: BTreeMap<String, String>,
    /// `short-alias` of a manifest → plugin: the host's `/pumbo<short-name>`.
    short_aliases: BTreeMap<String, String>,
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl Commands {
    pub fn new(plugins: &BTreeMap<String, Arc<PluginSlot>>) -> Result<Commands, String> {
        let mut sensitive = BTreeMap::new();
        let mut short_aliases = BTreeMap::new();
        for slot in plugins.values() {
            if let Some(a) = &slot.manifest.short_alias {
                short_aliases.insert(a.clone(), slot.id.clone());
            }
            for name in slot.manifest.sensitive() {
                if let Some(other) = sensitive.insert(name.clone(), slot.id.clone())
                    && other != slot.id
                {
                    return Err(format!(
                        "sensitive command \"{name}\" reserved by {other} and {}",
                        slot.id
                    ));
                }
            }
        }
        Ok(Commands {
            inner: RwLock::new(Inner::default()),
            sensitive,
            short_aliases,
        })
    }

    /// Returns whether the set of commands changed.
    pub fn register(&self, slot: &PluginSlot, mut spec: CommandSpec) -> Result<bool, String> {
        spec.name = spec.name.to_ascii_lowercase();
        for a in &mut spec.aliases {
            *a = a.to_ascii_lowercase();
        }
        let names: Vec<String> = std::iter::once(spec.name.clone())
            .chain(spec.aliases.iter().cloned())
            .collect();
        if let Some(bad) = names.iter().find(|n| !valid_name(n)) {
            return Err(format!("invalid command name \"{bad}\""));
        }
        let mut inner = self
            .inner
            .write()
            .map_err(|_| "command table poisoned".to_string())?;
        if spec.umbrella {
            let short = slot
                .manifest
                .short_name
                .clone()
                .ok_or("umbrella commands need `short-name` in the manifest")?;
            let entry = Arc::new(Entry {
                plugin: slot.id.clone(),
                spec,
            });
            let mut changed = false;
            for n in names {
                let key = (short.clone(), n);
                if inner
                    .umbrella
                    .get(&key)
                    .is_none_or(|e| e.spec != entry.spec)
                {
                    inner.umbrella.insert(key, Arc::clone(&entry));
                    changed = true;
                }
            }
            return Ok(changed);
        }
        for n in &names {
            if RESERVED.contains(&n.as_str()) {
                return Err(format!("command \"{n}\" is reserved by the proxy"));
            }
            if let Some(owner) = self.short_aliases.get(n) {
                return Err(format!("command \"{n}\" is the short alias of {owner}"));
            }
            if let Some(owner) = self.sensitive.get(n)
                && owner != &slot.id
            {
                return Err(format!("command \"{n}\" is reserved by {owner}"));
            }
            if let Some(e) = inner.roots.get(n)
                && e.plugin != slot.id
            {
                return Err(format!("command \"{n}\" is registered by {}", e.plugin));
            }
        }
        if names
            .iter()
            .any(|n| self.sensitive.get(n) == Some(&slot.id))
        {
            spec.sensitive = true;
        }
        let entry = Arc::new(Entry {
            plugin: slot.id.clone(),
            spec,
        });
        let mut changed = false;
        for n in names {
            if inner.roots.get(&n).is_none_or(|e| e.spec != entry.spec) {
                inner.roots.insert(n, Arc::clone(&entry));
                changed = true;
            }
        }
        Ok(changed)
    }

    /// Drops a plugin's registrations before its new instance runs `init`.
    pub fn clear_plugin(&self, id: &str) {
        if let Ok(mut inner) = self.inner.write() {
            inner.roots.retain(|_, e| e.plugin != id);
            inner.umbrella.retain(|_, e| e.plugin != id);
        }
    }

    pub fn root(&self, name: &str) -> Option<Arc<Entry>> {
        self.inner.read().ok()?.roots.get(name).cloned()
    }

    pub fn umbrella(&self, short: &str, name: &str) -> Option<Arc<Entry>> {
        self.inner
            .read()
            .ok()?
            .umbrella
            .get(&(short.to_string(), name.to_string()))
            .cloned()
    }

    /// Umbrella commands of a plugin, once each (without aliases).
    pub fn umbrella_of(&self, short: &str) -> Vec<Arc<Entry>> {
        let Ok(inner) = self.inner.read() else {
            return Vec::new();
        };
        inner
            .umbrella
            .iter()
            .filter(|((s, n), e)| s == short && *n == e.spec.name)
            .map(|(_, e)| Arc::clone(e))
            .collect()
    }

    pub fn is_sensitive(&self, name: &str) -> bool {
        self.sensitive.contains_key(name)
    }

    pub fn visible(&self, host: &HostInner, id: PlayerId) -> Vec<VisibleCommand> {
        let Ok(inner) = self.inner.read() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if host.has(id, "pumbo.proxy.plugins", &host.player_levels(id)) {
            out.push(VisibleCommand {
                name: "pumbo".into(),
                aliases: Vec::new(),
                usage: "/pumbo [plugin] ...".into(),
                plugin: "proxy".into(),
                subcommands: Vec::new(),
            });
        }
        for (name, e) in &inner.roots {
            if *name != e.spec.name {
                continue;
            }
            let Some(slot) = host.plugins.get(&e.plugin) else {
                continue;
            };
            if allowed(host, slot, e, CommandSender::Player(id)).is_ok() {
                out.push(VisibleCommand {
                    name: e.spec.name.clone(),
                    aliases: e.spec.aliases.clone(),
                    usage: e.spec.usage.clone(),
                    plugin: e.plugin.clone(),
                    subcommands: Vec::new(),
                });
            }
        }
        drop(inner);
        // `/pumbo<short-name>` and its `short-alias` (`/pf`) when the player
        // may use one of its subcommands, as `/pumbo` resolves them.
        let lv = host.player_levels(id);
        for slot in host.plugins.values() {
            let m = &slot.manifest;
            let Some(short) = &m.short_name else {
                continue;
            };
            let mut subs: Vec<String> = crate::admin::STANDARD
                .iter()
                .filter(|s| host.has(id, &format!("pumbo.{short}.{s}"), &lv))
                .map(|s| s.to_string())
                .collect();
            if let Some(d) = slot.description() {
                subs.extend(
                    d.actions
                        .iter()
                        .filter(|a| host.has(id, &a.permission, &lv))
                        .map(|a| a.name.clone()),
                );
            }
            subs.extend(
                self.umbrella_of(short)
                    .iter()
                    .filter(|e| allowed(host, slot, e, CommandSender::Player(id)).is_ok())
                    .map(|e| e.spec.name.clone()),
            );
            if subs.is_empty() {
                continue;
            }
            out.push(VisibleCommand {
                name: format!("pumbo{short}"),
                aliases: m.short_alias.iter().cloned().collect(),
                usage: format!("/pumbo{short} <subcommand>"),
                plugin: slot.id.clone(),
                subcommands: subs,
            });
        }
        out
    }
}

/// Why a command is not available to a sender.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Denied {
    /// Does not exist for this sender (scope, virtual-only).
    Hidden,
    NoPermission,
}

pub(crate) fn allowed(
    host: &HostInner,
    slot: &PluginSlot,
    e: &Entry,
    sender: CommandSender,
) -> Result<(), Denied> {
    let CommandSender::Player(id) = sender else {
        return Ok(());
    };
    if !host.in_scope(slot, id) {
        return Err(Denied::Hidden);
    }
    if e.spec.virtual_only && !host.player(id).is_some_and(|p| p.in_virtual) {
        return Err(Denied::Hidden);
    }
    match e.spec.state {
        PermissionState::Always => Ok(()),
        PermissionState::Never => Err(Denied::NoPermission),
        PermissionState::Permission => {
            let node = e.spec.permission.as_deref().unwrap_or_default();
            if !node.is_empty() && host.has(id, node, &host.player_levels(id)) {
                Ok(())
            } else {
                Err(Denied::NoPermission)
            }
        }
    }
}

pub(crate) fn split(line: &str) -> (String, Vec<String>) {
    let line = line.trim().trim_start_matches('/');
    let mut words = line.split_whitespace();
    let root = words.next().unwrap_or_default().to_ascii_lowercase();
    (root, words.map(str::to_string).collect())
}

pub(crate) fn sender_name(host: &HostInner, sender: CommandSender) -> String {
    match sender {
        CommandSender::Console => "console".into(),
        CommandSender::Player(id) => host
            .player(id)
            .map(|p| p.profile.name)
            .unwrap_or_else(|| format!("player {id}")),
    }
}

/// Runs a command line from a player or the console.
pub(crate) fn dispatch(host: &Arc<HostInner>, sender: CommandSender, line: &str) -> CommandOutcome {
    let (root, args) = split(line);
    if root.is_empty() {
        return CommandOutcome::NotOurs;
    }
    let messages = &host.cfg.plugins.messages;
    if let Some(outcome) = crate::admin::umbrella(host, sender, &root, &args) {
        return outcome;
    }
    let Some(entry) = host.commands.root(&root) else {
        if host.commands.is_sensitive(&root) {
            tracing::info!(sender = %sender_name(host, sender), "/{root} (arguments hidden): plugin not loaded");
            return CommandOutcome::Refused(host.message(&messages.login_unavailable));
        }
        return CommandOutcome::NotOurs;
    };
    let Some(slot) = host.plugins.get(&entry.plugin) else {
        return CommandOutcome::NotOurs;
    };
    match allowed(host, slot, &entry, sender) {
        Ok(()) => {}
        Err(Denied::Hidden) if !entry.spec.sensitive => return CommandOutcome::NotOurs,
        Err(Denied::Hidden) => {
            return CommandOutcome::Refused(host.message(&messages.command_unavailable));
        }
        Err(Denied::NoPermission) => {
            return CommandOutcome::Refused(host.message(&messages.no_permission));
        }
    }
    if entry.spec.sensitive {
        tracing::info!(plugin = %slot.id, sender = %sender_name(host, sender), "/{root} (arguments hidden)");
    } else {
        tracing::debug!(plugin = %slot.id, sender = %sender_name(host, sender), "/{root} {}", args.join(" "));
    }
    let unavailable = if entry.spec.sensitive {
        &messages.login_unavailable
    } else {
        &messages.command_unavailable
    };
    if slot.status() != Status::Running {
        return CommandOutcome::Refused(host.message(unavailable));
    }
    let event = CommandEvent {
        player: match sender {
            CommandSender::Player(id) => Some(id),
            CommandSender::Console => None,
        },
        name: entry.spec.name.clone(),
        args,
    };
    let job = crate::actor::job(move |acc, g| {
        Box::pin(async move {
            let _ = g.call_on_command(acc, event).await;
        })
    });
    match slot.submit(job) {
        Ok(()) => CommandOutcome::Handled,
        Err(_) => CommandOutcome::Refused(host.message(unavailable)),
    }
}
