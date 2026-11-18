//! Translation providers: `passthrough` (same versions) and `multiversion`
//! (translation inside the proxy, `pumbo-translate-mv`).

pub mod multiversion;

use std::sync::Arc;

use pumbo_core::registry::{ModuleConfig, ModuleError, ModuleRegistry};
use pumbo_core::translate::{TranslationPlan, TranslatorProvider};
use pumbo_protocol::ProtocolVersion;

pub const PASSTHROUGH: &str = "passthrough";

/// Equal versions (or the backend not known yet): no translation.
#[derive(Debug)]
pub struct Passthrough;

impl TranslatorProvider for Passthrough {
    fn name(&self) -> &str {
        PASSTHROUGH
    }

    fn plan(
        &self,
        client: ProtocolVersion,
        backend: Option<ProtocolVersion>,
    ) -> Option<TranslationPlan> {
        match backend {
            None => Some(TranslationPlan::Passthrough),
            Some(b) if b == client => Some(TranslationPlan::Passthrough),
            Some(_) => None,
        }
    }
}

fn passthrough_factory(_: &ModuleConfig) -> Result<Arc<dyn TranslatorProvider>, ModuleError> {
    Ok(Arc::new(Passthrough))
}

pub fn register(reg: &mut ModuleRegistry) -> Result<(), ModuleError> {
    reg.translators.register(PASSTHROUGH, passthrough_factory)?;
    reg.translators
        .register(multiversion::MULTIVERSION, multiversion::factory)?;
    Ok(())
}
