//! Extension point: server engines. An adapter says what a server of its
//! engine needs from the proxy: defaults for its `servers` entry and repairs
//! of packets the engine gets wrong.

use pumbo_protocol::{PacketKind, Phase, ProtocolVersion, RawFrame};
use serde::Deserialize;

/// Cancelling a signed message leaves a gap in the signature chain. Vanilla
/// and Paper then reject every later signed message of that player until
/// they reconnect ("chat chain broken"); Pumpkin does not check (E4 test).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignedChatCancel {
    /// Drop it with a replacement `chat_ack`; the player stays.
    Drop,
    /// Disconnect the player with `messages.signed-chat-blocked`.
    Kick,
}

/// A server engine and, when known, its version: `pumpkin`, `0.2.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineRef {
    pub name: String,
    pub version: Option<String>,
}

/// What a server of an engine gets for the keys its `servers` entry leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendDefaults {
    pub signed_chat_cancel: SignedChatCancel,
    pub chat_session_forwarding: bool,
}

/// A repair a filter made, for the log and the metrics.
#[derive(Debug, PartialEq, Eq)]
pub struct Repair {
    /// Label `kind` of `pumboprox_fixed_packets_total`.
    pub kind: &'static str,
    /// What was wrong, as the log names it: "a malformed trail particle".
    pub what: &'static str,
    /// Where the fault is tracked: "Pumpkin issue #3065".
    pub cause: &'static str,
}

/// What a filter decides about one frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Filtered {
    /// The frame goes on unchanged.
    Forward,
    /// This payload goes on under the same packet id instead.
    Replace {
        payload: Vec<u8>,
        repair: &'static Repair,
    },
}

/// The repairs of one server connection: called for frames from the server,
/// only for the kinds in `interests`. The proxy calls it for the play frames
/// it forwards after the server's login, once its own handling is done: a
/// kind the proxy decodes itself (keep-alive, boss bars, resource packs,
/// commands, disconnect) reaches the proxy unrepaired, so a filter cannot
/// repair one the proxy fails to read.
pub trait PacketFilter: Send {
    /// The caller reads it once, when the connection is attached.
    fn interests(&self) -> &[PacketKind];
    /// `frame` is in the version the filter was made for; `phase` and `kind`
    /// are what the caller already knows about it.
    fn backend_to_client(&mut self, phase: Phase, kind: PacketKind, frame: &RawFrame) -> Filtered;
}

/// What servers of one engine need from the proxy.
pub trait EngineAdapter: Send + Sync + std::fmt::Debug {
    /// The engine's name in configs and in the registry: `pumpkin`.
    fn name(&self) -> &str;
    /// Values for the keys a server's entry leaves out.
    fn defaults(&self, engine: &EngineRef) -> BackendDefaults;
    /// The engine's repairs for a connection whose frames are in `wire`
    /// (the server's version; without a translator also the client's).
    /// `None`: this engine and version need none.
    fn quirks(&self, _engine: &EngineRef, _wire: ProtocolVersion) -> Option<Box<dyn PacketFilter>> {
        None
    }
}
