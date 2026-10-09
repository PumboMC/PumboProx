//! Extension point: passing the player's identity to the backend.

use crate::profile::ForwardedPlayer;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ForwardingError {
    #[error("backend asked for unsupported forwarding version {0}")]
    UnsupportedVersion(u8),
    #[error("{0}")]
    Other(String),
}

/// How the identity is passed on (Velocity modern, legacy, none, future ones).
/// The module gets two hooks in the login to the backend; both default to
/// "change nothing", so a new mode implements only what it uses.
pub trait ForwardingMode: Send + Sync + std::fmt::Debug {
    /// Module name from the config.
    fn name(&self) -> &str;

    /// Address in the `intention` packet to the backend (legacy appends data here).
    fn handshake_address(&self, original: &str, _player: &ForwardedPlayer) -> String {
        original.to_string()
    }

    /// Answer to the backend's `custom_query` in the login phase. `None` means
    /// "not my channel" (the proxy then answers "not understood").
    fn answer_login_query(
        &self,
        _channel: &str,
        _data: &[u8],
        _player: &ForwardedPlayer,
    ) -> Option<Result<Vec<u8>, ForwardingError>> {
        None
    }

    /// Whether the backend must ask for the identity during login. A backend
    /// that finishes login without asking is misconfigured (it would see the
    /// proxy's address and an offline UUID), so the proxy refuses it.
    fn expects_login_query(&self) -> bool {
        false
    }

    /// Whether the mode needs a local backend (otherwise the proxy refuses to start, §3.1).
    fn requires_local_backend(&self) -> bool {
        false
    }
}
