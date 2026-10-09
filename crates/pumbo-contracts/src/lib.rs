//! Contracts of the `pumbo:` namespace (plan §6.6.2): bus topics and services
//! shared by PumboProx plugins, with their versions and payload types.
//!
//! Payloads travel as CBOR maps with named fields. Within one major version
//! only new optional fields may be added (`#[serde(default)]` on the reader
//! side); anything else is a new major version.
//!
//! License: MIT OR Apache-2.0.

use serde::{Deserialize, Serialize};

/// A topic or service name with its version (`name@major.minor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contract {
    pub name: &'static str,
    pub major: u16,
    pub minor: u16,
    /// Plugin id allowed to publish the topic or provide the service; `None`
    /// means any plugin that declares it (e.g. a permission provider).
    pub owner: Option<&'static str>,
}

impl Contract {
    pub const fn new(
        name: &'static str,
        major: u16,
        minor: u16,
        owner: Option<&'static str>,
    ) -> Self {
        Contract {
            name,
            major,
            minor,
            owner,
        }
    }

    /// `name@major.minor` as used in manifests.
    pub fn versioned(&self) -> String {
        format!("{}@{}.{}", self.name, self.major, self.minor)
    }
}

pub const PLAYER_AUTHENTICATED: Contract =
    Contract::new("pumbo:player-authenticated", 1, 0, Some("pumbo-auth"));
pub const PLAYER_REGISTERED: Contract =
    Contract::new("pumbo:player-registered", 1, 0, Some("pumbo-auth"));
pub const PLAYER_PUNISHED: Contract =
    Contract::new("pumbo:player-punished", 1, 0, Some("pumbo-bans"));
pub const PUNISHMENT_REVOKED: Contract =
    Contract::new("pumbo:punishment-revoked", 1, 0, Some("pumbo-bans"));
pub const RANK_CHANGED: Contract = Contract::new("pumbo:rank-changed", 1, 0, Some("pumbo-perms"));
pub const SKIN_CHANGED: Contract = Contract::new("pumbo:skin-changed", 1, 0, Some("pumbo-skins"));
pub const ATTACK_STATE: Contract = Contract::new("pumbo:attack-state", 1, 0, Some("pumbo-filter"));
/// Events and session changes of PumboBridge (types in `pumbo-bridge-proto`:
/// `BusEvent`, `api::ServerStatus`); published by the host itself.
pub const BRIDGE_EVENT: Contract = Contract::new("pumbo:bridge-event", 1, 0, Some(HOST));
pub const BRIDGE_STATUS: Contract = Contract::new("pumbo:bridge-status", 1, 0, Some(HOST));

/// Owner id of contracts the host itself provides or publishes.
pub const HOST: &str = "proxy";

/// Every `pumbo:` topic. The host lets only the owner publish one.
pub const TOPICS: &[Contract] = &[
    PLAYER_AUTHENTICATED,
    PLAYER_REGISTERED,
    PLAYER_PUNISHED,
    PUNISHMENT_REVOKED,
    RANK_CHANGED,
    SKIN_CHANGED,
    ATTACK_STATE,
    BRIDGE_EVENT,
    BRIDGE_STATUS,
];

/// Permission provider service (plan §5.8.4); any plugin with
/// `permission-provider: true` may provide it.
pub const PERMISSIONS: Contract = Contract::new("pumbo:permissions", 1, 0, None);
pub const AUTH_ACCOUNTS: Contract = Contract::new("pumboauth:accounts", 1, 0, Some("pumbo-auth"));
pub const BANS_PUNISH: Contract = Contract::new("pumbobans:punish", 1, 0, Some("pumbo-bans"));
/// PumboBridge commands and queries (types in `pumbo-bridge-proto`); the
/// host provides it natively when the bridge module is on.
pub const BRIDGE: Contract = Contract::new("pumbo:bridge", 1, 0, Some(HOST));

/// Every `pumbo:` service.
pub const SERVICES: &[Contract] = &[PERMISSIONS, BRIDGE];

/// Looks up a `pumbo:` topic by name.
pub fn topic(name: &str) -> Option<&'static Contract> {
    TOPICS.iter().find(|t| t.name == name)
}

/// UUID as a 32-digit lowercase hex string without dashes.
pub type UuidHex = String;

/// `pumbo:player-authenticated@1.0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerAuthenticated {
    pub uuid: UuidHex,
    pub name: String,
    pub method: AuthMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMethod {
    Password,
    Premium,
    Session,
    TwoFactor,
}

/// `pumbo:player-registered@1.0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerRegistered {
    pub uuid: UuidHex,
    pub name: String,
    /// Unix time in seconds.
    pub at: u64,
}

/// `pumbo:player-punished@1.0` and `pumbo:punishment-revoked@1.0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Punishment {
    pub id: u64,
    pub kind: PunishmentKind,
    /// Player UUID or IP address.
    pub target: String,
    /// Unix time in seconds; `None` = permanent.
    #[serde(default)]
    pub until: Option<u64>,
    pub author: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PunishmentKind {
    Ban,
    IpBan,
    Mute,
    Warn,
    Kick,
}

/// Context as written in contracts: `global`, `group=<name>`, `server=<name>`.
pub type ContextString = String;

/// `pumbo:rank-changed@1.0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankChanged {
    pub uuid: UuidHex,
    pub context: ContextString,
    #[serde(default)]
    pub old: Option<String>,
    #[serde(default)]
    pub new: Option<String>,
}

/// `pumbo:skin-changed@1.0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkinChanged {
    pub uuid: UuidHex,
}

/// `pumbo:attack-state@1.0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttackState {
    pub active: bool,
    pub level: u8,
}

/// `pumbo:permissions@1.0` method `check-offline`: request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOffline {
    pub uuid: UuidHex,
    pub node: String,
    pub context: ContextString,
}

/// `pumbo:permissions@1.0` method `check-offline`: answer; `None` = no entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOfflineAnswer {
    #[serde(default)]
    pub value: Option<bool>,
}

/// `pumbo:permissions@1.0` method `file`, called by the host when the
/// provider starts: `permissions.yml` as a PumboPerms export (JSON, players
/// named only by nickname under `by-name`), for the provider to take over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsFile {
    /// Hash of `data` (FNV-1a 64, hex): the same file gives the same one.
    pub fingerprint: String,
    pub data: String,
}

/// `pumbo:permissions@1.0` method `export`: the provider's data for a server
/// behind the proxy (PumboBridge `perms-export`); answer: [`PermissionsExport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsExportRequest {
    pub server: String,
    /// Server groups of `server`, config order.
    #[serde(default)]
    pub groups: Vec<String>,
}

/// Answer of `export`: a PumboPerms export (JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsExport {
    pub data: String,
}

/// Method names of `pumbo:permissions@1.0`.
pub const METHOD_CHECK_OFFLINE: &str = "check-offline";
pub const METHOD_FILE: &str = "file";
pub const METHOD_EXPORT: &str = "export";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_have_owners_and_unique_names() {
        for (i, t) in TOPICS.iter().enumerate() {
            assert!(t.name.starts_with("pumbo:"), "{}", t.name);
            assert!(t.owner.is_some(), "{}", t.name);
            assert!(TOPICS.iter().skip(i + 1).all(|o| o.name != t.name));
        }
        assert_eq!(PLAYER_PUNISHED.versioned(), "pumbo:player-punished@1.0");
        assert_eq!(topic("pumbo:skin-changed"), Some(&SKIN_CHANGED));
    }
}
