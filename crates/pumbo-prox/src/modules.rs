//! Registering the built-in modules and building them from the config.

use std::sync::Arc;

use pumbo_core::engine::{EngineAdapter, EngineRef, SignedChatCancel};
use pumbo_core::forwarding::ForwardingMode;
use pumbo_core::hook::{SessionHook, SessionHookFactory, SessionInfo};
use pumbo_core::identity::Authenticator;
use pumbo_core::registry::{ModuleConfig, ModuleError, ModuleRegistry};
use pumbo_core::routing::BackendSelector;
use pumbo_core::section::SectionInfo;
use pumbo_core::storage::KeyValueStore;
use pumbo_core::translate::TranslatorProvider;
use pumbo_protocol::VersionRegistry;

use crate::config::{Config, ServerConfig};
use crate::server::Proxy;

/// Registry with every module compiled into the binary. A new module (e.g. a
/// Bedrock listener, another forwarding mode) is a new crate and one line here.
pub fn builtin() -> Result<ModuleRegistry, ModuleError> {
    let mut reg = ModuleRegistry::new();
    pumbo_listener_java::register(&mut reg)?;
    pumbo_forwarding::register(&mut reg)?;
    pumbo_routing::register(&mut reg)?;
    pumbo_translate::register(&mut reg)?;
    pumbo_store::register(&mut reg)?;
    pumbo_identity::register(&mut reg)?;
    pumbo_engines::register(&mut reg)?;
    Ok(reg)
}

/// The engine a server gets while nothing names or reveals its engine: the
/// one the proxy assumed before it had adapters.
const DEFAULT_ENGINE: &str = pumbo_engines::PUMPKIN;

/// A server's adapter and the engine it stands for.
#[derive(Debug, Clone)]
pub struct ServerEngine {
    pub adapter: Arc<dyn EngineAdapter>,
    pub engine: EngineRef,
}

impl ServerEngine {
    /// Whether the server gets the client's chat session and what a
    /// cancelled signed message does there: its entry's keys, the rest from
    /// the engine.
    pub fn chat(&self, server: Option<&ServerConfig>) -> (bool, SignedChatCancel) {
        let d = self.adapter.defaults(&self.engine);
        let sessions = server.and_then(|s| s.chat_session_forwarding);
        let cancel = server.and_then(|s| s.signed_chat_cancel);
        (
            sessions.unwrap_or(d.chat_session_forwarding),
            cancel.unwrap_or(d.signed_chat_cancel),
        )
    }
}

/// The engine of every server.
#[derive(Debug)]
pub struct Engines {
    default: Arc<ServerEngine>,
}

impl Engines {
    /// Shared, not borrowed: an engine found while the proxy runs lives
    /// outside this table.
    pub fn of(&self, _server: &str) -> Arc<ServerEngine> {
        self.default.clone()
    }
}

/// The server software of `managed-servers`: a list next to the registry,
/// from the crate of the engine adapters.
pub fn server_sources() -> Vec<Arc<dyn pumbo_servers::Source>> {
    pumbo_engines::sources()
}

/// The session hooks of the built-in modules, in calling order; a new hook
/// is one line here.
pub(crate) fn session_hooks(proxy: &Arc<Proxy>, s: &SessionInfo<'_>) -> Vec<Box<dyn SessionHook>> {
    let factories: [&dyn SessionHookFactory; 2] = [
        &crate::messaging::Factory(proxy),
        &crate::bridge::gate::Factory(proxy),
    ];
    factories.iter().filter_map(|f| f.for_session(s)).collect()
}

/// Supported protocol versions: one module per generated table set
/// (`pumbo-data`). A new Minecraft version is new data, not a change here.
pub fn versions() -> Result<VersionRegistry, pumbo_data::DataError> {
    let mut reg = VersionRegistry::new();
    pumbo_data::register_all(&mut reg)?;
    Ok(reg)
}

/// Modules built from the config (without listeners, which start separately
/// because they open sockets).
#[derive(Debug)]
pub struct Modules {
    pub forwarding: Arc<dyn ForwardingMode>,
    pub selector: Arc<dyn BackendSelector>,
    pub translators: Vec<Arc<dyn TranslatorProvider>>,
    pub store: Arc<dyn KeyValueStore>,
    pub authenticator: Arc<dyn Authenticator>,
    pub engines: Engines,
}

pub fn build(reg: &ModuleRegistry, cfg: &Config) -> Result<Modules, ModuleError> {
    for l in &cfg.listener {
        reg.listeners.get(&l.transport)?;
    }
    let mut fwd = cfg.forwarding.rest.clone();
    if cfg.check_only {
        fwd.insert(pumbo_forwarding::modern::NO_CREATE.into(), true.into());
    }
    let forwarding = (reg.forwarding.get(&cfg.forwarding.mode)?)(&fwd)?;
    let mut routing = cfg.routing.rest.clone();
    if !cfg.forced_hosts.is_empty() {
        let forced = cfg
            .forced_hosts
            .iter()
            .map(|(h, v)| (h.clone(), serde_json::Value::from(v.clone())))
            .collect::<serde_json::Map<_, _>>();
        routing.insert("forced-hosts".into(), forced.into());
    }
    let selector = (reg.selectors.get(&cfg.routing.selector)?)(&routing)?;
    // Translation providers have no section of their own.
    let none = ModuleConfig::new();
    let translators = cfg
        .translation
        .chain
        .iter()
        .map(|name| {
            if !pumbo_translate::HAS_MULTIVERSION && name == pumbo_translate::MULTIVERSION {
                return Err(ModuleError::Config {
                    name: name.clone(),
                    message: format!(
                        "this build has no version translation (built without the translate feature); remove {name} from translation.chain"
                    ),
                });
            }
            (reg.translators.get(name)?)(&none)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let store = (reg.stores.get(&cfg.storage.backend)?)(&cfg.storage.rest)?;
    let authenticator =
        (reg.authenticators.get(&cfg.authentication.service)?)(&cfg.authentication.rest)?;
    let adapter = (reg.engines.get(DEFAULT_ENGINE)?)();
    // The key in the registry is the engine's one name: configs look it up,
    // logs and metrics show it.
    debug_assert_eq!(adapter.name(), DEFAULT_ENGINE);
    let engine = EngineRef {
        name: DEFAULT_ENGINE.to_string(),
        version: None,
    };
    let engines = Engines {
        default: Arc::new(ServerEngine { adapter, engine }),
    };
    Ok(Modules {
        forwarding,
        selector,
        translators,
        store,
        authenticator,
        engines,
    })
}

/// A `listener` section as module config.
pub fn listener_config(l: &crate::config::ListenerConfig) -> ModuleConfig {
    let mut t = l.rest.clone();
    t.insert("bind".into(), l.bind.clone().into());
    t
}

/// Every section of `pumboprox.yml` with the schema of its keys: the
/// proxy's, the plugin host's and the translation module's. The proxy reads
/// `translation` itself (a field of [`Config`]), so for the rules on unknown
/// keys it is one of the proxy's sections.
pub fn sections() -> Vec<SectionInfo> {
    let mut all = crate::config::sections();
    all.extend(pumbo_host::config::sections());
    all.extend(pumbo_translate::sections());
    all
}

#[cfg(test)]
mod tests {
    use pumbo_core::engine::BackendDefaults;

    use super::*;

    /// An adapter with the given defaults, registered in place of Pumpkin's.
    #[derive(Debug)]
    struct Fixed(BackendDefaults);

    impl EngineAdapter for Fixed {
        fn name(&self) -> &str {
            pumbo_engines::PUMPKIN
        }
        fn defaults(&self, _: &EngineRef) -> BackendDefaults {
            self.0
        }
    }

    const STRICT: BackendDefaults = BackendDefaults {
        signed_chat_cancel: SignedChatCancel::Kick,
        chat_session_forwarding: false,
    };
    const LOOSE: BackendDefaults = BackendDefaults {
        signed_chat_cancel: SignedChatCancel::Drop,
        chat_session_forwarding: true,
    };

    fn engine(d: BackendDefaults) -> ServerEngine {
        ServerEngine {
            adapter: Arc::new(Fixed(d)),
            engine: EngineRef {
                name: pumbo_engines::PUMPKIN.into(),
                version: None,
            },
        }
    }

    #[test]
    fn every_server_gets_the_registered_engine() {
        let mut reg = builtin().unwrap();
        reg.engines = ModuleRegistry::new().engines;
        reg.engines
            .register(pumbo_engines::PUMPKIN, || Arc::new(Fixed(STRICT)))
            .unwrap();
        let cfg = Config::parse(
            "listener:\n  - bind: \"127.0.0.1:0\"\nforwarding:\n  mode: none\n\
             servers:\n  lobby: { address: \"127.0.0.1:1\", protocol: 777 }\n  \
             games: { address: \"127.0.0.1:2\", protocol: 777 }\n",
        )
        .unwrap();
        let m = build(&reg, &cfg).unwrap();
        for server in ["lobby", "games", "nowhere"] {
            let e = m.engines.of(server);
            assert_eq!(e.adapter.defaults(&e.engine), STRICT, "{server}");
            let want = EngineRef {
                name: pumbo_engines::PUMPKIN.into(),
                version: None,
            };
            assert_eq!(e.engine, want, "{server}");
        }
    }

    #[test]
    fn a_servers_chat_keys_win_over_its_engine() {
        let entry = |sessions, cancel| ServerConfig {
            address: "127.0.0.1:1".into(),
            protocol: None,
            chat_session_forwarding: sessions,
            signed_chat_cancel: cancel,
            allow_transfers: false,
            allow_public: false,
        };
        for (d, other) in [(STRICT, LOOSE), (LOOSE, STRICT)] {
            let e = engine(d);
            let theirs = (d.chat_session_forwarding, d.signed_chat_cancel);
            assert_eq!(e.chat(None), theirs);
            assert_eq!(e.chat(Some(&entry(None, None))), theirs);
            let keys = entry(
                Some(other.chat_session_forwarding),
                Some(other.signed_chat_cancel),
            );
            let mine = (other.chat_session_forwarding, other.signed_chat_cancel);
            assert_eq!(e.chat(Some(&keys)), mine);
            // One key of the entry, the other from the engine.
            let half = entry(Some(other.chat_session_forwarding), None);
            let mixed = (other.chat_session_forwarding, d.signed_chat_cancel);
            assert_eq!(e.chat(Some(&half)), mixed);
        }
    }
}
