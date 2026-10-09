//! Modules that forward the player's identity to the backend.

pub mod modern;
pub mod none;

use pumbo_core::registry::{ModuleError, ModuleRegistry};

pub fn register(reg: &mut ModuleRegistry) -> Result<(), ModuleError> {
    reg.forwarding.register(modern::NAME, modern::factory)?;
    reg.forwarding.register(none::NAME, none::factory)?;
    Ok(())
}
