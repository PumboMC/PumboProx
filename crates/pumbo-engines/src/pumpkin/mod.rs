//! Pumpkin, the Rust server.

use std::sync::Arc;

use pumbo_core::engine::{
    BackendDefaults, EngineAdapter, EngineRef, PacketFilter, SignedChatCancel,
};
use pumbo_protocol::ProtocolVersion;

mod source;
mod trail;

pub use source::{PUMPKIN_API, Pumpkin, pumpkin_asset};

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

    fn quirks(&self, _: &EngineRef, wire: ProtocolVersion) -> Option<Box<dyn PacketFilter>> {
        trail::TrailFix::for_version(wire).map(|f| Box::new(f) as Box<dyn PacketFilter>)
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
