//! Servers that need nothing special from the proxy: vanilla, Paper and
//! every other server the proxy finds no engine for.

use std::sync::Arc;

use pumbo_core::engine::{
    BackendDefaults, EngineAdapter, EngineProbe, EngineRef, SignedChatCancel,
};

/// A server that needs nothing special: a cancelled signed message
/// disconnects the player, no repairs.
#[derive(Debug)]
pub struct Plain {
    name: &'static str,
    /// The `minecraft:brand` its servers send, exactly.
    brand: Option<&'static str>,
}

pub fn vanilla() -> Arc<dyn EngineAdapter> {
    Arc::new(Plain {
        name: crate::VANILLA,
        brand: Some("vanilla"),
    })
}

pub fn paper() -> Arc<dyn EngineAdapter> {
    Arc::new(Plain {
        name: crate::PAPER,
        brand: Some("Paper"),
    })
}

pub fn generic() -> Arc<dyn EngineAdapter> {
    Arc::new(Plain {
        name: crate::GENERIC,
        brand: None,
    })
}

impl EngineAdapter for Plain {
    fn name(&self) -> &str {
        self.name
    }

    /// These servers check the chat chain: after a gap every later signed
    /// message of the player is rejected, so the player is told and leaves.
    fn defaults(&self, _: &EngineRef) -> BackendDefaults {
        BackendDefaults {
            signed_chat_cancel: SignedChatCancel::Kick,
            chat_session_forwarding: true,
        }
    }

    fn matches(&self, probe: &EngineProbe<'_>) -> Option<EngineRef> {
        (self.brand.is_some() && probe.brand == self.brand).then(|| EngineRef {
            name: self.name.into(),
            version: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use pumbo_protocol::ProtocolVersion;

    use super::*;

    #[test]
    fn plain_servers_kick_and_repair_nothing() {
        for adapter in [vanilla(), paper(), generic()] {
            let engine = EngineRef {
                name: adapter.name().into(),
                version: None,
            };
            let d = adapter.defaults(&engine);
            assert_eq!(d.signed_chat_cancel, SignedChatCancel::Kick, "{adapter:?}");
            assert!(d.chat_session_forwarding, "{adapter:?}");
            for p in [ProtocolVersion::V776, ProtocolVersion::V777] {
                assert!(adapter.quirks(&engine, p).is_none(), "{adapter:?}");
            }
        }
    }
}
