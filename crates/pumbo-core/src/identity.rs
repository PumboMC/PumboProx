//! Extension point: authenticating players in online mode
//! (Mojang's sessionserver in E3, other services later).

use std::net::IpAddr;

use crate::BoxFuture;
use crate::profile::GameProfile;

/// What to check after the key exchange.
#[derive(Debug, Clone)]
pub struct AuthRequest {
    pub username: String,
    /// Server hash in Mojang's notation (SHA-1 as a signed number, hex).
    pub server_hash: String,
    /// Client address, if the config asks to check it (`prevent-proxy-connections`).
    pub client_ip: Option<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    Authenticated(GameProfile),
    /// The service confirmed that the player is not logged in.
    Rejected,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("authentication service unavailable: {0}")]
    Unavailable(String),
    #[error("deadline exceeded")]
    Timeout,
}

pub trait Authenticator: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn authenticate(&self, req: AuthRequest) -> BoxFuture<'_, Result<AuthOutcome, AuthError>>;
    /// Whether a premium account with this name exists. `online-mode =
    /// "per-player"` uses it to pick online mode for the connection until a
    /// plugin decides in `on-pre-login` (§2.7).
    fn is_premium<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<bool, AuthError>>;
}
