//! The teleport gate as a session hook. The bridge holds a teleport while the
//! client has another one to confirm, so the hook follows the server's
//! `player_position`, the client's `accept_teleportation` and each play
//! `login`.

use std::sync::Arc;

use pumbo_core::hook::{
    HookCx, HookInterests, SessionHook, SessionHookFactory, SessionInfo, Verdict,
};
use pumbo_protocol::packets::world::{AcceptTeleportation, PlayerPosition};
use pumbo_protocol::{Direction, PacketKind, RawFrame, packets};
use tokio::time::Instant;

use crate::server::Proxy;
use crate::session::ctx;

use super::{Tp, TpGate};

/// Makes the gate hook of each session.
pub(crate) struct Factory<'a>(pub(crate) &'a Arc<Proxy>);

impl SessionHookFactory for Factory<'_> {
    fn for_session(&self, s: &SessionInfo<'_>) -> Option<Box<dyn SessionHook>> {
        // Without a gate (a poisoned player list) the packets are still
        // read, and a broken one still ends the session.
        Some(Box::new(Hook(self.0.teleport_gate(s.player.profile.id))))
    }
}

struct Hook(Option<TpGate>);

impl SessionHook for Hook {
    fn interests(&self) -> HookInterests {
        HookInterests {
            client: &[PacketKind::AcceptTeleportation],
            backend: &[PacketKind::PlayerPosition],
            ..HookInterests::default()
        }
    }

    /// A teleport the client must confirm: PumboBridge holds the next one
    /// until then ("Wrong teleport id" otherwise).
    fn on_backend(&mut self, cx: &mut HookCx<'_>, f: &RawFrame) -> Verdict {
        let cb = ctx(cx.module, Direction::Clientbound);
        match packets::decode::<PlayerPosition>(&f.payload, &cb) {
            Ok(p) => {
                if let Some(g) = &self.0 {
                    g.send_replace(Tp::Awaiting(p.teleport_id));
                }
                Verdict::Forward
            }
            Err(e) => Verdict::End(e.to_string()),
        }
    }

    fn on_client(&mut self, cx: &mut HookCx<'_>, f: &RawFrame) -> Verdict {
        let sb = ctx(cx.module, Direction::Serverbound);
        match packets::decode::<AcceptTeleportation>(&f.payload, &sb) {
            Ok(a) => {
                if let Some(g) = &self.0 {
                    g.send_if_modified(|t| {
                        let done = *t == Tp::Awaiting(a.id);
                        if done {
                            *t = Tp::Confirmed(Instant::now());
                        }
                        done
                    });
                }
                Verdict::Forward
            }
            Err(e) => Verdict::End(e.to_string()),
        }
    }

    /// The spawn teleport is due; set before the proxy records the new
    /// server, so PumboBridge's `send-to` never sees it with a free gate.
    fn on_joined(&mut self, _: &mut HookCx<'_>) {
        if let Some(g) = &self.0 {
            g.send_replace(Tp::Sent(Instant::now()));
        }
    }
}
