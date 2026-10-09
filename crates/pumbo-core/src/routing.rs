//! Extension point: backend selection (`try` order today, load balancing later).

use pumbo_protocol::ProtocolVersion;

/// A backend from the config with its state from the health ping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    pub name: String,
    pub address: String,
    /// Backend protocol, if declared or known from the ping.
    pub protocol: Option<ProtocolVersion>,
    pub online: bool,
    pub players: u32,
}

/// Why a server is being selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectReason {
    InitialJoin,
    Fallback,
}

#[derive(Debug, Clone)]
pub struct SelectContext<'a> {
    pub reason: SelectReason,
    pub virtual_host: &'a str,
    pub client_protocol: ProtocolVersion,
    /// The server the player just dropped from (on fallback).
    pub previous: Option<&'a str>,
}

/// Returns candidates in the order to try. The proxy tries them one by one.
pub trait BackendSelector: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn candidates(&self, ctx: &SelectContext<'_>, backends: &[BackendInfo]) -> Vec<String>;
}
