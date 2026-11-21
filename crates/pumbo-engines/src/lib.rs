//! Server engine adapters: what servers of each engine need from the proxy.
//! Each registers in the `engines` slot of [`ModuleRegistry`] by its name.

use std::sync::Arc;

use pumbo_core::{ModuleError, ModuleRegistry};

pub mod pumpkin;

/// The Pumpkin adapter's name.
pub const PUMPKIN: &str = "pumpkin";

pub fn register(reg: &mut ModuleRegistry) -> Result<(), ModuleError> {
    reg.engines.register(PUMPKIN, pumpkin::adapter)
}

/// The server software the proxy can download and run.
pub fn sources() -> Vec<Arc<dyn pumbo_servers::Source>> {
    vec![Arc::new(pumpkin::Pumpkin::default())]
}
