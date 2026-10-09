//! The PumboBridge contract (pumbo-plugins `docs/pumbo-bridge-spec.md`): the
//! TCP protocol between PumboProx and the `pumbobridge` plugin on every
//! Pumpkin server, and the types of the `pumbo:bridge@1.0` service that
//! PumboProx plugins call.
//!
//! - [`wire`]: messages, frames with sequence numbers and HMAC tags, the
//!   handshake proofs. No I/O: both sides feed bytes in and take bytes out.
//! - [`api`]: arguments and results of the bridge methods. The proxy forwards
//!   a plugin's payload to the bridge as it is, so one type serves the service
//!   call and the wire command.
//! - `client` (feature `sdk`): the typed service client for proxy plugins.
//!
//! Land protection is not part of the bridge: the future PumboGuard sends its
//! rules through [`method::GUARD_RULES`] and gets [`wire::Event::Denied`];
//! nothing in 0.1 sends or handles either.
//!
//! License: MIT OR Apache-2.0.

pub mod api;
pub mod wire;

pub use {ciborium, uuid};

#[cfg(feature = "sdk")]
pub mod client;

/// Protocol version (major, minor) of the wire format.
pub const PROTO: [u16; 2] = [1, 0];
/// Service name in the PumboProx registry.
pub const SERVICE: &str = "pumbo:bridge";
pub const SERVICE_MAJOR: u16 = 1;
pub const SERVICE_MINOR: u16 = 0;
/// Host name of the status ping that pairs a session with a server
/// (`pumbo-bridge.<32 hex>`).
pub const PING_PREFIX: &str = "pumbo-bridge.";
/// Bus topics the proxy publishes (`server` + the event fields).
pub const TOPIC_EVENT: &str = "pumbo:bridge-event";
pub const TOPIC_STATUS: &str = "pumbo:bridge-status";

/// Method names (`cmd.method` on the wire, kebab-case service methods).
pub mod method {
    pub const TELEPORT: &str = "teleport";
    pub const SET_GAMEMODE: &str = "set-gamemode";
    pub const HEAL: &str = "heal";
    pub const EFFECT: &str = "effect";
    pub const FLY: &str = "fly";
    pub const INV_GET: &str = "inv-get";
    pub const INV_SET: &str = "inv-set";
    pub const INV_GIVE: &str = "inv-give";
    pub const INV_CLEAR: &str = "inv-clear";
    pub const SHOW_ITEMS: &str = "show-items";
    /// Computed by the proxy only; never from a plugin.
    pub const PERM_SET: &str = "perm-set";
    /// The proxy's permission table for PumboPerms' import; proxy only.
    pub const PERMS_EXPORT: &str = "perms-export";
    pub const Q_PLAYER: &str = "q-player";
    pub const Q_SERVER: &str = "q-server";
    pub const Q_SPAWN: &str = "q-spawn";
    pub const Q_ENTITIES: &str = "q-entities";
    /// Reserved for PumboGuard (rules evaluated by `pumbo-guard-core` inside
    /// the bridge); not in 0.1.
    pub const GUARD_RULES: &str = "guard-rules";
    /// Service only (answered by the proxy).
    pub const STATUS: &str = "status";
    pub const SEND_TO: &str = "send-to";

    /// Methods that change something on a server: only `trusted-plugins`.
    pub const COMMANDS: &[&str] = &[
        TELEPORT,
        SET_GAMEMODE,
        HEAL,
        EFFECT,
        FLY,
        INV_SET,
        INV_GIVE,
        INV_CLEAR,
        SHOW_ITEMS,
        SEND_TO,
    ];
    /// Methods any plugin with `uses` may call.
    pub const QUERIES: &[&str] = &[Q_PLAYER, Q_SERVER, Q_SPAWN, Q_ENTITIES, INV_GET, STATUS];
}

/// Error codes: `res.err` on the wire and `service-error::rejected` text for
/// plugins.
pub mod err {
    /// The player is not on that server.
    pub const NO_PLAYER: &str = "no-player";
    pub const BAD_ARGS: &str = "bad-args";
    pub const UNKNOWN_ID: &str = "unknown-id";
    /// The bridge (or this Pumpkin version) does not have the method.
    pub const UNSUPPORTED: &str = "unsupported";
    pub const BUSY: &str = "busy";
    /// A held teleport waited too long for the previous one.
    pub const EXPIRED: &str = "expired";
    /// A newer teleport for the same player replaced a held one.
    pub const SUPERSEDED: &str = "superseded";
    pub const FAILED: &str = "failed";
    /// Proxy side: no bridge session for that server.
    pub const NO_BRIDGE: &str = "no-bridge";
    pub const TIMEOUT: &str = "timeout";
    pub const DISCONNECTED: &str = "disconnected";
    pub const NOT_ALLOWED: &str = "not-allowed";
    /// Proxy side: the player is not online (no server to default to).
    pub const OFFLINE: &str = "offline";
}
