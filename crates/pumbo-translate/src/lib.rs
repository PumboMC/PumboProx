//! Translation providers: `passthrough` (same versions) and, with the
//! feature of that name, `multiversion` (translation inside the proxy,
//! `pumbo-translate-mv`).

#[cfg(feature = "multiversion")]
pub mod multiversion;
mod pairs;

pub use pairs::ExperimentalPairs;

use std::sync::Arc;

use pumbo_core::registry::{ModuleConfig, ModuleError, ModuleRegistry};
use pumbo_core::section::{SectionInfo, keys_schema};
use pumbo_core::translate::{TranslationPlan, TranslatorProvider};
use pumbo_protocol::ProtocolVersion;
use serde::Deserialize;

pub const PASSTHROUGH: &str = "passthrough";
/// The translator inside the proxy, built with the `multiversion` feature.
pub const MULTIVERSION: &str = "multiversion";
/// Whether this build has [`MULTIVERSION`]; the proxy asks here.
pub const HAS_MULTIVERSION: bool = cfg!(feature = "multiversion");
/// The key of `translation` that lists the experimental pairs turned on.
pub const EXPERIMENTAL_PAIRS: &str = "experimental-pairs";

/// The `translation` section of `pumboprox.yml`. Providers read their keys
/// here; they have no sections of their own.
#[derive(Debug, Clone, Deserialize)]
pub struct TranslationConfig {
    /// Provider order; the first with a decision wins.
    #[serde(default = "default_chain")]
    pub chain: Vec<String>,
    /// Experimental pairs the owner turns on, each as "<client release>><server release>".
    #[serde(default, rename = "experimental-pairs")]
    pub experimental_pairs: Vec<String>,
}

fn default_chain() -> Vec<String> {
    let mut chain = Vec::new();
    if HAS_MULTIVERSION {
        chain.push(MULTIVERSION.to_string());
    }
    chain.push(PASSTHROUGH.to_string());
    chain
}

impl Default for TranslationConfig {
    fn default() -> Self {
        Self {
            chain: default_chain(),
            experimental_pairs: Vec::new(),
        }
    }
}

/// The section of this module, with the schema of its keys. The proxy
/// parses it with its own sections (it is a field of the proxy's config), so
/// for the rules on unknown keys it counts as one of the proxy's.
pub fn sections() -> Vec<SectionInfo> {
    vec![SectionInfo {
        key: "translation",
        schema: keys_schema::<TranslationConfig>(),
    }]
}

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
    #[cfg(feature = "multiversion")]
    reg.translators
        .register(MULTIVERSION, multiversion::factory)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumbo_core::section::fields;

    /// The registered section is closed and has the fields of its struct.
    #[test]
    fn the_section_has_the_fields_of_its_struct() {
        let s = sections();
        assert_eq!(s.iter().map(|s| s.key).collect::<Vec<_>>(), ["translation"]);
        let keys: Vec<&str> = s[0].schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, fields::<TranslationConfig>());
        assert_eq!(s[0].schema["additionalProperties"], false);
    }
}
