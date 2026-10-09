//! Golden tests against recordings of real vanilla servers (plan §5.7, §7.2).
//!
//! `pumbo-datagen record` plays scripted sessions against the vanilla server of
//! every protocol and stores the frames in `target/pumbo-data-full/<protocol>/`
//! (or `$PUMBO_DATA_FULL`). Here every recorded packet the codec decodes must
//! encode back to the same bytes, and every text component must survive
//! `pumbo-text` (NBT → model → NBT, compared without compound order).
//!
//! Recordings are not in git; without them the test reports what it skipped.
//! `PUMBO_REQUIRE_RECORDINGS=1` turns missing recordings into a failure.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use pumbo_data::DataVersion;
use pumbo_nbt::Tag;
use pumbo_protocol::packets::common::ServerLinkLabel;
use pumbo_protocol::packets::play::{BossAction, NumberFormat};
use pumbo_protocol::packets::{AnyPacket, Ctx, decode_any, encode_any};
use pumbo_protocol::{Direction, PacketKind, Phase, VersionModule};
use pumbo_testclient::recording;
use pumbo_text::{Component, TextFormat};

fn full_root() -> PathBuf {
    std::env::var_os("PUMBO_DATA_FULL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/pumbo-data-full")
        })
}

/// Text components inside a decoded packet.
fn texts(p: &AnyPacket) -> Vec<&Tag> {
    use AnyPacket as A;
    match p {
        A::PlaySystemChat(x) => vec![&x.content],
        A::PlayTitle(x) => vec![&x.text],
        A::PlaySubtitle(x) => vec![&x.text],
        A::PlayActionBar(x) => vec![&x.text],
        A::PlayTabList(x) => vec![&x.header, &x.footer],
        A::PlayDisconnect(x) | A::ConfigDisconnect(x) => vec![&x.reason],
        A::PlayBossEvent(x) => match &x.action {
            BossAction::Add { title, .. } | BossAction::UpdateTitle(title) => vec![title],
            _ => vec![],
        },
        A::PlayResourcePackPush(x) | A::ConfigResourcePackPush(x) => x.prompt.iter().collect(),
        A::ConfigServerLinks(x) => x
            .links
            .iter()
            .filter_map(|l| match &l.label {
                ServerLinkLabel::Custom(t) => Some(t),
                ServerLinkLabel::BuiltIn(_) => None,
            })
            .collect(),
        A::PlaySetObjective(x) => x
            .display
            .iter()
            .flat_map(|d| {
                let mut v = vec![&d.title];
                if let Some(NumberFormat::Fixed(t)) = &d.number_format {
                    v.push(t);
                }
                v
            })
            .collect(),
        A::PlaySetPlayerTeam(x) => x
            .parameters
            .iter()
            .flat_map(|p| [&p.display_name, &p.prefix, &p.suffix])
            .collect(),
        A::PlayCommandSuggestions(x) => x
            .matches
            .iter()
            .filter_map(|m| m.tooltip.as_ref())
            .collect(),
        _ => vec![],
    }
}

#[derive(Default)]
struct Stats {
    decoded: BTreeMap<String, usize>,
    undecoded: usize,
    texts: usize,
    failures: Vec<String>,
}

fn check_recording(module: &DataVersion, frames: &[recording::Recorded], stats: &mut Stats) {
    let f = module.features();
    let format = TextFormat {
        snake_case_events: f.text_nbt_snake_case,
        shadow_color: f.text_shadow_color,
    };
    for (i, frame) in frames.iter().enumerate() {
        let Some(kind) = module.packet_kind(frame.phase, frame.direction, frame.id) else {
            stats.undecoded += 1;
            continue;
        };
        let ctx = Ctx::new(module, frame.direction);
        let what = format!(
            "frame {i} {:?}/{:?}/{}",
            frame.phase,
            frame.direction,
            kind.name()
        );
        match decode_any(frame.phase, frame.direction, kind, &frame.payload, &ctx) {
            Ok(Some(packet)) => {
                match encode_any(&packet, &ctx) {
                    Ok(bytes) if bytes == frame.payload.as_ref() => {}
                    Ok(_) => stats
                        .failures
                        .push(format!("{what}: re-encoded bytes differ")),
                    Err(e) => stats.failures.push(format!("{what}: encode: {e}")),
                }
                if let AnyPacket::LoginDisconnect(d) = &packet {
                    stats.texts += 1;
                    let original: Result<serde_json::Value, _> =
                        serde_json::from_str(&d.reason_json);
                    match (Component::from_json(&d.reason_json), original) {
                        (Ok(c), Ok(original)) if c.to_json_value(format) == original => {}
                        (c, _) => stats.failures.push(format!(
                            "{what}: JSON text changed: {} -> {c:?}",
                            d.reason_json
                        )),
                    }
                }
                for t in texts(&packet) {
                    stats.texts += 1;
                    match Component::from_nbt(t) {
                        Ok(c) => {
                            let back = c.to_nbt(format);
                            if !back.equivalent(t) {
                                stats
                                    .failures
                                    .push(format!("{what}: text changed: {t:?} -> {back:?}"));
                            }
                        }
                        Err(e) => stats.failures.push(format!("{what}: text: {e}")),
                    }
                }
                *stats
                    .decoded
                    .entry(format!(
                        "{}/{}/{}",
                        frame.phase.report_name(),
                        frame.direction.report_name(),
                        kind.name()
                    ))
                    .or_insert(0) += 1;
            }
            Ok(None) => stats.undecoded += 1,
            Err(e) => stats.failures.push(format!("{what}: decode: {e}")),
        }
    }
}

/// Clientbound packets every recorded protocol must contain (the play script
/// makes the server send them), plus version-specific ones.
fn expected(module: &DataVersion) -> BTreeSet<String> {
    let mut want: BTreeSet<String> = [
        "login/clientbound/login_compression",
        "login/clientbound/login_finished",
        "configuration/clientbound/select_known_packs",
        "configuration/clientbound/registry_data",
        "configuration/clientbound/update_tags",
        "configuration/clientbound/update_enabled_features",
        "configuration/clientbound/custom_payload",
        "configuration/clientbound/resource_pack_push",
        "configuration/clientbound/server_links",
        "configuration/clientbound/finish_configuration",
        "play/clientbound/login",
        "play/clientbound/commands",
        "play/clientbound/keep_alive",
        "play/clientbound/system_chat",
        "play/clientbound/set_title_text",
        "play/clientbound/set_subtitle_text",
        "play/clientbound/set_titles_animation",
        "play/clientbound/set_action_bar_text",
        "play/clientbound/clear_titles",
        "play/clientbound/boss_event",
        "play/clientbound/set_objective",
        "play/clientbound/set_display_objective",
        "play/clientbound/set_player_team",
        "play/clientbound/command_suggestions",
        "play/clientbound/transfer",
        "play/clientbound/disconnect",
        "play/clientbound/bundle_delimiter",
        "status/clientbound/status_response",
        "status/clientbound/pong_response",
        "login/clientbound/login_disconnect",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let has = |phase, kind| {
        module
            .packet_id(phase, Direction::Clientbound, kind)
            .is_some()
    };
    if has(Phase::Play, PacketKind::ShowDialog) {
        want.insert("play/clientbound/show_dialog".into());
        want.insert("play/clientbound/clear_dialog".into());
    }
    if has(Phase::Configuration, PacketKind::CodeOfConduct) {
        want.insert("configuration/clientbound/code_of_conduct".into());
    }
    want
}

#[test]
fn recordings_of_vanilla_servers_round_trip() {
    let root = full_root();
    let mut missing = Vec::new();
    let mut all_failures = Vec::new();
    for v in pumbo_data::protocols() {
        let tables = pumbo_data::tables(v).unwrap();
        let module = DataVersion::new(tables);
        let dir = root.join(v.0.to_string());
        let mut stats = Stats::default();
        let mut found = false;
        for name in ["session-known.rec", "session-none.rec", "session-probe.rec"] {
            let path = dir.join(name);
            if let Ok(frames) = recording::read(&path) {
                found = true;
                check_recording(&module, &frames, &mut stats);
            }
        }
        if !found {
            missing.push(v.0);
            continue;
        }
        let covered: BTreeSet<String> = stats.decoded.keys().cloned().collect();
        let absent: Vec<String> = expected(&module).difference(&covered).cloned().collect();
        if !absent.is_empty() {
            stats
                .failures
                .push(format!("expected packets not recorded: {absent:?}"));
        }
        eprintln!(
            "protocol {v}: {} decoded packets of {} kinds re-encoded byte for byte, {} texts, {} frames passed raw, {} failures",
            stats.decoded.values().sum::<usize>(),
            stats.decoded.len(),
            stats.texts,
            stats.undecoded,
            stats.failures.len()
        );
        if std::env::var_os("PUMBO_GOLDEN_VERBOSE").is_some() {
            for (k, n) in &stats.decoded {
                eprintln!("    {k}: {n}");
            }
        }
        for f in stats.failures.iter().take(20) {
            eprintln!("    {f}");
        }
        all_failures.extend(stats.failures.into_iter().map(|f| format!("{v}: {f}")));
    }
    if !missing.is_empty() {
        eprintln!(
            "no recordings for protocols {missing:?} in {} (run `pumbo-datagen record`)",
            root.display()
        );
        assert!(
            std::env::var_os("PUMBO_REQUIRE_RECORDINGS").is_none(),
            "recordings required"
        );
    }
    assert!(all_failures.is_empty(), "{} failures", all_failures.len());
}

#[test]
fn full_registry_data_matches_the_synced_names() {
    for v in pumbo_data::protocols() {
        let (Some(full), Ok(Some(synced))) =
            (pumbo_data::full_registry_data(v), pumbo_data::synced(v))
        else {
            continue;
        };
        let module = DataVersion::new(pumbo_data::tables(v).unwrap());
        let ctx = Ctx::new(&module, Direction::Clientbound);
        let frames = recording::decode(full).unwrap();
        let mut registries = 0;
        for f in frames {
            if let Ok(Some(AnyPacket::ConfigRegistryData(r))) = decode_any(
                f.phase,
                f.direction,
                PacketKind::RegistryData,
                &f.payload,
                &ctx,
            ) {
                registries += 1;
                let names: Vec<&String> = r.entries.iter().map(|e| &e.id).collect();
                let want = synced.registry(&r.registry).unwrap();
                assert_eq!(
                    names,
                    want.entries.iter().collect::<Vec<_>>(),
                    "{v} {}",
                    r.registry
                );
                assert!(
                    r.entries.iter().all(|e| e.data.is_some()),
                    "{v} {}",
                    r.registry
                );
            }
        }
        assert_eq!(registries, synced.registries.len(), "{v}");
    }
}
