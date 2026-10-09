//! Packet layout features per protocol version (plan §1.1, §2.4).
//!
//! The codec asks for a feature instead of comparing version numbers, so a new
//! protocol whose layouts are already known needs no code: features are
//! defined by the first (and, if removed, last) protocol that has them, and a
//! newer protocol inherits everything still open-ended.

use crate::ProtocolVersion;

macro_rules! features {
    ($($(#[$doc:meta])* $field:ident: $since:expr, $until:expr;)*) => {
        /// Layout features of one protocol version.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        #[non_exhaustive]
        pub struct VersionFeatures {
            $($(#[$doc])* pub $field: bool,)*
        }

        impl VersionFeatures {
            /// Features of a protocol, from the table above.
            pub const fn for_protocol(v: ProtocolVersion) -> Self {
                Self {
                    $($field: in_range(v.0, $since, $until),)*
                }
            }

            /// Names and values, for diagnostics and the data report.
            pub fn list(&self) -> Vec<(&'static str, bool)> {
                vec![$((stringify!($field), self.$field),)*]
            }
        }
    };
}

const fn in_range(v: i32, since: Option<i32>, until: Option<i32>) -> bool {
    let after = match since {
        Some(s) => v >= s,
        None => true,
    };
    let before = match until {
        Some(u) => v <= u,
        None => true,
    };
    after && before
}

features! {
    /// `login_finished` carries the "strict error handling" flag (up to 767).
    login_finished_strict_flag: None, Some(767);
    /// `login_finished` ends with a session ID (from 776).
    login_finished_session_id: Some(776), None;
    /// `client_information` ends with the particle status (from 768).
    client_information_particles: Some(768), None;
    /// Play `login` has the sea level after the portal cooldown (from 768).
    play_login_sea_level: Some(768), None;
    /// Play `login` has the online-mode flag before `enforces_secure_chat` (from 776).
    play_login_online_mode: Some(776), None;
    /// Play `login` encodes game modes as VarInts, the previous one as id + 1
    /// with 0 for none (from 777; bytes before).
    play_login_game_mode_varint: Some(777), None;
    /// Signed chat and commands end with a checksum byte (from 770).
    chat_checksum: Some(770), None;
    /// Team name-tag visibility and collision rule are VarInt enums (from 770;
    /// strings before).
    team_rules_varint: Some(770), None;
    /// Team parameters order prefix and suffix right after the display name and
    /// the friendly flags last (from 776).
    team_prefix_first: Some(776), None;
    /// Team color is optional (a present flag before the VarInt; from 776).
    team_color_optional: Some(776), None;
    /// `custom_click_action` carries its NBT payload behind a VarInt size
    /// (from 771, the packet's first version).
    custom_click_action_sized: Some(771), None;
    /// Text styles may carry `shadow_color` (from 769).
    text_shadow_color: Some(769), None;
    /// Text components in NBT use snake_case event fields (`click_event`,
    /// `hover_event`) and the new event structure (from 770).
    text_nbt_snake_case: Some(770), None;

    // Virtual world packets (E6), confirmed with the vanilla recordings.
    /// `player_position` starts with the teleport ID, carries a velocity and
    /// int flags (from 768; before: ID last, byte flags, no velocity).
    player_position_velocity: Some(768), None;
    /// `accept_teleportation` echoes the position and rotation the client took
    /// (from 777; found against vanilla 26.3, as in Pumpkin's `SConfirmTeleport`).
    accept_teleport_position: Some(777), None;
    /// `set_time` ends with a "time of day ticks" flag (768 to 774).
    set_time_ticking_flag: Some(768), Some(774);
    /// `set_time` carries world clocks instead of the time of day (from 775).
    set_time_clocks: Some(775), None;
    /// `set_default_spawn_position` is a dimension, position, yaw and pitch
    /// (from 773; before: position and angle).
    spawn_position_dimension: Some(773), None;
    /// Chunk heightmaps are a list of (type, longs) instead of NBT (from 770).
    chunk_heightmaps_list: Some(770), None;
    /// Paletted containers in chunk sections have no data length (from 770).
    chunk_data_unsized: Some(770), None;
    /// Chunk sections count fluid blocks after the non-air blocks (from 775).
    chunk_section_fluid_count: Some(775), None;
    /// BitSets are little-endian bytes without trailing zeros instead of
    /// longs (from 777).
    bitset_bytes: Some(777), None;
    /// `player_info_update` has the list order action (from 768).
    player_info_list_order: Some(768), None;
    /// `player_info_update` has the show-hat action (from 769).
    player_info_hat: Some(769), None;
    /// `dimension_type` uses environment attributes and a `skybox` instead of
    /// `effects` (from 774; registry data, not a packet layout).
    dimension_type_attributes: Some(774), None;
    /// Biome effects carry `music_volume` (from 769; vanilla writes the
    /// default 1.0 back, found by the E6 data pack test).
    biome_music_volume: Some(769), None;
    /// `dimension_type` names its world clock (from 775).
    dimension_type_clock: Some(775), None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds() {
        let f = VersionFeatures::for_protocol(ProtocolVersion::V767);
        assert!(f.login_finished_strict_flag && !f.text_nbt_snake_case);
        let f = VersionFeatures::for_protocol(ProtocolVersion::V770);
        assert!(!f.login_finished_strict_flag && f.text_nbt_snake_case);
        // A future protocol inherits the open-ended features.
        let f = VersionFeatures::for_protocol(ProtocolVersion(900));
        assert_eq!(f, VersionFeatures::for_protocol(ProtocolVersion::V777));
    }
}
