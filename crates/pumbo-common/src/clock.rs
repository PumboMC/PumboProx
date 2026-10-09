//! Wall-clock time behind a trait, so that time-dependent logic can be tested
//! with a clock that only moves when the test says so.
//!
//! Core logic takes `now_ms: u64` arguments; the platform layer reads them from
//! a [`Clock`] it owns.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Source of the current time.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// Milliseconds since the Unix epoch from the system clock (0 if it is unavailable).
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)).unwrap_or(0)
}

/// The system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

/// A clock that stands still until it is set or advanced.
#[derive(Debug, Default)]
pub struct ManualClock {
    ms: AtomicU64,
}

impl ManualClock {
    pub fn new(ms: u64) -> Self {
        Self { ms: AtomicU64::new(ms) }
    }

    pub fn set(&self, ms: u64) {
        self.ms.store(ms, Ordering::Relaxed);
    }

    /// Moves the clock forward (saturating at `u64::MAX`).
    pub fn advance(&self, ms: u64) {
        let now = self.ms.load(Ordering::Relaxed);
        self.ms.store(now.saturating_add(ms), Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.ms.load(Ordering::Relaxed)
    }
}

impl<C: Clock + ?Sized> Clock for Arc<C> {
    fn now_ms(&self) -> u64 {
        (**self).now_ms()
    }
}

impl<C: Clock + ?Sized> Clock for &C {
    fn now_ms(&self) -> u64 {
        (**self).now_ms()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deadline(clock: &dyn Clock, ttl: u64) -> u64 {
        clock.now_ms() + ttl
    }

    #[test]
    fn manual_clock_moves_only_when_told() {
        let c = ManualClock::new(1_000);
        assert_eq!(c.now_ms(), 1_000);
        c.advance(500);
        assert_eq!(deadline(&c, 10), 1_510);
        c.set(5);
        assert_eq!(c.now_ms(), 5);
        c.advance(u64::MAX);
        assert_eq!(c.now_ms(), u64::MAX);
    }

    #[test]
    fn shared_and_system_clocks() {
        let shared: Arc<dyn Clock> = Arc::new(ManualClock::new(7));
        assert_eq!(shared.now_ms(), 7);
        // 2020-01-01 as a sanity floor for the real clock
        assert!(SystemClock.now_ms() > 1_577_836_800_000);
    }
}
