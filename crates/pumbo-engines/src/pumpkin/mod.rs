//! Pumpkin, the Rust server.

use std::sync::Arc;

use pumbo_core::engine::{
    BackendDefaults, EngineAdapter, EngineProbe, EngineRef, PacketFilter, SignedChatCancel,
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

    fn matches(&self, probe: &EngineProbe<'_>) -> Option<EngineRef> {
        (probe.brand == Some("Pumpkin")).then(|| EngineRef {
            name: crate::PUMPKIN.into(),
            version: None,
        })
    }

    /// No Pumpkin release has an own version older than 26.2.
    fn speaks(&self, protocol: ProtocolVersion) -> bool {
        protocol >= ProtocolVersion::V776
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pumpkin() -> EngineRef {
        EngineRef {
            name: crate::PUMPKIN.into(),
            version: None,
        }
    }

    #[test]
    fn pumpkin_starts_at_26_2() {
        assert!(!PumpkinAdapter.speaks(ProtocolVersion::V775));
        for p in [776, 777, 778] {
            assert!(PumpkinAdapter.speaks(ProtocolVersion(p)), "{p}");
        }
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
