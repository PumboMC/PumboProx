//! Remaps of the version translator on known samples (tables from `pumbo-data`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use pumbo_protocol::ProtocolVersion;
use pumbo_translate::multiversion::version_data;
use pumbo_translate_mv::Tables;

fn tables(client: i32) -> Tables {
    Tables::new(
        version_data(ProtocolVersion(client)).unwrap(),
        version_data(ProtocolVersion(777)).unwrap(),
    )
    .unwrap()
}

fn state(protocol: i32, name: &str, props: &[(&str, &str)]) -> i32 {
    let t = pumbo_data::tables(ProtocolVersion(protocol)).unwrap();
    t.block_state(name, props).unwrap() as i32
}

fn item(protocol: i32, name: &str) -> i32 {
    let t = pumbo_data::tables(ProtocolVersion(protocol)).unwrap();
    t.registry("item").unwrap().id(name).unwrap() as i32
}

fn entity(protocol: i32, name: &str) -> i32 {
    let t = pumbo_data::tables(ProtocolVersion(protocol)).unwrap();
    t.registry("entity_type").unwrap().id(name).unwrap() as i32
}

#[test]
fn known_blocks() {
    let t = tables(767);
    // Barrier: 14380 in 26.3, 10365 in 1.21.1 (plan §2.4).
    assert_eq!(state(777, "barrier", &[("waterlogged", "true")]), 14380);
    assert_eq!(t.block_state_for_client(14380), 10365);
    let stairs = [
        ("facing", "east"),
        ("half", "top"),
        ("shape", "outer_left"),
        ("waterlogged", "true"),
    ];
    assert_eq!(
        t.block_state_for_client(state(777, "oak_stairs", &stairs)),
        state(767, "oak_stairs", &stairs)
    );
    // New blocks get the stand-ins of ViaVersion Mappings with their properties.
    assert_eq!(
        t.block_state_for_client(state(777, "pale_oak_stairs", &stairs)),
        state(767, "birch_stairs", &stairs)
    );
    let chain = [("axis", "x"), ("waterlogged", "false")];
    assert_eq!(
        t.block_state_for_client(state(777, "iron_chain", &chain)),
        state(767, "chain", &chain)
    );
    let slab = [("type", "top"), ("waterlogged", "false")];
    assert_eq!(
        t.block_state_for_client(state(777, "red_wool_slab", &slab)),
        state(767, "brick_slab", &slab)
    );
    assert_eq!(
        t.block_state_for_client(state(777, "sulfur", &[])),
        state(767, "sandstone", &[])
    );
    // Since 1.21.2 pale oak is native.
    let t = tables(768);
    assert_eq!(
        t.block_state_for_client(state(777, "pale_oak_stairs", &stairs)),
        state(768, "pale_oak_stairs", &stairs)
    );
}

#[test]
fn stand_ins_compose_mappings_steps() {
    // 26.1.2: through 26.3 -> 26.2 -> 26.1, e.g. red_concrete_stairs -> cinnabar_brick_stairs -> brick_stairs.
    let t = tables(775);
    let stairs = [
        ("facing", "north"),
        ("half", "bottom"),
        ("shape", "inner_right"),
        ("waterlogged", "false"),
    ];
    let slab = [("type", "double"), ("waterlogged", "true")];
    for (server, client, props) in [
        ("lime_concrete_stairs", "prismarine_stairs", &stairs[..]),
        ("red_concrete_stairs", "brick_stairs", &stairs),
        ("cinnabar_stairs", "granite_stairs", &stairs),
        ("black_concrete_slab", "blackstone_slab", &slab),
        ("white_wool_slab", "quartz_slab", &slab),
        ("poplar_planks", "birch_planks", &[]),
    ] {
        assert_eq!(
            t.block_state_for_client(state(777, server, props)),
            state(775, client, props),
            "{server}"
        );
    }
    assert_eq!(
        t.item_for_client(item(777, "lime_concrete_stairs")),
        Some(item(775, "prismarine_stairs"))
    );
    // Whole states with their own stand-in (26.1 -> 1.21.11).
    let t = tables(774);
    let note = |instrument| {
        [
            ("instrument", instrument),
            ("note", "3"),
            ("powered", "true"),
        ]
    };
    assert_eq!(
        t.block_state_for_client(state(777, "note_block", &note("trumpet"))),
        state(774, "note_block", &note("didgeridoo"))
    );
}

#[test]
fn every_block_state_maps_and_shared_states_are_exact() {
    for client in 767..=776 {
        let t = tables(client);
        let ct = pumbo_data::tables(ProtocolVersion(client)).unwrap();
        let st = pumbo_data::tables(ProtocolVersion(777)).unwrap();
        let count = ct.block_state_count() as i32;
        for id in 0..st.block_state_count() as i32 {
            let mapped = t.block_state_for_client(id);
            let block = st.blocks.iter().rfind(|b| b.first_state <= id as u32);
            // Air (state 0) is also what a state without a stand-in gets; Mappings
            // itself drops only the hanging sides of pale moss carpets.
            assert!(
                (0..count).contains(&mapped)
                    && (mapped != 0
                        || id == 0
                        || block.is_some_and(|b| b.name == "minecraft:pale_moss_carpet")),
                "{client}: state {id} of {:?}",
                block.map(|b| &b.name)
            );
        }
        // Every client state is reached from the server state of the same block and values.
        for b in &ct.blocks {
            let Some(sb) = st.block(&b.name) else {
                continue;
            };
            if sb.properties != b.properties {
                continue;
            }
            for offset in 0..b.state_count() {
                let server = (sb.first_state + offset) as i32;
                assert_eq!(
                    t.block_state_for_client(server),
                    (b.first_state + offset) as i32,
                    "{client}: {}",
                    b.name
                );
            }
        }
    }
}

#[test]
fn known_items_and_entities() {
    let t = tables(767);
    // filled_map: 1238 in 26.3, 982 in 1.21.1 (plan §2.4).
    assert_eq!(item(777, "filled_map"), 1238);
    assert_eq!(t.item_for_client(1238), Some(982));
    assert_eq!(
        t.item_for_client(item(777, "iron_chain")),
        Some(item(767, "chain"))
    );
    assert_eq!(
        t.item_for_client(item(777, "copper_sword")),
        Some(item(767, "iron_sword"))
    );
    assert_eq!(
        t.item_for_client(item(777, "copper_helmet")),
        Some(item(767, "iron_helmet"))
    );
    assert_eq!(
        t.item_for_client(item(777, "pale_oak_planks")),
        Some(item(767, "birch_planks"))
    );
    assert_eq!(
        t.item_for_client(item(777, "netherite_spear")),
        Some(item(767, "netherite_sword"))
    );
    assert_eq!(
        t.entity_type_for_client(entity(777, "oak_boat")),
        Some(entity(767, "boat"))
    );
    assert_eq!(
        t.entity_type_for_client(entity(777, "creaking")),
        Some(entity(767, "warden"))
    );
    assert_eq!(
        t.entity_type_for_client(entity(777, "cushion")),
        Some(entity(767, "text_display"))
    );
    // Our override: ViaBackwards makes mannequins fake players.
    assert_eq!(
        t.entity_type_for_client(entity(777, "mannequin")),
        Some(entity(767, "armor_stand"))
    );
    let t = tables(768);
    assert_eq!(
        t.entity_type_for_client(entity(777, "creaking")),
        Some(entity(768, "creaking"))
    );
    // Every server item and entity type has a client one.
    let st = pumbo_data::tables(ProtocolVersion(777)).unwrap();
    let items = st.registry("item").unwrap().len() as i32;
    let entities = st.registry("entity_type").unwrap().len() as i32;
    for client in 767..=776 {
        let t = tables(client);
        assert!(
            (0..items).all(|i| t.item_for_client(i).is_some()),
            "{client}"
        );
        assert!(
            (0..entities).all(|i| t.entity_type_for_client(i).is_some()),
            "{client}"
        );
    }
}
