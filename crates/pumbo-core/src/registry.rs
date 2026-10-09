//! Registry of module factories. The binary registers the built-in modules and
//! the config picks them by name. The core imports no implementation.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::BoxFuture;
use crate::forwarding::ForwardingMode;
use crate::identity::Authenticator;
use crate::listener::Listener;
use crate::routing::BackendSelector;
use crate::storage::KeyValueStore;
use crate::translate::TranslatorProvider;

/// Config section handed to a module factory (a mapping of `pumboprox.yml`).
pub type ModuleConfig = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("unknown {kind} module \"{name}\" (available: {available})")]
    Unknown {
        kind: &'static str,
        name: String,
        available: String,
    },
    #[error("{kind} module \"{name}\" is already registered")]
    Duplicate { kind: &'static str, name: String },
    #[error("configuration of module \"{name}\": {message}")]
    Config { name: String, message: String },
    #[error("starting module \"{name}\": {source}")]
    Io {
        name: String,
        #[source]
        source: std::io::Error,
    },
}

pub type ListenerFactory =
    fn(&ModuleConfig) -> BoxFuture<'_, Result<Box<dyn Listener>, ModuleError>>;
pub type ForwardingFactory = fn(&ModuleConfig) -> Result<Arc<dyn ForwardingMode>, ModuleError>;
pub type SelectorFactory = fn(&ModuleConfig) -> Result<Arc<dyn BackendSelector>, ModuleError>;
pub type TranslatorFactory = fn(&ModuleConfig) -> Result<Arc<dyn TranslatorProvider>, ModuleError>;
pub type AuthenticatorFactory = fn(&ModuleConfig) -> Result<Arc<dyn Authenticator>, ModuleError>;
pub type StoreFactory = fn(&ModuleConfig) -> Result<Arc<dyn KeyValueStore>, ModuleError>;

/// One family of modules: name → factory.
#[derive(Debug)]
pub struct Slot<F> {
    kind: &'static str,
    factories: BTreeMap<String, F>,
}

impl<F: Copy> Slot<F> {
    fn new(kind: &'static str) -> Self {
        Self {
            kind,
            factories: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, name: &str, factory: F) -> Result<(), ModuleError> {
        if self.factories.contains_key(name) {
            return Err(ModuleError::Duplicate {
                kind: self.kind,
                name: name.to_string(),
            });
        }
        self.factories.insert(name.to_string(), factory);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Result<F, ModuleError> {
        self.factories
            .get(name)
            .copied()
            .ok_or_else(|| ModuleError::Unknown {
                kind: self.kind,
                name: name.to_string(),
                available: self.names().collect::<Vec<_>>().join(", "),
            })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.factories.keys().map(String::as_str)
    }
}

/// All extension points of the proxy.
#[derive(Debug)]
pub struct ModuleRegistry {
    pub listeners: Slot<ListenerFactory>,
    pub forwarding: Slot<ForwardingFactory>,
    pub selectors: Slot<SelectorFactory>,
    pub translators: Slot<TranslatorFactory>,
    pub authenticators: Slot<AuthenticatorFactory>,
    pub stores: Slot<StoreFactory>,
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self {
            listeners: Slot::new("listener"),
            forwarding: Slot::new("forwarding"),
            selectors: Slot::new("selector"),
            translators: Slot::new("translator"),
            authenticators: Slot::new("authenticator"),
            stores: Slot::new("store"),
        }
    }
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Helper for factories: reads an optional string from the section.
pub fn opt_str<'a>(
    cfg: &'a ModuleConfig,
    module: &str,
    key: &str,
) -> Result<Option<&'a str>, ModuleError> {
    match cfg.get(key) {
        None => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ModuleError::Config {
            name: module.to_string(),
            message: format!("\"{key}\" must be a string"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::{BackendInfo, SelectContext};

    #[derive(Debug)]
    struct First;
    impl BackendSelector for First {
        fn name(&self) -> &str {
            "first"
        }
        fn candidates(&self, _: &SelectContext<'_>, b: &[BackendInfo]) -> Vec<String> {
            b.iter().take(1).map(|b| b.name.clone()).collect()
        }
    }

    fn first(_: &ModuleConfig) -> Result<Arc<dyn BackendSelector>, ModuleError> {
        Ok(Arc::new(First))
    }

    #[test]
    fn registry_by_name() {
        let mut reg = ModuleRegistry::new();
        reg.selectors.register("first", first).unwrap();
        assert!(matches!(
            reg.selectors.register("first", first),
            Err(ModuleError::Duplicate { .. })
        ));
        let sel = (reg.selectors.get("first").unwrap())(&ModuleConfig::new()).unwrap();
        assert_eq!(sel.name(), "first");
        let err = reg.selectors.get("balancer").map(|_| ()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown selector module \"balancer\" (available: first)"
        );
    }
}
