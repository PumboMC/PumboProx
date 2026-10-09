//! Handshake and status.

use super::{Ctx, Packet};
use crate::PacketKind;
use crate::types::{DecodeError, EncodeError, MAX_STRING, Reader, WriteExt};

/// `intention` (Handshake).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intention {
    pub protocol: i32,
    pub address: String,
    pub port: u16,
    /// 1 status, 2 login, 3 transfer.
    pub intent: i32,
}

impl Intention {
    pub const STATUS: i32 = 1;
    pub const LOGIN: i32 = 2;
    pub const TRANSFER: i32 = 3;
}

impl Packet for Intention {
    const KIND: PacketKind = PacketKind::Intention;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            protocol: r.varint()?,
            address: r.string(255)?,
            port: r.u16()?,
            intent: r.varint()?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_varint(self.protocol);
        out.put_string(&self.address, 255)?;
        out.put_u16(self.port);
        out.put_varint(self.intent);
        Ok(())
    }
}

/// `status_request`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRequest;

impl Packet for StatusRequest {
    const KIND: PacketKind = PacketKind::StatusRequest;
    fn decode(_: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self)
    }
    fn encode(&self, _: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        Ok(())
    }
}

/// `status_response`: the status JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusResponse {
    pub json: String,
}

impl Packet for StatusResponse {
    const KIND: PacketKind = PacketKind::StatusResponse;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            json: r.string(MAX_STRING)?,
        })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_string(&self.json, MAX_STRING)
    }
}

/// `ping_request` (status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingRequest {
    pub time: i64,
}

impl Packet for PingRequest {
    const KIND: PacketKind = PacketKind::PingRequest;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self { time: r.i64()? })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_i64(self.time);
        Ok(())
    }
}

/// `pong_response` (status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PongResponse {
    pub time: i64,
}

impl Packet for PongResponse {
    const KIND: PacketKind = PacketKind::PongResponse;
    fn decode(r: &mut Reader<'_>, _: &Ctx<'_>) -> Result<Self, DecodeError> {
        Ok(Self { time: r.i64()? })
    }
    fn encode(&self, out: &mut Vec<u8>, _: &Ctx<'_>) -> Result<(), EncodeError> {
        out.put_i64(self.time);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::round_trip;
    use super::*;

    #[test]
    fn round_trips() {
        round_trip(
            &Intention {
                protocol: 777,
                address: "play.example.org\0FML3\0".into(),
                port: 25565,
                intent: Intention::LOGIN,
            },
            777,
        );
        round_trip(&StatusRequest, 767);
        round_trip(
            &StatusResponse {
                json: r#"{"version":{"name":"26.3","protocol":777}}"#.into(),
            },
            777,
        );
        round_trip(&PingRequest { time: -5 }, 767);
        round_trip(&PongResponse { time: i64::MAX }, 767);
    }
}
