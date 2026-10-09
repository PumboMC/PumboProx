//! Play phase: the packets forwarding needs (§2.3).

use pumbo_nbt::Tag;
use uuid::Uuid;

use super::common::empty_packet;
use super::{Ctx, Packet, Text};
use crate::PacketKind;
use crate::types::{DecodeError, EncodeError, MAX_STRING, Position, Reader, WriteExt};

empty_packet!(
    /// `start_configuration`: back to the configuration phase.
    StartConfiguration,
    StartConfiguration
);
empty_packet!(
    /// `configuration_acknowledged`.
    ConfigurationAcknowledged,
    ConfigurationAcknowledged
);
empty_packet!(
    /// `bundle_delimiter`: starts or ends a bundle of packets.
    BundleDelimiter,
    BundleDelimiter
);

/// Where the player died.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeathLocation {
    pub dimension: String,
    pub position: Position,
}

/// Current and previous game mode of play `login` and `respawn`: VarInts
/// from 777 (previous as ID + 1, 0 for none), a byte and a signed byte before.
pub(crate) fn read_game_modes(
    r: &mut Reader<'_>,
    f: crate::VersionFeatures,
) -> Result<(i32, Option<i32>), DecodeError> {
    Ok(if f.play_login_game_mode_varint {
        let gm = r.varint()?;
        let prev = r.varint()?;
        (gm, (prev != 0).then(|| prev.wrapping_sub(1)))
    } else {
        let gm = i32::from(r.u8()?);
        let prev = r.i8()?;
        (gm, (prev != -1).then_some(i32::from(prev)))
    })
}

pub(crate) fn put_game_modes(
    out: &mut Vec<u8>,
    f: crate::VersionFeatures,
    game_mode: i32,
    previous: Option<i32>,
) {
    if f.play_login_game_mode_varint {
        out.put_varint(game_mode);
        out.put_varint(previous.map_or(0, |g| g.wrapping_add(1)));
    } else {
        out.put_u8(game_mode as u8);
        out.put_i8(previous.map_or(-1, |g| g as i8));
    }
}

/// `login` (play).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub entity_id: i32,
    pub hardcore: bool,
    pub dimensions: Vec<String>,
    pub max_players: i32,
    pub view_distance: i32,
    pub simulation_distance: i32,
    pub reduced_debug_info: bool,
    pub respawn_screen: bool,
    pub limited_crafting: bool,
    pub dimension_type: i32,
    pub dimension: String,
    pub hashed_seed: i64,
    pub game_mode: i32,
    pub previous_game_mode: Option<i32>,
    pub debug: bool,
    pub flat: bool,
    pub death_location: Option<DeathLocation>,
    pub portal_cooldown: i32,
    /// From 768.
    pub sea_level: i32,
    /// From 776.
    pub online_mode: bool,
    pub enforces_secure_chat: bool,
}

impl Packet for Login {
    const KIND: PacketKind = PacketKind::Login;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let f = ctx.features;
        let entity_id = r.i32()?;
        let hardcore = r.bool()?;
        let n = r.count(4096, 1, "dimensions")?;
        let dimensions = (0..n).map(|_| r.identifier()).collect::<Result<_, _>>()?;
        let max_players = r.varint()?;
        let view_distance = r.varint()?;
        let simulation_distance = r.varint()?;
        let reduced_debug_info = r.bool()?;
        let respawn_screen = r.bool()?;
        let limited_crafting = r.bool()?;
        let dimension_type = r.varint()?;
        let dimension = r.identifier()?;
        let hashed_seed = r.i64()?;
        let (game_mode, previous_game_mode) = read_game_modes(r, f)?;
        let debug = r.bool()?;
        let flat = r.bool()?;
        let death_location = r.option(|r| {
            Ok(DeathLocation {
                dimension: r.identifier()?,
                position: r.position()?,
            })
        })?;
        let portal_cooldown = r.varint()?;
        let sea_level = if f.play_login_sea_level {
            r.varint()?
        } else {
            0
        };
        let online_mode = if f.play_login_online_mode {
            r.bool()?
        } else {
            false
        };
        let enforces_secure_chat = r.bool()?;
        Ok(Self {
            entity_id,
            hardcore,
            dimensions,
            max_players,
            view_distance,
            simulation_distance,
            reduced_debug_info,
            respawn_screen,
            limited_crafting,
            dimension_type,
            dimension,
            hashed_seed,
            game_mode,
            previous_game_mode,
            debug,
            flat,
            death_location,
            portal_cooldown,
            sea_level,
            online_mode,
            enforces_secure_chat,
        })
    }

    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        let f = ctx.features;
        out.put_i32(self.entity_id);
        out.put_bool(self.hardcore);
        out.put_len(self.dimensions.len(), 4096, "dimensions")?;
        for d in &self.dimensions {
            out.put_identifier(d)?;
        }
        out.put_varint(self.max_players);
        out.put_varint(self.view_distance);
        out.put_varint(self.simulation_distance);
        out.put_bool(self.reduced_debug_info);
        out.put_bool(self.respawn_screen);
        out.put_bool(self.limited_crafting);
        out.put_varint(self.dimension_type);
        out.put_identifier(&self.dimension)?;
        out.put_i64(self.hashed_seed);
        put_game_modes(out, f, self.game_mode, self.previous_game_mode);
        out.put_bool(self.debug);
        out.put_bool(self.flat);
        out.put_bool(self.death_location.is_some());
        if let Some(d) = &self.death_location {
            out.put_identifier(&d.dimension)?;
            out.put_position(d.position);
        }
        out.put_varint(self.portal_cooldown);
        if f.play_login_sea_level {
            out.put_varint(self.sea_level);
        }
        if f.play_login_online_mode {
            out.put_bool(self.online_mode);
        }
        out.put_bool(self.enforces_secure_chat);
        Ok(())
    }
}

/// One suggestion.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub text: String,
    pub tooltip: Option<Text>,
}

/// `command_suggestions` (Tab completion answer).
#[derive(Debug, Clone, PartialEq)]
pub struct CommandSuggestions {
    pub id: i32,
    pub start: i32,
    pub length: i32,
    pub matches: Vec<Suggestion>,
}

impl Packet for CommandSuggestions {
    const KIND: PacketKind = PacketKind::CommandSuggestions;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let id = r.varint()?;
        let start = r.varint()?;
        let length = r.varint()?;
        let n = r.count(1 << 16, 2, "suggestions")?;
        let mut matches = Vec::with_capacity(n);
        for _ in 0..n {
            matches.push(Suggestion {
                text: r.string(MAX_STRING)?,
                tooltip: r.option(|r| r.nbt_required(ctx.nbt))?,
            });
        }
        Ok(Self {
            id,
            start,
            length,
            matches,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.id);
        out.put_varint(self.start);
        out.put_varint(self.length);
        out.put_len(self.matches.len(), 1 << 16, "suggestions")?;
        for m in &self.matches {
            out.put_string(&m.text, MAX_STRING)?;
            out.put_bool(m.tooltip.is_some());
            if let Some(t) = &m.tooltip {
                out.put_nbt(Some(t))?;
            }
        }
        Ok(())
    }
}

/// `system_chat`.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemChat {
    pub content: Text,
    pub overlay: bool,
}

impl Packet for SystemChat {
    const KIND: PacketKind = PacketKind::SystemChat;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            content: r.nbt_required(ctx.nbt)?,
            overlay: r.bool()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_nbt(Some(&self.content))?;
        out.put_bool(self.overlay);
        Ok(())
    }
}

macro_rules! text_packet {
    ($(#[$doc:meta])* $name:ident, $kind:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            pub text: Text,
        }

        impl Packet for $name {
            const KIND: PacketKind = PacketKind::$kind;
            fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
                Ok(Self {
                    text: r.nbt_required(ctx.nbt)?,
                })
            }
            fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
                out.put_nbt(Some(&self.text))
            }
        }
    };
}

text_packet!(
    /// `set_title_text`.
    SetTitleText,
    SetTitleText
);
text_packet!(
    /// `set_subtitle_text`.
    SetSubtitleText,
    SetSubtitleText
);
text_packet!(
    /// `set_action_bar_text`.
    SetActionBarText,
    SetActionBarText
);

/// `set_titles_animation` (ticks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetTitlesAnimation {
    pub fade_in: i32,
    pub stay: i32,
    pub fade_out: i32,
}

impl Packet for SetTitlesAnimation {
    const KIND: PacketKind = PacketKind::SetTitlesAnimation;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            fade_in: r.i32()?,
            stay: r.i32()?,
            fade_out: r.i32()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_i32(self.fade_in);
        out.put_i32(self.stay);
        out.put_i32(self.fade_out);
        Ok(())
    }
}

/// `clear_titles`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearTitles {
    pub reset: bool,
}

impl Packet for ClearTitles {
    const KIND: PacketKind = PacketKind::ClearTitles;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self { reset: r.bool()? })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_bool(self.reset);
        Ok(())
    }
}

/// Boss bar operation.
#[derive(Debug, Clone, PartialEq)]
pub enum BossAction {
    Add {
        title: Text,
        progress: f32,
        color: i32,
        overlay: i32,
        flags: u8,
    },
    Remove,
    UpdateProgress(f32),
    UpdateTitle(Text),
    UpdateStyle {
        color: i32,
        overlay: i32,
    },
    UpdateFlags(u8),
}

/// `boss_event`.
#[derive(Debug, Clone, PartialEq)]
pub struct BossEvent {
    pub id: Uuid,
    pub action: BossAction,
}

impl Packet for BossEvent {
    const KIND: PacketKind = PacketKind::BossEvent;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let id = r.uuid()?;
        let action = match r.varint()? {
            0 => BossAction::Add {
                title: r.nbt_required(ctx.nbt)?,
                progress: r.f32()?,
                color: r.varint()?,
                overlay: r.varint()?,
                flags: r.u8()?,
            },
            1 => BossAction::Remove,
            2 => BossAction::UpdateProgress(r.f32()?),
            3 => BossAction::UpdateTitle(r.nbt_required(ctx.nbt)?),
            4 => BossAction::UpdateStyle {
                color: r.varint()?,
                overlay: r.varint()?,
            },
            5 => BossAction::UpdateFlags(r.u8()?),
            _ => return Err(DecodeError::Invalid("boss bar action")),
        };
        Ok(Self { id, action })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_uuid(&self.id);
        match &self.action {
            BossAction::Add {
                title,
                progress,
                color,
                overlay,
                flags,
            } => {
                out.put_varint(0);
                out.put_nbt(Some(title))?;
                out.put_f32(*progress);
                out.put_varint(*color);
                out.put_varint(*overlay);
                out.put_u8(*flags);
            }
            BossAction::Remove => out.put_varint(1),
            BossAction::UpdateProgress(p) => {
                out.put_varint(2);
                out.put_f32(*p);
            }
            BossAction::UpdateTitle(t) => {
                out.put_varint(3);
                out.put_nbt(Some(t))?;
            }
            BossAction::UpdateStyle { color, overlay } => {
                out.put_varint(4);
                out.put_varint(*color);
                out.put_varint(*overlay);
            }
            BossAction::UpdateFlags(f) => {
                out.put_varint(5);
                out.put_u8(*f);
            }
        }
        Ok(())
    }
}

/// `tab_list` (header and footer).
#[derive(Debug, Clone, PartialEq)]
pub struct TabList {
    pub header: Text,
    pub footer: Text,
}

impl Packet for TabList {
    const KIND: PacketKind = PacketKind::TabList;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            header: r.nbt_required(ctx.nbt)?,
            footer: r.nbt_required(ctx.nbt)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_nbt(Some(&self.header))?;
        out.put_nbt(Some(&self.footer))
    }
}

/// How scores of an objective are shown.
#[derive(Debug, Clone, PartialEq)]
pub enum NumberFormat {
    Blank,
    /// Style compound (text component styling fields).
    Styled(Tag),
    Fixed(Text),
}

/// Display part of `set_objective` (create and update).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectiveDisplay {
    pub title: Text,
    /// 0 integer, 1 hearts.
    pub render_type: i32,
    pub number_format: Option<NumberFormat>,
}

/// `set_objective`.
#[derive(Debug, Clone, PartialEq)]
pub struct SetObjective {
    pub name: String,
    /// 0 create, 1 remove, 2 update.
    pub method: i8,
    pub display: Option<ObjectiveDisplay>,
}

impl Packet for SetObjective {
    const KIND: PacketKind = PacketKind::SetObjective;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let name = r.string(MAX_STRING)?;
        let method = r.i8()?;
        let display = match method {
            0 | 2 => Some(ObjectiveDisplay {
                title: r.nbt_required(ctx.nbt)?,
                render_type: r.varint()?,
                number_format: r.option(|r| {
                    Ok(match r.varint()? {
                        0 => NumberFormat::Blank,
                        1 => NumberFormat::Styled(r.nbt_required(ctx.nbt)?),
                        2 => NumberFormat::Fixed(r.nbt_required(ctx.nbt)?),
                        _ => return Err(DecodeError::Invalid("number format")),
                    })
                })?,
            }),
            1 => None,
            _ => return Err(DecodeError::Invalid("objective method")),
        };
        Ok(Self {
            name,
            method,
            display,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.name, MAX_STRING)?;
        out.put_i8(self.method);
        match (self.method, &self.display) {
            (0 | 2, Some(d)) => {
                out.put_nbt(Some(&d.title))?;
                out.put_varint(d.render_type);
                out.put_bool(d.number_format.is_some());
                match &d.number_format {
                    None => {}
                    Some(NumberFormat::Blank) => out.put_varint(0),
                    Some(NumberFormat::Styled(s)) => {
                        out.put_varint(1);
                        out.put_nbt(Some(s))?;
                    }
                    Some(NumberFormat::Fixed(t)) => {
                        out.put_varint(2);
                        out.put_nbt(Some(t))?;
                    }
                }
            }
            (1, None) => {}
            _ => return Err(EncodeError::Invalid("objective method and display")),
        }
        Ok(())
    }
}

/// Name-tag visibility or collision rule: a string before 770, an enum after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamRule {
    Name(String),
    Id(i32),
}

/// Team parameters (create and update).
#[derive(Debug, Clone, PartialEq)]
pub struct TeamParameters {
    pub display_name: Text,
    pub friendly_flags: u8,
    pub name_tag_visibility: TeamRule,
    pub collision_rule: TeamRule,
    /// Formatting color ID; `None` only from 776 (before, 21 means reset).
    pub color: Option<i32>,
    pub prefix: Text,
    pub suffix: Text,
}

/// `set_player_team`.
#[derive(Debug, Clone, PartialEq)]
pub struct SetPlayerTeam {
    pub name: String,
    /// 0 create, 1 remove, 2 update, 3 add entities, 4 remove entities.
    pub method: i8,
    pub parameters: Option<TeamParameters>,
    pub entities: Vec<String>,
}

fn read_rule(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<TeamRule, DecodeError> {
    Ok(if ctx.features.team_rules_varint {
        TeamRule::Id(r.varint()?)
    } else {
        TeamRule::Name(r.string(40)?)
    })
}

fn read_color(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Option<i32>, DecodeError> {
    if ctx.features.team_color_optional {
        r.option(Reader::varint)
    } else {
        Ok(Some(r.varint()?))
    }
}

fn write_color(out: &mut Vec<u8>, color: Option<i32>, ctx: &Ctx<'_>) {
    if ctx.features.team_color_optional {
        out.put_bool(color.is_some());
        if let Some(c) = color {
            out.put_varint(c);
        }
    } else {
        out.put_varint(color.unwrap_or(21));
    }
}

fn write_rule(out: &mut Vec<u8>, rule: &TeamRule, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
    match (rule, ctx.features.team_rules_varint) {
        (TeamRule::Id(id), true) => out.put_varint(*id),
        (TeamRule::Name(s), false) => out.put_string(s, 40)?,
        _ => return Err(EncodeError::Invalid("team rule form for this version")),
    }
    Ok(())
}

impl TeamParameters {
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let display_name = r.nbt_required(ctx.nbt)?;
        if ctx.features.team_prefix_first {
            let prefix = r.nbt_required(ctx.nbt)?;
            let suffix = r.nbt_required(ctx.nbt)?;
            let name_tag_visibility = read_rule(r, ctx)?;
            let collision_rule = read_rule(r, ctx)?;
            let color = read_color(r, ctx)?;
            let friendly_flags = r.u8()?;
            Ok(Self {
                display_name,
                friendly_flags,
                name_tag_visibility,
                collision_rule,
                color,
                prefix,
                suffix,
            })
        } else {
            let friendly_flags = r.u8()?;
            let name_tag_visibility = read_rule(r, ctx)?;
            let collision_rule = read_rule(r, ctx)?;
            let color = read_color(r, ctx)?;
            let prefix = r.nbt_required(ctx.nbt)?;
            let suffix = r.nbt_required(ctx.nbt)?;
            Ok(Self {
                display_name,
                friendly_flags,
                name_tag_visibility,
                collision_rule,
                color,
                prefix,
                suffix,
            })
        }
    }

    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_nbt(Some(&self.display_name))?;
        if ctx.features.team_prefix_first {
            out.put_nbt(Some(&self.prefix))?;
            out.put_nbt(Some(&self.suffix))?;
            write_rule(out, &self.name_tag_visibility, ctx)?;
            write_rule(out, &self.collision_rule, ctx)?;
            write_color(out, self.color, ctx);
            out.put_u8(self.friendly_flags);
        } else {
            out.put_u8(self.friendly_flags);
            write_rule(out, &self.name_tag_visibility, ctx)?;
            write_rule(out, &self.collision_rule, ctx)?;
            write_color(out, self.color, ctx);
            out.put_nbt(Some(&self.prefix))?;
            out.put_nbt(Some(&self.suffix))?;
        }
        Ok(())
    }
}

impl Packet for SetPlayerTeam {
    const KIND: PacketKind = PacketKind::SetPlayerTeam;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let name = r.string(MAX_STRING)?;
        let method = r.i8()?;
        if !(0..=4).contains(&method) {
            return Err(DecodeError::Invalid("team method"));
        }
        let parameters = if method == 0 || method == 2 {
            Some(TeamParameters::decode(r, ctx)?)
        } else {
            None
        };
        let entities = if method == 0 || method == 3 || method == 4 {
            let n = r.count(1 << 16, 1, "team entities")?;
            (0..n)
                .map(|_| r.string(MAX_STRING))
                .collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        Ok(Self {
            name,
            method,
            parameters,
            entities,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.name, MAX_STRING)?;
        out.put_i8(self.method);
        match (self.method, &self.parameters) {
            (0 | 2, Some(p)) => p.encode(out, ctx)?,
            (1 | 3 | 4, None) => {}
            _ => return Err(EncodeError::Invalid("team method and parameters")),
        }
        if matches!(self.method, 0 | 3 | 4) {
            out.put_len(self.entities.len(), 1 << 16, "team entities")?;
            for e in &self.entities {
                out.put_string(e, MAX_STRING)?;
            }
        }
        Ok(())
    }
}

/// `set_display_objective`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetDisplayObjective {
    pub slot: i32,
    pub name: String,
}

impl Packet for SetDisplayObjective {
    const KIND: PacketKind = PacketKind::SetDisplayObjective;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            slot: r.varint()?,
            name: r.string(MAX_STRING)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.slot);
        out.put_string(&self.name, MAX_STRING)
    }
}

/// A dialog: registry entry or inline definition.
#[derive(Debug, Clone, PartialEq)]
pub enum DialogRef {
    Registry(i32),
    Inline(Text),
}

/// `show_dialog` in play (771+).
#[derive(Debug, Clone, PartialEq)]
pub struct ShowDialog {
    pub dialog: DialogRef,
}

impl Packet for ShowDialog {
    const KIND: PacketKind = PacketKind::ShowDialog;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let id = r.varint()?;
        let dialog = if id == 0 {
            DialogRef::Inline(r.nbt_required(ctx.nbt)?)
        } else {
            DialogRef::Registry(id.wrapping_sub(1))
        };
        Ok(Self { dialog })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        match &self.dialog {
            DialogRef::Inline(t) => {
                out.put_varint(0);
                out.put_nbt(Some(t))?;
            }
            DialogRef::Registry(id) => out.put_varint(id.wrapping_add(1)),
        }
        Ok(())
    }
}

/// `chat_command` (unsigned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCommand {
    pub command: String,
}

impl Packet for ChatCommand {
    const KIND: PacketKind = PacketKind::ChatCommand;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            command: r.string(MAX_STRING)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.command, MAX_STRING)
    }
}

/// Acknowledgement fields of signed chat (§2.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastSeen {
    pub offset: i32,
    /// Fixed BitSet of the 20 last seen messages.
    pub acknowledged: [u8; 3],
    /// From 770.
    pub checksum: u8,
}

impl LastSeen {
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let offset = r.varint()?;
        let mut acknowledged = [0u8; 3];
        acknowledged.copy_from_slice(r.fixed_bitset(20)?);
        let checksum = if ctx.features.chat_checksum {
            r.u8()?
        } else {
            0
        };
        Ok(Self {
            offset,
            acknowledged,
            checksum,
        })
    }

    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) {
        out.put_varint(self.offset);
        out.put_bytes(&self.acknowledged);
        if ctx.features.chat_checksum {
            out.put_u8(self.checksum);
        }
    }
}

/// One signed command argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgumentSignature {
    pub name: String,
    pub signature: Box<[u8; 256]>,
}

fn signature(r: &mut Reader<'_>) -> Result<Box<[u8; 256]>, DecodeError> {
    let mut s = Box::new([0u8; 256]);
    s.copy_from_slice(r.take(256)?);
    Ok(s)
}

/// `chat_command_signed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCommandSigned {
    pub command: String,
    pub timestamp: i64,
    pub salt: i64,
    pub arguments: Vec<ArgumentSignature>,
    pub last_seen: LastSeen,
}

impl Packet for ChatCommandSigned {
    const KIND: PacketKind = PacketKind::ChatCommandSigned;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let command = r.string(MAX_STRING)?;
        let timestamp = r.i64()?;
        let salt = r.i64()?;
        let n = r.count(8, 257, "argument signatures")?;
        let mut arguments = Vec::with_capacity(n);
        for _ in 0..n {
            arguments.push(ArgumentSignature {
                name: r.string(16)?,
                signature: signature(r)?,
            });
        }
        Ok(Self {
            command,
            timestamp,
            salt,
            arguments,
            last_seen: LastSeen::decode(r, ctx)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.command, MAX_STRING)?;
        out.put_i64(self.timestamp);
        out.put_i64(self.salt);
        out.put_len(self.arguments.len(), 8, "argument signatures")?;
        for a in &self.arguments {
            out.put_string(&a.name, 16)?;
            out.put_bytes(a.signature.as_slice());
        }
        self.last_seen.encode(out, ctx);
        Ok(())
    }
}

/// `chat` (a chat message from the client).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chat {
    pub message: String,
    pub timestamp: i64,
    pub salt: i64,
    pub signature: Option<Box<[u8; 256]>>,
    pub last_seen: LastSeen,
}

impl Packet for Chat {
    const KIND: PacketKind = PacketKind::Chat;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            message: r.string(256)?,
            timestamp: r.i64()?,
            salt: r.i64()?,
            signature: r.option(signature)?,
            last_seen: LastSeen::decode(r, ctx)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.message, 256)?;
        out.put_i64(self.timestamp);
        out.put_i64(self.salt);
        out.put_bool(self.signature.is_some());
        if let Some(s) = &self.signature {
            out.put_bytes(s.as_slice());
        }
        self.last_seen.encode(out, ctx);
        Ok(())
    }
}

/// `chat_ack` (Message Acknowledgment): what the proxy sends in place of a
/// cancelled message (§2.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatAck {
    pub offset: i32,
}

impl Packet for ChatAck {
    const KIND: PacketKind = PacketKind::ChatAck;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            offset: r.varint()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.offset);
        Ok(())
    }
}

/// `chat_session_update` (Player Session).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSessionUpdate {
    pub session_id: Uuid,
    pub expires_at: i64,
    pub public_key: Vec<u8>,
    pub key_signature: Vec<u8>,
}

impl Packet for ChatSessionUpdate {
    const KIND: PacketKind = PacketKind::ChatSessionUpdate;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            session_id: r.uuid()?,
            expires_at: r.i64()?,
            public_key: r.byte_array(512)?.to_vec(),
            key_signature: r.byte_array(4096)?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_uuid(&self.session_id);
        out.put_i64(self.expires_at);
        out.put_byte_array(&self.public_key, 512)?;
        out.put_byte_array(&self.key_signature, 4096)
    }
}

/// `command_suggestion` (Tab completion request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSuggestion {
    pub id: i32,
    pub text: String,
}

impl Packet for CommandSuggestion {
    const KIND: PacketKind = PacketKind::CommandSuggestion;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            id: r.varint()?,
            text: r.string(32_500)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.id);
        out.put_string(&self.text, 32_500)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::round_trip;
    use super::*;
    use pumbo_nbt::Compound;

    fn text(s: &str) -> Text {
        Tag::String(s.into())
    }

    #[test]
    fn login_in_every_layout() {
        for protocol in [767, 768, 775, 776, 777] {
            round_trip(
                &Login {
                    entity_id: 42,
                    hardcore: false,
                    dimensions: vec!["minecraft:overworld".into(), "minecraft:the_end".into()],
                    max_players: 20,
                    view_distance: 10,
                    simulation_distance: 8,
                    reduced_debug_info: false,
                    respawn_screen: true,
                    limited_crafting: false,
                    dimension_type: 0,
                    dimension: "minecraft:overworld".into(),
                    hashed_seed: -77,
                    game_mode: 1,
                    previous_game_mode: if protocol == 768 { None } else { Some(0) },
                    debug: false,
                    flat: true,
                    death_location: Some(DeathLocation {
                        dimension: "minecraft:overworld".into(),
                        position: Position { x: 1, y: -64, z: 3 },
                    }),
                    portal_cooldown: 0,
                    sea_level: if protocol >= 768 { 63 } else { 0 },
                    online_mode: protocol >= 776,
                    enforces_secure_chat: protocol == 777,
                },
                protocol,
            );
        }
    }

    #[test]
    fn round_trips() {
        round_trip(&StartConfiguration, 767);
        round_trip(&ConfigurationAcknowledged, 767);
        round_trip(&BundleDelimiter, 767);
        round_trip(
            &CommandSuggestions {
                id: 1,
                start: 5,
                length: 2,
                matches: vec![
                    Suggestion {
                        text: "lobby".into(),
                        tooltip: Some(text("Lobby")),
                    },
                    Suggestion {
                        text: "survival".into(),
                        tooltip: None,
                    },
                ],
            },
            777,
        );
        round_trip(
            &SystemChat {
                content: text("hi"),
                overlay: true,
            },
            777,
        );
        round_trip(&SetTitleText { text: text("t") }, 777);
        round_trip(&SetSubtitleText { text: text("s") }, 777);
        round_trip(&SetActionBarText { text: text("a") }, 777);
        round_trip(
            &SetTitlesAnimation {
                fade_in: 10,
                stay: 70,
                fade_out: 20,
            },
            777,
        );
        round_trip(&ClearTitles { reset: true }, 777);
        for action in [
            BossAction::Add {
                title: text("boss"),
                progress: 0.5,
                color: 2,
                overlay: 0,
                flags: 1,
            },
            BossAction::Remove,
            BossAction::UpdateProgress(1.0),
            BossAction::UpdateTitle(text("x")),
            BossAction::UpdateStyle {
                color: 1,
                overlay: 4,
            },
            BossAction::UpdateFlags(6),
        ] {
            round_trip(
                &BossEvent {
                    id: Uuid::from_u128(3),
                    action,
                },
                777,
            );
        }
        round_trip(
            &TabList {
                header: text("h"),
                footer: text("f"),
            },
            777,
        );
        for number_format in [
            None,
            Some(NumberFormat::Blank),
            Some(NumberFormat::Styled(Tag::Compound(Compound(vec![(
                "color".into(),
                Tag::String("red".into()),
            )])))),
            Some(NumberFormat::Fixed(text("-"))),
        ] {
            round_trip(
                &SetObjective {
                    name: "kills".into(),
                    method: 0,
                    display: Some(ObjectiveDisplay {
                        title: text("Kills"),
                        render_type: 0,
                        number_format,
                    }),
                },
                777,
            );
        }
        round_trip(
            &SetObjective {
                name: "kills".into(),
                method: 1,
                display: None,
            },
            777,
        );
        for protocol in [767, 770, 776, 777] {
            let rule = |s: &str, id| {
                if protocol >= 770 {
                    TeamRule::Id(id)
                } else {
                    TeamRule::Name(s.into())
                }
            };
            round_trip(
                &SetPlayerTeam {
                    name: "red".into(),
                    method: 0,
                    parameters: Some(TeamParameters {
                        display_name: text("Red"),
                        friendly_flags: 3,
                        name_tag_visibility: rule("always", 0),
                        collision_rule: rule("never", 1),
                        color: Some(12),
                        prefix: text("[R] "),
                        suffix: text(""),
                    }),
                    entities: vec!["Notch".into()],
                },
                protocol,
            );
            round_trip(
                &SetPlayerTeam {
                    name: "red".into(),
                    method: 4,
                    parameters: None,
                    entities: vec!["Notch".into()],
                },
                protocol,
            );
        }
        round_trip(
            &SetDisplayObjective {
                slot: 1,
                name: "kills".into(),
            },
            777,
        );
        round_trip(
            &ShowDialog {
                dialog: DialogRef::Registry(3),
            },
            771,
        );
        round_trip(
            &ShowDialog {
                dialog: DialogRef::Inline(Tag::Compound(Compound::new())),
            },
            771,
        );
        round_trip(
            &ChatCommand {
                command: "server lobby".into(),
            },
            777,
        );
        for protocol in [767, 770] {
            let last_seen = LastSeen {
                offset: 2,
                acknowledged: [1, 0, 0x0F],
                checksum: if protocol >= 770 { 9 } else { 0 },
            };
            round_trip(
                &ChatCommandSigned {
                    command: "msg Notch hi".into(),
                    timestamp: 1,
                    salt: 2,
                    arguments: vec![ArgumentSignature {
                        name: "message".into(),
                        signature: Box::new([5u8; 256]),
                    }],
                    last_seen: last_seen.clone(),
                },
                protocol,
            );
            round_trip(
                &Chat {
                    message: "hello".into(),
                    timestamp: 1,
                    salt: 2,
                    signature: None,
                    last_seen,
                },
                protocol,
            );
        }
        round_trip(&ChatAck { offset: 3 }, 777);
        round_trip(
            &ChatSessionUpdate {
                session_id: Uuid::from_u128(1),
                expires_at: 99,
                public_key: vec![1; 294],
                key_signature: vec![2; 512],
            },
            777,
        );
        round_trip(
            &CommandSuggestion {
                id: 4,
                text: "/server l".into(),
            },
            777,
        );
    }
}
