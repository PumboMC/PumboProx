//! The native service `pumbo:bridge@1.0`: calls, teleports and `send-to`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::future::BoxFuture;
use pumbo_bridge_proto::api;
use pumbo_bridge_proto::wire::Msg;
use pumbo_bridge_proto::{err, method};
use pumbo_host::wit::services::ServiceError;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::server::SessionCmd;

use super::{ARRIVE_TTL_MS, Bridge, Link, Reply, TELEPORT_WAIT, Tp, TpGate, cbor, rejected};

impl Bridge {
    /// A call of `pumbo:bridge@1.0` by plugin `caller`.
    async fn service(
        &self,
        caller: &str,
        m: &str,
        payload: &[u8],
    ) -> Result<Vec<u8>, ServiceError> {
        let is_command = method::COMMANDS.contains(&m);
        if !is_command && !method::QUERIES.contains(&m) {
            return Err(
                if [method::PERM_SET, method::PERMS_EXPORT, method::GUARD_RULES].contains(&m) {
                    rejected(err::NOT_ALLOWED, None)
                } else {
                    ServiceError::UnknownMethod
                },
            );
        }
        if is_command && !self.cfg.trusted_plugins.iter().any(|p| p == caller) {
            return Err(rejected(err::NOT_ALLOWED, None));
        }
        let bad = |e: String| rejected(err::BAD_ARGS, Some(e));
        if m == method::STATUS {
            let s: api::Status = ciborium::from_reader(payload).map_err(|e| bad(e.to_string()))?;
            return cbor(&self.status(s.server.as_deref()));
        }
        if m == method::SEND_TO {
            let s: api::SendTo = ciborium::from_reader(payload).map_err(|e| bad(e.to_string()))?;
            return self.send_to(s);
        }
        let args: ciborium::Value =
            ciborium::from_reader(payload).map_err(|e| bad(e.to_string()))?;
        let route: api::Route = args.deserialized().unwrap_or_default();
        let server = match route.server {
            Some(s) => s,
            None => {
                let who = route
                    .player
                    .or(route.viewer)
                    .ok_or_else(|| bad("server or player needed".into()))?;
                self.proxy()
                    .and_then(|p| p.server_of(who))
                    .ok_or_else(|| rejected(err::OFFLINE, None))?
            }
        };
        let v = self
            .exec(&server, m, args, route.player)
            .await
            .map_err(|(c, d)| rejected(&c, d))?;
        cbor(&v)
    }

    /// Runs one method on a server's bridge (teleports wait for the
    /// previous one first).
    async fn exec(
        &self,
        server: &str,
        m: &str,
        args: ciborium::Value,
        player: Option<Uuid>,
    ) -> Reply {
        let link = self
            .link(server)
            .ok_or_else(|| (err::NO_BRIDGE.to_string(), None))?;
        let caps_known = link
            .caps
            .lock()
            .map(|c| !c.is_empty() && !c.iter().any(|x| x == m));
        if caps_known.unwrap_or(false) {
            return Err((err::UNSUPPORTED.into(), None));
        }
        let gate = match (m == method::TELEPORT, player) {
            (true, Some(p)) => {
                let gate = self.proxy().and_then(|x| x.teleport_gate(p));
                if let Some(g) = &gate {
                    self.hold(p, g).await.map_err(|c| (c.to_string(), None))?;
                }
                gate
            }
            _ => None,
        };
        let r = self.call(&link, m, args).await;
        if let Some(g) = gate
            && r.is_err()
        {
            g.send_if_modified(|t| {
                let sent = matches!(t, Tp::Sent(_));
                if sent {
                    *t = Tp::Free;
                }
                sent
            });
        }
        r
    }

    async fn call(&self, link: &Link, m: &str, args: ciborium::Value) -> Reply {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = link.pending.lock() {
            p.insert(id, tx);
        }
        let sent = link.send(Msg::Cmd {
            id,
            method: m.into(),
            args,
        });
        let forget = || {
            if let Ok(mut p) = link.pending.lock() {
                p.remove(&id);
            }
        };
        if !sent {
            forget();
            return Err((err::BUSY.into(), None));
        }
        let wait = Duration::from_millis(self.cfg.command_timeout_ms);
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err((err::DISCONNECTED.into(), None)),
            Err(_) => {
                // A late answer finds no waiter and is dropped.
                forget();
                Err((err::TIMEOUT.into(), None))
            }
        }
    }

    /// Waits until the player has no unconfirmed teleport (spec §4.2).
    async fn hold(&self, player: Uuid, gate: &TpGate) -> Result<(), &'static str> {
        let generation = self.next.fetch_add(1, Ordering::Relaxed);
        let (cancel_tx, mut cancel) = oneshot::channel();
        if let Some((_, old)) = self.lock().held.insert(player, (generation, cancel_tx)) {
            let _ = old.send(());
        }
        let deadline = Instant::now() + TELEPORT_WAIT;
        let mut rx = gate.subscribe();
        let r = loop {
            let state = *rx.borrow_and_update();
            let Some(until) = state.busy() else {
                break Ok(());
            };
            let wake = until.unwrap_or(deadline).min(deadline);
            tokio::select! {
                _ = &mut cancel => break Err(err::SUPERSEDED),
                () = tokio::time::sleep_until(deadline) => break Err(err::EXPIRED),
                r = rx.changed() => if r.is_err() { break Ok(()) },
                () = tokio::time::sleep_until(wake) => {}
            }
        };
        {
            let mut st = self.lock();
            if st.held.get(&player).is_some_and(|(g, _)| *g == generation) {
                st.held.remove(&player);
            }
        }
        if r.is_ok() {
            gate.send_replace(Tp::Sent(Instant::now()));
        }
        r
    }

    /// `send-to`: `no-bridge` before the player moves; the actions run on
    /// the target after the player joined and confirmed its spawn teleport.
    fn send_to(&self, s: api::SendTo) -> Result<Vec<u8>, ServiceError> {
        if self.link(&s.server).is_none() {
            return Err(rejected(err::NO_BRIDGE, None));
        }
        let proxy = self.proxy().ok_or_else(|| rejected(err::OFFLINE, None))?;
        let queued = proxy.send_to(
            s.player,
            SessionCmd::Connect {
                server: s.server.clone(),
                quiet: false,
            },
        );
        if !queued {
            return Err(rejected(err::OFFLINE, None));
        }
        if !s.actions.is_empty()
            && let Some(me) = self.me.upgrade()
        {
            tokio::spawn(async move { me.arrive(s).await });
        }
        cbor(&())
    }

    async fn arrive(self: Arc<Self>, s: api::SendTo) {
        let deadline =
            Instant::now() + Duration::from_millis(u64::from(s.ttl_ms.unwrap_or(ARRIVE_TTL_MS)));
        let arrived = loop {
            let Some(proxy) = self.proxy() else { return };
            // Server first: the session marks the spawn teleport as due
            // before it changes the server.
            let there = proxy.server_of(s.player).as_deref() == Some(s.server.as_str());
            let free = proxy
                .teleport_gate(s.player)
                .is_some_and(|g| g.borrow().busy().is_none());
            if there && free {
                break true;
            }
            if Instant::now() >= deadline || proxy.teleport_gate(s.player).is_none() {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        if !arrived {
            debug!(
                "send-to {}: {} did not arrive in time, {} actions dropped",
                s.server,
                s.player,
                s.actions.len()
            );
            return;
        }
        for a in s.actions {
            if let Err((c, d)) = self
                .exec(&s.server, &a.method, a.args, Some(s.player))
                .await
            {
                warn!(
                    "send-to {}: {} for {} failed: {c} {}",
                    s.server,
                    a.method,
                    s.player,
                    d.unwrap_or_default()
                );
            }
        }
    }
}

/// The native provider of `pumbo:bridge@1.0`.
pub(super) struct Service(pub(super) Arc<Bridge>);

impl pumbo_host::NativeService for Service {
    fn call(
        &self,
        caller: String,
        method: String,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<Vec<u8>, ServiceError>> {
        let b = self.0.clone();
        Box::pin(async move { b.service(&caller, &method, &payload).await })
    }

    fn admin(&self, args: &[String], console: bool, secrets: bool) -> Vec<String> {
        self.0.admin(args, console, secrets)
    }
}
