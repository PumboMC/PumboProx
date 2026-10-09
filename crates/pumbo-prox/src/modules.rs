//! Registering the built-in modules and building them from the config.

use std::sync::Arc;

use pumbo_core::forwarding::ForwardingMode;
use pumbo_core::identity::Authenticator;
use pumbo_core::registry::{ModuleConfig, ModuleError, ModuleRegistry};
use pumbo_core::routing::BackendSelector;
use pumbo_core::storage::KeyValueStore;
use pumbo_core::translate::TranslatorProvider;
use pumbo_protocol::VersionRegistry;

use crate::config::Config;

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
    Ok(reg)
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
}

pub fn build(reg: &ModuleRegistry, cfg: &Config) -> Result<Modules, ModuleError> {
    for l in &cfg.listener {
        reg.listeners.get(&l.transport)?;
    }
    let forwarding = (reg.forwarding.get(&cfg.forwarding.mode)?)(&cfg.forwarding.rest)?;
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
    let translators = cfg
        .translation
        .chain
        .iter()
        .map(|name| {
            let section = cfg.sections.get(name).cloned().unwrap_or_default();
            (reg.translators.get(name)?)(&section)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let store = (reg.stores.get(&cfg.storage.backend)?)(&cfg.storage.rest)?;
    let authenticator =
        (reg.authenticators.get(&cfg.authentication.service)?)(&cfg.authentication.rest)?;
    Ok(Modules {
        forwarding,
        selector,
        translators,
        store,
        authenticator,
    })
}

/// A `listener` section as module config.
pub fn listener_config(l: &crate::config::ListenerConfig) -> ModuleConfig {
    let mut t = l.rest.clone();
    t.insert("bind".into(), l.bind.clone().into());
    t
}
