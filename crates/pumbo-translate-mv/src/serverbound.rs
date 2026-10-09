//! Client packets to the server.

use crate::entity::write_lp_vec3;
use crate::version::{V1_21_2, V1_21_4, V1_21_5, V1_21_6, V26_1, V26_3};
use crate::wire::{Get, Put, copy, copy_var_int};
use crate::{Ctx, Handler, Phase, config};

/// The handler of a client packet, by its server name.
pub(crate) fn to_server(phase: Phase, name: &str) -> Option<Handler> {
    Some(match (phase, name) {
        (Phase::Configuration, "select_known_packs") => config::known_packs_to_server,
        (_, "client_information") => config::client_information,
        (Phase::Play, "accept_teleportation") => accept_teleportation,
        (Phase::Play, "move_player_pos") => move_pos,
        (Phase::Play, "move_player_pos_rot") => move_pos_rot,
        (Phase::Play, "move_player_rot") => move_rot,
        (Phase::Play, "move_player_status_only") => move_status,
        (Phase::Play, "move_vehicle") => move_vehicle,
        (Phase::Play, "player_input") => player_input,
        (Phase::Play, "player_command") => player_command,
        (Phase::Play, "punch") => punch,
        (Phase::Play, "player_action") => player_action,
        (Phase::Play, "interact") => interact,
        (Phase::Play, "use_item_on") => use_item_on,
        (Phase::Play, "sign_update") => sign_update,
        (Phase::Play, "set_creative_mode_slot") => creative_slot,
        (Phase::Play, "container_click") => container_click,
        (Phase::Play, "chat" | "chat_command_signed") => chat_checksum,
        (Phase::Play, "container_close" | "container_button_click") => window_first,
        (Phase::Play, "place_recipe") => place_recipe,
        (Phase::Play, "change_difficulty") => change_difficulty,
        _ => return None,
    })
}

/// `accept_teleportation`: 26.3 adds the position and rotation the client
/// arrived at; the target of that teleport is sent. Clients before 1.21.4 do
/// not send `player_loaded`, so it follows their first confirmation.
fn accept_teleportation(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let id = r.get_var_int()?;
    let mut out = Vec::with_capacity(32);
    out.put_var_int(id);
    if let Some(&(_, pos, rot)) = ctx.s.teleports.iter().find(|t| t.0 == id) {
        ctx.s.position = pos;
        ctx.s.rotation = rot;
    }
    ctx.s.teleports.retain(|t| t.0 != id);
    ctx.s.position.iter().for_each(|c| out.put_f64(*c));
    ctx.s.rotation.iter().for_each(|c| out.put_f32(*c));
    if ctx.client() < V1_21_4 && !ctx.s.loaded_sent {
        ctx.s.loaded_sent = true;
        ctx.send_server("accept_teleportation", out);
        ctx.send_server("player_loaded", Vec::new());
        return None;
    }
    Some(out)
}

/// The on-ground flag of 1.21 movement as the 1.21.2+ flags byte.
fn movement_flags(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    let flags = r.get_u8()?;
    out.put_u8(if ctx.client() < V1_21_2 {
        flags & 1
    } else {
        flags
    });
    Some(())
}

fn move_pos(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(25);
    for i in 0..3 {
        let c = r.get_f64()?;
        if let Some(p) = ctx.s.position.get_mut(i) {
            *p = c;
        }
        out.put_f64(c);
    }
    movement_flags(ctx, &mut r, &mut out)?;
    Some(out)
}

fn move_pos_rot(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(33);
    for i in 0..3 {
        let c = r.get_f64()?;
        if let Some(p) = ctx.s.position.get_mut(i) {
            *p = c;
        }
        out.put_f64(c);
    }
    for i in 0..2 {
        let a = r.get_f32()?;
        if let Some(p) = ctx.s.rotation.get_mut(i) {
            *p = a;
        }
        out.put_f32(a);
    }
    movement_flags(ctx, &mut r, &mut out)?;
    Some(out)
}

fn move_rot(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(9);
    for i in 0..2 {
        let a = r.get_f32()?;
        if let Some(p) = ctx.s.rotation.get_mut(i) {
            *p = a;
        }
        out.put_f32(a);
    }
    movement_flags(ctx, &mut r, &mut out)?;
    Some(out)
}

fn move_status(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(1);
    movement_flags(ctx, &mut r, &mut out)?;
    Some(out)
}

/// `move_vehicle`: on-ground flag since 1.21.4.
fn move_vehicle(ctx: &mut Ctx<'_>, r: &[u8]) -> Option<Vec<u8>> {
    let mut out = r.to_vec();
    if ctx.client() < V1_21_4 {
        out.put_bool(false);
    }
    Some(out)
}

/// `player_input`: before 1.21.2 the vehicle steering (two floats and a flags
/// byte: jump 1, sneak 2) instead of the key bitmask.
fn player_input(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_2 {
        return Some(r.to_vec());
    }
    let sideways = r.get_f32()?;
    let forward = r.get_f32()?;
    let flags = r.get_u8()?;
    let mut input = 0u8;
    if forward > 0.0 {
        input |= 1;
    } else if forward < 0.0 {
        input |= 2;
    }
    if sideways > 0.0 {
        input |= 4;
    } else if sideways < 0.0 {
        input |= 8;
    }
    if flags & 1 != 0 {
        input |= 16;
    }
    if flags & 2 != 0 {
        input |= 32;
    }
    Some(vec![input])
}

/// `player_command`: sneaking moved to `player_input` in 1.21.6 (actions
/// shifted by two). 1.21.2+ clients already send it there; 1.21 clients get
/// a `player_input` with the sneak key.
fn player_command(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_6 {
        return Some(r.to_vec());
    }
    let entity = r.get_var_int()?;
    let action = r.get_var_int()?;
    let jump = r.get_var_int()?;
    match action {
        0 | 1 => {
            if ctx.client() < V1_21_2 {
                ctx.send_server("player_input", vec![if action == 0 { 32 } else { 0 }]);
            }
            None
        }
        _ => {
            let mut out = Vec::with_capacity(6);
            out.put_var_int(entity);
            out.put_var_int(action - 2);
            out.put_var_int(jump);
            Some(out)
        }
    }
}

/// `swing` of older clients is the 26.3 `punch`, without fields.
fn punch(ctx: &mut Ctx<'_>, r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V26_3 {
        return Some(r.to_vec());
    }
    Some(Vec::new())
}

/// `player_action` (26.3→26.2, ViaBackwards `EntityPacketRewriter26_3`):
/// 26.3 inserted `change_destroy_direction` as action 1, so every older
/// action from 1 on moves up by one. Without it a finished dig (2) reaches
/// the server as an abort and the block comes back.
fn player_action(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V26_3 {
        return Some(r.to_vec());
    }
    let action = r.get_var_int()?;
    let mut out = Vec::with_capacity(r.len() + 1);
    out.put_var_int(if action >= 1 { action + 1 } else { action });
    out.put_slice(r);
    Some(out)
}

/// `interact` before 26.1: attacks become `attack`, interact-at the new
/// `interact` with the offset as `LpVec3`; the plain interact that follows
/// an interact-at is dropped.
fn interact(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V26_1 {
        return Some(r.to_vec());
    }
    let entity = r.get_var_int()?;
    match r.get_var_int()? {
        1 => {
            let mut out = Vec::with_capacity(5);
            out.put_var_int(entity);
            ctx.send_server("attack", out);
            None
        }
        2 => {
            let offset = [
                f64::from(r.get_f32()?),
                f64::from(r.get_f32()?),
                f64::from(r.get_f32()?),
            ];
            let hand = r.get_var_int()?;
            let sneaking = r.get_bool()?;
            let mut out = Vec::with_capacity(16);
            out.put_var_int(entity);
            out.put_var_int(hand);
            write_lp_vec3(offset, &mut out);
            out.put_bool(sneaking);
            Some(out)
        }
        _ => None,
    }
}

/// `use_item_on`: world border flag since 1.21.2.
fn use_item_on(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_2 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(r.len() + 1);
    copy_var_int(&mut r, &mut out)?;
    copy(&mut r, 8, &mut out)?;
    copy_var_int(&mut r, &mut out)?;
    copy(&mut r, 13, &mut out)?;
    out.put_bool(false);
    copy_var_int(&mut r, &mut out)?;
    Some(out)
}

/// `sign_update`: 26.3 sends the side after the lines as a text slot ID.
fn sign_update(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V26_3 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(r.len() + 1);
    copy(&mut r, 8, &mut out)?;
    let front = r.get_bool()?;
    for _ in 0..4 {
        out.put_str(r.get_str()?);
    }
    out.put_var_int(i32::from(front));
    Some(out)
}

/// A client stack with its item and component IDs as the server's.
/// `lengths`: components are length-prefixed (untrusted stacks, 1.21.5+).
/// Without lengths the components cannot be skipped, so they are dropped.
fn client_stack(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>, lengths: bool) -> Option<()> {
    let count = r.get_var_int()?;
    if count <= 0 {
        out.put_var_int(0);
        return Some(());
    }
    let item = ctx.t.items_back.get(r.get_var_int()?)?;
    out.put_var_int(count);
    out.put_var_int(item);
    if !lengths {
        out.put_var_int(0);
        out.put_var_int(0);
        return Some(());
    }
    let types = component_types_back(ctx);
    let added = r.get_len()?;
    let removed = r.get_len()?;
    let mut body = Vec::new();
    let mut kept = 0;
    for _ in 0..added {
        let kind = r.get_var_int()?;
        let len = r.get_len()?;
        let value = r.bytes(len)?;
        let Some(kind) = types(kind) else { continue };
        body.put_var_int(kind);
        body.put_len(len);
        body.put_slice(value);
        kept += 1;
    }
    let mut gone = Vec::new();
    for _ in 0..removed {
        if let Some(kind) = types(r.get_var_int()?) {
            gone.push(kind);
        }
    }
    out.put_len(kept);
    out.put_len(gone.len());
    out.put_slice(&body);
    gone.iter().for_each(|k| out.put_var_int(*k));
    Some(())
}

/// Client component type → server component type, by name.
fn component_types_back<'a>(ctx: &'a Ctx<'_>) -> impl Fn(i32) -> Option<i32> + 'a {
    move |id| {
        let name = ctx
            .t
            .client
            .registry("data_component_type")
            .get(usize::try_from(id).ok()?)?;
        let id = ctx
            .t
            .server
            .registry("data_component_type")
            .iter()
            .position(|n| n == name)?;
        i32::try_from(id).ok()
    }
}

fn creative_slot(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 4);
    copy(&mut r, 2, &mut out)?;
    // The server reads length-prefixed components; older clients' are dropped.
    client_stack(ctx, &mut r, &mut out, ctx.client() >= V1_21_5)?;
    Some(out)
}

/// A hashed stack (1.21.5+): IDs as the server's, hashes unchanged.
fn hashed_stack(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    if !r.get_bool()? {
        out.put_bool(false);
        return Some(());
    }
    let item = ctx.t.items_back.get(r.get_var_int()?)?;
    out.put_bool(true);
    out.put_var_int(item);
    copy_var_int(r, out)?;
    let types = component_types_back(ctx);
    let n = r.get_len()?;
    let mut body = Vec::new();
    let mut kept = 0;
    for _ in 0..n {
        let kind = r.get_var_int()?;
        let hash = r.get_i32()?;
        if let Some(kind) = types(kind) {
            body.put_var_int(kind);
            body.put_i32(hash);
            kept += 1;
        }
    }
    out.put_len(kept);
    out.put_slice(&body);
    let n = r.get_len()?;
    let mut gone = Vec::new();
    for _ in 0..n {
        if let Some(kind) = types(r.get_var_int()?) {
            gone.push(kind);
        }
    }
    out.put_len(gone.len());
    gone.iter().for_each(|k| out.put_var_int(*k));
    Some(())
}

/// `container_click`: hashed stacks since 1.21.5. Older clients send full
/// stacks, which are not converted: the server gets no predicted slots and
/// resynchronizes the container itself.
/// A container ID: a byte before 1.21.2, a VarInt since.
fn window_id(ctx: &Ctx<'_>, r: &mut &[u8], out: &mut Vec<u8>) -> Option<()> {
    if ctx.client() >= V1_21_2 {
        copy_var_int(r, out)?;
    } else {
        out.put_var_int(i32::from(r.get_u8()? as i8));
    }
    Some(())
}

fn window_first(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 2);
    window_id(ctx, &mut r, &mut out)?;
    out.put_slice(r);
    Some(out)
}

/// `place_recipe`: recipe display IDs since 1.21.2; 1.21 names recipes, which
/// the server no longer knows.
fn place_recipe(ctx: &mut Ctx<'_>, r: &[u8]) -> Option<Vec<u8>> {
    (ctx.client() >= V1_21_2).then(|| r.to_vec())
}

fn container_click(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(r.len() + 8);
    window_id(ctx, &mut r, &mut out)?;
    copy_var_int(&mut r, &mut out)?;
    copy(&mut r, 3, &mut out)?;
    copy_var_int(&mut r, &mut out)?;
    if ctx.client() < V1_21_5 {
        out.put_var_int(0);
        out.put_bool(false);
        return Some(out);
    }
    let n = r.get_len()?;
    out.put_len(n);
    for _ in 0..n {
        copy(&mut r, 2, &mut out)?;
        hashed_stack(ctx, &mut r, &mut out)?;
    }
    hashed_stack(ctx, &mut r, &mut out)?;
    Some(out)
}

/// `chat` and `chat_command_signed`: last-seen checksum since 1.21.5 (0 = none).
fn chat_checksum(ctx: &mut Ctx<'_>, r: &[u8]) -> Option<Vec<u8>> {
    let mut out = r.to_vec();
    if ctx.client() < V1_21_5 {
        out.put_u8(0);
    }
    Some(out)
}

/// `change_difficulty`: a byte before 1.21.6.
fn change_difficulty(ctx: &mut Ctx<'_>, mut r: &[u8]) -> Option<Vec<u8>> {
    if ctx.client() >= V1_21_6 {
        return Some(r.to_vec());
    }
    let mut out = Vec::with_capacity(2);
    out.put_var_int(i32::from(r.get_u8()?));
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::wire::Put;
    use crate::{Output, Tables, Translator, VersionData};

    fn version(protocol: i32) -> VersionData {
        let names = |n: &[&str]| n.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        VersionData {
            protocol,
            packets: [
                [vec![], names(&["finish_configuration"])],
                [vec![], names(&["player_action"])],
            ],
            ..VersionData::default()
        }
    }

    fn action(status: i32, sequence: i32) -> Vec<u8> {
        let mut p = Vec::new();
        p.put_var_int(status);
        p.put_slice(&[0, 0, 0, 0, 0, 0x40, 0, 0x05]); // block position
        p.put_u8(1); // face
        p.put_var_int(sequence);
        p
    }

    /// A 26.1 client's start (0), abort (1), finish (2), drops (3, 4), release (5), swap (6)
    /// and spear jab (7) as the 26.3 actions 0, 2..=8.
    #[test]
    fn player_actions_skip_change_destroy_direction() {
        let tables = Arc::new(Tables::new(version(775), version(777)).unwrap());
        let mut t = Translator::new(tables);
        let mut out = Output::default();
        t.to_server(0, &[], &mut out);
        out.clear();
        for (old, new) in [
            (0, 0),
            (1, 2),
            (2, 3),
            (3, 4),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 8),
        ] {
            t.to_server(0, &action(old, 42), &mut out);
            assert_eq!(out.to_server, [(0, action(new, 42))], "action {old}");
            out.clear();
        }
    }
}
