//! Pumpkin, the Rust server.

use std::sync::Arc;

use pumbo_core::engine::{BackendDefaults, EngineAdapter, EngineRef, SignedChatCancel};

#[derive(Debug)]
pub struct PumpkinAdapter;

pub fn adapter() -> Arc<dyn EngineAdapter> {
    Arc::new(PumpkinAdapter)
}

impl EngineAdapter for PumpkinAdapter {
    fn name(&self) -> &str {
        crate::PUMPKIN
    }

    /// Pumpkin does not check the chat chain, so a cancelled signed message
    /// is dropped (E4 test).
    fn defaults(&self, _: &EngineRef) -> BackendDefaults {
        BackendDefaults {
            signed_chat_cancel: SignedChatCancel::Drop,
            chat_session_forwarding: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use pumbo_core::ModuleRegistry;

    use super::*;

    fn pumpkin() -> EngineRef {
        EngineRef {
            name: crate::PUMPKIN.into(),
            version: None,
        }
    }

    #[test]
    fn the_adapter_registers_as_pumpkin() {
        let mut reg = ModuleRegistry::new();
        crate::register(&mut reg).unwrap();
        assert_eq!(reg.engines.names().collect::<Vec<_>>(), ["pumpkin"]);
        assert_eq!((reg.engines.get("pumpkin").unwrap())().name(), "pumpkin");
    }

    #[test]
    fn pumpkin_keeps_todays_defaults() {
        let d = PumpkinAdapter.defaults(&pumpkin());
        assert_eq!(
            d,
            BackendDefaults {
                signed_chat_cancel: SignedChatCancel::Drop,
                chat_session_forwarding: true,
            }
        );
    }
}
