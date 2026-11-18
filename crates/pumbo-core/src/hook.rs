//! Extension point: session hooks. A module sees the packets of a player's
//! session that it asked for, decides what happens to each, and may answer
//! the server or move the player, without code in the session loop.

use std::borrow::Cow;
use std::net::SocketAddr;

use pumbo_protocol::{PacketKind, Phase, RawFrame, VersionModule};

use crate::profile::ForwardedPlayer;

/// What a hook decides about one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// On to the next hook, then to the other side.
    Forward,
    /// The frame goes no further.
    Drop,
    /// A broken or hostile peer: the session ends with this protocol error.
    End(String),
}

/// The frames a hook wants; the session never calls it for others.
#[derive(Debug, Clone, Default)]
pub struct HookInterests {
    /// Packets from the client, by kind, once the proxy lets them through.
    pub client: &'static [PacketKind],
    /// Packets from the backend, by kind, once the proxy lets them through.
    /// The backend's play `login` reaches hooks only as
    /// [`SessionHook::on_joined`] and its `disconnect` never does, so neither
    /// belongs here.
    pub backend: &'static [PacketKind],
    /// Plugin message channels (`custom_payload`) from the backend, by exact
    /// name, in every phase: in configuration and in play before the play
    /// `login` too, so a hook checks [`HookCx::phase`]. A hook that also wants
    /// the kind `custom_payload` gets each frame once.
    pub backend_channels: Cow<'static, [Cow<'static, str>]>,
}

/// What a hook asks the session to send or do; the session acts on it right
/// after the call, before the frame goes on.
#[derive(Debug, Default)]
pub struct HookOut {
    /// Plugin messages for the current backend, in order: channel, data.
    pub to_backend: Vec<(Cow<'static, str>, Vec<u8>)>,
    /// Move the player to this server; a refusal is only logged.
    pub connect: Option<String>,
}

/// What a hook knows about the session at a call.
pub struct HookCx<'a> {
    pub player: &'a ForwardedPlayer,
    /// The client's version: frames from both sides arrive in it.
    pub module: &'a dyn VersionModule,
    /// The server the player plays on, once its play `login` arrived.
    pub server: Option<&'a str>,
    /// Phase of the stream the frame came on.
    pub phase: Phase,
    pub out: &'a mut HookOut,
}

/// A module's part in one session.
pub trait SessionHook: Send {
    /// What the hook wants. The session reads it once, when it starts.
    fn interests(&self) -> HookInterests;

    fn on_client(&mut self, _cx: &mut HookCx<'_>, _f: &RawFrame) -> Verdict {
        Verdict::Forward
    }

    fn on_backend(&mut self, _cx: &mut HookCx<'_>, _f: &RawFrame) -> Verdict {
        Verdict::Forward
    }

    /// The backend's play `login` reached the client; the proxy records the
    /// new server right after this, and the output goes out once it has.
    fn on_joined(&mut self, _cx: &mut HookCx<'_>) {}
}

/// What a factory knows about a new session.
pub struct SessionInfo<'a> {
    pub player: &'a ForwardedPlayer,
    /// The client's address with its port.
    pub peer: SocketAddr,
}

/// Makes a module's hook for each new session.
pub trait SessionHookFactory: Send + Sync {
    /// This session's hook, or `None` when the module has nothing to do in it.
    fn for_session(&self, s: &SessionInfo<'_>) -> Option<Box<dyn SessionHook>>;
}
