//! Differences between tables: releases of one protocol and consecutive
//! protocols (the report reviewed when a new Minecraft version comes out,
//! plan §2.10).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use pumbo_data::Tables;
use pumbo_protocol::{KNOWN_PACKETS, ProtocolVersion, VersionFeatures};

type Key = (String, String, String);

fn packet_map(t: &Tables) -> BTreeMap<Key, i32> {
    t.packets
        .iter()
        .map(|p| {
            (
                (
                    p.phase.report_name().to_string(),
                    p.direction.report_name().to_string(),
                    p.name.clone(),
                ),
                p.id,
            )
        })
        .collect()
}

/// True if two tables have the same content (releases ignored).
pub fn same_content(a: &Tables, b: &Tables) -> bool {
    a.packets == b.packets
        && a.aliases == b.aliases
        && a.registries == b.registries
        && a.blocks == b.blocks
}

/// Lines describing what changed from `old` to `new`.
pub fn describe(old: &Tables, new: &Tables) -> Vec<String> {
    let mut out = Vec::new();
    let (a, b) = (packet_map(old), packet_map(new));

    let known: BTreeSet<Key> = KNOWN_PACKETS
        .iter()
        .map(|(p, d, k)| {
            (
                p.report_name().to_string(),
                d.report_name().to_string(),
                k.name().to_string(),
            )
        })
        .collect();
    let mut moved = 0;
    for key in &known {
        match (a.get(key), b.get(key)) {
            (Some(x), Some(y)) if x != y => moved += 1,
            (None, Some(y)) => out.push(format!(
                "known packet added: {}/{}/{} = {y}",
                key.0, key.1, key.2
            )),
            (Some(x), None) => out.push(format!(
                "KNOWN PACKET REMOVED: {}/{}/{} (was {x})",
                key.0, key.1, key.2
            )),
            _ => {}
        }
    }
    if moved > 0 {
        out.push(format!("known packets with a new id: {moved}"));
    }

    // All packets: per phase and direction, removed and added names. Both at
    // once may be a rename that needs an alias.
    let groups: BTreeSet<(String, String)> = a
        .keys()
        .chain(b.keys())
        .map(|(p, d, _)| (p.clone(), d.clone()))
        .collect();
    for (phase, dir) in groups {
        let names = |m: &BTreeMap<Key, i32>| -> BTreeSet<String> {
            m.keys()
                .filter(|(p, d, _)| *p == phase && *d == dir)
                .map(|(_, _, n)| n.clone())
                .collect()
        };
        let (na, nb) = (names(&a), names(&b));
        let removed: Vec<_> = na.difference(&nb).cloned().collect();
        let added: Vec<_> = nb.difference(&na).cloned().collect();
        if !added.is_empty() {
            out.push(format!("{phase}/{dir} added: {}", added.join(", ")));
        }
        if !removed.is_empty() {
            out.push(format!("{phase}/{dir} removed: {}", removed.join(", ")));
        }
        if !added.is_empty() && !removed.is_empty() {
            out.push(format!(
                "{phase}/{dir}: CHECK FOR RENAMES (add an alias in reports.rs if a packet was renamed)"
            ));
        }
    }

    for ra in &old.registries {
        let Some(rb) = new.registry(&ra.name) else {
            out.push(format!("registry {} removed", ra.name));
            continue;
        };
        let (sa, sb): (BTreeSet<&String>, BTreeSet<&String>) =
            (ra.entries.iter().collect(), rb.entries.iter().collect());
        let added: Vec<&str> = sb.difference(&sa).map(|s| s.as_str()).collect();
        let removed: Vec<&str> = sa.difference(&sb).map(|s| s.as_str()).collect();
        if added.is_empty() && removed.is_empty() {
            if ra.entries != rb.entries {
                out.push(format!("registry {}: same entries, new ids", ra.name));
            }
            continue;
        }
        let list = |v: &[&str]| {
            if v.len() <= 12 {
                v.join(", ")
            } else {
                format!("{} entries", v.len())
            }
        };
        out.push(format!(
            "registry {}: {} -> {} entries; added: {}; removed: {}",
            ra.name,
            ra.len(),
            rb.len(),
            list(&added),
            list(&removed)
        ));
    }

    let ba: BTreeSet<&str> = old.blocks.iter().map(|b| b.name.as_str()).collect();
    let bb: BTreeSet<&str> = new.blocks.iter().map(|b| b.name.as_str()).collect();
    let added = bb.difference(&ba).count();
    let removed = ba.difference(&bb).count();
    if added > 0 || removed > 0 || old.block_state_count() != new.block_state_count() {
        out.push(format!(
            "blocks: {} -> {} ({added} added, {removed} removed), states {} -> {}",
            old.blocks.len(),
            new.blocks.len(),
            old.block_state_count(),
            new.block_state_count()
        ));
    }
    out
}

/// Report over all protocols, oldest first.
pub fn report(all: &[Tables]) -> String {
    let mut out =
        String::from("# Generated by pumbo-datagen. Changes between consecutive protocols.\n");
    let mut prev: Option<&Tables> = None;
    for t in all {
        let releases = t.release_names().join(", ");
        let _ = writeln!(out, "\n## protocol {} ({releases})", t.protocol);
        let _ = writeln!(
            out,
            "packets {}, blocks {}, block states {}",
            t.packets.len(),
            t.blocks.len(),
            t.block_state_count()
        );
        for r in &t.registries {
            let _ = writeln!(out, "registry {}: {}", r.name, r.len());
        }
        let features: Vec<String> = VersionFeatures::for_protocol(ProtocolVersion(t.protocol.0))
            .list()
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.to_string())
            .collect();
        let _ = writeln!(out, "layout features: {}", features.join(", "));
        if let Some(p) = prev {
            let _ = writeln!(out, "changes since {}:", p.protocol);
            for line in describe(p, t) {
                let _ = writeln!(out, "- {line}");
            }
        }
        prev = Some(t);
    }
    out
}
