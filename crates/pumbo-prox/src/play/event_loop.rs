//! The session loop: one `select!` per session over the client, the
//! current server, the next one, the session's commands and its timers.

use std::sync::Arc;
use std::time::Duration;

use pumbo_protocol::packets::common::KeepAlive;
use pumbo_text::Component;
use tokio::time::{Instant, sleep_until};

use crate::conn::{Conn, ConnError, WRITE_TIMEOUT};
use crate::metrics::Metrics;
use crate::session::{queue_out, random_bytes, stopped};

use super::switching::next_event;
use super::{BACKLOG, End, KickFuture, Next, OWN_KEEP_ALIVE, Play, end_kick};

async fn kick_decision(
    f: &mut Option<KickFuture>,
) -> (crate::plugins::KickDecision, Box<Component>, bool) {
    match f {
        Some(fut) => fut.as_mut().await,
        None => std::future::pending().await,
    }
}

/// Writes the current server and, with `read`, reads it ([`Conn::fill_writing`]).
async fn backend_io(
    conn: Option<&mut Conn>,
    room: Option<usize>,
    read: bool,
) -> Result<Option<usize>, ConnError> {
    match conn {
        Some(c) => c.fill_writing(room, read).await,
        None => std::future::pending().await,
    }
}

impl Play<'_> {
    pub(super) async fn run(&mut self) -> End {
        let m = self.proxy.metrics.clone();
        let idle = Duration::from_millis(self.rt.config.limits.idle_timeout_ms);
        let mut shutdown = self.proxy.shutdown_signal();
        let mut changes = self.proxy.runtime_changes();
        // Built once, not on every turn of the loop: each new timer and Notify
        // registration takes a lock in the runtime.
        let stop = stopped(&mut shutdown);
        let commands_changed = self.commands_changed.clone();
        let commands = commands_changed.notified();
        let timer = sleep_until(self.deadline(idle));
        let urgent = self.urgent.clone();
        let urgent_signal = urgent.notify.notified();
        tokio::pin!(stop, commands, timer, urgent_signal);
        loop {
            if let Some(cmd) = urgent.take()
                && let Some(end) = self.urgent_cmd(cmd)
            {
                return end;
            }
            self.follow_runtime();
            if let Err(end) = self.progress() {
                return end;
            }
            if let Err(end) = self.write_servers() {
                return end;
            }
            // Every client packet moves the deadline later. The armed timer is
            // only moved earlier, or re-armed once it fired: an early wake is
            // harmless, `timers` checks each deadline against the clock.
            let deadline = self.deadline(idle);
            if deadline < timer.deadline() || timer.is_elapsed() {
                timer.as_mut().reset(deadline);
            }
            // While a kick waits for plugins the backend is not read any more,
            // nor while the client has a backlog; the client is not read while
            // the backend has one. Both are still written.
            let kick_pending = self.pending_kick.is_some();
            let client_full = self.client_full();
            let backend_full = self.backend_full();
            tokio::select! {
                biased;
                () = &mut stop => return end_kick("The proxy is shutting down."),
                // Taken at the top of the loop.
                () = &mut urgent_signal => urgent_signal.set(urgent.notify.notified()),
                // The client is written here, so it is read while it takes its
                // data slowly: holding its packets back would pass them on in
                // one burst later, over the limits of the proxy and backend.
                r = self.client.fill_writing(client_full.then_some(BACKLOG), !backend_full) => {
                    let n = match r {
                        Ok(n) => n,
                        Err(e) => return End::client_lost(e),
                    };
                    // It took its backlog: the backend is read again.
                    let Some(n) = n else { continue };
                    self.last_client = Instant::now();
                    Metrics::add(&m.bytes_from_clients, n as u64);
                    if !self.bytes.take(n as f64) {
                        Metrics::inc(&m.kicked_packet_rate);
                        return end_kick("Too many packets.");
                    }
                    if let Some(end) = self.client_frames().await {
                        return end;
                    }
                }
                d = kick_decision(&mut self.pending_kick) => {
                    self.pending_kick = None;
                    if let Some(end) = self.on_kick(d) {
                        return end;
                    }
                }
                r = backend_io(
                    self.backend.as_mut().map(|b| &mut b.conn),
                    backend_full.then_some(BACKLOG),
                    !kick_pending && !client_full,
                ) => {
                    let end = match r {
                        // It took its backlog: the client is read again.
                        Ok(None) => continue,
                        Ok(Some(n)) => {
                            Metrics::add(&m.bytes_from_backends, n as u64);
                            if let Some(b) = self.backend.as_mut() {
                                b.last_read = Instant::now();
                            }
                            self.backend_frames()
                        }
                        Err(_) => self.backend_lost(None),
                    };
                    if let Some(end) = end {
                        return end;
                    }
                }
                ev = next_event(&mut self.next) => {
                    if let Some(end) = self.on_next(ev) {
                        return end;
                    }
                }
                Some(cmd) = self.cmds.recv() => {
                    if let Some(end) = self.command(cmd) {
                        return end;
                    }
                }
                () = &mut commands => {
                    // A change stored meanwhile is covered by this rebuild.
                    commands.set(commands_changed.notified());
                    if commands.as_mut().enable() {
                        commands.set(commands_changed.notified());
                    }
                    self.on_commands_changed();
                }
                // The Pumbox waits for a new server list (`progress`).
                _ = changes.changed(), if self.pumbox.is_some() => {}
                () = &mut timer => {
                    if let Some(end) = self.timers(idle) {
                        return end;
                    }
                }
            }
        }
    }

    /// A reload or a change of the servers run by the proxy replaced the
    /// runtime: the server list, commands and settings follow it at once (the
    /// idle limit at the next session), the client gets the command tree again.
    pub(super) fn follow_runtime(&mut self) {
        let now = self.proxy.runtime();
        if !Arc::ptr_eq(&now, &self.rt) {
            self.rt = now;
            self.resend_commands();
        }
    }

    /// The client is behind by more than [`BACKLOG`].
    fn client_full(&self) -> bool {
        self.client.pending() > BACKLOG
    }

    /// The current server is behind by more than [`BACKLOG`].
    fn backend_full(&self) -> bool {
        self.backend
            .as_ref()
            .is_some_and(|b| b.conn.pending() > BACKLOG)
    }

    fn deadline(&self, idle: Duration) -> Instant {
        // Not read while its server has a backlog, so not idle either.
        let mut d = if self.backend_full() {
            Instant::now() + idle
        } else {
            self.last_client + idle
        };
        // The next write to a peer that stopped reading fails: the client
        // ends the session, a server is lost.
        let stalls = [
            self.client.stalled_since(),
            self.backend.as_ref().and_then(|b| b.conn.stalled_since()),
            match &self.next {
                Next::Ready { conn, .. } => conn.stalled_since(),
                _ => None,
            },
        ];
        for s in stalls.into_iter().flatten() {
            d = d.min(s + WRITE_TIMEOUT);
        }
        match &self.backend {
            // Not read meanwhile, so not idle either.
            Some(_) if self.client_full() => {}
            Some(b) => d = d.min(b.last_read + idle),
            None => d = d.min(self.next_own_keep_alive),
        }
        if let Some(a) = self.ack_deadline {
            d = d.min(a);
        }
        if let Some(g) = self.gate_deadline {
            d = d.min(g);
        }
        if self.tab_configured() {
            d = d.min(self.next_tab);
        }
        if let Next::Ready { since, .. } = &self.next {
            d = d.min(*since + Duration::from_millis(self.rt.config.switching.config_buffer_ms));
        }
        d
    }

    fn timers(&mut self, idle: Duration) -> Option<End> {
        let now = Instant::now();
        if !self.backend_full() && now >= self.last_client + idle {
            Metrics::inc(&self.proxy.metrics.timeouts_idle);
            return Some(end_kick("Timed out."));
        }
        if self.ack_deadline.is_some_and(|d| now >= d) {
            return Some(end_kick("Timed out while switching servers."));
        }
        if self.gate_deadline.is_some_and(|d| now >= d) {
            return Some(end_kick("You took too long to log in."));
        }
        if self.tab_configured() && now >= self.next_tab {
            self.next_tab = now + Duration::from_millis(self.rt.config.tab.refresh_ms.max(100));
            self.config_tab(false);
        }
        if !self.client_full()
            && self
                .backend
                .as_ref()
                .is_some_and(|b| now >= b.last_read + idle)
        {
            Metrics::inc(&self.proxy.metrics.timeouts_idle);
            return self.backend_lost(Some(Component::text("Timed out.")));
        }
        if self.backend.is_none() && now >= self.next_own_keep_alive {
            self.next_own_keep_alive = now + OWN_KEEP_ALIVE;
            let id = random_bytes::<8>().map(i64::from_be_bytes).unwrap_or(1);
            self.own_keep_alive = Some(id);
            self.own_keep_alive_at = now;
            let phase = self.out_phase;
            if let Err(e) = queue_out(&mut self.client, self.module, phase, &KeepAlive { id }) {
                return Some(e.into());
            }
        }
        let limit = Duration::from_millis(self.rt.config.switching.config_buffer_ms);
        if let Next::Ready { since, .. } = &self.next
            && now >= *since + limit
        {
            self.next_failed(format!("still configuring after {limit:?}"));
            return self.try_next();
        }
        None
    }

    /// Writes to the servers what their sockets take now; the rest goes out
    /// in `run` while the session goes on, as for the client.
    fn write_servers(&mut self) -> Result<(), End> {
        if let Some(b) = self.backend.as_mut()
            && b.conn.try_write().is_err()
            && let Some(end) = self.backend_lost(None)
        {
            return Err(end);
        }
        if let Next::Ready { conn, .. } = &mut self.next
            && conn.try_write().is_err()
        {
            self.next_failed("connection closed".into());
            if let Some(end) = self.try_next() {
                return Err(end);
            }
        }
        Ok(())
    }
}
