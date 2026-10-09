//! Packets the proxy understands (plan §2.3), named as in Mojang's
//! `packets.json` report.
//!
//! A [`PacketKind`] is a report name; the phase and direction come from the
//! context of a lookup, so `keep_alive` is one kind for both directions and
//! both phases. [`KNOWN_PACKETS`] lists every (phase, direction, kind) the
//! proxy decodes or sends. The list grows when the proxy starts to understand a
//! new packet, never when a protocol version is added.

use crate::{Direction, Phase};

macro_rules! packet_kinds {
    ($($kind:ident => $name:literal),* $(,)?) => {
        /// Packet name from Mojang's report (without the `minecraft:` prefix).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum PacketKind {
            $($kind,)*
        }

        impl PacketKind {
            /// Every kind, in declaration order.
            pub const ALL: &'static [PacketKind] = &[$(PacketKind::$kind,)*];

            /// Current report name.
            pub const fn name(self) -> &'static str {
                match self {
                    $(PacketKind::$kind => $name,)*
                }
            }

            /// Kind for a current report name (aliases are resolved by the
            /// data generator, not here).
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(PacketKind::$kind),)*
                    _ => None,
                }
            }
        }
    };
}

packet_kinds! {
    AcceptCodeOfConduct => "accept_code_of_conduct",
    AcceptTeleportation => "accept_teleportation",
    BossEvent => "boss_event",
    BundleDelimiter => "bundle_delimiter",
    Chat => "chat",
    ChatAck => "chat_ack",
    ChatCommand => "chat_command",
    ChatCommandSigned => "chat_command_signed",
    ChatSessionUpdate => "chat_session_update",
    ChunkBatchFinished => "chunk_batch_finished",
    ChunkBatchReceived => "chunk_batch_received",
    ChunkBatchStart => "chunk_batch_start",
    ClearDialog => "clear_dialog",
    ClearTitles => "clear_titles",
    ClientInformation => "client_information",
    ClientTickEnd => "client_tick_end",
    CodeOfConduct => "code_of_conduct",
    CommandSuggestion => "command_suggestion",
    CommandSuggestions => "command_suggestions",
    Commands => "commands",
    ConfigurationAcknowledged => "configuration_acknowledged",
    ContainerSetSlot => "container_set_slot",
    CookieRequest => "cookie_request",
    CookieResponse => "cookie_response",
    CustomClickAction => "custom_click_action",
    CustomPayload => "custom_payload",
    CustomQuery => "custom_query",
    CustomQueryAnswer => "custom_query_answer",
    CustomReportDetails => "custom_report_details",
    Disconnect => "disconnect",
    FinishConfiguration => "finish_configuration",
    ForgetLevelChunk => "forget_level_chunk",
    GameEvent => "game_event",
    Hello => "hello",
    Intention => "intention",
    KeepAlive => "keep_alive",
    Key => "key",
    LevelChunkWithLight => "level_chunk_with_light",
    LevelParticles => "level_particles",
    Login => "login",
    LoginAcknowledged => "login_acknowledged",
    LoginCompression => "login_compression",
    LoginDisconnect => "login_disconnect",
    LoginFinished => "login_finished",
    MapItemData => "map_item_data",
    MovePlayerPos => "move_player_pos",
    MovePlayerPosRot => "move_player_pos_rot",
    MovePlayerRot => "move_player_rot",
    MovePlayerStatusOnly => "move_player_status_only",
    Ping => "ping",
    PingRequest => "ping_request",
    PlayerAbilities => "player_abilities",
    PlayerInfoUpdate => "player_info_update",
    PlayerLoaded => "player_loaded",
    PlayerPosition => "player_position",
    Pong => "pong",
    PongResponse => "pong_response",
    PostEffects => "post_effects",
    RegistryData => "registry_data",
    ResetChat => "reset_chat",
    ResourcePack => "resource_pack",
    ResourcePackPop => "resource_pack_pop",
    ResourcePackPush => "resource_pack_push",
    Respawn => "respawn",
    SelectKnownPacks => "select_known_packs",
    ServerLinks => "server_links",
    SetActionBarText => "set_action_bar_text",
    SetChunkCacheCenter => "set_chunk_cache_center",
    SetDefaultSpawnPosition => "set_default_spawn_position",
    SetDisplayObjective => "set_display_objective",
    SetExperience => "set_experience",
    SetHeldSlot => "set_held_slot",
    SetObjective => "set_objective",
    SetPlayerInventory => "set_player_inventory",
    SetPlayerTeam => "set_player_team",
    SetSubtitleText => "set_subtitle_text",
    SetTime => "set_time",
    SetTitleText => "set_title_text",
    SetTitlesAnimation => "set_titles_animation",
    ShowDialog => "show_dialog",
    Sound => "sound",
    StartConfiguration => "start_configuration",
    StatusRequest => "status_request",
    StatusResponse => "status_response",
    StoreCookie => "store_cookie",
    SystemChat => "system_chat",
    TabList => "tab_list",
    Transfer => "transfer",
    UpdateEnabledFeatures => "update_enabled_features",
    UpdateTags => "update_tags",
}

use Direction::{Clientbound as C, Serverbound as S};
use PacketKind as K;
use Phase::{Configuration as Cfg, Handshake as Hs, Login as Lg, Play as Pl, Status as St};

/// Every (phase, direction, kind) the proxy understands (§2.3). Handshake,
/// status, login and configuration are complete; play has the forwarding set
/// plus what the virtual server (PumboAPI) needs.
pub const KNOWN_PACKETS: &[(Phase, Direction, PacketKind)] = &[
    (Hs, S, K::Intention),
    (St, S, K::StatusRequest),
    (St, S, K::PingRequest),
    (St, C, K::StatusResponse),
    (St, C, K::PongResponse),
    (Lg, S, K::Hello),
    (Lg, S, K::Key),
    (Lg, S, K::LoginAcknowledged),
    (Lg, S, K::CustomQueryAnswer),
    (Lg, S, K::CookieResponse),
    (Lg, C, K::Hello),
    (Lg, C, K::LoginCompression),
    (Lg, C, K::LoginFinished),
    (Lg, C, K::CustomQuery),
    (Lg, C, K::CookieRequest),
    (Lg, C, K::LoginDisconnect),
    (Cfg, S, K::ClientInformation),
    (Cfg, S, K::CustomPayload),
    (Cfg, S, K::SelectKnownPacks),
    (Cfg, S, K::FinishConfiguration),
    (Cfg, S, K::KeepAlive),
    (Cfg, S, K::Pong),
    (Cfg, S, K::ResourcePack),
    (Cfg, S, K::CookieResponse),
    (Cfg, S, K::CustomClickAction),
    (Cfg, S, K::AcceptCodeOfConduct),
    (Cfg, C, K::CookieRequest),
    (Cfg, C, K::FinishConfiguration),
    (Cfg, C, K::RegistryData),
    (Cfg, C, K::UpdateTags),
    (Cfg, C, K::UpdateEnabledFeatures),
    (Cfg, C, K::SelectKnownPacks),
    (Cfg, C, K::KeepAlive),
    (Cfg, C, K::Ping),
    (Cfg, C, K::Disconnect),
    (Cfg, C, K::CustomPayload),
    (Cfg, C, K::ResourcePackPush),
    (Cfg, C, K::ResourcePackPop),
    (Cfg, C, K::StoreCookie),
    (Cfg, C, K::Transfer),
    (Cfg, C, K::ResetChat),
    (Cfg, C, K::ServerLinks),
    (Cfg, C, K::CustomReportDetails),
    (Cfg, C, K::ShowDialog),
    (Cfg, C, K::ClearDialog),
    (Cfg, C, K::CodeOfConduct),
    (Cfg, C, K::PostEffects),
    (Pl, C, K::StartConfiguration),
    (Pl, C, K::Login),
    (Pl, C, K::KeepAlive),
    (Pl, C, K::Commands),
    (Pl, C, K::CommandSuggestions),
    (Pl, C, K::CustomPayload),
    (Pl, C, K::SystemChat),
    (Pl, C, K::SetTitleText),
    (Pl, C, K::SetSubtitleText),
    (Pl, C, K::SetTitlesAnimation),
    (Pl, C, K::ClearTitles),
    (Pl, C, K::SetActionBarText),
    (Pl, C, K::BossEvent),
    (Pl, C, K::TabList),
    (Pl, C, K::Disconnect),
    (Pl, C, K::Transfer),
    (Pl, C, K::StoreCookie),
    (Pl, C, K::BundleDelimiter),
    (Pl, C, K::ResourcePackPush),
    (Pl, C, K::ResourcePackPop),
    (Pl, C, K::SetObjective),
    (Pl, C, K::SetPlayerTeam),
    (Pl, C, K::SetDisplayObjective),
    (Pl, C, K::ShowDialog),
    (Pl, C, K::ClearDialog),
    (Pl, C, K::PlayerPosition),
    (Pl, C, K::GameEvent),
    (Pl, C, K::LevelChunkWithLight),
    (Pl, C, K::LevelParticles),
    (Pl, C, K::SetChunkCacheCenter),
    (Pl, C, K::ChunkBatchStart),
    (Pl, C, K::ChunkBatchFinished),
    (Pl, C, K::SetDefaultSpawnPosition),
    (Pl, C, K::MapItemData),
    (Pl, C, K::ContainerSetSlot),
    (Pl, C, K::SetPlayerInventory),
    (Pl, C, K::SetHeldSlot),
    (Pl, C, K::SetExperience),
    (Pl, C, K::Sound),
    (Pl, C, K::SetTime),
    (Pl, C, K::PlayerAbilities),
    (Pl, C, K::Respawn),
    (Pl, C, K::ForgetLevelChunk),
    (Pl, C, K::PlayerInfoUpdate),
    (Pl, C, K::Ping),
    (Pl, S, K::ConfigurationAcknowledged),
    (Pl, S, K::KeepAlive),
    (Pl, S, K::ChatCommand),
    (Pl, S, K::ChatCommandSigned),
    (Pl, S, K::Chat),
    (Pl, S, K::ChatAck),
    (Pl, S, K::ChatSessionUpdate),
    (Pl, S, K::CommandSuggestion),
    (Pl, S, K::CustomPayload),
    (Pl, S, K::ClientInformation),
    (Pl, S, K::ResourcePack),
    (Pl, S, K::AcceptTeleportation),
    (Pl, S, K::MovePlayerPos),
    (Pl, S, K::MovePlayerPosRot),
    (Pl, S, K::MovePlayerRot),
    (Pl, S, K::MovePlayerStatusOnly),
    (Pl, S, K::PlayerLoaded),
    (Pl, S, K::ClientTickEnd),
    (Pl, S, K::ChunkBatchReceived),
    (Pl, S, K::Pong),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for k in PacketKind::ALL {
            assert_eq!(PacketKind::from_name(k.name()), Some(*k));
        }
        assert_eq!(PacketKind::from_name("game_profile"), None);
    }

    #[test]
    fn known_list_has_no_duplicates_and_uses_every_kind() {
        let mut seen = std::collections::BTreeSet::new();
        for entry in KNOWN_PACKETS {
            assert!(seen.insert(*entry), "duplicate {entry:?}");
        }
        for k in PacketKind::ALL {
            assert!(
                KNOWN_PACKETS.iter().any(|(_, _, kind)| kind == k),
                "{k:?} is not used"
            );
        }
    }
}
