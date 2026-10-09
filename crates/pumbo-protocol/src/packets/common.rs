//! Packets and structures shared by several phases.

use pumbo_nbt::Tag;
use uuid::Uuid;

use super::{Ctx, Packet, Text};
use crate::PacketKind;
use crate::types::{DecodeError, EncodeError, MAX_STRING, Reader, WriteExt};

/// Plugin message data from a server.
pub const MAX_PAYLOAD_FROM_SERVER: usize = 1_048_576;
/// Plugin message data from a client.
pub const MAX_PAYLOAD_FROM_CLIENT: usize = 32_767;
/// Cookie payload.
pub const MAX_COOKIE: usize = 5120;
/// Profile properties per player.
const MAX_PROPERTIES: usize = 16;
/// `custom_click_action` payload bytes.
const MAX_CLICK_PAYLOAD: usize = 65_536;

/// Profile property (e.g. `textures`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub value: String,
    pub signature: Option<String>,
}

/// Game profile as sent in `login_finished`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameProfile {
    pub id: Uuid,
    pub name: String,
    pub properties: Vec<Property>,
}

impl GameProfile {
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let id = r.uuid()?;
        let name = r.string(16)?;
        let properties = Self::decode_properties(r)?;
        Ok(Self {
            id,
            name,
            properties,
        })
    }

    /// The property list (also in `player_info_update`).
    pub fn decode_properties(r: &mut Reader<'_>) -> Result<Vec<Property>, DecodeError> {
        let n = r.count(MAX_PROPERTIES, 3, "properties")?;
        let mut properties = Vec::with_capacity(n);
        for _ in 0..n {
            properties.push(Property {
                name: r.string(64)?,
                value: r.string(MAX_STRING)?,
                signature: r.option(|r| r.string(1024))?,
            });
        }
        Ok(properties)
    }

    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        out.put_uuid(&self.id);
        out.put_string(&self.name, 16)?;
        self.encode_properties(out)
    }

    pub fn encode_properties(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        out.put_len(self.properties.len(), MAX_PROPERTIES, "properties")?;
        for p in &self.properties {
            out.put_string(&p.name, 64)?;
            out.put_string(&p.value, MAX_STRING)?;
            out.put_bool(p.signature.is_some());
            if let Some(s) = &p.signature {
                out.put_string(s, 1024)?;
            }
        }
        Ok(())
    }
}

macro_rules! empty_packet {
    ($(#[$doc:meta])* $name:ident, $kind:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name;

        impl Packet for $name {
            const KIND: PacketKind = PacketKind::$kind;
            fn decode(_: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
                Ok(Self)
            }
            fn encode(&self, _: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
                Ok(())
            }
        }
    };
}
pub(crate) use empty_packet;

empty_packet!(
    /// `clear_dialog` (configuration and play, 771+).
    ClearDialog,
    ClearDialog
);

/// `keep_alive`, both directions, configuration and play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepAlive {
    pub id: i64,
}

impl Packet for KeepAlive {
    const KIND: PacketKind = PacketKind::KeepAlive;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self { id: r.i64()? })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_i64(self.id);
        Ok(())
    }
}

/// `ping` from the server (configuration and play).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    pub id: i32,
}

impl Packet for Ping {
    const KIND: PacketKind = PacketKind::Ping;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self { id: r.i32()? })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_i32(self.id);
        Ok(())
    }
}

/// `pong` from the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pong {
    pub id: i32,
}

impl Packet for Pong {
    const KIND: PacketKind = PacketKind::Pong;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self { id: r.i32()? })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_i32(self.id);
        Ok(())
    }
}

/// `custom_payload` from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientboundCustomPayload {
    pub channel: String,
    pub data: Vec<u8>,
}

impl Packet for ClientboundCustomPayload {
    const KIND: PacketKind = PacketKind::CustomPayload;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            channel: r.identifier()?,
            data: r
                .rest_max(MAX_PAYLOAD_FROM_SERVER, "plugin message")?
                .to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        if self.data.len() > MAX_PAYLOAD_FROM_SERVER {
            return Err(EncodeError::TooLong {
                what: "plugin message",
                got: self.data.len(),
                max: MAX_PAYLOAD_FROM_SERVER,
            });
        }
        out.put_identifier(&self.channel)?;
        out.put_bytes(&self.data);
        Ok(())
    }
}

/// `custom_payload` from the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerboundCustomPayload {
    pub channel: String,
    pub data: Vec<u8>,
}

impl Packet for ServerboundCustomPayload {
    const KIND: PacketKind = PacketKind::CustomPayload;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            channel: r.identifier()?,
            data: r
                .rest_max(MAX_PAYLOAD_FROM_CLIENT, "plugin message")?
                .to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        if self.data.len() > MAX_PAYLOAD_FROM_CLIENT {
            return Err(EncodeError::TooLong {
                what: "plugin message",
                got: self.data.len(),
                max: MAX_PAYLOAD_FROM_CLIENT,
            });
        }
        out.put_identifier(&self.channel)?;
        out.put_bytes(&self.data);
        Ok(())
    }
}

/// `disconnect` (configuration and play): reason as NBT text.
#[derive(Debug, Clone, PartialEq)]
pub struct Disconnect {
    pub reason: Text,
}

impl Packet for Disconnect {
    const KIND: PacketKind = PacketKind::Disconnect;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            reason: r.nbt_required(ctx.nbt)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_nbt(Some(&self.reason))
    }
}

/// `resource_pack_push`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourcePackPush {
    pub id: Uuid,
    pub url: String,
    pub hash: String,
    pub forced: bool,
    pub prompt: Option<Text>,
}

impl Packet for ResourcePackPush {
    const KIND: PacketKind = PacketKind::ResourcePackPush;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            id: r.uuid()?,
            url: r.string(MAX_STRING)?,
            hash: r.string(40)?,
            forced: r.bool()?,
            prompt: r.option(|r| r.nbt_required(ctx.nbt))?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_uuid(&self.id);
        out.put_string(&self.url, MAX_STRING)?;
        out.put_string(&self.hash, 40)?;
        out.put_bool(self.forced);
        out.put_bool(self.prompt.is_some());
        if let Some(p) = &self.prompt {
            out.put_nbt(Some(p))?;
        }
        Ok(())
    }
}

/// `resource_pack_pop`; `None` removes every pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePackPop {
    pub id: Option<Uuid>,
}

impl Packet for ResourcePackPop {
    const KIND: PacketKind = PacketKind::ResourcePackPop;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            id: r.option(Reader::uuid)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_bool(self.id.is_some());
        if let Some(id) = &self.id {
            out.put_uuid(id);
        }
        Ok(())
    }
}

/// `resource_pack` from the client (status of a pack).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePackResponse {
    pub id: Uuid,
    /// 0 loaded, 1 declined, 2 failed download, 3 accepted, 4 downloaded,
    /// 5 invalid URL, 6 failed reload, 7 discarded.
    pub result: i32,
}

impl ResourcePackResponse {
    pub const SUCCESSFULLY_LOADED: i32 = 0;
    pub const DECLINED: i32 = 1;
    pub const ACCEPTED: i32 = 3;
    pub const DOWNLOADED: i32 = 4;
}

impl Packet for ResourcePackResponse {
    const KIND: PacketKind = PacketKind::ResourcePack;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            id: r.uuid()?,
            result: r.varint()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_uuid(&self.id);
        out.put_varint(self.result);
        Ok(())
    }
}

/// `store_cookie`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCookie {
    pub key: String,
    pub payload: Vec<u8>,
}

impl Packet for StoreCookie {
    const KIND: PacketKind = PacketKind::StoreCookie;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            key: r.identifier()?,
            payload: r.byte_array(MAX_COOKIE)?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_identifier(&self.key)?;
        out.put_byte_array(&self.payload, MAX_COOKIE)
    }
}

/// `cookie_request` (login, configuration, play).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieRequest {
    pub key: String,
}

impl Packet for CookieRequest {
    const KIND: PacketKind = PacketKind::CookieRequest;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            key: r.identifier()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_identifier(&self.key)
    }
}

/// `cookie_response` (login, configuration, play).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieResponse {
    pub key: String,
    pub payload: Option<Vec<u8>>,
}

impl Packet for CookieResponse {
    const KIND: PacketKind = PacketKind::CookieResponse;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            key: r.identifier()?,
            payload: r.option(|r| Ok(r.byte_array(MAX_COOKIE)?.to_vec()))?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_identifier(&self.key)?;
        out.put_bool(self.payload.is_some());
        if let Some(p) = &self.payload {
            out.put_byte_array(p, MAX_COOKIE)?;
        }
        Ok(())
    }
}

/// `transfer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub host: String,
    pub port: i32,
}

impl Packet for Transfer {
    const KIND: PacketKind = PacketKind::Transfer;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            host: r.string(MAX_STRING)?,
            port: r.varint()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.host, MAX_STRING)?;
        out.put_varint(self.port);
        Ok(())
    }
}

/// `client_information` (configuration and play).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInformation {
    pub locale: String,
    pub view_distance: i8,
    pub chat_mode: i32,
    pub chat_colors: bool,
    pub skin_parts: u8,
    pub main_hand: i32,
    pub text_filtering: bool,
    pub server_listing: bool,
    /// From 768; 0 (all) before.
    pub particle_status: i32,
}

impl Packet for ClientInformation {
    const KIND: PacketKind = PacketKind::ClientInformation;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            locale: r.string(16)?,
            view_distance: r.i8()?,
            chat_mode: r.varint()?,
            chat_colors: r.bool()?,
            skin_parts: r.u8()?,
            main_hand: r.varint()?,
            text_filtering: r.bool()?,
            server_listing: r.bool()?,
            particle_status: if ctx.features.client_information_particles {
                r.varint()?
            } else {
                0
            },
        })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.locale, 16)?;
        out.put_i8(self.view_distance);
        out.put_varint(self.chat_mode);
        out.put_bool(self.chat_colors);
        out.put_u8(self.skin_parts);
        out.put_varint(self.main_hand);
        out.put_bool(self.text_filtering);
        out.put_bool(self.server_listing);
        if ctx.features.client_information_particles {
            out.put_varint(self.particle_status);
        }
        Ok(())
    }
}

/// One server link.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerLinkLabel {
    BuiltIn(i32),
    Custom(Text),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerLink {
    pub label: ServerLinkLabel,
    pub url: String,
}

/// `server_links`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerLinks {
    pub links: Vec<ServerLink>,
}

impl Packet for ServerLinks {
    const KIND: PacketKind = PacketKind::ServerLinks;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let n = r.count(1024, 3, "server links")?;
        let mut links = Vec::with_capacity(n);
        for _ in 0..n {
            let label = if r.bool()? {
                ServerLinkLabel::BuiltIn(r.varint()?)
            } else {
                ServerLinkLabel::Custom(r.nbt_required(ctx.nbt)?)
            };
            links.push(ServerLink {
                label,
                url: r.string(MAX_STRING)?,
            });
        }
        Ok(Self { links })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_len(self.links.len(), 1024, "server links")?;
        for l in &self.links {
            match &l.label {
                ServerLinkLabel::BuiltIn(id) => {
                    out.put_bool(true);
                    out.put_varint(*id);
                }
                ServerLinkLabel::Custom(t) => {
                    out.put_bool(false);
                    out.put_nbt(Some(t))?;
                }
            }
            out.put_string(&l.url, MAX_STRING)?;
        }
        Ok(())
    }
}

/// `custom_report_details`: (title, description) pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomReportDetails {
    pub details: Vec<(String, String)>,
}

impl Packet for CustomReportDetails {
    const KIND: PacketKind = PacketKind::CustomReportDetails;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        let n = r.count(32, 2, "report details")?;
        let mut details = Vec::with_capacity(n);
        for _ in 0..n {
            details.push((r.string(128)?, r.string(4096)?));
        }
        Ok(Self { details })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_len(self.details.len(), 32, "report details")?;
        for (title, description) in &self.details {
            out.put_string(title, 128)?;
            out.put_string(description, 4096)?;
        }
        Ok(())
    }
}

/// `custom_click_action` from the client (771+).
#[derive(Debug, Clone, PartialEq)]
pub struct CustomClickAction {
    pub id: String,
    pub payload: Option<Tag>,
}

impl Packet for CustomClickAction {
    const KIND: PacketKind = PacketKind::CustomClickAction;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let id = r.identifier()?;
        let payload = if ctx.features.custom_click_action_sized {
            let n = r.len(MAX_CLICK_PAYLOAD, "click payload")?;
            let mut inner = Reader::new(r.take(n)?);
            let tag = inner.nbt(ctx.nbt)?;
            inner.finish()?;
            tag
        } else {
            r.option(|r| r.nbt_required(ctx.nbt))?
        };
        Ok(Self { id, payload })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_identifier(&self.id)?;
        if ctx.features.custom_click_action_sized {
            let mut nbt = Vec::new();
            nbt.put_nbt(self.payload.as_ref())?;
            out.put_byte_array(&nbt, MAX_CLICK_PAYLOAD)?;
        } else {
            out.put_bool(self.payload.is_some());
            if let Some(p) = &self.payload {
                out.put_nbt(Some(p))?;
            }
        }
        Ok(())
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
    fn round_trips() {
        round_trip(&KeepAlive { id: -1 }, 767);
        round_trip(&Ping { id: 3 }, 767);
        round_trip(&Pong { id: 3 }, 767);
        round_trip(
            &ClientboundCustomPayload {
                channel: "minecraft:brand".into(),
                data: b"\x07vanilla".to_vec(),
            },
            777,
        );
        round_trip(
            &ServerboundCustomPayload {
                channel: "minecraft:brand".into(),
                data: b"\x06fabric".to_vec(),
            },
            777,
        );
        round_trip(
            &Disconnect {
                reason: text("bye"),
            },
            777,
        );
        round_trip(
            &ResourcePackPush {
                id: Uuid::from_u128(1),
                url: "https://example.org/p.zip".into(),
                hash: "0".repeat(40),
                forced: true,
                prompt: Some(text("please")),
            },
            777,
        );
        round_trip(&ResourcePackPop { id: None }, 767);
        round_trip(
            &ResourcePackPop {
                id: Some(Uuid::from_u128(2)),
            },
            767,
        );
        round_trip(
            &ResourcePackResponse {
                id: Uuid::from_u128(2),
                result: ResourcePackResponse::ACCEPTED,
            },
            767,
        );
        round_trip(
            &StoreCookie {
                key: "a:b".into(),
                payload: vec![1, 2],
            },
            767,
        );
        round_trip(&CookieRequest { key: "a:b".into() }, 767);
        round_trip(
            &CookieResponse {
                key: "a:b".into(),
                payload: Some(vec![3]),
            },
            767,
        );
        round_trip(
            &Transfer {
                host: "example.org".into(),
                port: 25565,
            },
            767,
        );
        for protocol in [767, 768] {
            round_trip(
                &ClientInformation {
                    locale: "pl_pl".into(),
                    view_distance: 10,
                    chat_mode: 0,
                    chat_colors: true,
                    skin_parts: 0x7F,
                    main_hand: 1,
                    text_filtering: false,
                    server_listing: true,
                    particle_status: if protocol >= 768 { 2 } else { 0 },
                },
                protocol,
            );
        }
        round_trip(
            &ServerLinks {
                links: vec![
                    ServerLink {
                        label: ServerLinkLabel::BuiltIn(6),
                        url: "https://example.org".into(),
                    },
                    ServerLink {
                        label: ServerLinkLabel::Custom(text("Wiki")),
                        url: "https://example.org/wiki".into(),
                    },
                ],
            },
            767,
        );
        round_trip(
            &CustomReportDetails {
                details: vec![("Server".into(), "PumboProx".into())],
            },
            767,
        );
        round_trip(&ClearDialog, 771);
        for protocol in [771, 777] {
            round_trip(
                &CustomClickAction {
                    id: "example:click".into(),
                    payload: Some(Tag::Compound(Compound(vec![("x".into(), Tag::Int(1))]))),
                },
                protocol,
            );
            round_trip(
                &CustomClickAction {
                    id: "example:click".into(),
                    payload: None,
                },
                protocol,
            );
        }
    }
}
