//! Servers from the proxy (`managed-servers`): `pumbo-servers` wired into
//! the proxy. Its servers join the server list, and `/prox download` and
//! `/prox server` drive it. Permissions `pumbo.proxy.servers.<node>`,
//! by default for `commands.operators` and the console.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pumbo_core::engine::EngineRef;
use pumbo_servers::{Manager, Network, ProcessRunner, Source, State};
use tracing::{info, warn};

use crate::commands::{Reply, Source as Who};
use crate::config::{Config, ServerConfig, ServerProtocol};
use crate::server::{Proxy, Runtime, SessionCmd};

mod control;
mod download;
mod view;
mod watch;

pub(crate) use crate::ui::{button, line, reply, row, usage};
use control::server;
use download::download;

const SERVER_SUBS: &[&str] = &["help", "new", "start", "stop", "restart", "logs", "delete"];

// ------------------------------------------------------------------ setup

/// With `managed-servers.enabled`: reads `servers.yml`, puts the servers in
/// the server list, takes over the ones a crashed proxy left running and
/// starts the `autostart` ones. Needs the bridge key, so after the bridge.
pub fn start(proxy: &Arc<Proxy>) -> Result<(), String> {
    start_with(proxy, crate::modules::server_sources())
}

/// [`start`] with other server software (tests: a local release server).
pub fn start_with(proxy: &Arc<Proxy>, sources: Vec<Arc<dyn Source>>) -> Result<(), String> {
    let config = proxy.runtime().config.clone();
    if !config.managed_servers.enabled {
        return Ok(());
    }
    let manager = Manager::new(
        config.managed_servers.clone(),
        network(&config, proxy.bridge.get().map(|b| b.as_ref()))?,
        sources,
        Arc::new(ProcessRunner::default()),
    )
    .map_err(|e| format!("managed-servers: {e}"))?;
    let weak = Arc::downgrade(proxy);
    manager.on_change(move || {
        let Some(p) = weak.upgrade() else {
            return;
        };
        if let Err(e) = p.refresh() {
            warn!("server list not updated: {e}");
        }
        first_ready(&p);
    });
    let _ = proxy.servers.set(manager.clone());
    proxy.refresh()?;
    manager.boot();
    Ok(())
}

/// With `routing.try` empty, the first server of the proxy that is ready
/// goes there (in the file, comments stay), so players can join it.
fn first_ready(proxy: &Proxy) {
    let (Some(m), Some(_)) = (proxy.servers.get(), proxy.config_path()) else {
        return;
    };
    // Two servers ready at once: the second sees the first in the list.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !proxy.runtime().config.try_order().is_empty() {
        return;
    }
    let Some(name) = m.entries().into_keys().find(|n| m.state(n) == State::Ready) else {
        return;
    };
    let order = crate::config_edit::flow_list(std::slice::from_ref(&name));
    match crate::config_edit::save(proxy, |text| {
        crate::config_edit::set_key(text, "routing", "try", Some(&order))
    }) {
        Ok(()) => info!("{name} is ready: added to routing.try, players join it now"),
        Err(e) => warn!("{name} not added to routing.try: {e}"),
    }
}

/// The forwarding secret and, when the proxy runs one, the bridge for the
/// files of new servers.
fn network(config: &Config, bridge: Option<&crate::bridge::Bridge>) -> Result<Network, String> {
    let forwarding_secret = if config.forwarding.mode == "modern" {
        let get = |k: &str| config.forwarding.rest.get(k).and_then(|v| v.as_str());
        let secret = match get("secret-env") {
            Some(var) => {
                std::env::var(var).map_err(|_| format!("environment variable {var} is not set"))?
            }
            None => {
                let path = get("secret-file").unwrap_or("forwarding.secret");
                std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?
            }
        };
        Some(secret.trim().to_string())
    } else {
        None
    };
    // The key the bridge runs with (from key-env or key-file).
    let bridge = bridge.map(|b| (config.bridge.listen.clone(), b.key_hex()));
    Ok(Network {
        forwarding_secret,
        bridge_releases: bridge
            .is_some()
            .then(|| pumbo_servers::versions::PUMBOBRIDGE_API.to_string()),
        bridge,
    })
}

/// The servers of the manager as `servers:` entries: loopback, the protocol
/// of their Minecraft release.
pub fn backends(proxy: &Proxy, m: &Manager) -> BTreeMap<String, ServerConfig> {
    m.entries()
        .into_iter()
        .map(|(name, e)| {
            let protocol = m.minecraft(&e).and_then(|release| {
                proxy
                    .versions
                    .versions()
                    .find(|v| {
                        proxy
                            .versions
                            .get(*v)
                            .is_some_and(|m| m.release_names().contains(&release))
                    })
                    .map(|v| v.0)
            });
            let backend = ServerConfig {
                engine: engine_of(&e.source, &e.version),
                address: format!("127.0.0.1:{}", e.port),
                protocol: protocol.map_or(ServerProtocol::Auto, ServerProtocol::Fixed),
                chat_session_forwarding: None,
                signed_chat_cancel: None,
                allow_transfers: false,
                allow_public: false,
            };
            (name, backend)
        })
        .collect()
}

/// The engine of a server the proxy runs: its software's, when this build
/// has that software. The manager's file names it, so an unknown name
/// cannot stop the proxy.
pub fn engine_of(source: &str, version: &str) -> Option<EngineRef> {
    crate::modules::server_sources()
        .iter()
        .any(|s| s.name() == source)
        .then(|| EngineRef {
            name: source.to_string(),
            version: Some(version.to_string()),
        })
}

/// `pumbo.proxy.servers.<node>`; operators and the console by default.
pub fn can(proxy: &Proxy, rt: &Runtime, who: &Who<'_>, node: &str) -> bool {
    crate::commands::permitted(proxy, rt, who, &format!("pumbo.proxy.servers.{node}"))
}

/// `pumbo.proxy.download`.
fn may(proxy: &Proxy, rt: &Runtime, who: &Who<'_>) -> bool {
    crate::commands::permitted(proxy, rt, who, "pumbo.proxy.download")
}

/// Sends the result of a command that finishes later to whoever ran it.
type Say = Arc<dyn Fn(&str) + Send + Sync>;

fn say(proxy: &Arc<Proxy>, who: &Who<'_>) -> Say {
    match who {
        Who::Console => Arc::new(|t: &str| info!("{}", line(t).plain_text())),
        Who::Player { id, .. } => {
            let (p, id) = (Arc::downgrade(proxy), *id);
            Arc::new(move |t: &str| {
                if let Some(p) = p.upgrade() {
                    p.send_to(id, SessionCmd::Message(Box::new(line(t))));
                }
            })
        }
    }
}

/// `/prox download …` and `/prox servers …`; `args` are the words after `prox`.
pub fn command(proxy: &Arc<Proxy>, rt: &Runtime, who: &Who<'_>, args: &[&str]) -> Reply {
    let Some(m) = proxy.servers.get().cloned() else {
        return reply(
            "<err>Servers from the proxy are off <s>(managed-servers.enabled in pumboprox.yml)",
        );
    };
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("download") => download(proxy, rt, who, m, rest),
        _ => server(proxy, rt, who, m, rest),
    }
}

/// Completion after `prox download|server`: `words` are the finished words
/// after `prox`, `arg` the index of the word being typed among them.
pub fn suggest(proxy: &Proxy, words: &[&str], arg: usize) -> Vec<String> {
    let Some(m) = proxy.servers.get() else {
        return Vec::new();
    };
    let word = |i: usize| {
        words
            .get(i)
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default()
    };
    let numbered = |src: &str| {
        let list = m.versions.cached(src).unwrap_or_default();
        (1..=list.len())
            .map(|n| format!("#{n}"))
            .chain(list.into_iter().map(|r| r.tag))
            .collect::<Vec<_>>()
    };
    let names = || m.entries().into_keys().collect::<Vec<_>>();
    let active = m.versions.downloads();
    match (word(0).as_str(), arg) {
        ("download", 1) => {
            let mut v: Vec<String> = m
                .versions
                .source_names()
                .into_iter()
                .map(String::from)
                .collect();
            v.extend(["paper".into(), "stop".into(), "help".into()]);
            v
        }
        ("download", 2) if word(1) == "stop" => {
            let mut v: Vec<String> = active.iter().map(|(s, _)| s.to_string()).collect();
            v.extend(active.iter().map(|(_, t)| t.clone()));
            v.dedup();
            v
        }
        ("download", 3) if word(1) == "stop" => active
            .iter()
            .filter(|(s, _)| *s == word(2))
            .map(|(_, t)| t.clone())
            .collect(),
        ("download", 2) => {
            let mut v = vec!["list".to_string()];
            v.extend(numbered(&word(1)));
            v
        }
        ("servers", 1) => SERVER_SUBS.iter().map(|s| s.to_string()).collect(),
        ("servers", 2)
            if matches!(
                word(1).as_str(),
                "start" | "stop" | "restart" | "logs" | "delete"
            ) =>
        {
            names()
        }
        ("servers", 3) if word(1) == "new" => {
            let mut v = vec!["latest".to_string()];
            for src in m.versions.source_names() {
                v.extend(m.versions.installed(src));
                v.extend(numbered(src).into_iter().filter(|s| s.starts_with('#')));
            }
            v.dedup();
            v
        }
        ("servers", 3) if word(1) == "delete" => vec!["confirm".into()],
        ("servers", 3) if word(1) == "logs" => vec!["20".into(), "50".into(), "100".into()],
        ("servers", 4) if word(1) == "new" => m.template_names(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_managed_server_of_unknown_software_gets_no_engine() {
        let pumpkin = EngineRef {
            name: "pumpkin".into(),
            version: Some("0.2.0+26.3-26.51".into()),
        };
        assert_eq!(engine_of("pumpkin", "0.2.0+26.3-26.51"), Some(pumpkin));
        assert_eq!(engine_of("stone", "1.0"), None);
    }
}
