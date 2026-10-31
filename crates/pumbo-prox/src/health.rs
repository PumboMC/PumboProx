//! Passive health of backends: logins that fail before the backend answers
//! (no connection, a timeout, the connection closed early) put a server
//! offline for a while, and one login that gets through brings it back. A
//! kick from the backend itself (a ban, a whitelist) never counts.
//!
//! An offline server is not left out: players try the other servers first
//! and it last, so a network with one lobby still reaches it the moment it
//! is back. Once its wait is over, one player tries it in its usual place.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

/// Failed logins in a row that put a server offline.
pub const FAILURES: u32 = 3;
/// The first wait before an offline server is tried in its place again;
/// each failed try doubles it up to [`MAX_WAIT`].
const FIRST_WAIT: Duration = Duration::from_secs(5);
const MAX_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct Entry {
    failures: u32,
    offline: bool,
    wait: Duration,
    retry_at: Instant,
    /// When the version changes for the end of a wait (one at a time).
    wake_at: Instant,
}

#[derive(Debug, Default)]
pub struct Health {
    servers: Mutex<HashMap<String, Entry>>,
    /// Changes when a server goes offline or online and when its wait ends,
    /// so players waiting in the Pumbox try the servers again.
    version: Arc<AtomicU64>,
}

impl Health {
    /// A login reached the server; `true` when it was offline.
    pub fn succeeded(&self, server: &str) -> bool {
        let was = self
            .servers
            .lock()
            .ok()
            .and_then(|mut s| s.remove(server))
            .is_some_and(|e| e.offline);
        if was {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        was
    }

    /// A login failed before the server answered; the wait when this put it
    /// offline.
    pub fn failed(&self, server: &str) -> Option<Duration> {
        let mut s = self.servers.lock().ok()?;
        let now = Instant::now();
        let e = s.entry(server.to_string()).or_insert(Entry {
            failures: 0,
            offline: false,
            wait: FIRST_WAIT,
            retry_at: now,
            wake_at: now,
        });
        e.failures = e.failures.saturating_add(1);
        let went = e.failures >= FAILURES && !e.offline;
        if e.failures >= FAILURES {
            e.wait = if went {
                FIRST_WAIT
            } else {
                (e.wait * 2).min(MAX_WAIT)
            };
            e.offline = true;
            e.retry_at = now + e.wait;
        }
        // Players in the Pumbox try again once the wait is over.
        let wait = e.wait;
        let wake = e.wake_at <= now;
        if wake {
            e.wake_at = now + wait;
        }
        drop(s);
        if wake {
            self.wake_after(wait);
        }
        if went {
            self.version.fetch_add(1, Ordering::Relaxed);
        }
        went.then_some(wait)
    }

    /// Whether players try the server in its place: online, or offline with
    /// its wait over (the caller is then the one who tries, the next ones
    /// wait again).
    pub fn usable(&self, server: &str) -> bool {
        let Ok(mut s) = self.servers.lock() else {
            return true;
        };
        let Some(e) = s.get_mut(server).filter(|e| e.offline) else {
            return true;
        };
        let now = Instant::now();
        if now < e.retry_at {
            return false;
        }
        e.retry_at = now + e.wait;
        true
    }

    /// Not offline (for plugins and metrics).
    pub fn online(&self, server: &str) -> bool {
        self.servers
            .lock()
            .map(|s| !s.get(server).is_some_and(|e| e.offline))
            .unwrap_or(true)
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Changes the version once `wait` is over.
    fn wake_after(&self, wait: Duration) {
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let version = self.version.clone();
            rt.spawn(async move {
                tokio::time::sleep(wait).await;
                version.fetch_add(1, Ordering::Relaxed);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn offline_after_failures_in_a_row_until_a_login_gets_through() {
        let h = Health::default();
        assert_eq!(h.failed("lobby"), None);
        assert!(!h.succeeded("lobby") && h.online("lobby"));
        // A success in between starts the count again.
        assert_eq!(h.failed("lobby"), None);
        assert_eq!(h.failed("lobby"), None);
        let v = h.version();
        assert_eq!(h.failed("lobby"), Some(FIRST_WAIT));
        assert!(!h.online("lobby") && !h.usable("lobby"));
        assert!(h.usable("survival") && h.online("survival"));
        assert_ne!(h.version(), v);
        // Failed tries while offline double the wait, without a new message.
        assert_eq!(h.failed("lobby"), None);
        assert_eq!(h.servers.lock().unwrap()["lobby"].wait, FIRST_WAIT * 2);
        for _ in 0..10 {
            h.failed("lobby");
        }
        assert_eq!(h.servers.lock().unwrap()["lobby"].wait, MAX_WAIT);
        let v = h.version();
        assert!(h.succeeded("lobby"));
        assert!(h.online("lobby") && h.usable("lobby"));
        assert_ne!(h.version(), v);
    }

    #[tokio::test(start_paused = true)]
    async fn the_end_of_a_wait_wakes_the_pumbox() {
        let h = Health::default();
        let v = h.version();
        h.failed("lobby");
        h.failed("lobby");
        assert_eq!(h.version(), v, "nothing changed yet");
        tokio::time::sleep(FIRST_WAIT + Duration::from_millis(10)).await;
        let v = h.version();
        assert_ne!(v, 0, "a failed login is tried again after the wait");
        h.failed("lobby");
        tokio::time::sleep(FIRST_WAIT + Duration::from_millis(10)).await;
        assert!(h.version() >= v + 2, "offline, then its wait over");
    }

    #[test]
    fn one_player_tries_an_offline_server_when_its_wait_is_over() {
        let h = Health::default();
        for _ in 0..FAILURES {
            h.failed("lobby");
        }
        h.servers.lock().unwrap().get_mut("lobby").unwrap().retry_at = Instant::now();
        assert!(h.usable("lobby"));
        assert!(!h.usable("lobby"), "the next ones wait again");
        assert!(!h.online("lobby"));
    }
}
