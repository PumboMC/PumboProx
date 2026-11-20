//! The engine's repairs of a server connection: its packet filter, the
//! packets the filter reads and what a repair reports.

use pumbo_core::engine::{Filtered, PacketFilter, Repair};
use pumbo_protocol::{Direction, KNOWN_PACKETS, PacketKind, Phase, RawFrame};
use tracing::warn;

use crate::metrics::Metrics;
use crate::server::Proxy;
use crate::session::Fail;

use super::hooks::{Kinds, has, kinds};
use super::{Play, Step};

/// A server connection's packet filter and the packets it reads.
pub(super) struct Quirks {
    filter: Box<dyn PacketFilter>,
    wants: Kinds,
}

impl Quirks {
    pub(super) fn new(filter: Box<dyn PacketFilter>) -> Self {
        let list = filter.interests();
        // A kind that never reaches the filter would leave its repair silent.
        debug_assert!(delivered(list), "{list:?}");
        Quirks {
            wants: kinds(list),
            filter,
        }
    }
}

impl Play<'_> {
    #[inline]
    pub(super) fn quirks_want(&self, kind: PacketKind) -> bool {
        self.backend
            .as_ref()
            .and_then(|b| b.quirks.as_ref())
            .is_some_and(|q| has(&q.wants, kind))
    }

    /// `Step::Forward` for a frame the filter leaves alone; a replaced one is
    /// counted, reported once per server, queued for the client and dropped.
    pub(super) fn quirk(
        &mut self,
        phase: Phase,
        kind: PacketKind,
        f: &RawFrame,
    ) -> Result<Step, Fail> {
        let Some(b) = self.backend.as_mut() else {
            return Ok(Step::Forward);
        };
        let Some(q) = b.quirks.as_mut() else {
            return Ok(Step::Forward);
        };
        match q.filter.backend_to_client(phase, kind, f) {
            Filtered::Forward => Ok(Step::Forward),
            Filtered::Replace { payload, repair } => {
                report_repair(self.proxy, &b.name, repair);
                self.client.queue(f.id, &payload)?;
                Ok(Step::Drop)
            }
        }
    }
}

/// Whether every kind of `list` can reach a filter: a play packet from the
/// server other than its `login` and `disconnect`, which the core ends with.
pub(super) fn delivered(list: &[PacketKind]) -> bool {
    list.iter().all(|k| {
        !matches!(k, PacketKind::Login | PacketKind::Disconnect)
            && KNOWN_PACKETS.iter().any(|(phase, d, kind)| {
                *phase == Phase::Play && *d == Direction::Clientbound && kind == k
            })
    })
}

/// Counts a repair and logs the first one of each server.
pub(crate) fn report_repair(proxy: &Proxy, server: &str, repair: &'static Repair) {
    if let Some(c) = proxy.metrics.fixed(repair.kind) {
        Metrics::inc(c);
    }
    if proxy.first_report(repair.kind, server) {
        warn!(
            "fixed {} from {server} ({}); this stops by itself once the server sends correct packets",
            repair.what, repair.cause
        );
    }
}
