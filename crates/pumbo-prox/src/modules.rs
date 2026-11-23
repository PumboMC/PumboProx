//! Registering the built-in modules and building them from the config.

use std::collections::BTreeMap;
use std::sync::Arc;

use pumbo_core::engine::{EngineAdapter, EngineProbe, EngineRef, SignedChatCancel};
use pumbo_core::forwarding::ForwardingMode;
use pumbo_core::hook::{SessionHook, SessionHookFactory, SessionInfo};
use pumbo_core::identity::Authenticator;
use pumbo_core::registry::{ModuleConfig, ModuleError, ModuleRegistry};
use pumbo_core::routing::BackendSelector;
use pumbo_core::section::SectionInfo;
use pumbo_core::storage::KeyValueStore;
use pumbo_core::translate::TranslatorProvider;
use pumbo_protocol::{ProtocolVersion, VersionRegistry};

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

/// The engine of a server the proxy cannot recognise and whose protocol no
/// release of [`DEFAULT_ENGINE`] has, and the adapter of an engine a bridge
/// names that this build has no adapter for.
pub const NO_ENGINE: &str = pumbo_engines::GENERIC;

/// Why a server has its engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineSource {
    /// Its `servers` entry, or the software of a server the proxy runs.
    Declared,
    /// What its bridge said.
    Bridge,
    /// The brand it sent.
    Brand,
    /// Nothing named it: the proxy assumes one.
    Assumed,
}

/// A server's adapter, the engine it stands for and why.
#[derive(Debug, Clone)]
pub struct ServerEngine {
    pub adapter: Arc<dyn EngineAdapter>,
    pub engine: EngineRef,
    pub source: EngineSource,
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

/// Every engine adapter of the build, by name.
#[derive(Debug, Clone)]
pub struct Engines {
    adapters: BTreeMap<String, Arc<dyn EngineAdapter>>,
    /// [`DEFAULT_ENGINE`]'s and [`NO_ENGINE`]'s, which `build` requires.
    assumed: Arc<dyn EngineAdapter>,
    plain: Arc<dyn EngineAdapter>,
}

impl Engines {
    /// The adapter registered as `name`.
    pub fn adapter(&self, name: &str) -> Option<&Arc<dyn EngineAdapter>> {
        self.adapters.get(name)
    }

    /// The engine a brand certainly names: the first adapter, by name,
    /// whose `matches` answers.
    pub fn recognise(&self, brand: &str) -> Option<EngineRef> {
        let probe = EngineProbe {
            brand: Some(brand),
            protocol: None,
        };
        self.adapters.values().find_map(|a| a.matches(&probe))
    }

    /// A server's engine: its entry, then its bridge, then the engine its
    /// brand named, then the assumption.
    pub fn resolve(
        &self,
        declared: Option<&EngineRef>,
        bridge: Option<&EngineRef>,
        brand: Option<&EngineRef>,
        protocol: Option<ProtocolVersion>,
    ) -> ServerEngine {
        let signs = [
            (declared, EngineSource::Declared),
            (bridge, EngineSource::Bridge),
            (brand, EngineSource::Brand),
        ];
        if let Some((engine, source)) = signs.into_iter().find_map(|(e, s)| Some((e?, s))) {
            // A bridge may name an engine this build has no adapter for.
            let adapter = self.adapter(&engine.name).unwrap_or(&self.plain);
            return ServerEngine {
                adapter: adapter.clone(),
                engine: engine.clone(),
                source,
            };
        }
        // A server older than any release of the default engine is of
        // another one, which shows nothing.
        let adapter = match protocol {
            Some(p) if !self.assumed.speaks(p) => &self.plain,
            _ => &self.assumed,
        };
        ServerEngine {
            adapter: adapter.clone(),
            engine: EngineRef {
                name: adapter.name().to_string(),
                version: None,
            },
            source: EngineSource::Assumed,
        }
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
    // Translation providers have no section of their own; they read the
    // keys of `translation`.
    let mut keys = ModuleConfig::new();
    keys.insert(
        pumbo_translate::EXPERIMENTAL_PAIRS.into(),
        cfg.translation.experimental_pairs.clone().into(),
    );
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
            (reg.translators.get(name)?)(&keys)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let store = (reg.stores.get(&cfg.storage.backend)?)(&cfg.storage.rest)?;
    let authenticator =
        (reg.authenticators.get(&cfg.authentication.service)?)(&cfg.authentication.rest)?;
    let adapters: BTreeMap<String, Arc<dyn EngineAdapter>> = reg
        .engines
        .names()
        .map(|name| {
            let adapter = (reg.engines.get(name)?)();
            // The key in the registry is the engine's one name: configs
            // look it up, logs and metrics show it.
            debug_assert_eq!(adapter.name(), name);
            Ok((name.to_string(), adapter))
        })
        .collect::<Result<_, ModuleError>>()?;
    let engines = Engines {
        assumed: (reg.engines.get(DEFAULT_ENGINE)?)(),
        plain: (reg.engines.get(NO_ENGINE)?)(),
        adapters,
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
    use crate::config::ServerProtocol;

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
            engine: named(pumbo_engines::PUMPKIN),
            source: EngineSource::Assumed,
        }
    }

    fn named(name: &str) -> EngineRef {
        EngineRef {
            name: name.into(),
            version: None,
        }
    }

    /// The engines of the build.
    fn engines() -> Engines {
        let cfg = Config::parse("forwarding:\n  mode: none\n").unwrap();
        build(&builtin().unwrap(), &cfg).unwrap().engines
    }

    /// The adapter's name, the engine and the source of `e`.
    fn shown(e: &ServerEngine) -> (&str, &EngineRef, EngineSource) {
        (e.adapter.name(), &e.engine, e.source)
    }

    #[test]
    fn every_server_gets_the_registered_engine() {
        let mut reg = builtin().unwrap();
        reg.engines = ModuleRegistry::new().engines;
        reg.engines
            .register(pumbo_engines::PUMPKIN, || Arc::new(Fixed(STRICT)))
            .unwrap();
        reg.engines
            .register(pumbo_engines::GENERIC, pumbo_engines::plain::generic)
            .unwrap();
        let cfg = Config::parse(
            "listener:\n  - bind: \"127.0.0.1:0\"\nforwarding:\n  mode: none\n\
             servers:\n  lobby: { address: \"127.0.0.1:1\", protocol: 777 }\n",
        )
        .unwrap();
        let m = build(&reg, &cfg).unwrap();
        for protocol in [None, Some(ProtocolVersion::V777)] {
            let e = m.engines.resolve(None, None, None, protocol);
            assert_eq!(e.adapter.defaults(&e.engine), STRICT, "{protocol:?}");
            assert_eq!(e.engine, named(pumbo_engines::PUMPKIN), "{protocol:?}");
        }
    }

    #[test]
    fn the_entry_wins_then_the_bridge_then_the_brand() {
        use EngineSource::{Brand, Bridge, Declared};
        let e = engines();
        let (paper, vanilla, pumpkin) = (named("paper"), named("vanilla"), named("pumpkin"));
        let v777 = Some(ProtocolVersion::V777);
        let all = e.resolve(Some(&paper), Some(&vanilla), Some(&pumpkin), v777);
        assert_eq!(shown(&all), ("paper", &paper, Declared));
        let no_entry = e.resolve(None, Some(&vanilla), Some(&pumpkin), v777);
        assert_eq!(shown(&no_entry), ("vanilla", &vanilla, Bridge));
        let brand = e.resolve(None, None, Some(&vanilla), v777);
        assert_eq!(shown(&brand), ("vanilla", &vanilla, Brand));
        // A bridge may name an engine this build lacks: the generic adapter,
        // the bridge's engine.
        let odd = EngineRef {
            name: "stone".into(),
            version: Some("1.0".into()),
        };
        let unknown = e.resolve(None, Some(&odd), Some(&pumpkin), v777);
        assert_eq!(shown(&unknown), (NO_ENGINE, &odd, Bridge));
    }

    #[test]
    fn without_a_sign_a_server_is_pumpkin_unless_it_is_too_old() {
        let e = engines();
        for (protocol, adapter) in [
            (None, "pumpkin"),
            (Some(776), "pumpkin"),
            (Some(777), "pumpkin"),
            (Some(767), "generic"),
            (Some(775), "generic"),
        ] {
            let got = e.resolve(None, None, None, protocol.map(ProtocolVersion));
            let want = (adapter, &named(adapter), EngineSource::Assumed);
            assert_eq!(shown(&got), want, "{protocol:?}");
        }
    }

    /// Pumpkin 0.1.0-dev takes clients older than 26.2 and answers in their
    /// version; its brand still names it.
    #[test]
    fn a_pumpkin_brand_on_an_old_protocol_is_pumpkin() {
        let pumpkin = named("pumpkin");
        let got = engines().resolve(None, None, Some(&pumpkin), Some(ProtocolVersion::V767));
        assert_eq!(shown(&got), ("pumpkin", &pumpkin, EngineSource::Brand));
        assert_eq!(got.chat(None).1, SignedChatCancel::Drop);
    }

    #[test]
    fn every_server_source_has_an_engine_of_its_name() {
        let reg = builtin().unwrap();
        for source in server_sources() {
            assert!(reg.engines.get(source.name()).is_ok(), "{}", source.name());
        }
    }

    #[test]
    fn brands_are_recognised_through_the_adapters() {
        let e = engines();
        for (brand, name) in [
            ("Pumpkin", "pumpkin"),
            ("vanilla", "vanilla"),
            ("Paper", "paper"),
        ] {
            assert_eq!(e.recognise(brand), Some(named(name)), "{brand}");
        }
        for brand in ["MyNet", ""] {
            assert_eq!(e.recognise(brand), None, "{brand}");
        }
    }

    #[test]
    fn a_servers_chat_keys_win_over_its_engine() {
        let entry = |sessions, cancel| ServerConfig {
            address: "127.0.0.1:1".into(),
            protocol: ServerProtocol::Auto,
            engine: None,
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
