//! Login phase.

use uuid::Uuid;

use super::common::GameProfile;
use super::{Ctx, Packet};
use crate::PacketKind;
use crate::types::{DecodeError, EncodeError, Reader, WriteExt};

/// Byte arrays of the key exchange (an RSA-2048 key is 294 bytes as DER).
const MAX_KEY_BYTES: usize = 4096;
/// Login plugin message data.
pub const MAX_QUERY_DATA: usize = 1_048_576;
/// Login disconnect reason (JSON characters).
const MAX_DISCONNECT: usize = 262_144;

/// `hello` from the client (Login Start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginStart {
    pub name: String,
    pub uuid: Uuid,
}

impl Packet for LoginStart {
    const KIND: PacketKind = PacketKind::Hello;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            name: r.string(16)?,
            uuid: r.uuid()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.name, 16)?;
        out.put_uuid(&self.uuid);
        Ok(())
    }
}

/// `hello` from the server (Encryption Request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionRequest {
    pub server_id: String,
    pub public_key: Vec<u8>,
    pub verify_token: Vec<u8>,
    pub should_authenticate: bool,
}

impl Packet for EncryptionRequest {
    const KIND: PacketKind = PacketKind::Hello;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            server_id: r.string(20)?,
            public_key: r.byte_array(MAX_KEY_BYTES)?.to_vec(),
            verify_token: r.byte_array(MAX_KEY_BYTES)?.to_vec(),
            should_authenticate: r.bool()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.server_id, 20)?;
        out.put_byte_array(&self.public_key, MAX_KEY_BYTES)?;
        out.put_byte_array(&self.verify_token, MAX_KEY_BYTES)?;
        out.put_bool(self.should_authenticate);
        Ok(())
    }
}

/// `key` (Encryption Response).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionResponse {
    pub shared_secret: Vec<u8>,
    pub verify_token: Vec<u8>,
}

impl Packet for EncryptionResponse {
    const KIND: PacketKind = PacketKind::Key;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            shared_secret: r.byte_array(MAX_KEY_BYTES)?.to_vec(),
            verify_token: r.byte_array(MAX_KEY_BYTES)?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_byte_array(&self.shared_secret, MAX_KEY_BYTES)?;
        out.put_byte_array(&self.verify_token, MAX_KEY_BYTES)
    }
}

/// `login_compression`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginCompression {
    pub threshold: i32,
}

impl Packet for LoginCompression {
    const KIND: PacketKind = PacketKind::LoginCompression;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            threshold: r.varint()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.threshold);
        Ok(())
    }
}

/// `login_finished` (Login Success).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginFinished {
    pub profile: GameProfile,
    /// Only in 767 ("strict error handling"); ignored elsewhere.
    pub strict_error_handling: bool,
    /// From 776; `None` before.
    pub session_id: Option<Uuid>,
}

impl Packet for LoginFinished {
    const KIND: PacketKind = PacketKind::LoginFinished;
    fn decode(r: &mut Reader<'_>, ctx: &Ctx<'_>) -> Result<Self, DecodeError> {
        let profile = GameProfile::decode(r)?;
        let strict_error_handling = if ctx.features.login_finished_strict_flag {
            r.bool()?
        } else {
            false
        };
        let session_id = if ctx.features.login_finished_session_id {
            Some(r.uuid()?)
        } else {
            None
        };
        Ok(Self {
            profile,
            strict_error_handling,
            session_id,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, ctx: &Ctx<'_>) -> Result<(), EncodeError> {
        self.profile.encode(out)?;
        if ctx.features.login_finished_strict_flag {
            out.put_bool(self.strict_error_handling);
        }
        if ctx.features.login_finished_session_id {
            out.put_uuid(&self.session_id.unwrap_or(Uuid::nil()));
        }
        Ok(())
    }
}

/// `login_acknowledged`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginAcknowledged;

impl Packet for LoginAcknowledged {
    const KIND: PacketKind = PacketKind::LoginAcknowledged;
    fn decode(_: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self)
    }
    fn encode(&self, _: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        Ok(())
    }
}

/// `custom_query` (Login Plugin Request), used by Velocity forwarding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomQuery {
    pub message_id: i32,
    pub channel: String,
    pub data: Vec<u8>,
}

impl Packet for CustomQuery {
    const KIND: PacketKind = PacketKind::CustomQuery;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            message_id: r.varint()?,
            channel: r.identifier()?,
            data: r.rest_max(MAX_QUERY_DATA, "query data")?.to_vec(),
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.message_id);
        out.put_identifier(&self.channel)?;
        out.put_bytes(&self.data);
        Ok(())
    }
}

/// `custom_query_answer`; `data: None` means "not understood".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomQueryAnswer {
    pub message_id: i32,
    pub data: Option<Vec<u8>>,
}

impl Packet for CustomQueryAnswer {
    const KIND: PacketKind = PacketKind::CustomQueryAnswer;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            message_id: r.varint()?,
            data: r.option(|r| Ok(r.rest_max(MAX_QUERY_DATA, "query answer")?.to_vec()))?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.message_id);
        out.put_bool(self.data.is_some());
        if let Some(d) = &self.data {
            out.put_bytes(d);
        }
        Ok(())
    }
}

/// `login_disconnect`: the reason is a JSON text component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginDisconnect {
    pub reason_json: String,
}

impl Packet for LoginDisconnect {
    const KIND: PacketKind = PacketKind::LoginDisconnect;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            reason_json: r.string(MAX_DISCONNECT)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.reason_json, MAX_DISCONNECT)
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::Property;
    use super::super::test_support::round_trip;
    use super::*;

    #[test]
    fn round_trips() {
        round_trip(
            &LoginStart {
                name: "Notch".into(),
                uuid: Uuid::from_u128(7),
            },
            767,
        );
        round_trip(
            &EncryptionRequest {
                server_id: String::new(),
                public_key: vec![1; 294],
                verify_token: vec![2; 4],
                should_authenticate: true,
            },
            777,
        );
        round_trip(
            &EncryptionResponse {
                shared_secret: vec![3; 256],
                verify_token: vec![4; 256],
            },
            777,
        );
        round_trip(&LoginCompression { threshold: 256 }, 767);
        let profile = GameProfile {
            id: Uuid::from_u128(9),
            name: "jeb_".into(),
            properties: vec![Property {
                name: "textures".into(),
                value: "e30=".into(),
                signature: Some("c2ln".into()),
            }],
        };
        round_trip(
            &LoginFinished {
                profile: profile.clone(),
                strict_error_handling: true,
                session_id: None,
            },
            767,
        );
        round_trip(
            &LoginFinished {
                profile: profile.clone(),
                strict_error_handling: false,
                session_id: None,
            },
            775,
        );
        round_trip(
            &LoginFinished {
                profile,
                strict_error_handling: false,
                session_id: Some(Uuid::from_u128(5)),
            },
            776,
        );
        round_trip(&LoginAcknowledged, 767);
        round_trip(
            &CustomQuery {
                message_id: 1,
                channel: "velocity:player_info".into(),
                data: vec![4],
            },
            777,
        );
        round_trip(
            &CustomQueryAnswer {
                message_id: 1,
                data: None,
            },
            777,
        );
        round_trip(
            &CustomQueryAnswer {
                message_id: 2,
                data: Some(vec![9; 40]),
            },
            777,
        );
        round_trip(
            &LoginDisconnect {
                reason_json: r#"{"text":"bye"}"#.into(),
            },
            767,
        );
    }
}
