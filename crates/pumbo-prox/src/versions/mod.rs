//! Which client versions reach which servers: the one place that asks the
//! translation chain, for the handshake, the session and backend logins.

use pumbo_core::routing::BackendInfo;
use pumbo_core::translate::TranslationPlan;
use pumbo_protocol::{ProtocolVersion, VersionModule};
use pumbo_text::Component;

use crate::server::{Proxy, Runtime};
use crate::status::parse_text;

#[cfg(test)]
mod tests;

/// The first decision of the translation chain for `client` on a server of
/// `backend` (`None`: not known, or at the handshake).
pub(crate) fn plan(
    rt: &Runtime,
    client: ProtocolVersion,
    backend: Option<ProtocolVersion>,
) -> Option<TranslationPlan> {
    rt.modules
        .translators
        .iter()
        .find_map(|t| t.plan(client, backend))
}

/// The translation chain lets this client on `b`: the same version, or
/// translated in the proxy.
pub(crate) fn reaches(rt: &Runtime, client: ProtocolVersion, b: &BackendInfo) -> bool {
    matches!(
        plan(rt, client, b.protocol),
        Some(TranslationPlan::Passthrough | TranslationPlan::Translate(_))
    )
}

/// A network without servers, or one server that [`reaches`]: whatever its
/// health and whether players are routed to it.
pub(crate) fn has_way(rt: &Runtime, client: ProtocolVersion) -> bool {
    rt.backends.is_empty() || rt.backends.iter().any(|b| reaches(rt, client, b))
}

/// `messages.no-server-for-version` for a client no server takes: its
/// release and the releases of the servers.
pub(crate) fn no_way_text(proxy: &Proxy, rt: &Runtime, client: &dyn VersionModule) -> Component {
    let mut protocols: Vec<_> = rt.backends.iter().filter_map(|b| b.protocol).collect();
    protocols.sort();
    protocols.dedup();
    let supported: Vec<String> = protocols
        .iter()
        .filter_map(|p| proxy.versions.get(*p))
        .map(|m| releases(m.release_names()))
        .collect();
    parse_text(
        &rt.config
            .messages
            .no_server_for_version
            .replace("{version}", &releases(client.release_names()))
            .replace("{supported}", &supported.join(", ")),
    )
}

/// Releases of a protocol for messages: "26.3" or "26.1–26.1.2".
pub(crate) fn releases(names: &[String]) -> String {
    match (names.first(), names.last()) {
        (Some(a), Some(b)) if a != b => format!("{a}–{b}"),
        (Some(a), _) => a.clone(),
        _ => "?".into(),
    }
}
