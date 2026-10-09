//! Backend selection: the `try` list with fallback and forced hosts (§3.2).

use std::collections::BTreeMap;
use std::sync::Arc;

use pumbo_core::registry::{ModuleConfig, ModuleError, ModuleRegistry};
use pumbo_core::routing::{BackendInfo, BackendSelector, SelectContext, SelectReason};

pub const NAME: &str = "try";

/// Order from the config, or the forced host's list when the player joined
/// through that host (also for fallback, like Velocity); skips servers that are
/// offline and (on fallback) the server the player dropped from. Whether a
/// server's protocol suits the client is the translation chain's decision
/// (the session asks it per candidate).
#[derive(Debug)]
pub struct TryList {
    order: Vec<String>,
    /// Normalized host (lower case, no trailing dot) → order.
    forced: BTreeMap<String, Vec<String>>,
}

impl TryList {
    pub fn new(order: Vec<String>) -> Self {
        Self {
            order,
            forced: BTreeMap::new(),
        }
    }

    pub fn with_forced_hosts(mut self, forced: BTreeMap<String, Vec<String>>) -> Self {
        self.forced = forced
            .into_iter()
            .map(|(h, v)| (h.trim_end_matches('.').to_ascii_lowercase(), v))
            .collect();
        self
    }
}

impl BackendSelector for TryList {
    fn name(&self) -> &str {
        NAME
    }

    fn candidates(&self, ctx: &SelectContext<'_>, backends: &[BackendInfo]) -> Vec<String> {
        self.forced
            .get(ctx.virtual_host)
            .unwrap_or(&self.order)
            .iter()
            .filter_map(|name| backends.iter().find(|b| &b.name == name))
            .filter(|b| b.online)
            .filter(|b| {
                ctx.reason != SelectReason::Fallback || ctx.previous != Some(b.name.as_str())
            })
            .map(|b| b.name.clone())
            .collect()
    }
}

fn names(v: Option<&serde_json::Value>, what: &str) -> Result<Vec<String>, ModuleError> {
    let bad = || ModuleError::Config {
        name: NAME.to_string(),
        message: format!("{what} must be a list of server names"),
    };
    match v {
        None => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string).ok_or_else(bad))
            .collect(),
        Some(_) => Err(bad()),
    }
}

/// Section keys: `try` (list) and `forced-hosts` (host → list; the binary
/// passes the top-level `forced-hosts` here).
fn factory(cfg: &ModuleConfig) -> Result<Arc<dyn BackendSelector>, ModuleError> {
    let order = names(cfg.get("try"), "\"try\"")?;
    let mut forced = BTreeMap::new();
    if let Some(t) = cfg
        .get("forced-hosts")
        .and_then(serde_json::Value::as_object)
    {
        for (host, v) in t {
            forced.insert(host.clone(), names(Some(v), "a forced host")?);
        }
    }
    Ok(Arc::new(TryList::new(order).with_forced_hosts(forced)))
}

pub fn register(reg: &mut ModuleRegistry) -> Result<(), ModuleError> {
    reg.selectors.register(NAME, factory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumbo_protocol::ProtocolVersion;

    fn b(name: &str, protocol: Option<i32>, online: bool) -> BackendInfo {
        BackendInfo {
            name: name.into(),
            address: "127.0.0.1:1".into(),
            protocol: protocol.map(ProtocolVersion),
            online,
            players: 0,
        }
    }

    #[test]
    fn order_and_filters() {
        let sel = TryList::new(vec![
            "lobby".into(),
            "lobby2".into(),
            "old".into(),
            "down".into(),
        ]);
        let backends = [
            b("down", Some(777), false),
            b("old", Some(767), true),
            b("lobby2", None, true),
            b("lobby", Some(777), true),
        ];
        let ctx = SelectContext {
            reason: SelectReason::InitialJoin,
            virtual_host: "mc.example.org",
            client_protocol: ProtocolVersion::V777,
            previous: None,
        };
        assert_eq!(sel.candidates(&ctx, &backends), ["lobby", "lobby2", "old"]);
        let ctx = SelectContext {
            reason: SelectReason::Fallback,
            previous: Some("lobby"),
            ..ctx
        };
        assert_eq!(sel.candidates(&ctx, &backends), ["lobby2", "old"]);
        let sel = TryList::new(vec!["lobby".into()]).with_forced_hosts(BTreeMap::from([(
            "Survival.Example.ORG.".to_string(),
            vec!["lobby2".to_string(), "lobby".to_string()],
        )]));
        let forced = SelectContext {
            reason: SelectReason::InitialJoin,
            virtual_host: "survival.example.org",
            previous: None,
            ..ctx.clone()
        };
        assert_eq!(sel.candidates(&forced, &backends), ["lobby2", "lobby"]);
        let other = SelectContext {
            virtual_host: "other.example.org",
            ..forced
        };
        assert_eq!(sel.candidates(&other, &backends), ["lobby"]);
    }
}
