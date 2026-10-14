//! Permission entries and their resolution, shared by the host, the SDK and
//! plugins, so every side answers a check the same way.
//!
//! Resolution: levels server > group (config order) > global; the first level
//! with a matching entry decides. Within a level: an exact node before the
//! longest matching `a.b.*`, before `*`; on a tie a session node (set by a
//! plugin until the player leaves) wins over the file, the file wins over the
//! provider (the administrator has the last word) and `false` wins over `true`.
//! Resolution order modelled on the public LuckPerms documentation (MIT).

use std::collections::{BTreeMap, BTreeSet};

/// Context of an entry. Resolution: server > group (config order) > global.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PermContext {
    Global,
    Group(String),
    Server(String),
}

impl PermContext {
    /// Server and group names compare in lower case on every side.
    pub fn server(name: &str) -> Self {
        Self::Server(name.to_ascii_lowercase())
    }

    pub fn group(name: &str) -> Self {
        Self::Group(name.to_ascii_lowercase())
    }
}

/// One permission node in one context. `node` may end with `.*` or be `*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionEntry {
    pub node: String,
    pub value: bool,
    pub context: PermContext,
}

/// Where an entry comes from, highest priority first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Session,
    File,
    Provider,
}

/// The entries of one player, by layer.
#[derive(Debug, Clone, Copy, Default)]
pub struct Layers<'a> {
    pub session: &'a [PermissionEntry],
    pub file: &'a [PermissionEntry],
    pub provider: &'a [PermissionEntry],
}

impl<'a> Layers<'a> {
    fn iter(self) -> impl Iterator<Item = (Layer, &'a PermissionEntry)> {
        let tag =
            |layer: Layer, entries: &'a [PermissionEntry]| entries.iter().map(move |e| (layer, e));
        tag(Layer::Session, self.session)
            .chain(tag(Layer::File, self.file))
            .chain(tag(Layer::Provider, self.provider))
    }
}

/// Levels of a query, highest priority first.
pub fn levels(server: Option<&str>, groups: &[String]) -> Vec<PermContext> {
    let mut out = Vec::with_capacity(groups.len() + 2);
    if let Some(s) = server {
        out.push(PermContext::server(s));
    }
    out.extend(groups.iter().map(|g| PermContext::group(g)));
    out.push(PermContext::Global);
    out
}

/// How specific an entry is for `node`; `None` if it does not match.
fn specificity(entry: &str, node: &str) -> Option<usize> {
    if entry == node {
        return Some(usize::MAX);
    }
    if entry == "*" {
        return Some(0);
    }
    // `minecraft:*`: everything in a namespace.
    if let Some(ns) = entry.strip_suffix(":*") {
        let rest = node.strip_prefix(ns)?;
        return rest.starts_with(':').then_some(ns.len() + 1);
    }
    let prefix = entry.strip_suffix(".*")?;
    let rest = node.strip_prefix(prefix)?;
    rest.starts_with('.').then_some(prefix.len() + 1)
}

/// The decision and the entry that made it (for `/pumbo proxy perms check`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub value: bool,
    pub entry: PermissionEntry,
    pub layer: Layer,
}

pub fn resolve(layers: Layers<'_>, node: &str, levels: &[PermContext]) -> Option<Decision> {
    let node = node.to_ascii_lowercase();
    for level in levels {
        let mut best: Option<(usize, Layer, &PermissionEntry)> = None;
        // Layers come in priority order, so a tie keeps the earlier layer.
        for (layer, e) in layers.iter() {
            if e.context != *level {
                continue;
            }
            let Some(spec) = specificity(&e.node, &node) else {
                continue;
            };
            let better = match best {
                None => true,
                Some((bs, bl, be)) => {
                    spec > bs || (spec == bs && layer == bl && !e.value && be.value)
                }
            };
            if better {
                best = Some((spec, layer, e));
            }
        }
        if let Some((_, layer, e)) = best {
            return Some(Decision {
                value: e.value,
                entry: e.clone(),
                layer,
            });
        }
    }
    None
}

/// Decisions for a backend (PumboBridge `perm-set`): every node of `catalog`,
/// every namespaced node (`ns:…`, `ns:*`, `ns:a.*`) and every exact
/// `group.<name>` of the entries, resolved at `levels`; only nodes with a
/// decision, then `drop_shadowed`.
pub fn backend_set(
    layers: Layers<'_>,
    catalog: &[String],
    levels: &[PermContext],
) -> BTreeMap<String, bool> {
    let entries = || {
        layers
            .iter()
            .map(|(_, e)| e)
            .filter(|e| e.node.contains(':'))
    };
    let mut exact: BTreeSet<String> = catalog.iter().map(|n| n.to_ascii_lowercase()).collect();
    exact.extend(
        entries()
            .filter(|e| !e.node.ends_with('*'))
            .map(|e| e.node.clone()),
    );
    exact.extend(
        layers
            .iter()
            .map(|(_, e)| e)
            .filter(|e| e.node.starts_with("group.") && !e.node.ends_with('*'))
            .map(|e| e.node.clone()),
    );
    let wildcards = entries()
        .filter(|e| e.node.ends_with(":*") || e.node.ends_with(".*"))
        .map(|e| e.node.clone());
    // Only these entries can match an `ns:` or `group.` node; scanning the
    // whole set for each node made a large set cost milliseconds per player.
    let narrow = |s: &[PermissionEntry]| -> Vec<PermissionEntry> {
        s.iter()
            .filter(|e| {
                e.node.contains(':') || e.node.starts_with("group.") || e.node.ends_with('*')
            })
            .cloned()
            .collect()
    };
    let (session, file, provider) = (
        narrow(layers.session),
        narrow(layers.file),
        narrow(layers.provider),
    );
    let few = Layers {
        session: &session,
        file: &file,
        provider: &provider,
    };
    let mut set: BTreeMap<String, bool> = exact
        .iter()
        .cloned()
        .chain(wildcards)
        .filter_map(|n| {
            let from = match n.contains(':') || n.starts_with("group.") {
                true => few,
                false => layers,
            };
            let d = resolve(from, &n, levels)?;
            Some((n, d.value))
        })
        .collect();
    drop_shadowed(&mut set, &exact);
    set
}

/// Makes a set read the same through Pumpkin 0.2.0, which takes the broadest
/// matching attribute and reads an exact parent node as a wildcard for its
/// children. Removes a wildcard that has a narrower wildcard with another
/// value under it, and sets `false` for each `catalog` node without an entry
/// under an exact parent node set to `true`. Returns the removed wildcards.
pub fn drop_shadowed(set: &mut BTreeMap<String, bool>, catalog: &BTreeSet<String>) -> Vec<String> {
    let dropped: Vec<String> = set
        .iter()
        .filter(|(w, v)| {
            let Some(base) = w.strip_suffix('*') else {
                return false;
            };
            one_colon(w)
                && (base.ends_with(':') || base.ends_with('.'))
                && set.iter().any(|(n, nv)| {
                    nv != *v && n != *w && n.ends_with(".*") && n.starts_with(base) && one_colon(n)
                })
        })
        .map(|(w, _)| w.clone())
        .collect();
    for w in &dropped {
        set.remove(w);
    }
    let children: Vec<String> = catalog
        .iter()
        .filter(|c| one_colon(c) && !set.contains_key(*c))
        .filter(|c| {
            c.match_indices('.')
                .filter_map(|(i, _)| c.get(..i))
                .any(|p| p.contains(':') && set.get(p) == Some(&true))
        })
        .cloned()
        .collect();
    for c in children {
        set.insert(c, false);
    }
    dropped
}

/// Pumpkin reads wildcards and parent nodes only for `ns:key` nodes.
fn one_colon(node: &str) -> bool {
    node.matches(':').count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(node: &str, value: bool, ctx: PermContext) -> PermissionEntry {
        PermissionEntry {
            node: node.into(),
            value,
            context: ctx,
        }
    }

    fn srv(s: &str) -> PermContext {
        PermContext::Server(s.into())
    }

    fn grp(s: &str) -> PermContext {
        PermContext::Group(s.into())
    }

    use PermContext::Global;

    fn file_layer(entries: &[PermissionEntry]) -> Layers<'_> {
        Layers {
            file: entries,
            ..Layers::default()
        }
    }

    fn provider_layer(entries: &[PermissionEntry]) -> Layers<'_> {
        Layers {
            provider: entries,
            ..Layers::default()
        }
    }

    #[test]
    fn session_wins_a_tie_over_file_and_provider() {
        let session = [e("a.b", true, Global)];
        let file = [e("a.b", false, Global)];
        let provider = [e("a.b", false, Global), e("a.*", true, srv("survival"))];
        let layers = Layers {
            session: &session,
            file: &file,
            provider: &provider,
        };
        let d = resolve(layers, "a.b", &levels(None, &[])).unwrap();
        assert_eq!((d.value, d.layer), (true, Layer::Session));
        // The server level still comes before the global session node.
        let d = resolve(layers, "a.b", &levels(Some("survival"), &[])).unwrap();
        assert_eq!((d.value, d.layer), (true, Layer::Provider));
    }

    /// The case table from plan §5.8.4.
    #[test]
    fn resolution_cases() {
        let lv = levels(Some("survival"), &["survivals".into(), "all".into()]);
        let lobby = levels(Some("lobby"), &["all".into()]);
        type Case<'a> = (
            &'a str,
            Vec<PermissionEntry>,
            Vec<PermissionEntry>,
            &'a str,
            &'a [PermContext],
            Option<bool>,
        );
        let cases: Vec<Case> = vec![
            ("nothing", vec![], vec![], "a.b", &lv, None),
            (
                "global",
                vec![e("a.b", true, Global)],
                vec![],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "server beats global",
                vec![e("a.b", true, Global), e("a.b", false, srv("survival"))],
                vec![],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "other server ignored",
                vec![e("a.b", false, srv("lobby")), e("a.b", true, Global)],
                vec![],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "group beats global",
                vec![e("a.b", true, Global), e("a.b", false, grp("survivals"))],
                vec![],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "first group wins",
                vec![
                    e("a.b", true, grp("all")),
                    e("a.b", false, grp("survivals")),
                ],
                vec![],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "server beats group",
                vec![
                    e("a.b", false, grp("survivals")),
                    e("a.b", true, srv("survival")),
                ],
                vec![],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "exact beats wildcard",
                vec![e("a.*", false, Global), e("a.b", true, Global)],
                vec![],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "longest wildcard",
                vec![e("a.*", false, Global), e("a.b.*", true, Global)],
                vec![],
                "a.b.c",
                &lv,
                Some(true),
            ),
            (
                "star",
                vec![e("*", true, Global)],
                vec![],
                "x.y",
                &lv,
                Some(true),
            ),
            (
                "wildcard does not match its base",
                vec![e("a.b.*", true, Global)],
                vec![],
                "a.b",
                &lv,
                None,
            ),
            (
                "false wins a tie",
                vec![e("a.b", true, Global), e("a.b", false, Global)],
                vec![],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "file wins over provider",
                vec![e("a.b", true, Global)],
                vec![e("a.b", false, Global)],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "provider fills in",
                vec![],
                vec![e("a.b", true, srv("survival"))],
                "a.b",
                &lv,
                Some(true),
            ),
            (
                "level before layer",
                vec![e("a.b", true, Global)],
                vec![e("a.b", false, srv("survival"))],
                "a.b",
                &lv,
                Some(false),
            ),
            (
                "lobby sees global",
                vec![e("a.b", true, Global)],
                vec![e("a.b", false, srv("survival"))],
                "a.b",
                &lobby,
                Some(true),
            ),
            (
                "case-insensitive",
                vec![e("a.b", true, Global)],
                vec![],
                "A.B",
                &lv,
                Some(true),
            ),
        ];
        for (name, file, provider, node, levels, want) in cases {
            let layers = Layers {
                file: &file,
                provider: &provider,
                ..Layers::default()
            };
            let got = resolve(layers, node, levels).map(|d| d.value);
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn namespace_wildcard_matches_only_its_namespace() {
        let all = [e("minecraft:*", true, PermContext::Global)];
        let lv = levels(Some("survival"), &[]);
        assert_eq!(
            resolve(file_layer(&all), "minecraft:command.gamemode", &lv).map(|d| d.value),
            Some(true)
        );
        assert_eq!(resolve(file_layer(&all), "minecraftx:a", &lv), None);
    }

    /// A provider set of PumboPerms size (100 nodes in global and in three
    /// server contexts): a check is a scan of the player's set in memory, no
    /// plugin call.
    #[test]
    fn checks_from_a_provider_set_are_fast() {
        let mut set = Vec::new();
        for ctx in [Global, srv("lobby"), srv("survival"), srv("arena")] {
            for i in 0..100 {
                set.push(e(&format!("pumbo.node{i}.sub"), i % 2 == 0, ctx.clone()));
            }
            set.push(e("pumbo.*", true, ctx.clone()));
        }
        let lv = levels(Some("survival"), &["games".into()]);
        let t = std::time::Instant::now();
        let mut yes = 0;
        for i in 0..100_000 {
            let node = format!("pumbo.node{}.sub", i % 150);
            yes += usize::from(resolve(provider_layer(&set), &node, &lv).is_some_and(|d| d.value));
        }
        let took = t.elapsed();
        eprintln!(
            "[measure] 100000 checks, set of {} entries: {took:?}",
            set.len()
        );
        assert!(yes > 0);
        assert!(took < std::time::Duration::from_secs(10), "{took:?}");
    }

    /// `*` and a dotted wildcard without a namespace still decide catalog
    /// nodes; plain nodes in between change nothing.
    #[test]
    fn backend_set_keeps_every_entry_that_can_match() {
        let mut file = vec![
            e("*", true, Global),
            e("ns.sub.*", false, Global),
            e("group.vip", true, Global),
        ];
        file.extend((0..50).map(|i| e(&format!("pumbo.plain{i}"), false, Global)));
        let set = backend_set(
            file_layer(&file),
            &["ns.sub.x:y".into(), "other:node".into()],
            &levels(Some("survival"), &[]),
        );
        assert_eq!(set.get("ns.sub.x:y"), Some(&false));
        assert_eq!(set.get("other:node"), Some(&true));
        assert_eq!(set.get("group.vip"), Some(&true));
    }

    #[test]
    fn backend_set_resolves_catalog_and_namespaced_nodes() {
        let file = [
            e("minecraft:*", true, Global),
            e("pumbobans:ban", false, srv("survival")),
            e("pumbobans:mute", true, Global),
            e("pumbo.proxy.server", true, Global),
            e("group.vip", true, Global),
            e("group.*", true, Global),
        ];
        let set = backend_set(
            file_layer(&file),
            &["MINECRAFT:command.help".into(), "other:node".into()],
            &levels(Some("survival"), &[]),
        );
        let want: BTreeMap<String, bool> = [
            ("group.vip", true),
            ("minecraft:*", true),
            ("minecraft:command.help", true),
            ("pumbobans:ban", false),
            ("pumbobans:mute", true),
        ]
        .into_iter()
        .map(|(n, v)| (n.to_string(), v))
        .collect();
        assert_eq!(set, want);
    }

    #[test]
    fn server_context_ignores_case() {
        let set = [e("pumbo.proxy.server", false, srv("survival"))];
        let lv = levels(Some("Survival"), &[]);
        assert_eq!(
            resolve(provider_layer(&set), "pumbo.proxy.server", &lv).map(|d| d.value),
            Some(false)
        );
        let grouped = [e("pumbo.proxy.glist", true, PermContext::group("Games"))];
        let lv = levels(Some("arena"), &["GAMES".into()]);
        assert_eq!(
            resolve(provider_layer(&grouped), "pumbo.proxy.glist", &lv).map(|d| d.value),
            Some(true)
        );
    }

    fn map(entries: &[(&str, bool)]) -> BTreeMap<String, bool> {
        entries.iter().map(|(n, v)| (n.to_string(), *v)).collect()
    }

    fn names(nodes: &[&str]) -> BTreeSet<String> {
        nodes.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn drop_shadowed_keeps_pumpkin_answer_safe() {
        // Pumpkin reads `ns:*` before `ns:exempt.*`: the broader one goes.
        let mut set = map(&[
            ("ns:*", true),
            ("ns:exempt.*", false),
            ("ns:exempt.ban", false),
            ("ns:ban", true),
        ]);
        let dropped = drop_shadowed(&mut set, &names(&["ns:ban", "ns:exempt.ban"]));
        assert_eq!(dropped, ["ns:*"]);
        assert_eq!(
            set,
            map(&[
                ("ns:exempt.*", false),
                ("ns:exempt.ban", false),
                ("ns:ban", true)
            ])
        );

        // Same values, other namespaces and nodes with two colons stay.
        let mut set = map(&[
            ("ns:*", true),
            ("ns:a.*", true),
            ("other:a.*", false),
            ("ns:a:b.*", false),
        ]);
        let before = set.clone();
        assert!(drop_shadowed(&mut set, &BTreeSet::new()).is_empty());
        assert_eq!(set, before);

        // A chain loses every wildcard with a narrower one of another value.
        let mut set = map(&[("ns:*", true), ("ns:a.*", false), ("ns:a.b.*", true)]);
        assert_eq!(
            drop_shadowed(&mut set, &BTreeSet::new()),
            ["ns:*", "ns:a.*"]
        );
        assert_eq!(set, map(&[("ns:a.b.*", true)]));

        // A parent set to false already makes Pumpkin refuse its children.
        let mut set = map(&[("ns:notify", false)]);
        drop_shadowed(&mut set, &names(&["ns:notify.silent"]));
        assert_eq!(set, map(&[("ns:notify", false)]));
    }

    #[test]
    fn parent_node_does_not_grant_catalog_children() {
        let lv = levels(Some("survival"), &[]);
        let file = [
            e("pumbobans:notify", true, Global),
            e("minecraft:command.random", true, Global),
            e("minecraft:command.random.reset", true, srv("lobby")),
        ];
        let catalog: Vec<String> = [
            "pumbobans:notify.silent",
            "pumbobans:notify.alts",
            "minecraft:command.random.reset",
            "minecraft:command.help",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let set = backend_set(file_layer(&file), &catalog, &lv);
        assert_eq!(
            set,
            map(&[
                ("pumbobans:notify", true),
                ("pumbobans:notify.silent", false),
                ("pumbobans:notify.alts", false),
                ("minecraft:command.random", true),
                ("minecraft:command.random.reset", false),
            ])
        );
        // A child with its own entry keeps it.
        let set = backend_set(file_layer(&file), &catalog, &levels(Some("lobby"), &[]));
        assert_eq!(set.get("minecraft:command.random.reset"), Some(&true));
    }

    #[test]
    fn backend_set_sends_namespace_wildcards_with_their_decision() {
        let file = [
            e("pumbobans:*", true, Global),
            e("pumbobans:exempt.*", false, Global),
            e("pumbobans:mute.*", true, srv("lobby")),
            e("pumbo.*", true, Global),
        ];
        let catalog = ["pumbobans:ban".to_string(), "pumbobans:exempt.ban".into()];
        let set = backend_set(file_layer(&file), &catalog, &levels(Some("survival"), &[]));
        assert_eq!(
            set,
            map(&[
                ("pumbobans:ban", true),
                ("pumbobans:exempt.*", false),
                ("pumbobans:exempt.ban", false),
                ("pumbobans:mute.*", true),
            ])
        );
    }
}
