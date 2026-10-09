//! Player profile shared by the modules.

use uuid::Uuid;

/// Profile property (e.g. `textures`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub value: String,
    pub signature: Option<String>,
}

/// Player profile after authentication and plugin patches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameProfile {
    pub id: Uuid,
    pub name: String,
    pub properties: Vec<Property>,
}

/// Player identity passed to the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardedPlayer {
    pub profile: GameProfile,
    /// Client address (the real one behind ViaProxy, from the PROXY protocol).
    pub address: std::net::IpAddr,
}
