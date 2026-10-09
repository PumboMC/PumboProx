//! Item stacks and the packets that carry them. Components the client lacks
//! are dropped; components whose layout is not known here make the stack
//! unreadable, and a packet with an unreadable stack is dropped.

use crate::play::sound_holder;
use crate::version::{V1_21_2, V1_21_4, V1_21_5, V1_21_6, V1_21_9, V1_21_11, V26_1, V26_3, eras};
use crate::wire::{Get, Put, copy, copy_bool, copy_str, copy_var_int};
use crate::{Ctx, nbt};

eras! {
    /// Stacks inside components and slot displays.
    enum NestedFormat {
        /// `ItemStack`: count, ID, patch; empty container slots as count 0.
        Stack = V1_21,
        /// `ItemStackTemplate`: ID, count, patch; containers with a present flag.
        Template = V26_1,
    }
}

eras! {
    /// Components with a `show_in_tooltip` flag (replaced by `tooltip_display` in 1.21.5).
    enum TooltipFlags {
        Inline = V1_21,
        Display = V1_21_5,
    }
}

/// A holder set: a tag, or registry IDs.
enum Set<'a> {
    Tag(&'a str),
    Ids(Vec<i32>),
}

fn read_set<'a>(r: &mut &'a [u8]) -> Option<Set<'a>> {
    let n = r.get_var_int()?;
    if n == 0 {
        return Some(Set::Tag(r.get_str()?));
    }
    let mut ids = Vec::new();
    for _ in 1..n {
        ids.push(r.get_var_int()?);
    }
    Some(Set::Ids(ids))
}

/// Writes a set with its IDs mapped; IDs the client lacks are left out.
fn put_set(set: &Set<'_>, out: &mut Vec<u8>, map: &mut dyn FnMut(i32) -> Option<i32>) {
    match set {
        Set::Tag(tag) => {
            out.put_var_int(0);
            out.put_str(tag);
        }
        Set::Ids(ids) => {
            let mapped: Vec<i32> = ids.iter().filter_map(|id| map(*id)).collect();
            out.put_len(mapped.len() + 1);
            mapped.iter().for_each(|id| out.put_var_int(*id));
        }
    }
}

/// A holder set of a synced registry: IDs by entry name since 26.1; before,
/// only a tag (as an identifier) can be written. `false`: not representable.
fn synced_set(ctx: &mut Ctx<'_>, registry: &str, set: &Set<'_>, out: &mut Vec<u8>) -> bool {
    match set {
        _ if ctx.client() >= V26_1 => {
            put_set(set, out, &mut |id| dynamic_id(ctx, registry, id));
            true
        }
        Set::Tag(tag) => {
            out.put_str(tag);
            true
        }
        Set::Ids(_) => false,
    }
}

/// A synced registry entry as the client's (unchanged when the server's
/// entries are unknown).
fn dynamic_id(ctx: &mut Ctx<'_>, registry: &str, id: i32) -> Option<i32> {
    ctx.dynamic(registry).map_or(Some(id), |m| m.get(id))
}

fn opt(
    r: &mut &[u8],
    out: &mut Vec<u8>,
    f: impl FnOnce(&mut &[u8], &mut Vec<u8>) -> Option<()>,
) -> Option<()> {
    if copy_bool(r, out)? {
        f(r, out)
    } else {
        Some(())
    }
}

fn opt_sound(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    opt(r, out, |r, o| sound_holder(ctx, r, o))
}

fn effect_details(r: &mut &[u8], out: &mut Vec<u8>, depth: usize) -> Option<()> {
    if depth > 16 {
        return None;
    }
    copy_var_int(r, out)?;
    copy_var_int(r, out)?;
    copy(r, 3, out)?;
    if copy_bool(r, out)? {
        effect_details(r, out, depth + 1)?;
    }
    Some(())
}

fn copy_list<F>(r: &mut &[u8], out: &mut Vec<u8>, mut each: F) -> Option<()>
where
    F: FnMut(&mut &[u8], &mut Vec<u8>) -> Option<()>,
{
    let n = r.get_len()?;
    out.put_len(n);
    for _ in 0..n {
        each(r, out)?;
    }
    Some(())
}

/// Mob effects with their details; effects the client lacks are left out.
fn mob_effects(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let map = ctx.t.registry("mob_effect")?;
    let n = r.get_len()?;
    let mut body = Vec::new();
    let mut kept = 0;
    for _ in 0..n {
        let id = r.get_var_int()?;
        let mut details = Vec::new();
        effect_details(r, &mut details, 0)?;
        let Some(id) = map.get(id) else { continue };
        body.put_var_int(id);
        body.put_slice(&details);
        kept += 1;
    }
    out.put_len(kept);
    out.put_slice(&body);
    Some(())
}

/// Consume effects of `consumable` and `death_protection` (1.21.2+).
/// 26.3→26.2 (ViaBackwards `BlockItemPacketRewriter26_3`, ViaVersion
/// `ConsumeEffect.EFFECT_TYPES26_3`): `teleport_randomly` lost its
/// directional particles flag.
fn consume_effects(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let t = ctx.t;
    let types = t.registry("consume_effect_type")?;
    let n = r.get_len()?;
    let mut body = Vec::new();
    let mut kept = 0;
    for _ in 0..n {
        let kind = r.get_var_int()?;
        let name = t
            .server
            .registry("consume_effect_type")
            .get(usize::try_from(kind).ok()?)?;
        let mut v = Vec::new();
        match name.strip_prefix("minecraft:").unwrap_or(name) {
            "apply_effects" => {
                mob_effects(ctx, r, &mut v)?;
                copy(r, 4, &mut v)?;
            }
            "remove_effects" => {
                let effects = t.registry("mob_effect")?;
                put_set(&read_set(r)?, &mut v, &mut |id| effects.get(id));
            }
            "clear_all_effects" => {}
            "teleport_randomly" => {
                copy(r, 4, &mut v)?;
                let directional = r.get_bool()?;
                if ctx.client() >= V26_3 {
                    v.put_bool(directional);
                }
            }
            "play_sound" => sound_holder(ctx, r, &mut v)?,
            _ => return None,
        }
        let Some(kind) = types.get(kind) else {
            continue;
        };
        body.put_var_int(kind);
        body.put_slice(&v);
        kept += 1;
    }
    out.put_len(kept);
    out.put_slice(&body);
    Some(())
}

/// A registry holder of a synced registry: the entry, or 0 and the value
/// inline (`direct`, skipped: inline values are not translated).
/// `Some(None)`: inline or unknown to the client.
fn synced_holder(
    ctx: &mut Ctx<'_>,
    registry: &str,
    r: &mut &[u8],
    direct: impl FnOnce(&mut &[u8]) -> Option<()>,
) -> Option<Option<i32>> {
    let holder = r.get_var_int()?;
    if holder == 0 {
        direct(r)?;
        return Some(None);
    }
    Some(dynamic_id(ctx, registry, holder - 1))
}

fn skip_str(r: &mut &[u8]) -> Option<()> {
    r.get_str().map(|_| ())
}

fn skip_opt(r: &mut &[u8], f: impl FnOnce(&mut &[u8]) -> Option<()>) -> Option<()> {
    if r.get_bool()? { f(r) } else { Some(()) }
}

/// A sound holder: a registry entry, or 0, a name and an optional range.
fn skip_sound(r: &mut &[u8]) -> Option<()> {
    if r.get_var_int()? == 0 {
        skip_str(r)?;
        skip_opt(r, |r| r.bytes(4).map(|_| ()))?;
    }
    Some(())
}

/// `ResolvableInt` and `ResolvableFloat` (26.3): a number or a string.
fn skip_resolvable(r: &mut &[u8], width: usize) -> Option<()> {
    if r.get_bool()? {
        r.bytes(width).map(|_| ())
    } else {
        skip_str(r)
    }
}

/// `can_place_on` / `can_break` (1.21.11+ layout): read to the end; the
/// predicates hold block and component IDs and are not translated.
fn skip_adventure_predicate(ctx: &mut Ctx<'_>, r: &mut &[u8]) -> Option<()> {
    for _ in 0..r.get_len()? {
        skip_opt(r, |r| read_set(r).map(|_| ()))?;
        skip_opt(r, |r| {
            for _ in 0..r.get_len()? {
                skip_str(r)?;
                if r.get_bool()? {
                    skip_str(r)?;
                } else {
                    skip_opt(r, skip_str)?;
                    skip_opt(r, skip_str)?;
                }
            }
            Some(())
        })?;
        skip_opt(r, |r| nbt::split(r).map(|_| ()))?;
        // Exact components, then partial predicates.
        for _ in 0..r.get_len()? {
            let kind = r.get_var_int()?;
            let name = component_name(ctx.t, kind)?;
            component(ctx, &name, r, &mut Vec::new())?;
        }
        for _ in 0..r.get_len()? {
            r.get_bool()?;
            r.get_var_int()?;
            nbt::split(r)?;
        }
    }
    Some(())
}

fn component_name(t: &crate::Tables, kind: i32) -> Option<String> {
    let name = t
        .server
        .registry("data_component_type")
        .get(usize::try_from(kind).ok()?)?;
    Some(name.strip_prefix("minecraft:").unwrap_or(name).to_string())
}

/// A `ResolvableProfile` (1.21.9+); older clients get a `GameProfile`
/// (ViaBackwards `BlockItemPacketRewriter1_21_9`): name, ID and properties,
/// without the skin patch.
fn profile(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let start = *r;
    let mut old = Vec::new();
    if r.get_bool()? {
        let id = r.bytes(16)?;
        let name = r.get_str()?;
        old.put_bool(true);
        old.put_str(name);
        old.put_bool(true);
        old.put_slice(id);
    } else {
        opt(r, &mut old, copy_str)?;
        opt(r, &mut old, |r, o| copy(r, 16, o))?;
    }
    copy_list(r, &mut old, |r, o| {
        copy_str(r, o)?;
        copy_str(r, o)?;
        opt(r, o, copy_str)
    })?;
    for _ in 0..3 {
        skip_opt(r, skip_str)?;
    }
    skip_opt(r, |r| r.get_bool().map(|_| ()))?;
    if ctx.client() >= V1_21_9 {
        out.put_slice(start.get(..start.len() - r.len())?);
    } else {
        out.put_slice(&old);
    }
    Some(())
}

/// One server component value. `Some(true)`: written for the client;
/// `Some(false)`: read, but the client cannot have it (left out of the
/// stack). `None` only for a malformed value: every 26.3 component is read
/// (layouts from ViaVersion `StructuredDataKey` and `Protocol26_2To26_3`).
fn component(ctx: &mut Ctx<'_>, name: &str, r: &mut &[u8], out: &mut Vec<u8>) -> Option<bool> {
    let client = ctx.client();
    let flag = TooltipFlags::of(client) == TooltipFlags::Inline;
    match name {
        "custom_name" | "item_name" => nbt::text(client, r, out)?,
        "custom_data"
        | "debug_stick_state"
        | "recipes"
        | "container_loot"
        | "map_decorations"
        | "intangible_projectile"
        | "bucket_entity_data"
        | "lock" => {
            nbt::copy(r, out)?;
        }
        "max_stack_size"
        | "max_damage"
        | "damage"
        | "repair_cost"
        | "rarity"
        | "map_id"
        | "map_post_processing"
        | "ominous_bottle_amplifier"
        | "base_color"
        | "enchantable"
        | "dye"
        | "additional_trade_cost"
        | "wolf/collar"
        | "fox/variant"
        | "salmon/size"
        | "parrot/variant"
        | "tropical_fish/pattern"
        | "tropical_fish/base_color"
        | "tropical_fish/pattern_color"
        | "mooshroom/variant"
        | "rabbit/variant"
        | "horse/variant"
        | "llama/variant"
        | "axolotl/variant"
        | "cat/collar"
        | "sheep/color"
        | "shulker/color"
        // New in 26.3; the client never has them.
        | "block_transformer"
        | "villager_food"
        | "provides_pottery_pattern"
        | "cushion/color" => {
            copy_var_int(r, out)?;
        }
        "unbreakable" => {
            if flag {
                out.put_bool(true);
            }
        }
        "glider" | "creative_slot_lock" | "waxed" => {}
        "enchantment_glint_override" => {
            copy(r, 1, out)?;
        }
        "potion_duration_scale" | "minimum_attack_charge" => copy(r, 4, out)?,
        "use_effects" => copy(r, 6, out)?,
        "attack_range" => copy(r, 24, out)?,
        "item_model" | "tooltip_style" | "note_block_sound" => copy_str(r, out)?,
        "lore" => copy_list(r, out, |r, o| nbt::text(client, r, o))?,
        "tooltip_display" => {
            // 1.21.5+ (older clients get `hide_tooltip` from `patch`).
            let types = ctx.t.registry("data_component_type")?;
            copy(r, 1, out)?;
            let n = r.get_len()?;
            let mut hidden = Vec::new();
            for _ in 0..n {
                if let Some(id) = types.get(r.get_var_int()?) {
                    hidden.push(id);
                }
            }
            out.put_len(hidden.len());
            hidden.iter().for_each(|id| out.put_var_int(*id));
        }
        "enchantments" | "stored_enchantments" => {
            let n = r.get_len()?;
            let mut body = Vec::new();
            let mut kept = 0;
            for _ in 0..n {
                let id = r.get_var_int()?;
                let level = r.get_var_int()?;
                let Some(id) = dynamic_id(ctx, "enchantment", id) else {
                    continue;
                };
                body.put_var_int(id);
                body.put_var_int(level);
                kept += 1;
            }
            out.put_len(kept);
            out.put_slice(&body);
            if flag {
                out.put_bool(true);
            }
        }
        "dyed_color" => {
            copy(r, 4, out)?;
            if flag {
                out.put_bool(true);
            }
        }
        "custom_model_data" => {
            let floats = r.get_len()?;
            let mut first = None;
            let mut body = Vec::new();
            body.put_len(floats);
            for _ in 0..floats {
                let f = r.get_f32()?;
                first.get_or_insert(f);
                body.put_f32(f);
            }
            copy_list(r, &mut body, |r, o| copy(r, 1, o))?;
            copy_list(r, &mut body, copy_str)?;
            copy_list(r, &mut body, |r, o| copy(r, 4, o))?;
            if client >= V1_21_4 {
                out.put_slice(&body);
            } else {
                let Some(first) = first else {
                    return Some(false);
                };
                out.put_var_int(first as i32);
            }
        }
        "potion_contents" => {
            if r.get_bool()? {
                let id = r.get_var_int()?;
                match ctx.t.registry("potion")?.get(id) {
                    Some(id) => {
                        out.put_bool(true);
                        out.put_var_int(id);
                    }
                    None => out.put_bool(false),
                }
            } else {
                out.put_bool(false);
            }
            if copy_bool(r, out)? {
                copy(r, 4, out)?;
            }
            mob_effects(ctx, r, out)?;
            let has_name = r.get_bool()?;
            let custom = if has_name { Some(r.get_str()?) } else { None };
            if client >= V1_21_2 {
                out.put_bool(has_name);
                if let Some(s) = custom {
                    out.put_str(s);
                }
            }
        }
        "suspicious_stew_effects" => {
            let n = r.get_len()?;
            let mut body = Vec::new();
            let mut kept = 0;
            for _ in 0..n {
                let (id, duration) = (r.get_var_int()?, r.get_var_int()?);
                let Some(id) = ctx.t.registry("mob_effect")?.get(id) else {
                    continue;
                };
                body.put_var_int(id);
                body.put_var_int(duration);
                kept += 1;
            }
            out.put_len(kept);
            out.put_slice(&body);
        }
        "pot_decorations" => {
            // 26.3→26.2 (ViaBackwards `BlockItemPacketRewriter26_3`): four optional
            // item templates (back, left, right, front) became a list of items.
            let start = *r;
            let mut sides = [None; 4];
            for side in &mut sides {
                if r.get_bool()? {
                    *side = Some(r.get_var_int()?);
                    r.get_var_int()?;
                    patch(ctx, r, &mut Vec::new(), false)?;
                }
            }
            if client >= V26_3 {
                out.put_slice(start.get(..start.len() - r.len())?);
            } else {
                let t = ctx.t;
                let brick = t.server.registry("item").iter().position(|n| n == "brick");
                let brick = t.items.or(i32::try_from(brick?).ok()?, 0);
                out.put_len(sides.len());
                for side in sides {
                    out.put_var_int(side.map_or(brick, |id| t.items.or(id, brick)));
                }
            }
        }
        "block_state" => copy_list(r, out, |r, o| {
            copy_str(r, o)?;
            copy_str(r, o)
        })?,
        "entity_data" | "block_entity_data" => {
            let kind = r.get_var_int()?;
            let data = nbt::split(r)?;
            if client < V1_21_9 {
                return Some(false);
            }
            let registry = if name == "entity_data" {
                &ctx.t.entity_types
            } else {
                ctx.t.registry("block_entity_type")?
            };
            let Some(kind) = registry.get(kind) else {
                return Some(false);
            };
            out.put_var_int(kind);
            out.put_slice(data);
        }
        "food" => {
            copy_var_int(r, out)?;
            copy(r, 5, out)?;
            if client < V1_21_2 {
                out.put_f32(1.6);
                out.put_var_int(0);
                out.put_var_int(0);
            }
        }
        "consumable" => {
            // 1.21.2+. The `spear` animation (11) is new in 1.21.11; before, 5 is the spear/trident one.
            copy(r, 4, out)?;
            let animation = r.get_var_int()?;
            out.put_var_int(if animation == 11 && client < V1_21_11 {
                5
            } else {
                animation
            });
            sound_holder(ctx, r, out)?;
            copy(r, 1, out)?;
            consume_effects(ctx, r, out)?;
        }
        "death_protection" => consume_effects(ctx, r, out)?,
        "use_cooldown" => {
            copy(r, 4, out)?;
            opt(r, out, copy_str)?;
        }
        "damage_resistant" => {
            // 26.1→1.21.11 (ViaBackwards `BlockItemPacketRewriter26_1`): a damage type tag before 26.1.
            let set = read_set(r)?;
            if !synced_set(ctx, "damage_type", &set, out) {
                return Some(false);
            }
        }
        "damage_type" => {
            // 26.1→1.21.11: `Either<id, name>` before 26.1.
            let Some(id) = dynamic_id(ctx, "damage_type", r.get_var_int()?) else {
                return Some(false);
            };
            if client < V26_1 {
                out.put_bool(true);
            }
            out.put_var_int(id);
        }
        "tool" => {
            // 1.21.5 added `can_destroy_blocks_in_creative` (ViaVersion `ToolProperties.TYPE1_21_5`).
            let blocks = &ctx.t.blocks;
            copy_list(r, out, |r, o| {
                put_set(&read_set(r)?, o, &mut |id| blocks.get(id));
                opt(r, o, |r, o| copy(r, 4, o))?;
                opt(r, o, |r, o| copy(r, 1, o))
            })?;
            copy(r, 4, out)?;
            copy_var_int(r, out)?;
            let creative = r.get_bool()?;
            if client >= V1_21_5 {
                out.put_bool(creative);
            }
        }
        "weapon" => {
            copy_var_int(r, out)?;
            copy(r, 4, out)?;
        }
        "equippable" => {
            // ViaVersion `Equippable`: `equip_on_interact` since 1.21.5, shearing since 1.21.6.
            let slot = copy_var_int(r, out)?;
            sound_holder(ctx, r, out)?;
            opt(r, out, copy_str)?;
            opt(r, out, copy_str)?;
            let types = &ctx.t.entity_types;
            opt(r, out, |r, o| {
                put_set(&read_set(r)?, o, &mut |id| types.get(id));
                Some(())
            })?;
            copy(r, 3, out)?;
            let interact = r.get_bool()?;
            let shearable = r.get_bool()?;
            let mut shear_sound = Vec::new();
            sound_holder(ctx, r, &mut shear_sound)?;
            if client >= V1_21_5 {
                out.put_bool(interact);
            }
            if client >= V1_21_6 {
                out.put_bool(shearable);
                out.put_slice(&shear_sound);
            }
            // The saddle slot is new in 1.21.5.
            if slot > 6 && client < V1_21_5 {
                return Some(false);
            }
        }
        "repairable" => {
            let items = &ctx.t.items;
            put_set(&read_set(r)?, out, &mut |id| items.get(id));
        }
        "blocks_attacks" => {
            // 26.1→1.21.11 (ViaVersion `BlocksAttacks.TYPE1_21_5`): `bypassed_by` was a tag.
            copy(r, 8, out)?;
            let n = r.get_len()?;
            out.put_len(n);
            for _ in 0..n {
                copy(r, 4, out)?;
                if r.get_bool()? {
                    let set = read_set(r)?;
                    out.put_bool(true);
                    put_set(&set, out, &mut |id| dynamic_id(ctx, "damage_type", id));
                } else {
                    out.put_bool(false);
                }
                copy(r, 8, out)?;
            }
            copy(r, 12, out)?;
            if r.get_bool()? {
                let set = read_set(r)?;
                let mut v = Vec::new();
                if synced_set(ctx, "damage_type", &set, &mut v) {
                    out.put_bool(true);
                    out.put_slice(&v);
                } else {
                    out.put_bool(false);
                }
            } else {
                out.put_bool(false);
            }
            opt_sound(ctx, r, out)?;
            opt_sound(ctx, r, out)?;
        }
        "piercing_weapon" => {
            copy(r, 2, out)?;
            opt_sound(ctx, r, out)?;
            opt_sound(ctx, r, out)?;
        }
        "kinetic_weapon" => {
            copy_var_int(r, out)?;
            copy_var_int(r, out)?;
            for _ in 0..3 {
                opt(r, out, |r, o| {
                    copy_var_int(r, o)?;
                    copy(r, 8, o)
                })?;
            }
            copy(r, 8, out)?;
            opt_sound(ctx, r, out)?;
            opt_sound(ctx, r, out)?;
        }
        // 26.3 split `swing_animation`; older clients keep their default.
        "attack_animation" | "interact_animation" => {
            copy_var_int(r, out)?;
            copy_var_int(r, out)?;
            return Some(client >= V26_3);
        }
        "compostable" => skip_resolvable(r, 4)?,
        "cooking_fuel" | "brewing_fuel" => {
            skip_resolvable(r, 4)?;
            skip_resolvable(r, 4)?;
        }
        "mob_visibility" => {
            read_set(r)?;
            r.bytes(4)?;
        }
        "sign_text_front" | "sign_text_back" => {
            for _ in 0..4 {
                nbt::split(r)?;
            }
            skip_opt(r, |r| (0..4).try_for_each(|_| nbt::split(r).map(|_| ())))?;
            r.get_var_int()?;
            r.get_bool()?;
        }
        "can_place_on" | "can_break" => {
            skip_adventure_predicate(ctx, r)?;
            return Some(false);
        }
        "charged_projectiles" | "bundle_contents" => {
            // Lists of non-empty stacks: empty ones are left out.
            let n = r.get_len()?;
            let mut body = Vec::new();
            let mut kept = 0;
            for _ in 0..n {
                if nested(ctx, r, &mut body)? {
                    kept += 1;
                }
            }
            out.put_len(kept);
            out.put_slice(&body);
        }
        "use_remainder" | "sulfur_cube_content" => {
            if !nested(ctx, r, out)? {
                return Some(false);
            }
        }
        "container" => {
            // Slots by index: an empty one stays as an empty slot.
            let n = r.get_len()?;
            out.put_len(n);
            for _ in 0..n {
                let mut v = Vec::new();
                let present = r.get_bool()? && nested(ctx, r, &mut v)?;
                match NestedFormat::of(client) {
                    NestedFormat::Template => {
                        out.put_bool(present);
                        out.put_slice(&v);
                    }
                    NestedFormat::Stack if present => out.put_slice(&v),
                    NestedFormat::Stack => out.put_var_int(0),
                }
            }
        }
        "banner_patterns" => {
            let n = r.get_len()?;
            out.put_len(n);
            for _ in 0..n {
                let holder = r.get_var_int()?;
                if holder == 0 {
                    out.put_var_int(0);
                    copy_str(r, out)?;
                    copy_str(r, out)?;
                } else {
                    let id = holder - 1;
                    let id = ctx.dynamic("banner_pattern").map_or(id, |m| m.or(id, 0));
                    out.put_var_int(id + 1);
                }
                copy_var_int(r, out)?;
            }
        }
        "trim" => {
            // Inline materials and patterns are left out (26.3 changed the material's).
            let material = synced_holder(ctx, "trim_material", r, |r| {
                skip_str(r)?;
                nbt::split(r).map(|_| ())
            })?;
            let pattern = synced_holder(ctx, "trim_pattern", r, |r| {
                skip_str(r)?;
                nbt::split(r)?;
                r.get_bool().map(|_| ())
            })?;
            let (Some(material), Some(pattern)) = (material, pattern) else {
                return Some(false);
            };
            out.put_var_int(material + 1);
            out.put_var_int(pattern + 1);
            if flag {
                out.put_bool(true);
            }
        }
        "instrument" => {
            // 26.3 added the inline instrument's durability damage; 1.21.5-1.21.11 wrap
            // the holder in an `Either` (ViaVersion `Instrument1_21_2`).
            let Some(id) = synced_holder(ctx, "instrument", r, |r| {
                skip_sound(r)?;
                r.bytes(8)?;
                r.get_var_int()?;
                nbt::split(r).map(|_| ())
            })?
            else {
                return Some(false);
            };
            if (V1_21_5..V26_1).contains(&client) {
                out.put_bool(true);
            }
            out.put_var_int(id + 1);
        }
        "provides_trim_material" => {
            let Some(id) = synced_holder(ctx, "trim_material", r, |r| {
                skip_str(r)?;
                nbt::split(r).map(|_| ())
            })?
            else {
                return Some(false);
            };
            if client < V26_1 {
                out.put_bool(true);
            }
            out.put_var_int(id + 1);
        }
        "jukebox_playable" => {
            // 26.1→1.21.11: an `Either` holder, with a tooltip flag before 1.21.5.
            let Some(id) = synced_holder(ctx, "jukebox_song", r, |r| {
                skip_sound(r)?;
                nbt::split(r)?;
                r.bytes(4)?;
                r.get_var_int().map(|_| ())
            })?
            else {
                return Some(false);
            };
            if client < V26_1 {
                out.put_bool(true);
            }
            out.put_var_int(id + 1);
            if flag {
                out.put_bool(true);
            }
        }
        "provides_banner_patterns" => {
            // 26.1→1.21.11: a tag before 26.1.
            let set = read_set(r)?;
            if !synced_set(ctx, "banner_pattern", &set, out) {
                return Some(false);
            }
        }
        "writable_book_content" => copy_list(r, out, |r, o| {
            copy_str(r, o)?;
            opt(r, o, copy_str)
        })?,
        "written_book_content" => {
            copy_str(r, out)?;
            opt(r, out, copy_str)?;
            copy_str(r, out)?;
            copy_var_int(r, out)?;
            copy_list(r, out, |r, o| {
                nbt::text(client, r, o)?;
                opt(r, o, |r, o| nbt::text(client, r, o))
            })?;
            copy(r, 1, out)?;
        }
        "firework_explosion" => firework_explosion(r, out)?,
        "fireworks" => {
            copy_var_int(r, out)?;
            copy_list(r, out, firework_explosion)?;
        }
        "lodestone_tracker" => {
            opt(r, out, |r, o| {
                copy_str(r, o)?;
                copy(r, 8, o)
            })?;
            copy(r, 1, out)?;
        }
        "profile" => profile(ctx, r, out)?,
        "bees" => {
            // 1.21.9 added the entity type (ViaVersion `Bee.TYPE1_21_9`).
            let n = r.get_len()?;
            out.put_len(n);
            for _ in 0..n {
                let kind = r.get_var_int()?;
                if client >= V1_21_9 {
                    out.put_var_int(ctx.t.entity_types.or(kind, 0));
                }
                nbt::copy(r, out)?;
                copy_var_int(r, out)?;
                copy_var_int(r, out)?;
            }
        }
        "attribute_modifiers" => {
            let attributes = ctx.t.registry("attribute")?;
            let n = r.get_len()?;
            let mut body = Vec::new();
            let mut kept = 0;
            for _ in 0..n {
                let id = r.get_var_int()?;
                let mut m = Vec::new();
                copy_str(r, &mut m)?;
                copy(r, 8, &mut m)?;
                copy_var_int(r, &mut m)?;
                copy_var_int(r, &mut m)?;
                let display = r.get_var_int()?;
                let text = if display == 2 {
                    Some(nbt::split(r)?)
                } else {
                    None
                };
                if client >= V1_21_6 {
                    m.put_var_int(display);
                    if let Some(text) = text {
                        m.put_slice(text);
                    }
                }
                let Some(id) = attributes.get(id) else {
                    continue;
                };
                body.put_var_int(id);
                body.put_slice(&m);
                kept += 1;
            }
            out.put_len(kept);
            out.put_slice(&body);
            if flag {
                out.put_bool(true);
            }
        }
        "break_sound" => sound_holder(ctx, r, out)?,
        "villager/variant" => {
            let id = r.get_var_int()?;
            out.put_var_int(ctx.t.registry("villager_type")?.or(id, 0));
        }
        "painting/variant" => {
            let Some(id) = synced_holder(ctx, "painting_variant", r, |r| {
                r.get_var_int()?;
                r.get_var_int()?;
                skip_str(r)?;
                skip_opt(r, |r| nbt::split(r).map(|_| ()))?;
                skip_opt(r, |r| nbt::split(r).map(|_| ()))
            })?
            else {
                return Some(false);
            };
            out.put_var_int(id + 1);
        }
        "chicken/variant" | "zombie_nautilus/variant" => {
            // 26.1→1.21.11: `Either<id, name>` before 26.1.
            let registry = name.replace('/', "_");
            let Some(id) = dynamic_id(ctx, &registry, r.get_var_int()?) else {
                return Some(false);
            };
            if client < V26_1 {
                out.put_bool(true);
            }
            out.put_var_int(id);
        }
        // Entity variants of synced registries (1.21.5+).
        n if n.contains('/') => {
            let registry = n.replace('/', "_");
            let Some(id) = dynamic_id(ctx, &registry, r.get_var_int()?) else {
                return Some(false);
            };
            out.put_var_int(id);
        }
        _ => return None,
    }
    Some(true)
}

fn firework_explosion(r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    copy_var_int(r, out)?;
    copy_list(r, out, |r, o| copy(r, 4, o))?;
    copy_list(r, out, |r, o| copy(r, 4, o))?;
    copy(r, 2, out)
}

/// A component patch: added components the client has, then the removed ones.
/// `tail`: the patch ends the packet, so a component that does not read (a
/// malformed value: Pumpkin 0.2.0 writes `trim`, `profile` and
/// `pot_decorations` unlike vanilla 26.3) ends the patch there instead of
/// losing the packet; it and the rest of the input are left out.
fn patch(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>, tail: bool) -> Option<()> {
    let t = ctx.t;
    let types = t.registry("data_component_type")?;
    let server_types = t.server.registry("data_component_type");
    let added = r.get_len()?;
    let removed = r.get_len()?;
    let mut body = Vec::new();
    let mut kept = 0usize;
    let mut extra_hide = false;
    for _ in 0..added {
        let kind = r.get_var_int()?;
        let name = server_types.get(usize::try_from(kind).ok()?)?;
        let name = name.strip_prefix("minecraft:").unwrap_or(name);
        if name == "tooltip_display" && TooltipFlags::of(ctx.client()) == TooltipFlags::Inline {
            // Before 1.21.5 a hidden tooltip is its own component.
            extra_hide = r.get_bool()?;
            let n = r.get_len()?;
            for _ in 0..n {
                r.get_var_int()?;
            }
            continue;
        }
        let mut value = Vec::new();
        let Some(written) = component(ctx, name, r, &mut value) else {
            if !tail {
                return None;
            }
            *r = &[];
            break;
        };
        let (true, Some(client_kind)) = (written, types.get(kind)) else {
            continue;
        };
        body.put_var_int(client_kind);
        body.put_slice(&value);
        kept += 1;
    }
    if extra_hide
        && let Some(id) = t
            .client
            .registry("data_component_type")
            .iter()
            .position(|n| n == "hide_tooltip")
    {
        body.put_var_int(i32::try_from(id).ok()?);
        kept += 1;
    }
    let mut gone = Vec::new();
    for _ in 0..removed {
        if r.is_empty() && tail {
            break;
        }
        if let Some(id) = types.get(r.get_var_int()?) {
            gone.push(id);
        }
    }
    out.put_len(kept);
    out.put_len(gone.len());
    out.put_slice(&body);
    gone.iter().for_each(|id| out.put_var_int(*id));
    Some(())
}

/// A server item stack (`ItemStack`: count, ID, patch) for the client.
pub(crate) fn stack(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    stack_in(ctx, r, out, false)
}

/// A stack that ends the packet: see `patch`.
fn tail_stack(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    stack_in(ctx, r, out, true)
}

fn stack_in(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>, tail: bool) -> Option<()> {
    let count = r.get_var_int()?;
    if count <= 0 {
        out.put_var_int(0);
        return Some(());
    }
    let id = r.get_var_int()?;
    out.put_var_int(count);
    out.put_var_int(ctx.t.items.or(id, 0));
    patch(ctx, r, out, tail)
}

/// A non-empty stack inside a component or slot display: a template
/// (ID, count, patch) since 26.1, a full stack before. `Some(false)`: the
/// stack is empty (air or count 0) or its item has no client equivalent, and
/// nothing is written. 26.1+ clients reject an empty template ("Item must be
/// non-empty"), older ones an empty stack in these lists ("Empty ItemStack
/// not allowed"). Pumpkin 0.2.0 writes the projectiles of a loaded crossbow,
/// `use_remainder` and `sulfur_cube_content` as `0 0 0 0` (an air template).
fn nested(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<bool> {
    let id = r.get_var_int()?;
    let count = r.get_var_int()?;
    let mut components = Vec::new();
    patch(ctx, r, &mut components, false)?;
    let Some(id) = ctx.t.items.get(id).filter(|id| *id != 0 && count > 0) else {
        return Some(false);
    };
    if NestedFormat::of(ctx.client()) == NestedFormat::Template {
        out.put_var_int(id);
        out.put_var_int(count);
    } else {
        out.put_var_int(count);
        out.put_var_int(id);
    }
    out.put_slice(&components);
    Some(true)
}

// ---------------------------------------------------------------- packets

/// A container ID: a byte before 1.21.2, a VarInt since.
pub(crate) fn window_id(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let id = r.get_var_int()?;
    if ctx.client() >= V1_21_2 {
        out.put_var_int(id);
    } else {
        out.put_u8(id as u8);
    }
    Some(())
}

/// Packets that start with a container ID and need nothing else changed.
pub(crate) fn window_first(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_2 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(r.len());
    window_id(ctx, &mut r, &mut out)?;
    out.put_slice(r);
    Some(out)
}

/// `container_set_content`: a slot that does not read ends the input there
/// (see `patch`); the slots after it and the carried stack go out empty.
pub(crate) fn container_set_content(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 16);
    window_id(ctx, &mut r, &mut out)?;
    copy_var_int(&mut r, &mut out)?;
    let n = r.get_len()?;
    out.put_len(n);
    for _ in 0..=n {
        if r.is_empty() {
            out.put_var_int(0);
        } else {
            tail_stack(ctx, &mut r, &mut out)?;
        }
    }
    Some(out)
}

pub(crate) fn container_set_slot(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 8);
    window_id(ctx, &mut r, &mut out)?;
    copy_var_int(&mut r, &mut out)?;
    copy(&mut r, 2, &mut out)?;
    tail_stack(ctx, &mut r, &mut out)?;
    Some(out)
}

/// Before 1.21.2 the cursor and inventory slots are set with
/// `container_set_slot` on the pseudo containers -1 (cursor) and -2 (by inventory slot).
fn legacy_set_slot(ctx: &mut Ctx<'_>, window: u8, slot: i16, r: &mut &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![window];
    out.put_var_int(0);
    out.put_i16(slot);
    tail_stack(ctx, r, &mut out)?;
    ctx.send_client("container_set_slot", out);
    None
}

/// `set_cursor_item`: one stack.
pub(crate) fn single_stack(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() < V1_21_2 {
        return legacy_set_slot(ctx, 0xFF, -1, &mut r);
    }
    let mut out = Vec::with_capacity(r.len() + 8);
    tail_stack(ctx, &mut r, &mut out)?;
    Some(out)
}

pub(crate) fn set_player_inventory(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let slot = r.get_var_int()?;
    if ctx.client() < V1_21_2 {
        return legacy_set_slot(ctx, 0xFE, i16::try_from(slot).ok()?, &mut r);
    }
    let mut out = Vec::with_capacity(r.len() + 8);
    out.put_var_int(slot);
    tail_stack(ctx, &mut r, &mut out)?;
    Some(out)
}

/// Equipment slot added in 1.21.5.
const SADDLE: u8 = 7;

pub(crate) fn set_equipment(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 8);
    copy_var_int(&mut r, &mut out)?;
    let mut entries = Vec::new();
    loop {
        let slot = r.get_u8()?;
        let mut value = Vec::new();
        tail_stack(ctx, &mut r, &mut value)?;
        if !(slot & 0x7F == SADDLE && ctx.client() < V1_21_5) {
            entries.push((slot & 0x7F, value));
        }
        // An entry that ended the input (see `patch`) is the last one.
        if slot & 0x80 == 0 || r.is_empty() {
            break;
        }
    }
    let last = entries.len().checked_sub(1)?;
    for (i, (slot, value)) in entries.into_iter().enumerate() {
        out.put_u8(if i < last { slot | 0x80 } else { slot });
        out.put_slice(&value);
    }
    Some(out)
}

pub(crate) fn update_advancements(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 64);
    copy(&mut r, 1, &mut out)?;
    let n = r.get_len()?;
    out.put_len(n);
    for _ in 0..n {
        copy_str(&mut r, &mut out)?;
        if copy_bool(&mut r, &mut out)? {
            copy_str(&mut r, &mut out)?;
        }
        if copy_bool(&mut r, &mut out)? {
            nbt::copy(&mut r, &mut out)?;
            nbt::copy(&mut r, &mut out)?;
            stack(ctx, &mut r, &mut out)?;
            copy_var_int(&mut r, &mut out)?;
            let flags = r.get_i32()?;
            out.put_i32(flags);
            if flags & 1 != 0 {
                copy_str(&mut r, &mut out)?;
            }
            copy(&mut r, 8, &mut out)?;
        }
        copy_list(&mut r, &mut out, |r, o| copy_list(r, o, copy_str))?;
        copy(&mut r, 1, &mut out)?;
    }
    copy_list(&mut r, &mut out, copy_str)?;
    copy_list(&mut r, &mut out, |r, o| {
        copy_str(r, o)?;
        copy_list(r, o, |r, o| {
            copy_str(r, o)?;
            if copy_bool(r, o)? {
                copy(r, 8, o)?;
            }
            Some(())
        })
    })?;
    let show = r.get_bool()?;
    if ctx.client() >= V1_21_5 {
        out.put_bool(show);
    }
    Some(out)
}

/// An `ItemCost` of a trade: item, count and exact components.
fn item_cost(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    out.put_var_int(ctx.t.items.or(r.get_var_int()?, 0));
    copy_var_int(r, out)?;
    let t = ctx.t;
    let types = t.registry("data_component_type")?;
    let n = r.get_len()?;
    let mut body = Vec::new();
    let mut kept = 0;
    for _ in 0..n {
        let kind = r.get_var_int()?;
        let name = t
            .server
            .registry("data_component_type")
            .get(usize::try_from(kind).ok()?)?;
        let mut value = Vec::new();
        let written = component(
            ctx,
            name.strip_prefix("minecraft:").unwrap_or(name),
            r,
            &mut value,
        )?;
        let (true, Some(kind)) = (written, types.get(kind)) else {
            continue;
        };
        body.put_var_int(kind);
        body.put_slice(&value);
        kept += 1;
    }
    out.put_len(kept);
    out.put_slice(&body);
    Some(())
}

pub(crate) fn merchant_offers(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 32);
    window_id(ctx, &mut r, &mut out)?;
    let n = r.get_len()?;
    out.put_len(n);
    for _ in 0..n {
        item_cost(ctx, &mut r, &mut out)?;
        stack(ctx, &mut r, &mut out)?;
        if copy_bool(&mut r, &mut out)? {
            item_cost(ctx, &mut r, &mut out)?;
        }
        copy(&mut r, 1 + 4 * 4 + 4 + 4, &mut out)?;
    }
    out.put_slice(r);
    Some(out)
}

/// A slot display (1.21.2+; ViaVersion `RecipeDisplayRewriter*`). Types the
/// client lacks become `empty`; 26.3 made `tag` a holder set of items (IDs
/// become a `composite` of items before), 1.21.5 made the trim pattern of
/// `smithing_trim` a pattern holder (an `empty` display before).
fn slot_display(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>, depth: usize) -> Option<()> {
    if depth > 16 {
        return None;
    }
    let t = ctx.t;
    let client = ctx.client();
    let kind = r.get_var_int()?;
    let name = t
        .server
        .registry("slot_display")
        .get(usize::try_from(kind).ok()?)?;
    let mut name = name.strip_prefix("minecraft:").unwrap_or(name);
    let display_id = |n: &str| {
        t.client
            .registry("slot_display")
            .iter()
            .position(|c| c == n)
            .and_then(|i| i32::try_from(i).ok())
    };
    let mut v = Vec::new();
    match name {
        "empty" | "any_fuel" => {}
        "with_any_potion" => slot_display(ctx, r, &mut v, depth + 1)?,
        "only_with_component" => {
            slot_display(ctx, r, &mut v, depth + 1)?;
            let component = r.get_var_int()?;
            v.put_var_int(t.registry("data_component_type")?.or(component, 0));
        }
        "item" => v.put_var_int(t.items.or(r.get_var_int()?, 0)),
        "item_stack" => {
            if !nested(ctx, r, &mut v)? {
                name = "empty";
            }
        }
        "tag" => match read_set(r)? {
            set if client >= V26_3 => put_set(&set, &mut v, &mut |id| t.items.get(id)),
            Set::Tag(tag) => v.put_str(tag),
            Set::Ids(ids) => {
                name = "composite";
                let item = display_id("item")?;
                v.put_len(ids.len());
                for id in ids {
                    v.put_var_int(item);
                    v.put_var_int(t.items.or(id, 0));
                }
            }
        },
        "dyed" | "with_remainder" => {
            slot_display(ctx, r, &mut v, depth + 1)?;
            slot_display(ctx, r, &mut v, depth + 1)?;
        }
        "smithing_trim" => {
            slot_display(ctx, r, &mut v, depth + 1)?;
            slot_display(ctx, r, &mut v, depth + 1)?;
            let pattern = synced_holder(ctx, "trim_pattern", r, |r| {
                skip_str(r)?;
                nbt::split(r)?;
                r.get_bool().map(|_| ())
            })?;
            if client >= V1_21_5 {
                // ponytail: an inline pattern becomes the first one.
                v.put_var_int(pattern.unwrap_or(0) + 1);
            } else {
                v.put_var_int(display_id("empty")?);
            }
        }
        "composite" => {
            let n = r.get_len()?;
            v.put_len(n);
            for _ in 0..n {
                slot_display(ctx, r, &mut v, depth + 1)?;
            }
        }
        _ => return None,
    }
    match display_id(name) {
        Some(id) => {
            out.put_var_int(id);
            out.put_slice(&v);
        }
        None => out.put_var_int(display_id("empty")?),
    }
    Some(())
}

fn slot_displays(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>, n: usize) -> Option<()> {
    (0..n).try_for_each(|_| slot_display(ctx, r, out, 0))
}

/// A recipe display: its type as the client's, then its slot displays.
fn recipe_display(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let t = ctx.t;
    let kind = r.get_var_int()?;
    let name = t
        .server
        .registry("recipe_display")
        .get(usize::try_from(kind).ok()?)?;
    out.put_var_int(t.registry("recipe_display")?.get(kind)?);
    match name.strip_prefix("minecraft:").unwrap_or(name) {
        "crafting_shapeless" => {
            let n = copy_len(r, out)?;
            slot_displays(ctx, r, out, n + 2)
        }
        "crafting_shaped" => {
            copy_var_int(r, out)?;
            copy_var_int(r, out)?;
            let n = copy_len(r, out)?;
            slot_displays(ctx, r, out, n + 2)
        }
        "furnace" => {
            slot_displays(ctx, r, out, 4)?;
            copy_var_int(r, out)?;
            copy(r, 4, out)
        }
        "stonecutter" => slot_displays(ctx, r, out, 3),
        "smithing" => slot_displays(ctx, r, out, 5),
        _ => None,
    }
}

fn copy_len(r: &mut &[u8], out: &mut Vec<u8>) -> Option<usize> {
    let n = r.get_len()?;
    out.put_len(n);
    Some(n)
}

/// An ingredient: a holder set of items.
fn ingredient(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let items = &ctx.t.items;
    put_set(&read_set(r)?, out, &mut |id| items.get(id));
    Some(())
}

/// Recipe packets for 1.21.2+ clients (ViaVersion `RecipeDisplayRewriter`).
/// 1.21 has the old recipe format (full recipes in `update_recipes`) and
/// gets none: an empty recipe book, no stonecutter recipes.
// ponytail: 1.21 needs whole recipes rebuilt (ViaBackwards `RecipeStorage`); port it when 1.21.0 matters.
fn recipes_known(ctx: &Ctx<'_>) -> bool {
    ctx.client() >= V1_21_2
}

/// `update_recipes`: property sets (items) and stonecutter recipes.
pub(crate) fn update_recipes(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if !recipes_known(ctx) {
        return None;
    }
    let mut out = Vec::with_capacity(r.len() + 16);
    let n = copy_len(&mut r, &mut out)?;
    for _ in 0..n {
        copy_str(&mut r, &mut out)?;
        let items = r.get_len()?;
        let mut ids = Vec::with_capacity(items.min(4096));
        for _ in 0..items {
            if let Some(id) = ctx.t.items.get(r.get_var_int()?) {
                ids.push(id);
            }
        }
        out.put_len(ids.len());
        ids.iter().for_each(|id| out.put_var_int(*id));
    }
    let n = copy_len(&mut r, &mut out)?;
    for _ in 0..n {
        ingredient(ctx, &mut r, &mut out)?;
        slot_display(ctx, &mut r, &mut out, 0)?;
    }
    Some(out)
}

/// `recipe_book_add`: entries whose category the client lacks are left out.
pub(crate) fn recipe_book_add(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if !recipes_known(ctx) {
        return None;
    }
    let categories = ctx.t.registry("recipe_book_category")?;
    let mut out = Vec::with_capacity(r.len() + 16);
    let n = r.get_len()?;
    let mut body = Vec::new();
    let mut kept = 0;
    for _ in 0..n {
        let mut e = Vec::new();
        copy_var_int(&mut r, &mut e)?;
        recipe_display(ctx, &mut r, &mut e)?;
        opt(&mut r, &mut e, |r, o| copy_var_int(r, o).map(|_| ()))?;
        let category = categories.get(r.get_var_int()?);
        e.put_var_int(category.unwrap_or(0));
        if copy_bool(&mut r, &mut e)? {
            let m = copy_len(&mut r, &mut e)?;
            for _ in 0..m {
                ingredient(ctx, &mut r, &mut e)?;
            }
        }
        copy(&mut r, 1, &mut e)?;
        if category.is_none() {
            continue;
        }
        body.put_slice(&e);
        kept += 1;
    }
    out.put_len(kept);
    out.put_slice(&body);
    copy(&mut r, 1, &mut out)?;
    Some(out)
}

pub(crate) fn place_ghost_recipe(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if !recipes_known(ctx) {
        return None;
    }
    let mut out = Vec::with_capacity(r.len() + 16);
    copy_var_int(&mut r, &mut out)?;
    recipe_display(ctx, &mut r, &mut out)?;
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn eras() {
        use super::{NestedFormat, TooltipFlags};
        assert_eq!(NestedFormat::of(774), NestedFormat::Stack);
        assert_eq!(NestedFormat::of(775), NestedFormat::Template);
        assert_eq!(TooltipFlags::of(769), TooltipFlags::Inline);
        assert_eq!(TooltipFlags::of(770), TooltipFlags::Display);
    }
}
