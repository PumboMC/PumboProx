//! What the proxy learns about its servers while it runs. The facts live
//! here, in memory only; a server's protocol and engine in each runtime are
//! a function of its `servers` entry and these facts.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use pumbo_core::engine::EngineRef;
use pumbo_core::routing::BackendInfo;
use pumbo_protocol::ProtocolVersion;
use tracing::warn;

use crate::config::ServerConfig;
use crate::modules::{EngineSource, ServerEngine};
use crate::server::{Proxy, Runtime};

#[cfg(test)]
mod tests;

/// A brand longer than this is no engine's.
const MAX_BRAND: usize = 64;

/// What the servers revealed while the proxy ran: the engine their brand
/// named, what their bridges said. In memory only.
#[derive(Debug, Default)]
pub struct Detect {
    servers: Mutex<HashMap<String, Facts>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Facts {
    /// The address these are about; facts of another address do not count.
    address: String,
    /// The engine the first recognised brand named. It stays until the
    /// address changes or a reload.
    brand: Option<EngineRef>,
    /// The engine the server's bridge named.
    bridge: Option<EngineRef>,
}

impl Detect {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Facts>> {
        self.servers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The engine of `server` now, from its entry in `rt` and the facts.
    pub fn engine(&self, rt: &Runtime, server: &str) -> ServerEngine {
        let entry = rt.config.servers.get(server);
        let facts = entry.and_then(|s| current(&self.lock(), server, s).cloned());
        engine(rt, entry, facts.as_ref())
    }

    /// A server's brand from the configuration phase of one connection.
    /// Returns the engine for that connection.
    pub(crate) fn brand(
        &self,
        proxy: &Proxy,
        rt: &Runtime,
        server: &str,
        brand: &str,
    ) -> ServerEngine {
        let entry = rt.config.servers.get(server);
        let named = (brand.len() <= MAX_BRAND)
            .then(|| rt.modules.engines.recognise(brand))
            .flatten();
        let (Some(named), Some(s)) = (named, entry) else {
            return self.engine(rt, server);
        };
        let mut servers = self.lock();
        let facts = facts_of(&mut servers, server, s);
        let first = facts.brand.get_or_insert_with(|| named.clone()).clone();
        let mut mine = facts.clone();
        drop(servers);
        if first.name != named.name && proxy.first_report("brand-change", server) {
            warn!(
                "server {server} now shows the brand of {}, it showed {} before; the proxy keeps {} until a reload",
                named.name, first.name, first.name
            );
        }
        // The session saw this brand itself.
        mine.brand = Some(named);
        engine(rt, entry, Some(&mine))
    }

    /// The engine the bridge paired with `server` names; `None`: it names none.
    pub fn bridge_hello(&self, rt: &Runtime, server: &str, engine: Option<EngineRef>) {
        let Some(s) = rt.config.servers.get(server) else {
            return;
        };
        facts_of(&mut self.lock(), server, s).bridge = engine;
    }

    /// `rt`'s servers with the protocol and engine the facts give them.
    /// Changes nothing.
    pub fn backends(&self, rt: &Runtime) -> Vec<BackendInfo> {
        let servers = self.lock();
        rt.config
            .servers
            .iter()
            .map(|(name, s)| {
                let facts = current(&servers, name, s);
                let e = engine(rt, Some(s), facts);
                BackendInfo {
                    name: name.clone(),
                    address: s.address.clone(),
                    protocol: protocol(s),
                    engine: (e.source != EngineSource::Assumed).then_some(e.engine),
                    online: true,
                    players: 0,
                }
            })
            .collect()
    }

    /// After a reload the next recognised brand of each server counts
    /// again: its software may have changed under the same address.
    pub(crate) fn rearm(&self) {
        for facts in self.lock().values_mut() {
            facts.brand = None;
        }
    }
}

/// The facts of `server` when they are about the address of `entry`.
fn current<'f>(
    servers: &'f HashMap<String, Facts>,
    server: &str,
    entry: &ServerConfig,
) -> Option<&'f Facts> {
    servers.get(server).filter(|f| f.address == entry.address)
}

/// The facts of `server` to change: fresh ones when they were about
/// another address.
fn facts_of<'f>(
    servers: &'f mut HashMap<String, Facts>,
    server: &str,
    entry: &ServerConfig,
) -> &'f mut Facts {
    let facts = servers.entry(server.to_string()).or_default();
    if facts.address != entry.address {
        *facts = Facts {
            address: entry.address.clone(),
            ..Facts::default()
        };
    }
    facts
}

/// The protocol the proxy takes for a server.
fn protocol(entry: &ServerConfig) -> Option<ProtocolVersion> {
    entry.protocol.fixed().map(ProtocolVersion)
}

/// A server's engine from its entry and `facts`.
fn engine(rt: &Runtime, entry: Option<&ServerConfig>, facts: Option<&Facts>) -> ServerEngine {
    rt.modules.engines.resolve(
        entry.and_then(|s| s.engine.as_ref()),
        facts.and_then(|f| f.bridge.as_ref()),
        facts.and_then(|f| f.brand.as_ref()),
        entry.and_then(protocol),
    )
}
