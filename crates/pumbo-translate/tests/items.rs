//! Item stacks with 26.3 components for every older client: the packet with
//! the stack always arrives, components the client cannot have are left out,
//! and the ones that changed layout come out in the client's layout (as
//! ViaVersion's `StructuredDataKey` types of that version describe them).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;

use pumbo_protocol::ProtocolVersion;
use pumbo_translate::multiversion::version_data;
use pumbo_translate_mv as mv;

struct Side {
    data: mv::VersionData,
}

impl Side {
    fn new(protocol: i32) -> Self {
        Self {
            data: version_data(ProtocolVersion(protocol)).unwrap(),
        }
    }

    fn id(&self, registry: &str, name: &str) -> i32 {
        self.data.registries[registry]
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("{} has no {registry} {name}", self.data.protocol))
            as i32
    }

    fn has(&self, registry: &str, name: &str) -> bool {
        self.data.registries[registry].iter().any(|n| n == name)
    }

    fn packet(&self, phase: usize, name: &str) -> i32 {
        self.data.packets[phase][0]
            .iter()
            .position(|n| n == name)
            .unwrap() as i32
    }
}

fn var_int(out: &mut Vec<u8>, v: i32) {
    let mut v = v as u32;
    loop {
        if v < 0x80 {
            out.push(v as u8);
            return;
        }
        out.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
}

fn string(out: &mut Vec<u8>, s: &str) {
    var_int(out, s.len() as i32);
    out.extend_from_slice(s.as_bytes());
}

/// Network NBT string tag.
fn nbt_string(out: &mut Vec<u8>, s: &str) {
    out.push(8);
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// A 26.3 stack of one item with the given components (name, value bytes).
fn stack(server: &Side, item: &str, components: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut s = Vec::new();
    var_int(&mut s, 1);
    var_int(&mut s, server.id("item", item));
    var_int(&mut s, components.len() as i32);
    var_int(&mut s, 0);
    for (name, value) in components {
        var_int(&mut s, server.id("data_component_type", name));
        s.extend_from_slice(value);
    }
    s
}

/// `container_set_slot` (container 0, state 1, slot 36) through a translator; the client's stack.
fn translate(client: &Side, server: &Side, stack: &[u8]) -> Option<Vec<u8>> {
    let tables = Arc::new(mv::Tables::new(client.data.clone(), server.data.clone()).unwrap());
    let mut t = mv::Translator::new(tables);
    let mut out = mv::Output::default();
    t.to_client(server.packet(0, "finish_configuration"), &[], &mut out);
    out.clear();
    let mut p = vec![0, 1, 0, 36];
    p.extend_from_slice(stack);
    t.to_client(server.packet(1, "container_set_slot"), &p, &mut out);
    let (id, payload) = out.to_client.pop()?;
    assert_eq!(id, client.packet(1, "container_set_slot"));
    // Container ID (a byte before 1.21.2, VarInt 0 after), state, slot.
    Some(payload[4..].to_vec())
}

/// The client's stack of one item with exactly these components.
fn expected(client: &Side, item: &str, components: &[(&str, Vec<u8>)]) -> Vec<u8> {
    stack(client, item, components)
}

/// Sound holder of a registry sound.
fn sound(side: &Side, name: &str) -> Vec<u8> {
    let mut v = Vec::new();
    var_int(&mut v, side.id("sound_event", name) + 1);
    v
}

fn tool(side: &Side, protocol: i32) -> Vec<u8> {
    let mut v = Vec::new();
    var_int(&mut v, 1); // one rule
    var_int(&mut v, 2); // one block
    var_int(&mut v, side.id_block("stone"));
    v.push(1);
    v.extend_from_slice(&1.5f32.to_be_bytes());
    v.push(0);
    v.extend_from_slice(&1.0f32.to_be_bytes());
    var_int(&mut v, 2);
    if protocol >= 770 {
        v.push(1);
    }
    v
}

impl Side {
    fn id_block(&self, name: &str) -> i32 {
        self.data
            .blocks
            .iter()
            .position(|b| b.name == name)
            .unwrap() as i32
    }
}

fn consumable(side: &Side, protocol: i32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&1.6f32.to_be_bytes());
    var_int(&mut v, 1);
    v.extend_from_slice(&sound(side, "entity.generic.eat"));
    v.push(1);
    var_int(&mut v, 1);
    var_int(&mut v, side.id("consume_effect_type", "teleport_randomly"));
    v.extend_from_slice(&16.0f32.to_be_bytes());
    if protocol >= 777 {
        v.push(1);
    }
    v
}

fn equippable(side: &Side, protocol: i32) -> Vec<u8> {
    let mut v = Vec::new();
    var_int(&mut v, 5); // head
    v.extend_from_slice(&sound(side, "item.armor.equip_iron"));
    v.extend_from_slice(&[0, 0, 0]); // no model, overlay, entities
    v.extend_from_slice(&[1, 1, 1]);
    if protocol >= 770 {
        v.push(0);
    }
    if protocol >= 771 {
        v.push(0);
        v.extend_from_slice(&sound(side, "item.shears.snip"));
    }
    v
}

const UUID: [u8; 16] = [7; 16];

fn profile(protocol: i32) -> Vec<u8> {
    let mut v = Vec::new();
    if protocol >= 773 {
        v.push(1);
        v.extend_from_slice(&UUID);
        string(&mut v, "Notch");
    } else {
        v.push(1);
        string(&mut v, "Notch");
        v.push(1);
        v.extend_from_slice(&UUID);
    }
    var_int(&mut v, 1);
    string(&mut v, "textures");
    string(&mut v, "e30=");
    v.push(1);
    string(&mut v, "sig");
    if protocol >= 773 {
        v.extend_from_slice(&[0, 0, 0, 0]);
    }
    v
}

fn pot_decorations(side: &Side, protocol: i32) -> Vec<u8> {
    let mut v = Vec::new();
    if protocol >= 777 {
        v.push(1);
        var_int(&mut v, side.id("item", "angler_pottery_sherd"));
        v.extend_from_slice(&[1, 0, 0]);
        v.extend_from_slice(&[0, 0, 0]);
    } else {
        var_int(&mut v, 4);
        var_int(&mut v, side.id("item", "angler_pottery_sherd"));
        for _ in 0..3 {
            var_int(&mut v, side.id("item", "brick"));
        }
    }
    v
}

fn damage_resistant(protocol: i32) -> Vec<u8> {
    let mut v = Vec::new();
    if protocol >= 775 {
        v.push(0);
    }
    string(&mut v, "minecraft:is_fire");
    v
}

fn sign_text() -> Vec<u8> {
    let mut v = Vec::new();
    for line in ["a", "b", "c", "d"] {
        nbt_string(&mut v, line);
    }
    v.push(0);
    var_int(&mut v, 0);
    v.push(0);
    v
}

fn can_place_on() -> Vec<u8> {
    let mut v = Vec::new();
    var_int(&mut v, 1);
    v.push(1);
    var_int(&mut v, 0);
    string(&mut v, "minecraft:logs");
    v.push(1);
    var_int(&mut v, 1);
    string(&mut v, "axis");
    v.push(1);
    string(&mut v, "y");
    v.push(0); // no NBT
    var_int(&mut v, 1); // one exact component: damage 3
    var_int(&mut v, 3); // `damage` in 26.3 (custom_data, max_stack_size, max_damage, damage)
    var_int(&mut v, 3);
    var_int(&mut v, 0); // no partial predicates
    v
}

#[test]
fn stacks_with_26_3_components_arrive_in_the_clients_layout() {
    let server = Side::new(777);
    assert_eq!(server.id("data_component_type", "damage"), 3);
    for protocol in 767..=776 {
        let client = Side::new(protocol);
        let check = |label: &str, item: &str, value: Vec<u8>, want: Option<Vec<u8>>| {
            let got = translate(&client, &server, &stack(&server, item, &[(label, value)]))
                .unwrap_or_else(|| panic!("{protocol}: {label} dropped the packet"));
            let want = expected(
                &client,
                item,
                &want.map(|w| vec![(label, w)]).unwrap_or_default(),
            );
            assert_eq!(got, want, "{protocol}: {label}");
        };
        let has = |name: &str| client.has("data_component_type", name);
        check(
            "tool",
            "diamond_pickaxe",
            tool(&server, 777),
            Some(tool(&client, protocol)),
        );
        check(
            "profile",
            "player_head",
            profile(777),
            Some(profile(protocol)),
        );
        check(
            "pot_decorations",
            "decorated_pot",
            pot_decorations(&server, 777),
            Some(pot_decorations(&client, protocol)),
        );
        check(
            "equippable",
            "diamond_helmet",
            equippable(&server, 777),
            has("equippable").then(|| equippable(&client, protocol)),
        );
        check(
            "consumable",
            "bread",
            consumable(&server, 777),
            has("consumable").then(|| consumable(&client, protocol)),
        );
        check(
            "damage_resistant",
            "netherite_sword",
            damage_resistant(777),
            has("damage_resistant").then(|| damage_resistant(protocol)),
        );
        // New in 26.3 or not translated: left out, the stack stays.
        check("sign_text_front", "oak_sign", sign_text(), None);
        check("can_place_on", "stone", can_place_on(), None);
        check("attack_animation", "diamond_sword", vec![1, 6], None);
        check(
            "cooking_fuel",
            "coal",
            vec![1, 0, 0, 0, 200, 1, 63, 128, 0, 0],
            None,
        );

        // All together in one stack: nothing drops the packet.
        let all = [
            ("tool", tool(&server, 777)),
            ("sign_text_front", sign_text()),
            ("profile", profile(777)),
            ("can_place_on", can_place_on()),
            ("pot_decorations", pot_decorations(&server, 777)),
            ("cooking_fuel", vec![1, 0, 0, 0, 200, 1, 63, 128, 0, 0]),
            ("equippable", equippable(&server, 777)),
            ("consumable", consumable(&server, 777)),
            ("damage_resistant", damage_resistant(777)),
        ];
        let got = translate(&client, &server, &stack(&server, "stone", &all))
            .unwrap_or_else(|| panic!("{protocol}: the stack with every component was dropped"));
        let kept: Vec<(&str, Vec<u8>)> = [
            ("tool", Some(tool(&client, protocol))),
            ("profile", Some(profile(protocol))),
            ("pot_decorations", Some(pot_decorations(&client, protocol))),
            (
                "equippable",
                has("equippable").then(|| equippable(&client, protocol)),
            ),
            (
                "consumable",
                has("consumable").then(|| consumable(&client, protocol)),
            ),
            (
                "damage_resistant",
                has("damage_resistant").then(|| damage_resistant(protocol)),
            ),
        ]
        .into_iter()
        .filter_map(|(n, v)| Some((n, v?)))
        .collect();
        assert_eq!(got, expected(&client, "stone", &kept), "{protocol}: all");
    }
}

/// A nested stack of one arrow without components: a template (ID, count)
/// since 26.1, a stack (count, ID) before.
fn arrow(side: &Side, protocol: i32) -> Vec<u8> {
    let mut v = Vec::new();
    let id = side.id("item", "arrow");
    if protocol >= 775 {
        var_int(&mut v, id);
        var_int(&mut v, 1);
    } else {
        var_int(&mut v, 1);
        var_int(&mut v, id);
    }
    v.extend_from_slice(&[0, 0]);
    v
}

fn list(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut v = Vec::new();
    var_int(&mut v, entries.len() as i32);
    entries.iter().for_each(|e| v.extend_from_slice(e));
    v
}

/// Stacks inside components (crossbow projectiles, bundle and shulker box
/// contents, use remainder) come in the client's format and are never empty:
/// Pumpkin 0.2.0 writes a loaded crossbow's projectiles as air templates
/// (`0 0 0 0`), which a 26.1.2 client rejects ("Item must be non-empty") and
/// older ones too ("Empty ItemStack not allowed"). Those are left out; an
/// empty shulker box slot stays an empty slot.
#[test]
fn nested_stacks_are_never_empty() {
    let server = Side::new(777);
    let air = vec![0, 0, 0, 0];
    for protocol in 767..=776 {
        let client = Side::new(protocol);
        let check = |label: &str, item: &str, value: Vec<u8>, want: Option<Vec<u8>>| {
            let got = translate(&client, &server, &stack(&server, item, &[(label, value)]))
                .unwrap_or_else(|| panic!("{protocol}: {label} dropped the packet"));
            let want = expected(
                &client,
                item,
                &want.map(|w| vec![(label, w)]).unwrap_or_default(),
            );
            assert_eq!(got, want, "{protocol}: {label}");
        };
        let has = |name: &str| client.has("data_component_type", name);
        let (sent, got) = (arrow(&server, 777), arrow(&client, protocol));
        check(
            "charged_projectiles",
            "crossbow",
            list(&[air.clone(), sent.clone()]),
            Some(list(std::slice::from_ref(&got))),
        );
        check(
            "charged_projectiles",
            "crossbow",
            list(std::slice::from_ref(&air)),
            Some(list(&[])),
        );
        check(
            "bundle_contents",
            "bundle",
            list(&[sent.clone(), air.clone()]),
            Some(list(std::slice::from_ref(&got))),
        );
        check(
            "use_remainder",
            "bread",
            sent.clone(),
            has("use_remainder").then(|| got.clone()),
        );
        check("use_remainder", "bread", air.clone(), None);
        // Slots by index: present flags since 26.1, empty stacks (count 0) before.
        let slot = |present: &[u8]| [&[1u8][..], present].concat();
        let (empty_slot, full_slot) = if protocol >= 775 {
            (vec![0], slot(&got))
        } else {
            (vec![0], got.clone())
        };
        check(
            "container",
            "shulker_box",
            list(&[slot(&air), slot(&sent), vec![0]]),
            Some(list(&[empty_slot.clone(), full_slot, empty_slot])),
        );
    }
}

/// Stacks and block entities Pumpkin 0.2.0 sent a 26.3 client (`/give` with
/// components, spawners, chests, signs, pots; `translate_mv.rs` with
/// `PUMBO_MV_SLOTS`): every packet reaches every older client. Pumpkin writes `trim`, `profile` and `pot_decorations`
/// unlike vanilla 26.3; those stacks arrive without the malformed component.
#[test]
fn recorded_pumpkin_packets_reach_every_client() {
    let server = Side::new(777);
    let recorded = include_str!("data/pumpkin-0.2.0-play.txt");
    for protocol in 767..=776 {
        let client = Side::new(protocol);
        let tables = Arc::new(mv::Tables::new(client.data.clone(), server.data.clone()).unwrap());
        let mut t = mv::Translator::new(tables);
        let mut out = mv::Output::default();
        t.to_client(server.packet(0, "finish_configuration"), &[], &mut out);
        for line in recorded.lines() {
            let (name, hex) = line.split_once(' ').unwrap();
            let payload: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            out.clear();
            t.to_client(server.packet(1, name), &payload, &mut out);
            assert_eq!(out.to_client.len(), 1, "{protocol}: {line}");
        }
    }
}

/// Pumpkin 0.2.0's eyeblossom `trail` without options (Pumpkin issue #3065)
/// reaches older clients with the options the proxy gives 26.3 clients
/// (`fix_empty_trail`, D-COMPAT-1), not dropped.
#[test]
fn trail_without_options_reaches_older_clients_repaired() {
    let server = Side::new(777);
    for protocol in [776, 769] {
        let client = Side::new(protocol);
        let tables = Arc::new(mv::Tables::new(client.data.clone(), server.data.clone()).unwrap());
        let mut t = mv::Translator::new(tables);
        let mut out = mv::Output::default();
        t.to_client(server.packet(0, "finish_configuration"), &[], &mut out);
        out.clear();
        let position = [10.5f64, 64.5, -3.5]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect::<Vec<_>>();
        let mut p = Vec::new();
        var_int(&mut p, server.id("particle_type", "trail"));
        p.extend([0, 0]);
        p.extend(&position);
        p.extend([0; 24]);
        p.extend([1, 0]);
        t.to_client(server.packet(1, "level_particles"), &p, &mut out);
        let mut want = vec![0, 0];
        want.extend(&position);
        want.extend([0; 16]);
        want.extend(1i32.to_be_bytes());
        var_int(&mut want, client.id("particle_type", "trail"));
        for v in [10.5f64, 66.0, -3.5] {
            want.extend(v.to_be_bytes());
        }
        want.extend(0xFC7812i32.to_be_bytes());
        var_int(&mut want, 20);
        assert_eq!(
            out.to_client,
            [(client.packet(1, "level_particles"), want)],
            "{protocol}"
        );
    }
}
