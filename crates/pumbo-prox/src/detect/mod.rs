//! What the proxy learns about its servers while it runs. The facts live
//! here, in memory only; a server's protocol and engine in each runtime are
//! a function of its `servers` entry and these facts.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use pumbo_core::engine::EngineRef;
use pumbo_core::routing::BackendInfo;
use pumbo_protocol::ProtocolVersion;
use tracing::{info, warn};

use crate::config::{ServerConfig, ServerProtocol};
use crate::modules::{EngineSource, ServerEngine};
use crate::server::{Proxy, Runtime};

#[cfg(test)]
mod tests;

/// A brand longer than this is no engine's.
const MAX_BRAND: usize = 64;

/// What the servers revealed while the proxy ran: the answers to its
/// pings, the engine their brand named, what their bridges said. In
/// memory only.
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
    /// The last status answer and the protocol it was asked with:
    /// (asked, answered).
    answer: Option<(i32, i32)>,
    /// The last ping got no answer (one log line per outage).
    silent: bool,
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
                    protocol: protocol(s.protocol, facts.and_then(|f| f.answer)),
                    engine: (e.source != EngineSource::Assumed).then_some(e.engine),
                    online: true,
                    players: 0,
                }
            })
            .collect()
    }

    /// The result of one ping of `server`, asked with `asked`.
    pub fn answered(&self, rt: &Runtime, server: &str, asked: i32, answer: Result<i32, String>) {
        let Some(s) = rt.config.servers.get(server) else {
            return;
        };
        let mut servers = self.lock();
        let facts = facts_of(&mut servers, server, s);
        let said = match answer {
            Ok(n) if facts.answer == Some((asked, n)) => {
                facts.silent = false;
                None
            }
            Ok(n) => {
                facts.answer = Some((asked, n));
                facts.silent = false;
                Some(answer_line(server, s.protocol, (asked, n)))
            }
            Err(_) if facts.silent => None,
            Err(e) => {
                facts.silent = true;
                let kept = kept(s.protocol, facts.answer);
                Some(Said::Warn(format!(
                    "server {server} does not answer the status ping ({e}); {kept}"
                )))
            }
        };
        drop(servers);
        match said {
            Some(Said::Info(line)) => info!("{line}"),
            Some(Said::Warn(line)) => warn!("{line}"),
            None => {}
        }
    }

    /// The protocol `server` last answered with, whatever it was asked.
    pub fn last_answer(&self, server: &str) -> Option<i32> {
        self.lock().get(server)?.answer.map(|(_, n)| n)
    }

    /// Drops the facts of servers and addresses `rt` no longer has.
    pub(crate) fn forget(&self, rt: &Runtime) {
        self.lock().retain(|name, f| {
            rt.config
                .servers
                .get(name)
                .is_some_and(|s| s.address == f.address)
        });
    }

    /// The log lines of a new runtime `rt` whose servers were `old`: each
    /// engine that changed, and once per server an entry that the server
    /// contradicts.
    pub(crate) fn report(&self, proxy: &Proxy, old: &[BackendInfo], rt: &Runtime) {
        for b in &rt.backends {
            let before = old.iter().find(|o| o.name == b.name);
            if before.is_some_and(|o| o.engine == b.engine) {
                continue;
            }
            let e = self.engine(rt, &b.name);
            let source = match e.source {
                EngineSource::Declared => "from its entry",
                EngineSource::Bridge => "from its bridge",
                EngineSource::Brand => "from its brand",
                EngineSource::Assumed => "assumed",
            };
            info!("server {}: engine {} ({source})", b.name, e.engine.name);
        }
        let differ: Vec<(String, String, String)> = {
            let servers = self.lock();
            rt.config
                .servers
                .iter()
                .filter_map(|(name, s)| {
                    let declared = s.engine.as_ref()?;
                    let f = current(&servers, name, s)?;
                    let shown = [&f.bridge, &f.brand]
                        .into_iter()
                        .flatten()
                        .find(|e| e.name != declared.name)?;
                    Some((name.clone(), declared.name.clone(), shown.name.clone()))
                })
                .collect()
        };
        for (name, declared, shown) in differ {
            if proxy.first_report("engine-mismatch", &name) {
                warn!(
                    "server {name}: engine is set to {declared} but the server shows {shown}; the proxy keeps {declared}"
                );
            }
        }
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

/// The protocol a ping of a server with `entry` asks with: its own number,
/// so a server that answers in the asker's version confirms it; `None`:
/// the server is not asked.
pub(crate) fn ask(entry: ServerProtocol) -> Option<i32> {
    match entry {
        ServerProtocol::Auto => Some(crate::ping::ASK_ANY),
        ServerProtocol::Any => None,
        ServerProtocol::Fixed(m) => Some(m),
    }
}

/// The protocol the proxy takes for a server: the answer to what its entry
/// asks, when this build knows that version; otherwise the entry's number.
pub(crate) fn protocol(
    entry: ServerProtocol,
    answer: Option<(i32, i32)>,
) -> Option<ProtocolVersion> {
    match answer {
        Some((asked, n)) if Some(asked) == ask(entry) && known(n) => Some(ProtocolVersion(n)),
        _ => entry.fixed().map(ProtocolVersion),
    }
}

/// Whether this build has the tables of protocol `n`.
fn known(n: i32) -> bool {
    pumbo_data::protocols().any(|p| p.0 == n)
}

/// A log line of [`Detect::answered`].
enum Said {
    Info(String),
    Warn(String),
}

/// What the proxy says about a new answer, (asked, answered).
fn answer_line(server: &str, entry: ServerProtocol, answer: (i32, i32)) -> Said {
    let n = answer.1;
    if !known(n) {
        let kept = kept(entry, Some(answer));
        return Said::Warn(format!(
            "server {server} answers protocol {n}, which this build does not know; {kept}"
        ));
    }
    match entry {
        ServerProtocol::Fixed(m) if m != n => Said::Warn(format!(
            "server {server}: protocol is set to {m} but the server answers {n}; the proxy uses {n}. Fix servers.{server}.protocol, or set protocol: any when the server takes several versions itself"
        )),
        _ => {
            let v = crate::versions::names(&[ProtocolVersion(n)], "-");
            Said::Info(format!(
                "server {server} runs Minecraft {v} (protocol {n}), from its status answer"
            ))
        }
    }
}

/// What the proxy goes on with for a server whose last answer is `answer`.
fn kept(entry: ServerProtocol, answer: Option<(i32, i32)>) -> String {
    match (protocol(entry, answer), entry) {
        (None, _) => "its version stays unknown and every client is sent to it".into(),
        (Some(p), ServerProtocol::Fixed(m)) if p.0 == m => {
            format!("the proxy keeps protocol {m} from its entry")
        }
        (Some(p), _) => format!("the proxy keeps protocol {p} from its last answer"),
    }
}

/// A server's engine from its entry and `facts`.
fn engine(rt: &Runtime, entry: Option<&ServerConfig>, facts: Option<&Facts>) -> ServerEngine {
    rt.modules.engines.resolve(
        entry.and_then(|s| s.engine.as_ref()),
        facts.and_then(|f| f.bridge.as_ref()),
        facts.and_then(|f| f.brand.as_ref()),
        entry.and_then(|s| protocol(s.protocol, facts.and_then(|f| f.answer))),
    )
}
