//! Server engine adapters: what servers of each engine need from the proxy.
//! Each registers in the `engines` slot of [`ModuleRegistry`] by its name.

use std::sync::Arc;

use pumbo_core::{ModuleError, ModuleRegistry};

pub mod plain;
pub mod pumpkin;

/// The Pumpkin adapter's name.
pub const PUMPKIN: &str = "pumpkin";
/// The vanilla server's adapter.
pub const VANILLA: &str = "vanilla";
/// Paper's adapter.
pub const PAPER: &str = "paper";
/// The adapter of a server whose engine is no other's.
pub const GENERIC: &str = "generic";

pub fn register(reg: &mut ModuleRegistry) -> Result<(), ModuleError> {
    reg.engines.register(PUMPKIN, pumpkin::adapter)?;
    reg.engines.register(VANILLA, plain::vanilla)?;
    reg.engines.register(PAPER, plain::paper)?;
    reg.engines.register(GENERIC, plain::generic)
}

/// The server software the proxy can download and run.
pub fn sources() -> Vec<Arc<dyn pumbo_servers::Source>> {
    vec![Arc::new(pumpkin::Pumpkin::default())]
}

#[cfg(test)]
mod tests {
    use pumbo_core::engine::{EngineProbe, EngineRef};

    use super::*;

    #[test]
    fn the_adapters_register_by_name() {
        let mut reg = ModuleRegistry::new();
        register(&mut reg).unwrap();
        let names: Vec<&str> = reg.engines.names().collect();
        assert_eq!(names, ["generic", "paper", "pumpkin", "vanilla"]);
        for name in names {
            assert_eq!((reg.engines.get(name).unwrap())().name(), name);
        }
    }

    #[test]
    fn brands_match_exactly() {
        let mut reg = ModuleRegistry::new();
        register(&mut reg).unwrap();
        let names: Vec<&str> = reg.engines.names().collect();
        let matched = |brand: Option<&str>| -> Vec<EngineRef> {
            let probe = EngineProbe {
                brand,
                protocol: None,
            };
            names
                .iter()
                .filter_map(|n| (reg.engines.get(n).unwrap())().matches(&probe))
                .collect()
        };
        for (brand, name) in [("Pumpkin", PUMPKIN), ("vanilla", VANILLA), ("Paper", PAPER)] {
            let want = EngineRef {
                name: name.into(),
                version: None,
            };
            assert_eq!(matched(Some(brand)), [want], "{brand}");
        }
        for brand in [
            Some("paper"),
            Some("Pumpkin (x)"),
            Some(""),
            Some("MyNet"),
            None,
        ] {
            assert_eq!(matched(brand), [], "{brand:?}");
        }
    }
}
