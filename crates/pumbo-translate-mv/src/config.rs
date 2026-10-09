//! Configuration phase. The client gets its own version's vanilla registries
//! and tags instead of the server's (it knows the entries from its built-in
//! core pack), plus the server's own entries of `CUSTOM_ENTRIES` with their
//! data; the server's entry names are kept to remap IDs in play.

use crate::version::{V1_21_2, V1_21_9};
use crate::wire::{Get, Put};
use crate::{Ctx, Phase, nbt};

/// Synced registries whose entry data has the same layout for 1.21–26.3 (no
/// ViaBackwards `RegistryDataRewriter` step changes them): entries the client's
/// vanilla lacks (Pumpkin's `chat_type` `raw`, datapack damage types) go to it
/// with the server's data. Other registries keep the fallback entry.
// ponytail: biomes, dimension types and the rest need the per-step data
// conversions of ViaBackwards `RegistryDataRewriter*`; add them when a server sends its own.
const CUSTOM_ENTRIES: &[&str] = &["chat_type", "damage_type"];

fn full(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

fn short(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// `select_known_packs` from the server: the client is offered its own core
/// pack (one per release of its protocol); the server's offer is kept and
/// later confirmed in full, so the server sends entry names without data.
pub(crate) fn known_packs_to_client(ctx: &mut Ctx<'_>, payload: &[u8]) -> Option<Vec<u8>> {
    ctx.s.server_packs = Some(payload.to_vec());
    let mut out = Vec::new();
    out.put_len(ctx.t.client.releases.len());
    for release in &ctx.t.client.releases {
        out.put_str("minecraft");
        out.put_str("core");
        out.put_str(release);
    }
    Some(out)
}

/// The client's answer: remembered, and the server gets its own offer back.
pub(crate) fn known_packs_to_server(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let n = r.get_len()?;
    let mut core = false;
    for _ in 0..n {
        let (ns, id, version) = (r.get_str()?, r.get_str()?, r.get_str()?);
        core |=
            ns == "minecraft" && id == "core" && ctx.t.client.releases.iter().any(|v| v == version);
    }
    ctx.s.client_knows_core = core;
    ctx.s.server_packs.clone()
}

/// `registry_data`: the server's entry names are kept, with the data of its own
/// entries in `CUSTOM_ENTRIES`; the client gets its registries before the tags.
pub(crate) fn registry_data(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let registry = short(r.get_str()?).to_string();
    let n = r.get_len()?;
    let vanilla = CUSTOM_ENTRIES
        .contains(&registry.as_str())
        .then(|| {
            ctx.t
                .client
                .synced
                .registries
                .iter()
                .find(|(n, _)| *n == registry)
        })
        .flatten()
        .map(|(_, e)| e.iter().map(|e| full(e)).collect::<Vec<_>>());
    let mut names = Vec::with_capacity(n.min(4096));
    let mut custom = Vec::new();
    for _ in 0..n {
        let name = full(r.get_str()?);
        if r.get_bool()? {
            let data = nbt::split(&mut r)?;
            if vanilla.as_ref().is_some_and(|v| !v.contains(&name)) {
                custom.push((name.clone(), data.to_vec()));
            }
        }
        names.push(name);
    }
    if !custom.is_empty() {
        ctx.s.custom_entries.insert(registry.clone(), custom);
    }
    ctx.s.server_registries.insert(registry, names);
    None
}

/// `finish_configuration` from the server: the client's registries, if the
/// server sent no tags.
pub(crate) fn finish_configuration(ctx: &mut Ctx<'_>, payload: &[u8]) -> Option<Vec<u8>> {
    send_client_registries(ctx);
    Some(payload.to_vec())
}

/// The client's registries, once per configuration, after all of the server's.
fn send_client_registries(ctx: &mut Ctx<'_>) {
    if ctx.s.registries_sent {
        return;
    }
    ctx.s.registries_sent = true;
    if !ctx.s.client_knows_core && !ctx.t.client.full_registry_data.is_empty() {
        // ponytail: prebuilt payloads; the server's own entries keep the fallback.
        ctx.s.custom_entries.clear();
        for payload in ctx.t.client.full_registry_data.clone() {
            ctx.send_client("registry_data", payload);
        }
        return;
    }
    // ponytail: a client without the core pack and no full data gets names only and
    // disconnects with the vanilla "missing registry data" error; vanilla clients always confirm.
    let mut packets = Vec::new();
    for (registry, entries) in &ctx.t.client.synced.registries {
        let custom = ctx
            .s
            .custom_entries
            .get(registry)
            .map_or(&[][..], Vec::as_slice);
        let mut out = Vec::new();
        out.put_str(&full(registry));
        out.put_len(entries.len() + custom.len());
        for e in entries {
            out.put_str(&full(e));
            out.put_bool(false);
        }
        for (name, data) in custom {
            out.put_str(name);
            out.put_bool(true);
            out.put_slice(data);
        }
        packets.push(out);
    }
    for p in packets {
        ctx.send_client("registry_data", p);
    }
}

/// `update_tags` (configuration and play): the client's vanilla tags, in
/// configuration after its registries.
pub(crate) fn update_tags(ctx: &mut Ctx<'_>, _: &[u8]) -> Option<Vec<u8>> {
    if ctx.phase == Phase::Configuration {
        send_client_registries(ctx);
    }
    let tags = &ctx.t.client.synced.tags;
    let mut out = Vec::new();
    out.put_len(tags.len());
    for (registry, list) in tags {
        out.put_str(&full(registry));
        out.put_len(list.len());
        for (tag, ids) in list {
            out.put_str(&full(tag));
            out.put_len(ids.len());
            for id in ids {
                out.put_var_int(*id);
            }
        }
    }
    Some(out)
}

/// `code_of_conduct` (1.21.9+): older clients cannot show it, so the proxy
/// accepts it for them.
pub(crate) fn code_of_conduct(ctx: &mut Ctx<'_>, payload: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_9 {
        return Some(payload.to_vec());
    }
    ctx.send_server("accept_code_of_conduct", Vec::new());
    None
}

/// `client_information` (configuration and play): particle status since 1.21.2.
pub(crate) fn client_information(ctx: &mut Ctx<'_>, payload: &[u8]) -> Option<Vec<u8>> {
    let mut out = payload.to_vec();
    if ctx.client() < V1_21_2 {
        out.put_var_int(0);
    }
    Some(out)
}

/// `start_configuration` from the server: the next configuration resends the registries.
pub(crate) fn start_configuration(ctx: &mut Ctx<'_>, payload: &[u8]) -> Option<Vec<u8>> {
    ctx.s.registries_sent = false;
    ctx.s.server_registries.clear();
    ctx.s.custom_entries.clear();
    ctx.s.dynamic_reset();
    ctx.s.entities.clear();
    Some(payload.to_vec())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::wire::Put;
    use crate::{Output, Synced, Tables, Translator, VersionData};

    fn version(protocol: i32) -> VersionData {
        let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        VersionData {
            protocol,
            packets: [
                [
                    names(&["registry_data", "update_tags", "finish_configuration"]),
                    vec![],
                ],
                [names(&["disguised_chat"]), vec![]],
            ],
            synced: Synced {
                registries: vec![("chat_type".to_string(), names(&["chat", "say_command"]))],
                ..Synced::default()
            },
            ..VersionData::default()
        }
    }

    /// Pumpkin's own `raw` chat type (its player chat, decorated by the server) reaches
    /// the client with its data, and `disguised_chat` points at it instead of `chat`.
    #[test]
    fn server_chat_type_goes_to_the_client() {
        let tables = Arc::new(Tables::new(version(775), version(777)).unwrap());
        let mut t = Translator::new(tables);
        let mut out = Output::default();
        let raw = [0x0A, 0x08, 0, 1, b'k', 0, 2, b'%', b's', 0]; // {k: "%s"}
        let mut registry = Vec::new();
        registry.put_str("minecraft:chat_type");
        registry.put_len(4);
        for name in [
            "minecraft:chat",
            "minecraft:emote_command",
            "minecraft:say_command",
        ] {
            registry.put_str(name);
            registry.put_bool(false);
        }
        registry.put_str("minecraft:raw");
        registry.put_bool(true);
        registry.put_slice(&raw);
        t.to_client(0, &registry, &mut out);
        assert!(out.to_client.is_empty(), "sent with the tags");
        t.to_client(1, &[0], &mut out);
        let mut expected = Vec::new();
        expected.put_str("minecraft:chat_type");
        expected.put_len(3);
        for name in ["minecraft:chat", "minecraft:say_command"] {
            expected.put_str(name);
            expected.put_bool(false);
        }
        expected.put_str("minecraft:raw");
        expected.put_bool(true);
        expected.put_slice(&raw);
        assert_eq!(out.to_client[0], (0, expected));
        assert_eq!(out.to_client[1].0, 1);
        t.to_client(2, &[], &mut out);
        out.clear();
        let text = [0x08, 0, 2, b'h', b'i'];
        for (server, client) in [(4, 3), (2, 1), (3, 2)] {
            let mut chat = text.to_vec();
            chat.put_var_int(server);
            chat.put_slice(&text);
            chat.put_bool(false);
            t.to_client(0, &chat, &mut out);
            let (_, payload) = out.to_client.pop().unwrap();
            assert_eq!(payload[text.len()], client, "chat type holder {server}");
        }
    }
}
