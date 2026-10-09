//! Extension point: source of permissions (plan §1.1, §5.8.4).
//!
//! The host keeps permissions as data; a check never calls a source. Sources
//! only load the entries of a player (explicit async steps with a deadline):
//! `file` (`permissions.yml`, always loaded) and `plugin` (a provider plugin).
//! Native sources (SQL, LDAP) come later as modules.

use uuid::Uuid;

use crate::BoxFuture;

/// Context of an entry. Resolution: server > group (config order) > global.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PermContext {
    Global,
    Group(String),
    Server(String),
}

/// One permission node in one context. `node` may end with `.*` or be `*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionEntry {
    pub node: String,
    pub value: bool,
    pub context: PermContext,
}

/// Who the entries are for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub uuid: Uuid,
    pub name: String,
}

pub trait PermissionSource: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    /// All entries of a player, in every context.
    fn load<'a>(&'a self, who: &'a Subject) -> BoxFuture<'a, Result<Vec<PermissionEntry>, String>>;
}
