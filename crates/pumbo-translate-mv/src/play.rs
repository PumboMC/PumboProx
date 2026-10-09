//! Clientbound dispatch and the play packets without a module of their own.

use crate::version::{V1_21_2, V1_21_4, V1_21_5, V1_21_6, V1_21_9, V26_1, V26_2, V26_3, eras};
use crate::wire::{Get, Put, copy, copy_bool, copy_str, copy_var_int};
use crate::{Ctx, Handler, Phase, chunk, config, entity, item, nbt};

/// The handler of a server packet (by server name), if its payload changes.
pub(crate) fn to_client(phase: Phase, name: &str) -> Option<Handler> {
    Some(match (phase, name) {
        (Phase::Configuration, "select_known_packs") => config::known_packs_to_client,
        (Phase::Configuration, "registry_data") => config::registry_data,
        (Phase::Configuration, "finish_configuration") => config::finish_configuration,
        (_, "update_tags") => config::update_tags,
        (Phase::Configuration, "code_of_conduct") => config::code_of_conduct,
        (Phase::Play, "start_configuration") => config::start_configuration,
        (Phase::Play, "login") => login,
        (Phase::Play, "respawn") => respawn,
        (Phase::Play, "set_default_spawn_position") => spawn_position,
        (Phase::Play, "player_position") => player_position,
        (Phase::Play, "player_rotation") => player_rotation,
        (Phase::Play, "set_time") => set_time,
        (Phase::Play, "level_chunk_with_light") => chunk::level_chunk,
        (Phase::Play, "light_update") => chunk::light_update,
        (Phase::Play, "chunks_biomes") => chunk::chunks_biomes,
        (Phase::Play, "block_update") => chunk::block_update,
        (Phase::Play, "section_blocks_update") => chunk::section_blocks_update,
        (Phase::Play, "block_entity_data") => chunk::block_entity_data,
        (Phase::Play, "block_event") => chunk::block_event,
        (Phase::Play, "level_event") => chunk::level_event,
        (Phase::Play, "add_entity") => entity::add_entity,
        (Phase::Play, "remove_entities") => entity::remove_entities,
        (Phase::Play, "set_entity_data") => entity::set_entity_data,
        (Phase::Play, "set_entity_motion") => entity::set_entity_motion,
        (Phase::Play, "move_entity_pos") => entity::move_entity_pos,
        (Phase::Play, "move_entity_pos_rot") => entity::move_entity_pos_rot,
        (Phase::Play, "move_entity_rot") => entity::move_entity_rot,
        (Phase::Play, "entity_position_sync") => entity::entity_position_sync,
        (Phase::Play, "teleport_entity") => entity::teleport_entity,
        (Phase::Play, "animate") => entity::animate,
        (Phase::Play, "swing_animation") => entity::swing_animation,
        (Phase::Play, "update_attributes") => entity::update_attributes,
        (Phase::Play, "update_mob_effect" | "remove_mob_effect") => entity::update_mob_effect,
        (Phase::Play, "damage_event") => entity::damage_event,
        (Phase::Play, "container_set_content") => item::container_set_content,
        (Phase::Play, "container_set_slot") => item::container_set_slot,
        (Phase::Play, "set_cursor_item") => item::single_stack,
        (Phase::Play, "set_player_inventory") => item::set_player_inventory,
        (Phase::Play, "set_equipment") => item::set_equipment,
        (Phase::Play, "update_advancements") => item::update_advancements,
        (Phase::Play, "merchant_offers") => item::merchant_offers,
        (Phase::Play, "container_close" | "container_set_data" | "mount_screen_open") => {
            item::window_first
        }
        (Phase::Play, "update_recipes") => item::update_recipes,
        (Phase::Play, "recipe_book_add") => item::recipe_book_add,
        (Phase::Play, "place_ghost_recipe") => item::place_ghost_recipe,
        (Phase::Play, "commands") => commands,
        (Phase::Play, "sound") => sound,
        (Phase::Play, "sound_entity") => sound,
        (Phase::Play, "stop_sound") => stop_sound,
        (Phase::Play, "level_particles") => level_particles,
        (Phase::Play, "explode") => explode,
        (Phase::Play, "player_info_update") => player_info_update,
        (Phase::Play, "player_info_remove") => player_info_remove,
        (Phase::Play, "set_player_team") => set_player_team,
        (Phase::Play, "open_screen") => open_screen,
        (Phase::Play, "cooldown") => cooldown,
        (Phase::Play, "award_stats") => award_stats,
        (Phase::Play, "map_item_data") => map_item_data,
        (Phase::Play, "player_chat") => player_chat,
        (Phase::Play, "disguised_chat") => disguised_chat,
        (Phase::Play, "open_sign_editor") => open_sign_editor,
        (Phase::Play, "set_held_slot") => set_held_slot,
        (Phase::Play, "change_difficulty") => change_difficulty,
        (Phase::Play, "show_dialog") => show_dialog,
        // Text with click and hover events (layout changed in 1.21.5).
        (_, "disconnect")
        | (Phase::Play, "set_title_text" | "set_subtitle_text" | "set_action_bar_text") => one_text,
        (Phase::Play, "system_chat") => one_text,
        (Phase::Play, "tab_list") => tab_list,
        (Phase::Play, "boss_event") => boss_event,
        (Phase::Play, "player_combat_kill") => player_combat_kill,
        (Phase::Play, "server_data") => one_text,
        _ => return None,
    })
}

// ---------------------------------------------------------------- spawn info

/// The spawn info shared by `login` and `respawn`, up to the sea level.
fn spawn_info(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let client = ctx.client();
    let dimension = r.get_var_int()?;
    out.put_var_int(
        ctx.dynamic("dimension_type")
            .map_or(dimension, |m| m.or(dimension, 0)),
    );
    copy_str(r, out)?;
    copy(r, 8, out)?;
    // Game modes: VarInt since 26.3 (previous as ID + 1, 0 = none); byte and signed byte before.
    let game_mode = r.get_var_int()?;
    let previous = r.get_var_int()?;
    if client >= V26_3 {
        out.put_var_int(game_mode);
        out.put_var_int(previous);
    } else {
        out.put_u8(game_mode as u8);
        out.put_i8((previous - 1) as i8);
    }
    copy(r, 2, out)?;
    if copy_bool(r, out)? {
        copy_str(r, out)?;
        copy(r, 8, out)?;
    }
    copy_var_int(r, out)?;
    let sea_level = r.get_var_int()?;
    if client >= V1_21_2 {
        out.put_var_int(sea_level);
    }
    Some(())
}

fn login(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let client = ctx.client();
    ctx.s.loaded_sent = false;
    // A new play session: the client's player list starts empty.
    ctx.s.players.clear();
    let mut out = Vec::with_capacity(r.len());
    let id = r.get_i32()?;
    out.put_i32(id);
    if let Some(player) = ctx
        .t
        .server
        .registry("entity_type")
        .iter()
        .position(|n| n == "player")
    {
        ctx.s.entities.insert(id, i32::try_from(player).ok()?);
    }
    copy(&mut r, 1, &mut out)?;
    let n = r.get_len()?;
    out.put_len(n);
    for _ in 0..n {
        copy_str(&mut r, &mut out)?;
    }
    for _ in 0..3 {
        copy_var_int(&mut r, &mut out)?;
    }
    copy(&mut r, 3, &mut out)?;
    spawn_info(ctx, &mut r, &mut out)?;
    let online_mode = r.get_bool()?;
    if client >= V26_2 {
        out.put_bool(online_mode);
    }
    copy(&mut r, 1, &mut out)?;
    Some(out)
}

fn respawn(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    ctx.s.loaded_sent = false;
    let mut out = Vec::with_capacity(r.len());
    spawn_info(ctx, &mut r, &mut out)?;
    copy(&mut r, 1, &mut out)?;
    Some(out)
}

/// `set_default_spawn_position`: dimension and pitch since 1.21.9.
fn spawn_position(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_9 {
        return Some(r.to_vec());
    }
    r.get_str()?;
    let mut out = Vec::with_capacity(12);
    copy(&mut r, 12, &mut out)?;
    Some(out)
}

/// `player_position`: before 1.21.2 the flags are a byte, the teleport ID comes
/// last and there is no velocity. The target is kept for the 26.3 confirmation.
fn player_position(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let teleport = r.get_var_int()?;
    let pos = [r.get_f64()?, r.get_f64()?, r.get_f64()?];
    let velocity = r.bytes(24)?;
    let rot = [r.get_f32()?, r.get_f32()?];
    let flags = r.get_i32()?;
    // Bits 0..2 relative position, 3..4 relative rotation.
    let mut abs_pos = pos;
    for (i, p) in abs_pos.iter_mut().enumerate() {
        if flags & (1 << i) != 0 {
            *p += ctx.s.position.get(i).copied().unwrap_or(0.0);
        }
    }
    let mut abs_rot = rot;
    for (i, a) in abs_rot.iter_mut().enumerate() {
        if flags & (1 << (3 + i)) != 0 {
            *a += ctx.s.rotation.get(i).copied().unwrap_or(0.0);
        }
    }
    if ctx.s.teleports.len() >= 16 {
        ctx.s.teleports.remove(0);
    }
    ctx.s.teleports.push((teleport, abs_pos, abs_rot));
    let mut out = Vec::with_capacity(64);
    if ctx.client() >= V1_21_2 {
        out.put_var_int(teleport);
        pos.iter().for_each(|c| out.put_f64(*c));
        out.put_slice(velocity);
        rot.iter().for_each(|c| out.put_f32(*c));
        out.put_i32(flags);
    } else {
        pos.iter().for_each(|c| out.put_f64(*c));
        rot.iter().for_each(|c| out.put_f32(*c));
        out.put_u8((flags & 0x1F) as u8);
        out.put_var_int(teleport);
    }
    Some(out)
}

/// `player_rotation` (1.21.2+): relative flags since 1.21.9; older clients get
/// absolute angles from the last known rotation.
fn player_rotation(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_9 {
        return Some(r.to_vec());
    }
    let (yaw, rel_yaw, pitch, rel_pitch) =
        (r.get_f32()?, r.get_bool()?, r.get_f32()?, r.get_bool()?);
    let [base_yaw, base_pitch] = ctx.s.rotation;
    let yaw = if rel_yaw { yaw + base_yaw } else { yaw };
    let pitch = if rel_pitch { pitch + base_pitch } else { pitch };
    let mut out = Vec::with_capacity(8);
    out.put_f32(yaw);
    out.put_f32(pitch);
    Some(out)
}

/// `set_time`: since 26.1 world clocks; older clients get the overworld clock
/// as the time of day (negative before 1.21.2 when it does not advance).
fn set_time(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let client = ctx.client();
    let age = r.get_i64()?;
    let n = r.get_len()?;
    let mut clocks = Vec::with_capacity(n.min(8));
    for _ in 0..n {
        clocks.push((
            r.get_var_int()?,
            r.get_var_long()?,
            r.get_f32()?,
            r.get_f32()?,
        ));
    }
    let mut out = Vec::with_capacity(32);
    out.put_i64(age);
    if client >= V26_1 {
        let mut kept = Vec::new();
        for (id, ticks, partial, rate) in clocks {
            let Some(id) = ctx.dynamic("world_clock").map_or(Some(id), |m| m.get(id)) else {
                continue;
            };
            kept.push((id, ticks, partial, rate));
        }
        out.put_len(kept.len());
        for (id, ticks, partial, rate) in kept {
            out.put_var_int(id);
            out.put_var_long(ticks);
            out.put_f32(partial);
            out.put_f32(rate);
        }
        return Some(out);
    }
    let overworld = ctx
        .s
        .server_registries
        .get("world_clock")
        .and_then(|e| e.iter().position(|n| n == "minecraft:overworld"))
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0);
    let clock = clocks.iter().find(|c| c.0 == overworld).or(clocks.first());
    let (ticks, advancing) = match clock {
        Some(&(_, ticks, _, rate)) => {
            ctx.s.day_time = Some((ticks, rate > 0.0));
            (ticks, rate > 0.0)
        }
        None => ctx.s.day_time.unwrap_or((age, true)),
    };
    if client >= V1_21_2 {
        out.put_i64(ticks);
        out.put_bool(advancing);
    } else {
        out.put_i64(if advancing { ticks } else { -ticks.max(1) });
    }
    Some(out)
}

// ---------------------------------------------------------------- commands

const FLAG_ARGUMENT: u8 = 2;
const FLAG_REDIRECT: u8 = 8;
const FLAG_SUGGESTIONS: u8 = 16;
/// New in 1.21.6.
const FLAG_RESTRICTED: u8 = 32;

/// Bytes of an argument parser's properties.
fn parser_properties<'a>(parser: &str, r: &mut &'a [u8]) -> Option<&'a [u8]> {
    let start = *r;
    match parser {
        "brigadier:float" | "brigadier:integer" | "brigadier:double" | "brigadier:long" => {
            let flags = r.get_u8()?;
            let size = if parser.ends_with("float") || parser.ends_with("integer") {
                4
            } else {
                8
            };
            r.bytes(size * (usize::from(flags & 1) + usize::from((flags >> 1) & 1)))?;
        }
        "brigadier:string" => {
            r.get_var_int()?;
        }
        "entity" | "score_holder" => {
            r.get_u8()?;
        }
        "time" => {
            r.get_i32()?;
        }
        "resource_or_tag"
        | "resource_or_tag_key"
        | "resource"
        | "resource_key"
        | "resource_selector" => {
            r.get_str()?;
        }
        _ => {}
    }
    start.get(..start.len() - r.len())
}

/// `commands`: argument parsers as the client's; parsers it lacks become
/// quotable strings (as ViaVersion does).
fn commands(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let client = ctx.client();
    let parsers = ctx.t.registry("command_argument_type")?;
    let server_parsers = ctx.t.server.registry("command_argument_type");
    let string = ctx
        .t
        .client
        .registry("command_argument_type")
        .iter()
        .position(|n| n == "brigadier:string");
    let mut out = Vec::with_capacity(r.len());
    let n = r.get_len()?;
    out.put_len(n);
    for _ in 0..n {
        let mut flags = r.get_u8()?;
        if client < V1_21_6 {
            flags &= !FLAG_RESTRICTED;
        }
        out.put_u8(flags);
        let children = r.get_len()?;
        out.put_len(children);
        for _ in 0..children {
            copy_var_int(&mut r, &mut out)?;
        }
        if flags & FLAG_REDIRECT != 0 {
            copy_var_int(&mut r, &mut out)?;
        }
        if flags & 3 != 0 {
            copy_str(&mut r, &mut out)?;
        }
        if flags & 3 == FLAG_ARGUMENT {
            let id = r.get_var_int()?;
            let name = server_parsers.get(usize::try_from(id).ok()?)?;
            let name = name.strip_prefix("minecraft:").unwrap_or(name);
            let props = parser_properties(name, &mut r)?;
            match parsers.get(id) {
                Some(client_id) => {
                    out.put_var_int(client_id);
                    out.put_slice(props);
                }
                None => {
                    out.put_var_int(i32::try_from(string?).ok()?);
                    out.put_var_int(1);
                }
            }
        }
        if flags & FLAG_SUGGESTIONS != 0 {
            copy_str(&mut r, &mut out)?;
        }
    }
    copy_var_int(&mut r, &mut out)?;
    Some(out)
}

// ---------------------------------------------------------------- sounds and particles

const UI_SOURCE: i32 = 10;

/// A sound holder: registry IDs the client lacks become inline sounds by name.
pub(crate) fn sound_holder(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let holder = r.get_var_int()?;
    if holder == 0 {
        out.put_var_int(0);
        copy_str(r, out)?;
        if copy_bool(r, out)? {
            copy(r, 4, out)?;
        }
        return Some(());
    }
    match ctx.t.registry("sound_event")?.get(holder - 1) {
        Some(id) => out.put_var_int(id + 1),
        None => {
            let name = ctx
                .t
                .server
                .registry("sound_event")
                .get(usize::try_from(holder - 1).ok()?)?;
            out.put_var_int(0);
            out.put_str(&full(name));
            out.put_bool(false);
        }
    }
    Some(())
}

fn full(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

fn sound_source(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let source = r.get_var_int()?;
    out.put_var_int(if source == UI_SOURCE && ctx.client() < V1_21_6 {
        0
    } else {
        source
    });
    Some(())
}

/// `sound` and `sound_entity`: holder and source, the rest unchanged.
fn sound(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 32);
    sound_holder(ctx, &mut r, &mut out)?;
    sound_source(ctx, &mut r, &mut out)?;
    out.put_slice(r);
    Some(out)
}

fn stop_sound(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let flags = r.get_u8()?;
    let mut out = vec![flags];
    if flags & 1 != 0 {
        sound_source(ctx, &mut r, &mut out)?;
    }
    out.put_slice(r);
    Some(out)
}

eras! {
    enum ParticleFormat {
        V1_21 = V1_21,
        /// Dust colors are packed RGB ints.
        V1_21_2 = V1_21_2,
        /// Effect, instant effect, dragon breath and flash gained options.
        V1_21_9 = V1_21_9,
    }
}

fn rgb_floats(color: i32, out: &mut Vec<u8>) {
    for shift in [16, 8, 0] {
        out.put_f32(((color >> shift) & 0xFF) as f32 / 255.0);
    }
}

/// Reads one server particle (type and options) and writes the client's.
pub(crate) fn particle(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let format = ParticleFormat::of(ctx.client());
    let id = r.get_var_int()?;
    let name = ctx
        .t
        .server
        .registry("particle_type")
        .get(usize::try_from(id).ok()?)?
        .clone();
    let client_id = ctx.t.registry("particle_type")?.get(id)?;
    let short = |n: &str| n.strip_prefix("minecraft:").unwrap_or(n).to_string();
    let stand_in = short(
        ctx.t
            .client
            .registry("particle_type")
            .get(usize::try_from(client_id).ok()?)?,
    );
    let name = short(&name);
    // A stand-in (ViaVersion Mappings) keeps the options when their layout is
    // the same and drops them when it has none; otherwise it is not sent.
    let same_options = options(&stand_in) == options(&name);
    if !same_options && !options(&stand_in).is_empty() {
        return None;
    }
    out.put_var_int(client_id);
    let options_start = out.len();
    match name.as_str() {
        "block" | "block_marker" | "falling_dust" | "dust_pillar" | "block_crumble" => {
            out.put_var_int(ctx.t.block_state(r.get_var_int()?));
        }
        "dust" => {
            let color = r.get_i32()?;
            if format >= ParticleFormat::V1_21_2 {
                out.put_i32(color);
            } else {
                rgb_floats(color, out);
            }
            copy(r, 4, out)?;
        }
        "dust_color_transition" => {
            let (from, to) = (r.get_i32()?, r.get_i32()?);
            if format >= ParticleFormat::V1_21_2 {
                out.put_i32(from);
                out.put_i32(to);
            } else {
                rgb_floats(from, out);
                rgb_floats(to, out);
            }
            copy(r, 4, out)?;
        }
        "entity_effect" | "tinted_leaves" => copy(r, 4, out)?,
        "effect" | "instant_effect" => {
            let options = r.bytes(8)?;
            if format >= ParticleFormat::V1_21_9 {
                out.put_slice(options);
            }
        }
        "dragon_breath" | "flash" => {
            let options = r.bytes(4)?;
            if format >= ParticleFormat::V1_21_9 {
                out.put_slice(options);
            }
        }
        "sculk_charge" => copy(r, 4, out)?,
        "shriek" => {
            copy_var_int(r, out)?;
        }
        "item" => item::stack(ctx, r, out)?,
        "vibration" => {
            match copy_var_int(r, out)? {
                0 => copy(r, 8, out)?,
                _ => {
                    copy_var_int(r, out)?;
                    copy(r, 4, out)?;
                }
            }
            copy_var_int(r, out)?;
        }
        "trail" => {
            copy(r, 28, out)?;
            copy_var_int(r, out)?;
        }
        "geyser" | "geyser_plume" => copy(r, 4, out)?,
        "geyser_base" | "geyser_poof" => copy(r, 8, out)?,
        // Particles without options, or new ones whose options are unknown (a new
        // particle with options needs a branch here and in `options`).
        _ => {}
    }
    if !same_options {
        out.truncate(options_start);
    }
    Some(())
}

/// Particles sharing an option layout (ViaVersion `ParticleType` readers);
/// empty for those without options.
fn options(particle: &str) -> &str {
    match particle {
        "block" | "block_marker" | "falling_dust" | "dust_pillar" | "block_crumble" => "block",
        "entity_effect" | "tinted_leaves" => "color",
        "dust"
        | "dust_color_transition"
        | "effect"
        | "instant_effect"
        | "dragon_breath"
        | "flash"
        | "sculk_charge"
        | "shriek"
        | "item"
        | "vibration"
        | "trail"
        | "geyser"
        | "geyser_plume"
        | "geyser_base"
        | "geyser_poof" => particle,
        _ => "",
    }
}

/// Pumpkin 0.1 and 0.2 send the `trail` particle of an eyeblossom without its
/// options (Pumpkin issue #3065), and no client can decode that. Returns such a
/// `level_particles` payload with the options added, `None` for any other
/// payload (correct ones too), so the repair stops by itself once Pumpkin is
/// fixed. `particle_first`: the 26.3 layout (particle before the position),
/// otherwise 26.2's (particle last).
pub fn fix_empty_trail(payload: &[u8], trail: i32, particle_first: bool) -> Option<Vec<u8>> {
    let mut r = payload;
    let split = if particle_first {
        if r.get_var_int()? != trail {
            return None;
        }
        let split = payload.len() - r.len();
        // Without options only the flags, position, offset and speeds (50
        // bytes), the count and the randomization type follow.
        r.bytes(50)?;
        r.get_var_int()?;
        r.get_var_int()?;
        split
    } else {
        // Flags, position, offset, speed and count (46 bytes), then the particle.
        r.bytes(46)?;
        if r.get_var_int()? != trail {
            return None;
        }
        payload.len()
    };
    if !r.is_empty() {
        return None;
    }
    let (head, tail) = payload.split_at_checked(split)?;
    let mut position = (if particle_first { tail } else { payload }).get(2..)?;
    let (x, y, z) = (
        position.get_f64()?,
        position.get_f64()?,
        position.get_f64()?,
    );
    let mut out = Vec::with_capacity(payload.len() + 29);
    out.put_slice(head);
    // Vanilla's eyeblossom trail (`EyeblossomBlock.Type.spawnTransformParticle`)
    // at its mean: target 1.5 blocks up, 20 ticks. The packet does not tell
    // opening from closing, so it takes the opening color (closing: 0x5F5F5F).
    out.put_f64(x);
    out.put_f64(y + 1.5);
    out.put_f64(z);
    out.put_i32(0x00FC_7812);
    out.put_var_int(20);
    out.put_slice(tail);
    Some(out)
}

/// `level_particles`: 26.3 moved the particle to the front, split the speed per
/// axis, made the count a VarInt and added a randomization type.
fn level_particles(ctx: &mut Ctx<'_>, payload: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V26_3 {
        return Some(payload.to_vec());
    }
    // The same repair as for 26.3 clients in the proxy (`fix_empty_trail`).
    let id = { payload }.get_var_int()?;
    let trail = ctx
        .t
        .server
        .registry("particle_type")
        .get(usize::try_from(id).ok()?)
        .is_some_and(|n| n.strip_prefix("minecraft:").unwrap_or(n) == "trail");
    let fixed = trail.then(|| fix_empty_trail(payload, id, true)).flatten();
    let mut r = fixed.as_deref().unwrap_or(payload);
    let mut particle_out = Vec::new();
    particle(ctx, &mut r, &mut particle_out)?;
    let (limiter, always) = (r.get_bool()?, r.get_bool()?);
    let pos = r.bytes(24)?;
    let offset = [r.get_f32()?, r.get_f32()?, r.get_f32()?];
    let speed = [r.get_f32()?, r.get_f32()?, r.get_f32()?];
    let count = r.get_var_int()?;
    let (offset, max_speed) = if speed[0] == speed[1] && speed[1] == speed[2] {
        (offset, speed[0])
    } else if count <= 0 {
        // A count of 0 uses offset * speed as the velocity.
        (
            [
                offset[0] * speed[0],
                offset[1] * speed[1],
                offset[2] * speed[2],
            ],
            1.0,
        )
    } else {
        // ponytail: per-axis speeds need one packet per particle; the X speed stands for all.
        (offset, speed[0])
    };
    let mut out = Vec::with_capacity(64);
    out.put_bool(limiter);
    if ctx.client() >= V1_21_4 {
        out.put_bool(always);
    }
    out.put_slice(pos);
    offset.iter().for_each(|v| out.put_f32(*v));
    out.put_f32(max_speed);
    out.put_i32(count);
    out.put_slice(&particle_out);
    Some(out)
}

eras! {
    enum ExplosionFormat {
        /// Radius, affected blocks, motion floats, small and large particles.
        V1_21 = V1_21,
        /// Optional knockback (floats), one particle.
        V1_21_2 = V1_21_2,
        /// Knockback as doubles.
        V1_21_4 = V1_21_4,
        /// Radius, block count, block particles.
        V1_21_9 = V1_21_9,
        /// Play sound flag.
        V26_3 = V26_3,
    }
}

fn explode(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let format = ExplosionFormat::of(ctx.client());
    if format == ExplosionFormat::V26_3 {
        return Some(r.to_vec());
    }
    let center = r.bytes(24)?;
    let radius = r.get_f32()?;
    let blocks = r.get_i32()?;
    let knockback = if r.get_bool()? {
        Some([r.get_f64()?, r.get_f64()?, r.get_f64()?])
    } else {
        None
    };
    let mut particle_out = Vec::new();
    particle(ctx, &mut r, &mut particle_out)?;
    let mut sound_out = Vec::new();
    sound_holder(ctx, &mut r, &mut sound_out)?;
    let n = r.get_len()?;
    let mut block_particles = Vec::new();
    for _ in 0..n {
        particle(ctx, &mut r, &mut block_particles)?;
        copy(&mut r, 8, &mut block_particles)?;
        copy_var_int(&mut r, &mut block_particles)?;
    }
    if !r.get_bool()? {
        sound_out.clear();
        sound_out.put_var_int(0);
        sound_out.put_str("minecraft:intentionally_empty");
        sound_out.put_bool(false);
    }
    let mut out = Vec::with_capacity(64);
    out.put_slice(center);
    match format {
        ExplosionFormat::V1_21 => {
            out.put_f32(radius);
            out.put_var_int(0);
            knockback
                .unwrap_or_default()
                .iter()
                .for_each(|v| out.put_f32(*v as f32));
            out.put_var_int(0);
            out.put_slice(&particle_out);
            out.put_slice(&particle_out);
        }
        ExplosionFormat::V1_21_2 | ExplosionFormat::V1_21_4 => {
            out.put_bool(knockback.is_some());
            for v in knockback.unwrap_or_default() {
                if format == ExplosionFormat::V1_21_2 {
                    out.put_f32(v as f32);
                } else {
                    out.put_f64(v);
                }
            }
            out.put_slice(&particle_out);
        }
        ExplosionFormat::V1_21_9 | ExplosionFormat::V26_3 => {
            out.put_f32(radius);
            out.put_i32(blocks);
            out.put_bool(knockback.is_some());
            knockback
                .unwrap_or_default()
                .iter()
                .for_each(|v| out.put_f64(*v));
            out.put_slice(&particle_out);
        }
    }
    out.put_slice(&sound_out);
    if format >= ExplosionFormat::V1_21_9 {
        out.put_len(n);
        out.put_slice(&block_particles);
    }
    Some(out)
}

// ---------------------------------------------------------------- players and teams

const ACTION_ADD: u8 = 0x01;
const ACTION_CHAT: u8 = 0x02;
const ACTION_GAME_MODE: u8 = 0x04;
const ACTION_LISTED: u8 = 0x08;
const ACTION_LATENCY: u8 = 0x10;
const ACTION_DISPLAY_NAME: u8 = 0x20;
const ACTION_LIST_ORDER: u8 = 0x40;
const ACTION_HAT: u8 = 0x80;

/// `player_info_update`: the list order (1.21.2+) and hat (1.21.4+) actions
/// are dropped for clients without them. Added profiles are remembered for
/// spawns that come before them (see `Translator::hold`).
fn player_info_update(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let client = ctx.client();
    let mut keep = 0xFFu8;
    if client < V1_21_2 {
        keep &= !ACTION_LIST_ORDER;
    }
    if client < V1_21_4 {
        keep &= !ACTION_HAT;
    }
    let actions = r.get_u8()?;
    let mut out = Vec::with_capacity(r.len() + 1);
    out.put_u8(actions & keep);
    let n = r.get_len()?;
    out.put_len(n);
    for _ in 0..n {
        let uuid: [u8; 16] = r.array()?;
        out.put_slice(&uuid);
        if actions & ACTION_ADD != 0 {
            ctx.s.players.insert(uuid);
            ctx.s.waiting.retain(|w| *w != uuid);
            copy_str(&mut r, &mut out)?;
            let props = r.get_len()?;
            out.put_len(props);
            for _ in 0..props {
                copy_str(&mut r, &mut out)?;
                copy_str(&mut r, &mut out)?;
                if copy_bool(&mut r, &mut out)? {
                    copy_str(&mut r, &mut out)?;
                }
            }
        }
        if actions & ACTION_CHAT != 0 && copy_bool(&mut r, &mut out)? {
            copy(&mut r, 24, &mut out)?;
            for _ in 0..2 {
                let len = r.get_len()?;
                out.put_len(len);
                copy(&mut r, len, &mut out)?;
            }
        }
        if actions & ACTION_GAME_MODE != 0 {
            copy_var_int(&mut r, &mut out)?;
        }
        if actions & ACTION_LISTED != 0 {
            copy(&mut r, 1, &mut out)?;
        }
        if actions & ACTION_LATENCY != 0 {
            copy_var_int(&mut r, &mut out)?;
        }
        if actions & ACTION_DISPLAY_NAME != 0 {
            nbt::opt_text(client, &mut r, &mut out)?;
        }
        if actions & ACTION_LIST_ORDER != 0 {
            let order = r.get_var_int()?;
            if keep & ACTION_LIST_ORDER != 0 {
                out.put_var_int(order);
            }
        }
        if actions & ACTION_HAT != 0 {
            let hat = r.get_bool()?;
            if keep & ACTION_HAT != 0 {
                out.put_bool(hat);
            }
        }
    }
    Some(out)
}

/// `player_info_remove`: forgotten profiles; the payload is unchanged.
fn player_info_remove(ctx: &mut Ctx<'_>, payload: &[u8]) -> Option<Vec<u8>> {
    let mut r = payload;
    for _ in 0..r.get_len()? {
        let uuid: [u8; 16] = r.array()?;
        ctx.s.players.remove(&uuid);
    }
    Some(payload.to_vec())
}

eras! {
    enum TeamFormat {
        /// Rules as strings.
        V1_21 = V1_21,
        /// Rules as IDs.
        V1_21_5 = V1_21_5,
        /// Prefix and suffix before the rules, optional color, flags last; same as the server.
        V26_2 = V26_2,
    }
}

const VISIBILITY: [&str; 4] = ["always", "never", "hideForOtherTeams", "hideForOwnTeam"];
const COLLISION: [&str; 4] = ["always", "never", "pushOtherTeams", "pushOwnTeam"];
/// `ChatFormatting.RESET`, the color of a team without one.
const RESET: i32 = 21;

fn set_player_team(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let format = TeamFormat::of(ctx.client());
    if format == TeamFormat::V26_2 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(r.len() + 16);
    copy_str(&mut r, &mut out)?;
    let method = r.get_i8()?;
    out.put_i8(method);
    if method == 0 || method == 2 {
        let display = nbt::split(&mut r)?;
        let prefix = nbt::split(&mut r)?;
        let suffix = nbt::split(&mut r)?;
        let visibility = r.get_var_int()?;
        let collision = r.get_var_int()?;
        let color = if r.get_bool()? {
            r.get_var_int()?
        } else {
            RESET
        };
        let flags = r.get_i8()?;
        nbt::text(ctx.client(), &mut &display[..], &mut out)?;
        out.put_i8(flags);
        if format == TeamFormat::V1_21 {
            out.put_str(
                VISIBILITY
                    .get(usize::try_from(visibility).ok()?)
                    .copied()
                    .unwrap_or("always"),
            );
            out.put_str(
                COLLISION
                    .get(usize::try_from(collision).ok()?)
                    .copied()
                    .unwrap_or("always"),
            );
        } else {
            out.put_var_int(visibility);
            out.put_var_int(collision);
        }
        out.put_var_int(color);
        nbt::text(ctx.client(), &mut &prefix[..], &mut out)?;
        nbt::text(ctx.client(), &mut &suffix[..], &mut out)?;
    }
    out.put_slice(r);
    Some(out)
}

// ---------------------------------------------------------------- screens, stats, maps, chat

fn open_screen(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len());
    copy_var_int(&mut r, &mut out)?;
    out.put_var_int(ctx.t.registry("menu")?.get(r.get_var_int()?)?);
    nbt::text(ctx.client(), &mut r, &mut out)?;
    Some(out)
}

// ---------------------------------------------------------------- text

/// Packets that start with one text component; the rest is copied
/// (`system_chat` overlay flag, `server_data` icon).
fn one_text(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_5 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(r.len());
    nbt::text(ctx.client(), &mut r, &mut out)?;
    out.put_slice(r);
    Some(out)
}

fn tab_list(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_5 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(r.len());
    nbt::text(ctx.client(), &mut r, &mut out)?;
    nbt::text(ctx.client(), &mut r, &mut out)?;
    Some(out)
}

/// `boss_event`: the title of `add` (0) and `update_name` (3).
fn boss_event(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_5 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(r.len());
    copy(&mut r, 16, &mut out)?;
    let action = copy_var_int(&mut r, &mut out)?;
    if action == 0 || action == 3 {
        nbt::text(ctx.client(), &mut r, &mut out)?;
    }
    out.put_slice(r);
    Some(out)
}

fn player_combat_kill(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_5 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(r.len());
    copy_var_int(&mut r, &mut out)?;
    nbt::text(ctx.client(), &mut r, &mut out)?;
    Some(out)
}

/// `cooldown`: a cooldown group since 1.21.2, an item before (groups named
/// after an item only).
fn cooldown(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_2 {
        return Some(r.to_vec());
    }
    let group = r.get_str()?;
    let short = group.strip_prefix("minecraft:").unwrap_or(group);
    let item = ctx
        .t
        .server
        .registry("item")
        .iter()
        .position(|n| n == short)?;
    let mut out = Vec::with_capacity(8);
    out.put_var_int(ctx.t.items.get(i32::try_from(item).ok()?)?);
    copy_var_int(&mut r, &mut out)?;
    Some(out)
}

fn award_stats(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let t = ctx.t;
    let types = t.registry("stat_type")?;
    let n = r.get_len()?;
    let mut body = Vec::new();
    let mut kept = 0;
    for _ in 0..n {
        let (kind, stat, value) = (r.get_var_int()?, r.get_var_int()?, r.get_var_int()?);
        let kind_name = t
            .server
            .registry("stat_type")
            .get(usize::try_from(kind).ok()?)?;
        let stat = match kind_name.as_str() {
            "mined" => t.blocks.get(stat),
            "crafted" | "used" | "broken" | "picked_up" | "dropped" => t.items.get(stat),
            "killed" | "killed_by" => t.entity_types.get(stat),
            "custom" => t.registry("custom_stat").and_then(|m| m.get(stat)),
            _ => None,
        };
        let (Some(kind), Some(stat)) = (types.get(kind), stat) else {
            continue;
        };
        body.put_var_int(kind);
        body.put_var_int(stat);
        body.put_var_int(value);
        kept += 1;
    }
    let mut out = Vec::with_capacity(body.len() + 2);
    out.put_len(kept);
    out.put_slice(&body);
    Some(out)
}

fn map_item_data(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len());
    copy_var_int(&mut r, &mut out)?;
    copy(&mut r, 2, &mut out)?;
    if copy_bool(&mut r, &mut out)? {
        let types = ctx.t.registry("map_decoration_type")?;
        let n = r.get_len()?;
        let mut body = Vec::new();
        let mut kept = 0;
        for _ in 0..n {
            let kind = r.get_var_int()?;
            let start = r;
            r.bytes(3)?;
            if r.get_bool()? {
                nbt::split(&mut r)?;
            }
            let Some(kind) = types.get(kind) else {
                continue;
            };
            body.put_var_int(kind);
            body.put_slice(start.get(..start.len() - r.len())?);
            kept += 1;
        }
        out.put_len(kept);
        out.put_slice(&body);
    }
    out.put_slice(r);
    Some(out)
}

/// A chat type holder: registry IDs as the client's.
fn chat_type(ctx: &mut Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let holder = r.get_var_int()?;
    if holder == 0 {
        out.put_var_int(0);
        for _ in 0..2 {
            copy_str(r, out)?;
            let n = r.get_len()?;
            out.put_len(n);
            for _ in 0..n {
                copy_var_int(r, out)?;
            }
            nbt::copy(r, out)?;
        }
        return Some(());
    }
    let id = holder - 1;
    out.put_var_int(ctx.dynamic("chat_type").map_or(id, |m| m.or(id, 0)) + 1);
    Some(())
}

fn player_chat(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len());
    let global = r.get_var_int()?;
    if ctx.client() >= V1_21_5 {
        out.put_var_int(global);
    }
    copy(&mut r, 16, &mut out)?;
    copy_var_int(&mut r, &mut out)?;
    if copy_bool(&mut r, &mut out)? {
        copy(&mut r, 256, &mut out)?;
    }
    copy_str(&mut r, &mut out)?;
    copy(&mut r, 16, &mut out)?;
    let previous = r.get_len()?;
    out.put_len(previous);
    for _ in 0..previous {
        if copy_var_int(&mut r, &mut out)? == 0 {
            copy(&mut r, 256, &mut out)?;
        }
    }
    nbt::opt_text(ctx.client(), &mut r, &mut out)?;
    if copy_var_int(&mut r, &mut out)? == 2 {
        let longs = r.get_len()?;
        out.put_len(longs);
        copy(&mut r, longs.checked_mul(8)?, &mut out)?;
    }
    chat_type(ctx, &mut r, &mut out)?;
    nbt::text(ctx.client(), &mut r, &mut out)?;
    nbt::opt_text(ctx.client(), &mut r, &mut out)?;
    Some(out)
}

fn disguised_chat(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len());
    nbt::text(ctx.client(), &mut r, &mut out)?;
    chat_type(ctx, &mut r, &mut out)?;
    nbt::text(ctx.client(), &mut r, &mut out)?;
    nbt::opt_text(ctx.client(), &mut r, &mut out)?;
    Some(out)
}

/// `open_sign_editor`: 26.3 names the side as a text slot ID (0 back, 1 front)
/// instead of a front flag.
fn open_sign_editor(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V26_3 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(9);
    copy(&mut r, 8, &mut out)?;
    out.put_bool(r.get_var_int()? == 1);
    Some(out)
}

/// `set_held_slot`: a byte before 1.21.4.
fn set_held_slot(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_4 {
        return Some(r.to_vec());
    }
    Some(vec![r.get_var_int()? as u8])
}

/// `change_difficulty`: a byte before 1.21.6.
fn change_difficulty(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_6 {
        return Some(r.to_vec());
    }
    let mut out = vec![r.get_var_int()? as u8];
    out.put_slice(r);
    Some(out)
}

/// `show_dialog` (play): a dialog holder; registry IDs as the client's.
fn show_dialog(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let holder = r.get_var_int()?;
    let mut out = Vec::with_capacity(r.len() + 2);
    if holder == 0 {
        out.put_var_int(0);
        out.put_slice(r);
        return Some(out);
    }
    let id = holder - 1;
    out.put_var_int(ctx.dynamic("dialog").map_or(Some(id), |m| m.get(id))? + 1);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRAIL: i32 = 59;
    const FLAME: i32 = 3;

    /// What Pumpkin 0.2.0 sends for an eyeblossom (26.3 layout): the particle
    /// first, then flags, block center, zero offset and speeds, count 1.
    fn level_particles_26_3(particle: i32, options: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.put_var_int(particle);
        p.put_slice(options);
        p.put_slice(&[0, 0]);
        [10.5, 64.5, -3.5].iter().for_each(|v| p.put_f64(*v));
        (0..6).for_each(|_| p.put_f32(0.0));
        p.put_var_int(1);
        p.put_var_int(0);
        p
    }

    fn level_particles_26_2(particle: i32, options: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 0];
        [10.5, 64.5, -3.5].iter().for_each(|v| p.put_f64(*v));
        (0..4).for_each(|_| p.put_f32(0.0));
        p.put_i32(1);
        p.put_var_int(particle);
        p.put_slice(options);
        p
    }

    fn trail_options(target: [f64; 3], color: i32, duration: i32) -> Vec<u8> {
        let mut o = Vec::new();
        target.iter().for_each(|v| o.put_f64(*v));
        o.put_i32(color);
        o.put_var_int(duration);
        o
    }

    /// Reads a 26.3 `level_particles` with a trail in vanilla's field order
    /// (ViaVersion `TRAIL1_21_4`, `BlockItemPacketRewriter26_3`): target,
    /// color and duration, then the rest, and nothing may be left.
    fn read_trail_26_3(mut r: &[u8]) -> ([f64; 3], i32, i32, [f64; 3]) {
        assert_eq!(r.get_var_int(), Some(TRAIL));
        let target = [r.get_f64(), r.get_f64(), r.get_f64()].map(Option::unwrap);
        let (color, duration) = (r.get_i32().unwrap(), r.get_var_int().unwrap());
        r.bytes(2).unwrap();
        let pos = [r.get_f64(), r.get_f64(), r.get_f64()].map(Option::unwrap);
        r.bytes(24).unwrap();
        assert_eq!(r.get_var_int(), Some(1));
        assert_eq!(r.get_var_int(), Some(0));
        assert!(r.is_empty(), "{} bytes left", r.len());
        (target, color, duration, pos)
    }

    #[test]
    fn empty_trail_gets_vanilla_eyeblossom_options() {
        let fixed = fix_empty_trail(&level_particles_26_3(TRAIL, &[]), TRAIL, true).unwrap();
        let (target, color, duration, pos) = read_trail_26_3(&fixed);
        assert_eq!(pos, [10.5, 64.5, -3.5]);
        assert_eq!(target, [10.5, 66.0, -3.5]);
        assert_eq!((color, duration), (0xFC7812, 20));
        // The same options as a correct packet (what a fixed Pumpkin sends).
        let correct = level_particles_26_3(TRAIL, &trail_options(target, color, 20));
        assert_eq!(fixed, correct);

        let fixed = fix_empty_trail(&level_particles_26_2(TRAIL, &[]), TRAIL, false).unwrap();
        let correct = level_particles_26_2(TRAIL, &trail_options(target, color, 20));
        assert_eq!(fixed, correct);
    }

    #[test]
    fn correct_packets_and_other_particles_stay_as_they_are() {
        let options = trail_options([1.0, 2.0, 3.0], 0x5F5F5F, 7);
        for (packet, first) in [
            (level_particles_26_3(TRAIL, &options), true),
            (level_particles_26_3(FLAME, &[]), true),
            (level_particles_26_2(TRAIL, &options), false),
            (level_particles_26_2(FLAME, &[]), false),
            // A 26.2 packet read as 26.3 and the other way round.
            (level_particles_26_2(TRAIL, &[]), true),
            (level_particles_26_3(TRAIL, &[]), false),
        ] {
            assert_eq!(
                fix_empty_trail(&packet, TRAIL, first),
                None,
                "{packet:02x?}"
            );
        }
    }

    /// The check on the proxy's relay path: a `level_particles` with another
    /// particle reads one VarInt, a correct trail its fixed fields.
    #[test]
    fn checking_a_particle_packet_is_cheap() {
        let options = trail_options([1.0, 2.0, 3.0], 0x5F5F5F, 7);
        for (what, packet) in [
            ("other particle", level_particles_26_3(FLAME, &[])),
            ("correct trail", level_particles_26_3(TRAIL, &options)),
            ("broken trail (repaired)", level_particles_26_3(TRAIL, &[])),
        ] {
            let t = std::time::Instant::now();
            let mut fixed = 0;
            for _ in 0..1_000_000 {
                fixed += usize::from(
                    fix_empty_trail(std::hint::black_box(&packet), TRAIL, true).is_some(),
                );
            }
            let took = t.elapsed();
            eprintln!("[measure] 1000000 checks, {what}: {took:?} ({fixed} repaired)");
            assert!(took < std::time::Duration::from_secs(10), "{took:?}");
        }
    }

    #[test]
    fn cut_or_garbage_packets_are_left_alone() {
        for (packet, first) in [
            (level_particles_26_3(TRAIL, &[]), true),
            (level_particles_26_2(TRAIL, &[]), false),
        ] {
            for n in 0..packet.len() {
                assert_eq!(fix_empty_trail(&packet[..n], TRAIL, first), None, "{n}");
            }
        }
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        for len in 0..200 {
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            for first in [true, false] {
                let _ = fix_empty_trail(&junk, TRAIL, first);
            }
        }
    }
}
