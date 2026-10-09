//! Extension point: translating packets between the client's and the
//! backend's version.
//!
//! A chain of providers decides on every backend connection what to do with a
//! version difference (§3.2): pass frames through, send the client to an
//! external translator (ViaProxy), or translate inside the proxy
//! ([`TranslationPlan::Translate`]).

use std::net::SocketAddr;

use pumbo_protocol::{ProtocolVersion, RawFrame};

/// Frames one translation produced. A translator may answer the side it got
/// the frame from (e.g. accept something the client cannot show).
#[derive(Debug, Default)]
pub struct Translated {
    pub to_client: Vec<RawFrame>,
    pub to_backend: Vec<RawFrame>,
}

/// Translator of one backend connection, from the end of the backend login
/// (stateful: entity types, registry IDs and the phase of each stream are per
/// connection).
pub trait PacketTranslator: Send {
    fn client_to_backend(&mut self, frame: &RawFrame, out: &mut Translated);
    fn backend_to_client(&mut self, frame: &RawFrame, out: &mut Translated);
}

/// Decision for a pair of versions.
pub enum TranslationPlan {
    /// Equal versions: frames pass unchanged.
    Passthrough,
    /// The client goes through an external translator (ViaProxy) at this address (§8).
    External { address: SocketAddr },
    /// Translation inside the proxy process.
    Translate(Box<dyn PacketTranslator>),
}

impl std::fmt::Debug for TranslationPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Passthrough => f.write_str("Passthrough"),
            Self::External { address } => write!(f, "External({address})"),
            Self::Translate(_) => f.write_str("Translate(..)"),
        }
    }
}

/// Translation provider. The proxy asks providers in config order; the first
/// that returns `Some` wins. No decision means a "version mismatch" kick.
pub trait TranslatorProvider: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    /// `backend == None`: decision at handshake, before the backend is known.
    fn plan(
        &self,
        client: ProtocolVersion,
        backend: Option<ProtocolVersion>,
    ) -> Option<TranslationPlan>;
}
