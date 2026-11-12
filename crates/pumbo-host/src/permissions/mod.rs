//! Permissions as host data (plan §4.4 item 4, §5.8.4). A check reads the
//! table and never calls a plugin; resolution lives in
//! `pumbo_contracts::permissions`.
//!
//! Once the permission provider (PumboPerms) has loaded a player, its set
//! replaces the file for that player (PumboPerms spec §16: the provider took
//! the file over at its first start). While the provider is down, players
//! online keep its last set and `on-load-failure` decides new logins.
//! Session nodes (`permissions.set`) sit on top of both until the player
//! leaves.

mod file;
mod provider;

pub use file::FileSource;

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Mutex, RwLock};

pub use pumbo_contracts::permissions::{Decision, Layer, Layers, levels, resolve};
use pumbo_core::permissions::{PermContext, PermissionEntry, Subject};

use crate::wit::types::PlayerId;

/// Permissions of one online player.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlayerPerms {
    pub file: Vec<PermissionEntry>,
    pub provider: Vec<PermissionEntry>,
    /// Nodes from `permissions.set`, kept until the player leaves.
    pub session: Vec<PermissionEntry>,
    /// The provider loaded this player: its set replaces the file.
    pub loaded: bool,
    /// Past the point of loading (`load-at`): loaded again when the
    /// provider comes back.
    pub eligible: bool,
    /// Bumped by every `permissions.replace`: a load started before it is
    /// older than the set the provider pushed since.
    pub epoch: u64,
    /// Provider loads of the player now running; the retry and the
    /// provider's start leave such a player to them.
    pub loading: u32,
    /// `Permissions::instance` when the last load that got an answer
    /// started: the provider's start skips a player its new instance loaded.
    pub loaded_on: u64,
}

impl PlayerPerms {
    /// The layers that decide: session nodes, then the provider's set alone
    /// once it is loaded, otherwise the file.
    pub fn layers(&self) -> Layers<'_> {
        Layers {
            session: &self.session,
            file: if self.loaded { &[] } else { &self.file },
            provider: &self.provider,
        }
    }
}

/// The host table.
#[derive(Debug, Default)]
pub(crate) struct Permissions {
    pub file: RwLock<FileSource>,
    pub online: RwLock<HashMap<PlayerId, PlayerPerms>>,
    /// Fingerprint of the file the running provider last accepted.
    pub accepted: Mutex<Option<String>>,
    /// One handoff of the file at a time.
    pub handing: tokio::sync::Mutex<()>,
    /// Instances of the provider that ended (`plugin_down`): a load started
    /// since then gets its answer from the next one.
    pub instance: AtomicU64,
}

impl Permissions {
    pub fn decide(&self, id: PlayerId, node: &str, levels: &[PermContext]) -> Option<Decision> {
        let online = self.online.read().ok()?;
        resolve(online.get(&id)?.layers(), node, levels)
    }

    /// Like [`Permissions::decide`], from the entries for exactly `node`:
    /// `*` and `a.*` entries never grant it, but their denials still count.
    pub fn decide_exact(
        &self,
        id: PlayerId,
        node: &str,
        levels: &[PermContext],
    ) -> Option<Decision> {
        let online = self.online.read().ok()?;
        let layers = online.get(&id)?.layers();
        let node = node.to_ascii_lowercase();
        let only = |s: &[PermissionEntry]| -> Vec<PermissionEntry> {
            s.iter()
                .filter(|e| e.node == node || !e.value)
                .cloned()
                .collect()
        };
        let (session, file, provider) = (
            only(layers.session),
            only(layers.file),
            only(layers.provider),
        );
        let exact = Layers {
            session: &session,
            file: &file,
            provider: &provider,
        };
        resolve(exact, &node, levels)
    }

    pub fn load_file_layer(&self, id: PlayerId, who: &Subject) {
        let entries = self
            .file
            .read()
            .map(|f| f.entries_for(who))
            .unwrap_or_default();
        if let Ok(mut o) = self.online.write() {
            o.entry(id).or_default().file = entries;
        }
    }

    /// The provider pushed a new set (`permissions.replace`).
    pub fn replace_provider_layer(&self, id: PlayerId, entries: Vec<PermissionEntry>) -> bool {
        self.with(id, |p| {
            p.epoch += 1;
            p.provider = entries;
            p.loaded = true;
        })
        .is_some()
    }

    /// The set of a load started at `epoch` on the provider's `instance`;
    /// false when a newer push came in between (or the player left).
    pub fn load_provider_layer(
        &self,
        id: PlayerId,
        epoch: u64,
        instance: u64,
        entries: Vec<PermissionEntry>,
    ) -> bool {
        self.with(id, |p| {
            p.loaded_on = instance;
            if p.epoch == epoch {
                p.provider = entries;
                p.loaded = true;
            }
            p.epoch == epoch
        })
        .unwrap_or(false)
    }

    pub fn epoch(&self, id: PlayerId) -> Option<u64> {
        self.online.read().ok()?.get(&id).map(|p| p.epoch)
    }

    fn with<T>(&self, id: PlayerId, f: impl FnOnce(&mut PlayerPerms) -> T) -> Option<T> {
        self.online.write().ok()?.get_mut(&id).map(f)
    }

    /// One node for the session (`permissions.set`); `None` removes it.
    pub fn set_node(
        &self,
        id: PlayerId,
        node: &str,
        value: Option<bool>,
        ctx: PermContext,
    ) -> bool {
        let Ok(mut o) = self.online.write() else {
            return false;
        };
        let Some(p) = o.get_mut(&id) else {
            return false;
        };
        let node = node.to_ascii_lowercase();
        p.session.retain(|e| !(e.node == node && e.context == ctx));
        if let Some(value) = value {
            p.session.push(PermissionEntry {
                node,
                value,
                context: ctx,
            });
        }
        true
    }

    /// Marks the player as due for the provider; false for an unknown one.
    pub fn mark_eligible(&self, id: PlayerId) -> bool {
        self.online
            .write()
            .ok()
            .and_then(|mut o| o.get_mut(&id).map(|p| p.eligible = true))
            .is_some()
    }

    pub fn eligible(&self) -> Vec<PlayerId> {
        self.players(|p| p.eligible)
    }

    /// Players past the point of loading without a set from the provider
    /// and without a load running.
    pub fn unloaded(&self) -> Vec<PlayerId> {
        self.players(|p| p.eligible && !p.loaded && p.loading == 0)
    }

    /// Players past the point of loading that the provider's `instance` has
    /// not loaded, without a load running.
    pub fn not_loaded_by(&self, instance: u64) -> Vec<PlayerId> {
        self.players(|p| p.eligible && p.loading == 0 && (!p.loaded || p.loaded_on < instance))
    }

    /// Counts a provider load of the player as running until the guard drops.
    pub fn start_load(&self, id: PlayerId) -> LoadGuard<'_> {
        self.with(id, |p| p.loading += 1);
        LoadGuard { perms: self, id }
    }

    fn players(&self, f: impl Fn(&PlayerPerms) -> bool) -> Vec<PlayerId> {
        self.online
            .read()
            .map(|o| o.iter().filter(|(_, p)| f(p)).map(|(id, _)| *id).collect())
            .unwrap_or_default()
    }

    /// Every player back on the file; only for a proxy without a provider.
    /// Returns the players whose permissions change.
    #[cfg(test)]
    pub fn drop_provider(&self) -> Vec<PlayerId> {
        let Ok(mut o) = self.online.write() else {
            return Vec::new();
        };
        o.iter_mut()
            .filter(|(_, p)| p.loaded)
            .map(|(id, p)| {
                p.loaded = false;
                p.provider.clear();
                *id
            })
            .collect()
    }

    pub fn remove(&self, id: PlayerId) {
        if let Ok(mut o) = self.online.write() {
            o.remove(&id);
        }
    }
}

/// Ends a running provider load of a player when dropped.
pub(crate) struct LoadGuard<'a> {
    perms: &'a Permissions,
    id: PlayerId,
}

impl Drop for LoadGuard<'_> {
    fn drop(&mut self) {
        self.perms
            .with(self.id, |p| p.loading = p.loading.saturating_sub(1));
    }
}

/// FNV-1a 64 of a text, hex: tells a changed `permissions.yml` from the
/// same one across restarts.
fn fnv_hex(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use uuid::Uuid;

    use super::*;
    use crate::config::HostConfig;

    fn e(node: &str, value: bool, ctx: PermContext) -> PermissionEntry {
        PermissionEntry {
            node: node.into(),
            value,
            context: ctx,
        }
    }

    use PermContext::Global;

    fn file_layer(entries: &[PermissionEntry]) -> Layers<'_> {
        Layers {
            file: entries,
            ..Layers::default()
        }
    }

    const FILE: &str = r#"
groups:
  default:
    permissions: [pumbo.proxy.server]
    group:
      minigames: { permissions: ["-pumbo.proxy.server"] }
  vip:
    inherits: [default]
    permissions: [pumbo.proxy.glist, "-pumbo.proxy.alert"]
    server:
      survival: { permissions: [pumbo.skins.change] }
  mod:
    inherits: [vip]
    permissions: [pumbo.proxy.alert]

players:
  069a79f4-44e9-4726-a5be-fca90e38aaf5:
    name: Notch
    groups: [default]
    server:
      survival: { groups: [vip] }
  jeb_:
    groups: [mod]
"#;

    fn cfg() -> HostConfig {
        HostConfig::parse(
            r#"
servers:
  survival: { address: x }
  lobby: { address: y }
server-group:
  - name: minigames
    servers: [bedwars]
"#,
        )
        .unwrap()
    }

    #[test]
    fn file_source_flattening() {
        let (src, warnings) = FileSource::parse(FILE, &cfg()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let notch = Subject {
            uuid: Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap(),
            name: "Notch".into(),
        };
        let entries = src.entries_for(&notch);
        let has = |node: &str, server: &str, groups: &[&str]| {
            let groups: Vec<String> = groups.iter().map(|g| g.to_string()).collect();
            resolve(file_layer(&entries), node, &levels(Some(server), &groups)).map(|d| d.value)
        };
        assert_eq!(has("pumbo.proxy.server", "lobby", &[]), Some(true));
        assert_eq!(
            has("pumbo.proxy.server", "bedwars", &["minigames"]),
            Some(false)
        );
        // VIP only on survival.
        assert_eq!(has("pumbo.proxy.glist", "survival", &[]), Some(true));
        assert_eq!(has("pumbo.proxy.glist", "lobby", &[]), None);
        assert_eq!(has("pumbo.skins.change", "survival", &[]), Some(true));
        // default's `group.minigames` entry inherited through vip on survival
        // does not apply: survival is not in minigames.
        assert_eq!(has("pumbo.proxy.server", "survival", &[]), Some(true));

        // Inherited `-pumbo.proxy.alert` of vip is overridden by mod's own entry.
        let jeb = Subject {
            uuid: Uuid::nil(),
            name: "Jeb_".into(),
        };
        let entries = src.entries_for(&jeb);
        let r = resolve(
            file_layer(&entries),
            "pumbo.proxy.alert",
            &levels(None, &[]),
        )
        .map(|d| d.value);
        assert_eq!(r, Some(true));
        let r = resolve(
            file_layer(&entries),
            "pumbo.proxy.server",
            &levels(None, &[]),
        )
        .map(|d| d.value);
        assert_eq!(r, Some(true));

        // Unlisted players get the default group.
        let other = Subject {
            uuid: Uuid::nil(),
            name: "someone".into(),
        };
        let entries = src.entries_for(&other);
        let r = resolve(
            file_layer(&entries),
            "pumbo.proxy.server",
            &levels(None, &[]),
        )
        .map(|d| d.value);
        assert_eq!(r, Some(true));
    }

    #[test]
    fn file_export_for_the_bridge() {
        let text = r#"
groups:
  default:
    permissions: [pumbo.proxy.server, "minecraft:command.help"]
  builder:
    inherits: [default]
    permissions: ["-minecraft:command.gamemode"]
    server:
      survival: { permissions: ["minecraft:command.gamemode"] }
players:
  069a79f4-44e9-4726-a5be-fca90e38aaf5:
    name: Notch
    server:
      survival: { groups: [builder] }
  Jeb_:
    groups: [builder]
"#;
        let (src, _) = FileSource::parse(text, &cfg()).unwrap();
        let x = src.export("survival", &[]);
        assert_eq!(x["format"], "pumboperms");
        let builder = &x["groups"][0];
        assert_eq!(builder["name"], "builder");
        assert_eq!(
            builder["data"]["permissions"][0],
            serde_json::json!({"node": "minecraft:command.gamemode", "value": true}),
            "the server context wins over global"
        );
        assert_eq!(builder["data"]["parents"][0]["group"], "default");
        assert_eq!(
            x["groups"][1]["data"]["permissions"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "pumbo.* nodes stay on the proxy"
        );
        let users = x["users"].as_array().unwrap();
        assert_eq!(users.len(), 1, "players by nickname are left out");
        assert_eq!(users[0]["data"]["parents"][0]["group"], "builder");
        assert!(
            src.export("lobby", &[])["users"][0]["data"]["parents"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn snapshot_for_the_provider_keeps_contexts_and_names() {
        let (src, _) = FileSource::parse(FILE, &cfg()).unwrap();
        let x = src.snapshot();
        assert_eq!(x["format"], "pumboperms");
        let vip = x["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == "vip")
            .unwrap();
        let perms = vip["data"]["permissions"].as_array().unwrap();
        assert!(perms.contains(
            &serde_json::json!({"node": "pumbo.proxy.alert", "value": false, "context": {}})
        ));
        assert!(perms.contains(
            &serde_json::json!({"node": "pumbo.skins.change", "value": true, "context": {"server": "survival"}})
        ));
        assert_eq!(vip["data"]["parents"][0]["group"], "default");
        let notch = &x["users"][0];
        assert_eq!(notch["name"], "Notch");
        assert_eq!(
            notch["data"]["parents"],
            serde_json::json!([{"group": "default", "context": {}}, {"group": "vip", "context": {"server": "survival"}}])
        );
        assert_eq!(x["by-name"][0]["name"], "jeb_");
        assert_eq!(x["by-name"][0]["data"]["parents"][0]["group"], "mod");
        assert_eq!(fnv_hex("a"), fnv_hex("a"));
        assert_ne!(fnv_hex("a"), fnv_hex("b"));
    }

    fn one_online(id: PlayerId) -> Permissions {
        let p = Permissions::default();
        p.online.write().unwrap().insert(id, PlayerPerms::default());
        p
    }

    fn value(p: &Permissions, id: PlayerId, node: &str) -> Option<bool> {
        p.decide(id, node, &levels(None, &[])).map(|d| d.value)
    }

    #[test]
    fn provider_start_skips_players_its_instance_loaded() {
        let p = one_online(7);
        p.mark_eligible(7);
        let first = p.instance.load(Ordering::SeqCst);
        assert_eq!(p.not_loaded_by(first), vec![7]);
        // A login load the new instance answered before the file handoff.
        assert!(p.load_provider_layer(7, 0, first, Vec::new()));
        assert!(p.not_loaded_by(first).is_empty());
        // The provider went down: its next instance has to load the player.
        p.instance.fetch_add(1, Ordering::SeqCst);
        assert_eq!(p.not_loaded_by(first + 1), vec![7]);
    }

    #[test]
    fn session_node_survives_provider_replace() {
        let p = one_online(1);
        p.set_node(1, "minigame.spectate", Some(true), Global);
        p.replace_provider_layer(1, vec![e("pumbo.proxy.server", true, Global)]);
        assert_eq!(value(&p, 1, "minigame.spectate"), Some(true));
    }

    #[test]
    fn session_node_survives_provider_drop() {
        let p = one_online(1);
        p.replace_provider_layer(1, vec![e("pumbo.proxy.server", true, Global)]);
        p.set_node(1, "minigame.spectate", Some(true), Global);
        p.drop_provider();
        assert_eq!(value(&p, 1, "minigame.spectate"), Some(true));
    }

    #[test]
    fn clearing_session_node_keeps_provider_entry() {
        let p = one_online(1);
        p.replace_provider_layer(1, vec![e("minigame.spectate", false, Global)]);
        p.set_node(1, "minigame.spectate", Some(true), Global);
        p.set_node(1, "minigame.spectate", None, Global);
        assert_eq!(value(&p, 1, "minigame.spectate"), Some(false));
    }

    #[test]
    fn server_context_ignores_case() {
        let cfg = HostConfig::parse(
            "servers:\n  Survival: { address: x }\nserver-group:\n  - name: Games\n    servers: [Arena]\n",
        )
        .unwrap();
        let text = r#"
players:
  069a79f4-44e9-4726-a5be-fca90e38aaf5:
    server:
      survival: { permissions: [pumbo.proxy.glist, "minecraft:command.help"] }
    group:
      GAMES: { permissions: ["-pumbo.proxy.server"] }
"#;
        let (src, warnings) = FileSource::parse(text, &cfg).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let notch = Subject {
            uuid: Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap(),
            name: "Notch".into(),
        };
        let entries = src.entries_for(&notch);
        let at = |server: &str| levels(Some(server), &cfg.groups_of(server));
        let has = |node: &str, server: &str| {
            resolve(file_layer(&entries), node, &at(server)).map(|d| d.value)
        };
        assert_eq!(has("pumbo.proxy.glist", "Survival"), Some(true));
        assert_eq!(has("pumbo.proxy.server", "arena"), Some(false));
        let x = src.export("Survival", &[]);
        assert_eq!(
            x["users"][0]["data"]["permissions"][0]["node"],
            "minecraft:command.help"
        );

        let p = one_online(1);
        let ctx =
            crate::imports::perm_context(&crate::wit::types::Context::Server("SURVIVAL".into()));
        p.replace_provider_layer(1, vec![e("pumbo.proxy.alert", true, ctx)]);
        assert_eq!(
            p.decide(1, "pumbo.proxy.alert", &at("Survival"))
                .map(|d| d.value),
            Some(true)
        );
    }

    #[test]
    fn unknown_contexts_are_warnings() {
        let text = FILE.replace(
            "  mod:\n",
            "  x:\n    server: { nowhere: { permissions: [a] } }\n    group: { nogroup: { permissions: [b] } }\n    inherits: [ghost]\n  mod:\n",
        );
        let (_, warnings) = FileSource::parse(&text, &cfg()).unwrap();
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(FileSource::parse("groups:\n  x:\n    bogus: 1\n", &cfg()).is_err());
        // Not valid YAML: the error names the line.
        let err = FileSource::parse("groups:\n  x:\n    permissions: [a\n", &cfg()).unwrap_err();
        assert!(err.contains("line"), "{err}");
    }

    /// The host side of a change at network scale: 20 groups of 50 nodes
    /// with entries on 10 servers in each player's set, 300 players online,
    /// 3000 players in `permissions.yml`.
    #[test]
    #[ignore = "release benchmark"]
    fn host_lookups_at_scale() {
        use std::time::{Duration, Instant};
        let mut set = Vec::new();
        for g in 0..20 {
            set.push(e(&format!("group.g{g}"), true, Global));
            for i in 0..50 {
                let ctx = match i % 5 {
                    0 => PermContext::server(&format!("s{}", (g + i) % 10)),
                    _ => Global,
                };
                set.push(e(&format!("pumbo.g{g}.n{i}"), i % 3 != 0, ctx));
            }
            set.push(e(&format!("pumbog{g}:*"), true, Global));
        }
        let p = Permissions::default();
        for id in 0..300 {
            p.online.write().unwrap().insert(id, PlayerPerms::default());
            p.replace_provider_layer(id, set.clone());
        }
        let at = levels(Some("s3"), &["games".into()]);

        // Worst case: no entry anywhere, every level scans the whole set.
        let t = Instant::now();
        for i in 0..10_000u64 {
            assert!(p.decide(i % 300, "pumbo.none.here", &at).is_none());
        }
        let check = t.elapsed() / 10_000;

        let catalog: Vec<String> = (0..200)
            .map(|i| format!("pumbog{}:node{i}", i % 20))
            .collect();
        let t = Instant::now();
        let online = p.online.read().unwrap();
        for id in 0..300 {
            let set =
                pumbo_contracts::permissions::backend_set(online[&id].layers(), &catalog, &at);
            assert!(!set.is_empty());
        }
        drop(online);
        let sets = t.elapsed();

        let mut yaml =
            String::from("groups:\n  vip: { permissions: [pumbo.proxy.glist] }\nplayers:\n");
        for i in 0..3000u128 {
            yaml.push_str(&format!("  {}: {{ groups: [vip] }}\n", Uuid::from_u128(i)));
        }
        let (src, _) = FileSource::parse(&yaml, &cfg()).unwrap();
        let who = Subject {
            uuid: Uuid::from_u128(2999),
            name: "Last".into(),
        };
        let t = Instant::now();
        for _ in 0..100 {
            assert!(!src.entries_for(&who).is_empty());
        }
        let join = t.elapsed() / 100;

        eprintln!(
            "[measure] set of {} entries: check {check:?} (no entry), backend sets for 300 players {sets:?}, file layer at join {join:?}",
            set.len()
        );
        assert!(check < Duration::from_micros(50), "{check:?}");
        // The budget of one push to 300 players.
        assert!(sets < Duration::from_millis(50), "{sets:?}");
        assert!(join < Duration::from_millis(1), "{join:?}");
    }
}
