//! Packets the proxy decodes and encodes (plan §2.3), with per-version layouts
//! driven by [`VersionFeatures`] (§2.4).
//!
//! Each packet is a type implementing [`Packet`]. Handshake, status, login and
//! configuration are complete; play covers what forwarding needs and, in
//! `world`, what the virtual world (PumboAPI, E6) sends and reads.
//! Text components stay NBT trees ([`Text`]) or JSON strings here; `pumbo-text`
//! turns them into its model.

pub mod commands;
pub mod common;
pub mod configuration;
pub mod login;
pub mod play;
pub mod status;
pub mod world;

use pumbo_nbt::Limits as NbtLimits;

use crate::types::{DecodeError, EncodeError, Reader};
use crate::{Direction, PacketKind, Phase, ProtocolVersion, VersionFeatures, VersionModule};

/// A text component as network NBT (configuration and play phases).
pub type Text = pumbo_nbt::Tag;

/// What a packet needs to know about the connection.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub module: &'a dyn VersionModule,
    pub features: VersionFeatures,
    pub direction: Direction,
    /// NBT limits for this direction (stricter for data from clients).
    pub nbt: NbtLimits,
}

impl std::fmt::Debug for Ctx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("protocol", &self.module.protocol())
            .finish_non_exhaustive()
    }
}

impl<'a> Ctx<'a> {
    pub fn new(module: &'a dyn VersionModule, direction: Direction) -> Self {
        Self {
            module,
            features: module.features(),
            direction,
            nbt: match direction {
                Direction::Clientbound => NbtLimits::BACKEND,
                Direction::Serverbound => NbtLimits::CLIENT,
            },
        }
    }

    pub fn protocol(&self) -> ProtocolVersion {
        self.module.protocol()
    }
}

/// A packet type.
pub trait Packet: Sized {
    const KIND: PacketKind;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError>;
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError>;
}

/// Decodes a whole payload; leftover bytes are an error.
pub fn decode<P: Packet>(payload: &[u8], ctx: &Ctx<'_>) -> Result<P, DecodeError> {
    let mut r = Reader::new(payload);
    let p = P::decode(&mut r, ctx)?;
    r.finish()?;
    Ok(p)
}

/// Encodes a payload (without the packet ID).
pub fn encode<P: Packet>(packet: &P, ctx: &Ctx<'_>) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    packet.encode(&mut out, ctx)?;
    Ok(out)
}

/// Packet ID in this version, for sending.
pub fn id_of<P: Packet>(ctx: &Ctx<'_>, phase: Phase, direction: Direction) -> Option<i32> {
    ctx.module.packet_id(phase, direction, P::KIND)
}

macro_rules! any_packet {
    ($($variant:ident: $phase:ident $dir:ident => $ty:ty,)*) => {
        /// Any decodable packet, for tools, tests and fuzzing.
        #[derive(Debug, Clone, PartialEq)]
        #[allow(clippy::large_enum_variant)]
        pub enum AnyPacket {
            $($variant($ty),)*
        }

        /// Decodes a payload of a known kind. `Ok(None)` if the codec does not
        /// decode that packet (it passes through raw).
        pub fn decode_any(
            phase: Phase,
            direction: Direction,
            kind: PacketKind,
            payload: &[u8],
            ctx: &Ctx<'_>,
        ) -> Result<Option<AnyPacket>, DecodeError> {
            $(
                if phase == Phase::$phase
                    && direction == Direction::$dir
                    && kind == <$ty as Packet>::KIND
                {
                    return decode::<$ty>(payload, ctx).map(|p| Some(AnyPacket::$variant(p)));
                }
            )*
            Ok(None)
        }

        /// Encodes any packet (payload only).
        pub fn encode_any(packet: &AnyPacket, ctx: &Ctx<'_>) -> Result<Vec<u8>, EncodeError> {
            match packet {
                $(AnyPacket::$variant(p) => encode(p, ctx),)*
            }
        }

        /// Kinds that [`decode_any`] decodes, per phase and direction.
        pub const DECODED: &[(Phase, Direction, PacketKind)] = &[
            $((Phase::$phase, Direction::$dir, <$ty as Packet>::KIND),)*
        ];
    };
}

any_packet! {
    Intention: Handshake Serverbound => status::Intention,
    StatusRequest: Status Serverbound => status::StatusRequest,
    StatusResponse: Status Clientbound => status::StatusResponse,
    PingRequest: Status Serverbound => status::PingRequest,
    PongResponse: Status Clientbound => status::PongResponse,

    LoginStart: Login Serverbound => login::LoginStart,
    EncryptionRequest: Login Clientbound => login::EncryptionRequest,
    EncryptionResponse: Login Serverbound => login::EncryptionResponse,
    LoginCompression: Login Clientbound => login::LoginCompression,
    LoginFinished: Login Clientbound => login::LoginFinished,
    LoginAcknowledged: Login Serverbound => login::LoginAcknowledged,
    CustomQuery: Login Clientbound => login::CustomQuery,
    CustomQueryAnswer: Login Serverbound => login::CustomQueryAnswer,
    LoginDisconnect: Login Clientbound => login::LoginDisconnect,
    LoginCookieRequest: Login Clientbound => common::CookieRequest,
    LoginCookieResponse: Login Serverbound => common::CookieResponse,

    ConfigClientInformation: Configuration Serverbound => common::ClientInformation,
    ConfigCustomPayloadIn: Configuration Serverbound => common::ServerboundCustomPayload,
    ConfigKnownPacksIn: Configuration Serverbound => configuration::SelectKnownPacks,
    ConfigFinishAck: Configuration Serverbound => configuration::FinishConfiguration,
    ConfigKeepAliveIn: Configuration Serverbound => common::KeepAlive,
    ConfigPong: Configuration Serverbound => common::Pong,
    ConfigResourcePackResponse: Configuration Serverbound => common::ResourcePackResponse,
    ConfigCookieResponse: Configuration Serverbound => common::CookieResponse,
    ConfigCustomClickAction: Configuration Serverbound => common::CustomClickAction,
    ConfigAcceptCodeOfConduct: Configuration Serverbound => configuration::AcceptCodeOfConduct,
    ConfigCookieRequest: Configuration Clientbound => common::CookieRequest,
    ConfigFinish: Configuration Clientbound => configuration::FinishConfiguration,
    ConfigRegistryData: Configuration Clientbound => configuration::RegistryData,
    ConfigUpdateTags: Configuration Clientbound => configuration::UpdateTags,
    ConfigEnabledFeatures: Configuration Clientbound => configuration::UpdateEnabledFeatures,
    ConfigKnownPacksOut: Configuration Clientbound => configuration::SelectKnownPacks,
    ConfigKeepAliveOut: Configuration Clientbound => common::KeepAlive,
    ConfigPing: Configuration Clientbound => common::Ping,
    ConfigDisconnect: Configuration Clientbound => common::Disconnect,
    ConfigCustomPayloadOut: Configuration Clientbound => common::ClientboundCustomPayload,
    ConfigResourcePackPush: Configuration Clientbound => common::ResourcePackPush,
    ConfigResourcePackPop: Configuration Clientbound => common::ResourcePackPop,
    ConfigStoreCookie: Configuration Clientbound => common::StoreCookie,
    ConfigTransfer: Configuration Clientbound => common::Transfer,
    ConfigResetChat: Configuration Clientbound => configuration::ResetChat,
    ConfigServerLinks: Configuration Clientbound => common::ServerLinks,
    ConfigReportDetails: Configuration Clientbound => common::CustomReportDetails,
    ConfigShowDialog: Configuration Clientbound => configuration::ShowDialog,
    ConfigClearDialog: Configuration Clientbound => common::ClearDialog,
    ConfigCodeOfConduct: Configuration Clientbound => configuration::CodeOfConduct,
    ConfigPostEffects: Configuration Clientbound => configuration::PostEffects,

    PlayStartConfiguration: Play Clientbound => play::StartConfiguration,
    PlayLogin: Play Clientbound => play::Login,
    PlayKeepAliveOut: Play Clientbound => common::KeepAlive,
    PlayCommands: Play Clientbound => commands::Commands,
    PlayCommandSuggestions: Play Clientbound => play::CommandSuggestions,
    PlayCustomPayloadOut: Play Clientbound => common::ClientboundCustomPayload,
    PlaySystemChat: Play Clientbound => play::SystemChat,
    PlayTitle: Play Clientbound => play::SetTitleText,
    PlaySubtitle: Play Clientbound => play::SetSubtitleText,
    PlayTitlesAnimation: Play Clientbound => play::SetTitlesAnimation,
    PlayClearTitles: Play Clientbound => play::ClearTitles,
    PlayActionBar: Play Clientbound => play::SetActionBarText,
    PlayBossEvent: Play Clientbound => play::BossEvent,
    PlayTabList: Play Clientbound => play::TabList,
    PlayDisconnect: Play Clientbound => common::Disconnect,
    PlayTransfer: Play Clientbound => common::Transfer,
    PlayStoreCookie: Play Clientbound => common::StoreCookie,
    PlayBundleDelimiter: Play Clientbound => play::BundleDelimiter,
    PlayResourcePackPush: Play Clientbound => common::ResourcePackPush,
    PlayResourcePackPop: Play Clientbound => common::ResourcePackPop,
    PlaySetObjective: Play Clientbound => play::SetObjective,
    PlaySetPlayerTeam: Play Clientbound => play::SetPlayerTeam,
    PlaySetDisplayObjective: Play Clientbound => play::SetDisplayObjective,
    PlayShowDialog: Play Clientbound => play::ShowDialog,
    PlayClearDialog: Play Clientbound => common::ClearDialog,
    PlayPing: Play Clientbound => common::Ping,
    PlayConfigurationAck: Play Serverbound => play::ConfigurationAcknowledged,
    PlayKeepAliveIn: Play Serverbound => common::KeepAlive,
    PlayChatCommand: Play Serverbound => play::ChatCommand,
    PlayChatCommandSigned: Play Serverbound => play::ChatCommandSigned,
    PlayChat: Play Serverbound => play::Chat,
    PlayChatAck: Play Serverbound => play::ChatAck,
    PlayChatSessionUpdate: Play Serverbound => play::ChatSessionUpdate,
    PlayCommandSuggestion: Play Serverbound => play::CommandSuggestion,
    PlayCustomPayloadIn: Play Serverbound => common::ServerboundCustomPayload,
    PlayClientInformation: Play Serverbound => common::ClientInformation,
    PlayResourcePackResponse: Play Serverbound => common::ResourcePackResponse,
    PlayPong: Play Serverbound => common::Pong,

    PlayAbilities: Play Clientbound => world::PlayerAbilities,
    PlaySpawnPosition: Play Clientbound => world::SetDefaultSpawnPosition,
    PlayGameEvent: Play Clientbound => world::GameEvent,
    PlayChunkCacheCenter: Play Clientbound => world::SetChunkCacheCenter,
    PlayChunkBatchStart: Play Clientbound => world::ChunkBatchStart,
    PlayChunkBatchFinished: Play Clientbound => world::ChunkBatchFinished,
    PlayChunk: Play Clientbound => world::LevelChunkWithLight,
    PlayForgetChunk: Play Clientbound => world::ForgetLevelChunk,
    PlayPosition: Play Clientbound => world::PlayerPosition,
    PlaySetTime: Play Clientbound => world::SetTime,
    PlayMapItemData: Play Clientbound => world::MapItemData,
    PlayContainerSetSlot: Play Clientbound => world::ContainerSetSlot,
    PlayHeldSlot: Play Clientbound => world::SetHeldSlot,
    PlayExperience: Play Clientbound => world::SetExperience,
    PlaySound: Play Clientbound => world::Sound,
    PlayRespawn: Play Clientbound => world::Respawn,
    PlayInfoUpdate: Play Clientbound => world::PlayerInfoUpdate,
    PlayAcceptTeleportation: Play Serverbound => world::AcceptTeleportation,
    PlayMovePos: Play Serverbound => world::MovePlayerPos,
    PlayMovePosRot: Play Serverbound => world::MovePlayerPosRot,
    PlayMoveRot: Play Serverbound => world::MovePlayerRot,
    PlayMoveStatus: Play Serverbound => world::MovePlayerStatusOnly,
    PlayLoaded: Play Serverbound => world::PlayerLoaded,
    PlayTickEnd: Play Serverbound => world::ClientTickEnd,
    PlayChunkBatchReceived: Play Serverbound => world::ChunkBatchReceived,
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A version module for tests: no ID tables, features of `protocol`.
    #[derive(Debug)]
    pub struct TestVersion(pub i32, pub Vec<String>);

    impl VersionModule for TestVersion {
        fn protocol(&self) -> ProtocolVersion {
            ProtocolVersion(self.0)
        }
        fn release_names(&self) -> &[String] {
            &[]
        }
        fn packet_kind(&self, _: Phase, _: Direction, _: i32) -> Option<PacketKind> {
            None
        }
        fn packet_id(&self, _: Phase, _: Direction, _: PacketKind) -> Option<i32> {
            None
        }
        fn command_argument_type(&self, id: i32) -> Option<&str> {
            self.1.get(usize::try_from(id).ok()?).map(String::as_str)
        }
    }

    /// Encode, decode, encode again: both encodings must match and the decoded
    /// value must equal the original.
    pub fn round_trip<P: Packet + PartialEq + std::fmt::Debug>(p: &P, protocol: i32) {
        for dir in [Direction::Clientbound, Direction::Serverbound] {
            let v = TestVersion(protocol, Vec::new());
            let ctx = Ctx::new(&v, dir);
            let bytes = encode(p, &ctx).unwrap();
            let back: P = decode(&bytes, &ctx).unwrap();
            assert_eq!(&back, p, "protocol {protocol}");
            assert_eq!(encode(&back, &ctx).unwrap(), bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KNOWN_PACKETS;

    #[test]
    fn decoded_kinds_are_known_and_cover_the_early_phases() {
        for entry in DECODED {
            assert!(
                KNOWN_PACKETS.contains(entry),
                "{entry:?} not in KNOWN_PACKETS"
            );
        }
        for entry in KNOWN_PACKETS {
            if entry.0 != Phase::Play {
                assert!(DECODED.contains(entry), "{entry:?} has no codec");
            }
        }
    }
}
