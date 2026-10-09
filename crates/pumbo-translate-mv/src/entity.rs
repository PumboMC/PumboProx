//! Entities: spawning, movement, entity data, attributes and effects.

use crate::play::particle;
use crate::version::{V1_21_2, V26_3, eras};
use crate::wire::{Get, Put, copy, copy_var_int};
use crate::{Ctx, item, nbt};

eras! {
    enum VelocityFormat {
        /// Three shorts (1/8000 block per tick), after the angles.
        Shorts = V1_21,
        /// `LpVec3`, after the position.
        Packed = V1_21_9,
    }
}

eras! {
    enum MoveFormat {
        /// One delta; on ground last.
        V1_21 = V1_21,
        /// Step paths; on ground in the properties.
        V26_3 = V26_3,
    }
}

/// Low-precision vector (`LpVec3`, 1.21.9+).
pub(crate) fn read_lp_vec3(r: &mut &[u8]) -> Option<[f64; 3]> {
    let b0 = r.get_u8()?;
    if b0 == 0 {
        return Some([0.0; 3]);
    }
    let b1 = r.get_u8()?;
    let rest = u64::from(r.get_i32()? as u32);
    let packed = (rest << 16) | (u64::from(b1) << 8) | u64::from(b0);
    let mut scale = u64::from(b0 & 3);
    if b0 & 4 != 0 {
        scale |= u64::from(r.get_var_int()? as u32) << 2;
    }
    let unpack = |v: u64| ((v & 0x7FFF) as f64).min(32766.0) * 2.0 / 32766.0 - 1.0;
    let s = scale as f64;
    Some([
        unpack(packed >> 3) * s,
        unpack(packed >> 18) * s,
        unpack(packed >> 33) * s,
    ])
}

pub(crate) fn write_lp_vec3(v: [f64; 3], out: &mut Vec<u8>) {
    let clean = |x: f64| {
        if x.is_nan() {
            0.0
        } else {
            x.clamp(-1.7179869183e10, 1.7179869183e10)
        }
    };
    let [x, y, z] = v.map(clean);
    let max = x.abs().max(y.abs()).max(z.abs());
    if max < 3.051_944_088_384_301e-5 {
        out.put_u8(0);
        return;
    }
    let scale = max.ceil() as u64;
    let big = scale & 3 != scale;
    let markers = if big { (scale & 3) | 4 } else { scale };
    let pack = |v: f64| ((v / scale as f64 * 0.5 + 0.5) * 32766.0).round() as u64;
    let packed = markers | (pack(x) << 3) | (pack(y) << 18) | (pack(z) << 33);
    out.put_u8(packed as u8);
    out.put_u8((packed >> 8) as u8);
    out.put_i32((packed >> 16) as u32 as i32);
    if big {
        out.put_var_int((scale >> 2) as i32);
    }
}

fn put_short_velocity(v: [f64; 3], out: &mut Vec<u8>) {
    for c in v {
        out.put_i16((c.clamp(-3.9, 3.9) * 8000.0) as i16);
    }
}

/// Degrees as a protocol angle (1/256 of a turn).
fn angle(degrees: f32) -> u8 {
    (degrees * 256.0 / 360.0).floor() as i32 as u8
}

pub(crate) fn add_entity(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 4);
    let id = copy_var_int(&mut r, &mut out)?;
    let uuid: [u8; 16] = r.array()?;
    out.put_slice(&uuid);
    let kind = r.get_var_int()?;
    let client_kind = ctx.t.entity_types.get(kind)?;
    if Some(kind) == ctx.t.player_type && !ctx.s.players.contains(&uuid) {
        ctx.s.waiting.push(uuid);
    }
    out.put_var_int(client_kind);
    copy(&mut r, 24, &mut out)?;
    let velocity = read_lp_vec3(&mut r)?;
    let packed = VelocityFormat::of(ctx.client()) == VelocityFormat::Packed;
    if packed {
        write_lp_vec3(velocity, &mut out);
    }
    copy(&mut r, 3, &mut out)?;
    let data = r.get_var_int()?;
    let falling_block = ctx
        .t
        .server
        .registry("entity_type")
        .get(usize::try_from(kind).ok()?)
        .is_some_and(|n| n == "falling_block");
    out.put_var_int(if falling_block {
        ctx.t.block_state(data)
    } else {
        data
    });
    if !packed {
        put_short_velocity(velocity, &mut out);
    }
    ctx.s.entities.insert(id, kind);
    Some(out)
}

pub(crate) fn remove_entities(ctx: &mut Ctx<'_>, payload: &[u8]) -> Option<Vec<u8>> {
    let mut r = payload;
    let n = r.get_len()?;
    for _ in 0..n {
        let id = r.get_var_int()?;
        ctx.s.entities.remove(&id);
    }
    Some(payload.to_vec())
}

pub(crate) fn set_entity_motion(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if VelocityFormat::of(ctx.client()) == VelocityFormat::Packed {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(12);
    copy_var_int(&mut r, &mut out)?;
    put_short_velocity(read_lp_vec3(&mut r)?, &mut out);
    Some(out)
}

/// The position part of 26.3 relative moves: one delta or the sum of the steps.
fn read_delta(r: &mut &[u8]) -> Option<([i16; 3], bool)> {
    let properties = r.get_var_int()? as u32;
    let on_ground = properties & 1 != 0;
    let steps = properties >> 1;
    if steps == 0 {
        return Some(([r.get_i16()?, r.get_i16()?, r.get_i16()?], on_ground));
    }
    let mut sum = [0i32; 3];
    for _ in 0..steps {
        r.get_var_int()?;
        for s in &mut sum {
            *s += i32::from(r.get_i16()?);
        }
    }
    Some((
        sum.map(|v| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16),
        on_ground,
    ))
}

pub(crate) fn move_entity_pos(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if MoveFormat::of(ctx.client()) == MoveFormat::V26_3 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(12);
    copy_var_int(&mut r, &mut out)?;
    let (delta, on_ground) = read_delta(&mut r)?;
    delta.iter().for_each(|d| out.put_i16(*d));
    out.put_bool(on_ground);
    Some(out)
}

pub(crate) fn move_entity_pos_rot(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if MoveFormat::of(ctx.client()) == MoveFormat::V26_3 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(14);
    copy_var_int(&mut r, &mut out)?;
    let (delta, on_ground) = read_delta(&mut r)?;
    delta.iter().for_each(|d| out.put_i16(*d));
    copy(&mut r, 2, &mut out)?;
    out.put_bool(on_ground);
    Some(out)
}

pub(crate) fn move_entity_rot(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if MoveFormat::of(ctx.client()) == MoveFormat::V26_3 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(8);
    copy_var_int(&mut r, &mut out)?;
    let on_ground = r.get_bool()?;
    copy(&mut r, 2, &mut out)?;
    out.put_bool(on_ground);
    Some(out)
}

/// `entity_position_sync`: one absolute position (the last step of a path).
/// Velocity is not part of the 26.3 packet; older clients get zero. Before
/// 1.21.2 it is the old `teleport_entity`.
pub(crate) fn entity_position_sync(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V26_3 {
        return Some(r.to_vec());
    }
    let id = r.get_var_int()?;
    let pos = match r.get_var_int()? {
        0 => [r.get_f64()?, r.get_f64()?, r.get_f64()?],
        1 => {
            let n = r.get_len()?;
            let mut last = None;
            for _ in 0..n {
                last = Some([r.get_f64()?, r.get_f64()?, r.get_f64()?]);
                r.get_var_int()?;
            }
            last?
        }
        _ => return None,
    };
    let (yaw, pitch, on_ground) = (r.get_f32()?, r.get_f32()?, r.get_bool()?);
    let mut out = Vec::with_capacity(64);
    out.put_var_int(id);
    pos.iter().for_each(|c| out.put_f64(*c));
    if ctx.client() < V1_21_2 {
        out.put_u8(angle(yaw));
        out.put_u8(angle(pitch));
        out.put_bool(on_ground);
        ctx.send_client("teleport_entity", out);
        return None;
    }
    [0.0; 3].iter().for_each(|c| out.put_f64(*c));
    out.put_f32(yaw);
    out.put_f32(pitch);
    out.put_bool(on_ground);
    Some(out)
}

/// `teleport_entity` (1.21.2+ vehicle sync with relative flags). Before 1.21.2
/// the packet of that name is an absolute teleport, so only absolute ones go.
pub(crate) fn teleport_entity(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_2 {
        return Some(r.to_vec());
    }
    let id = r.get_var_int()?;
    let pos = [r.get_f64()?, r.get_f64()?, r.get_f64()?];
    r.bytes(24)?;
    let (yaw, pitch, flags, on_ground) = (r.get_f32()?, r.get_f32()?, r.get_i32()?, r.get_bool()?);
    if flags != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(32);
    out.put_var_int(id);
    pos.iter().for_each(|c| out.put_f64(*c));
    out.put_u8(angle(yaw));
    out.put_u8(angle(pitch));
    out.put_bool(on_ground);
    Some(out)
}

/// `animate`: 26.3 moved the swings to `swing_animation` and renumbered the rest.
pub(crate) fn animate(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V26_3 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(6);
    copy_var_int(&mut r, &mut out)?;
    out.put_u8(match r.get_u8()? {
        0 => 2,
        1 => 4,
        2 => 5,
        other => other,
    });
    Some(out)
}

/// `swing_animation` (26.3): an `animate` swing for older clients.
pub(crate) fn swing_animation(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(6);
    copy_var_int(&mut r, &mut out)?;
    let off_hand = r.get_var_int()? != 0;
    out.put_u8(if off_hand { 3 } else { 0 });
    ctx.send_client("animate", out);
    None
}

/// `update_attributes`: attributes the client lacks are dropped.
pub(crate) fn update_attributes(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let map = ctx.t.registry("attribute")?;
    let mut out = Vec::with_capacity(r.len());
    copy_var_int(&mut r, &mut out)?;
    let n = r.get_len()?;
    let mut body = Vec::with_capacity(r.len());
    let mut kept = 0;
    for _ in 0..n {
        let id = r.get_var_int()?;
        let start = r;
        r.bytes(8)?;
        let modifiers = r.get_len()?;
        for _ in 0..modifiers {
            r.get_str()?;
            r.bytes(9)?;
        }
        let Some(client) = map.get(id) else { continue };
        body.put_var_int(client);
        body.put_slice(start.get(..start.len() - r.len())?);
        kept += 1;
    }
    out.put_len(kept);
    out.put_slice(&body);
    Some(out)
}

pub(crate) fn update_mob_effect(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len());
    copy_var_int(&mut r, &mut out)?;
    out.put_var_int(ctx.t.registry("mob_effect")?.get(r.get_var_int()?)?);
    out.put_slice(r);
    Some(out)
}

pub(crate) fn damage_event(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len());
    copy_var_int(&mut r, &mut out)?;
    let kind = r.get_var_int()?;
    out.put_var_int(ctx.dynamic("damage_type").map_or(kind, |m| m.or(kind, 0)));
    out.put_slice(r);
    Some(out)
}

// ---------------------------------------------------------------- entity data

/// Reads one server value of serializer `name` and writes it for the client.
/// `None` for values that are not translated, which ends the entry list.
fn value(ctx: &mut Ctx<'_>, name: &str, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    match name {
        "byte" | "boolean" => copy(r, 1, out),
        "float" => copy(r, 4, out),
        "block_pos" => copy(r, 8, out),
        "rotations" | "vector3" => copy(r, 12, out),
        "quaternion" => copy(r, 16, out),
        "int"
        | "direction"
        | "optional_unsigned_int"
        | "pose"
        | "sniffer_state"
        | "armadillo_state"
        | "copper_golem_state"
        | "weathering_copper_state"
        | "humanoid_arm"
        | "dye_color" => copy_var_int(r, out).map(|_| ()),
        "long" => {
            out.put_var_long(r.get_var_long()?);
            Some(())
        }
        "string" => {
            out.put_str(r.get_str()?);
            Some(())
        }
        "component" => nbt::copy(r, out),
        "optional_component" => optional(r, out, nbt::copy),
        "optional_block_pos" => optional(r, out, |r, o| copy(r, 8, o)),
        "optional_living_entity_reference" => optional(r, out, |r, o| copy(r, 16, o)),
        "optional_global_pos" => optional(r, out, |r, o| {
            o.put_str(r.get_str()?);
            copy(r, 8, o)
        }),
        "block_state" | "optional_block_state" => {
            out.put_var_int(ctx.t.block_state(r.get_var_int()?));
            Some(())
        }
        "item_stack" => item::stack(ctx, r, out),
        "particle" => particle(ctx, r, out),
        "particles" => {
            let n = r.get_len()?;
            out.put_len(n);
            for _ in 0..n {
                particle(ctx, r, out)?;
            }
            Some(())
        }
        "villager_data" => {
            let kind = r.get_var_int()?;
            let profession = r.get_var_int()?;
            out.put_var_int(ctx.t.registry("villager_type")?.or(kind, 0));
            out.put_var_int(ctx.t.registry("villager_profession")?.or(profession, 0));
            copy_var_int(r, out).map(|_| ())
        }
        "painting_variant" => {
            let holder = r.get_var_int()?;
            if holder == 0 {
                return None;
            }
            let id = holder - 1;
            out.put_var_int(ctx.dynamic("painting_variant").map_or(id, |m| m.or(id, 0)) + 1);
            Some(())
        }
        n if n.ends_with("_variant") => {
            let id = r.get_var_int()?;
            out.put_var_int(ctx.dynamic(n).map_or(id, |m| m.or(id, 0)));
            Some(())
        }
        // Resolvable profiles and anything new: not translated.
        _ => None,
    }
}

fn optional(
    r: &mut &[u8],
    out: &mut Vec<u8>,
    f: impl FnOnce(&mut &[u8], &mut Vec<u8>) -> Option<()>,
) -> Option<()> {
    let present = r.get_bool()?;
    out.put_bool(present);
    if present { f(r, out) } else { Some(()) }
}

/// `set_entity_data`: the client's field and serializer IDs; fields it lacks
/// are dropped, and so are the entries after a value that is not translated.
pub(crate) fn set_entity_data(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let id = r.get_var_int()?;
    let kind = *ctx.s.entities.get(&id)?;
    let t = ctx.t;
    let entity = t
        .server
        .registry("entity_type")
        .get(usize::try_from(kind).ok()?)?;
    let fields = t
        .entity_data
        .fields
        .get(entity.strip_prefix("minecraft:").unwrap_or(entity))?;
    let mut out = Vec::with_capacity(r.len() + 4);
    out.put_var_int(id);
    let mut v = Vec::new();
    loop {
        let index = r.get_u8()?;
        if index == u8::MAX {
            break;
        }
        let serializer = r.get_var_int()?;
        let name = t
            .entity_data
            .serializer_names
            .get(usize::try_from(serializer).ok()?)?;
        v.clear();
        if value(ctx, name, &mut r, &mut v).is_none() {
            break;
        }
        let field = fields.get(usize::from(index)).copied().flatten();
        let client_serializer = t
            .entity_data
            .serializers
            .get(usize::try_from(serializer).ok()?)
            .copied()
            .flatten();
        if let (Some(field), Some(cs)) = (field, client_serializer) {
            out.put_u8(field);
            out.put_var_int(cs);
            out.put_slice(&v);
        }
    }
    out.put_u8(u8::MAX);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lp_vec3_round_trips() {
        for v in [
            [0.0, -0.0784, 0.0],
            [0.5, 0.25, -1.0],
            [10.0, -3.5, 2.0],
            [0.0; 3],
        ] {
            let mut out = Vec::new();
            write_lp_vec3(v, &mut out);
            let back = read_lp_vec3(&mut out.as_slice()).unwrap();
            for (a, b) in v.iter().zip(back) {
                assert!((a - b).abs() < 1e-3 * a.abs().max(1.0), "{v:?} -> {back:?}");
            }
        }
        assert_eq!(angle(90.0), 64);
        assert_eq!(angle(-90.0), 192);
    }
}
