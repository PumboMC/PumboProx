//! Native limits (§2.7): per address and per connection.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::LimitsConfig;

/// Rate limit with a burst of one second's worth. A take may push the bucket
/// below zero (one large packet is fine), the next take then fails.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    rate: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(per_second: u32) -> Self {
        let rate = f64::from(per_second);
        Self {
            rate,
            tokens: rate,
            last: Instant::now(),
        }
    }

    pub fn take(&mut self, n: f64) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.rate);
        if self.tokens < 0.0 {
            return false;
        }
        self.tokens -= n;
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpReject {
    Rate,
    Concurrent,
}

#[derive(Debug)]
struct IpState {
    concurrent: u32,
    window: Instant,
    connections: u32,
    statuses: u32,
}

/// Connections per address: concurrent count and per-second windows for new
/// connections and status pings. IPv6 counts per /64 (one host usually has
/// the whole prefix).
#[derive(Debug, Default)]
pub struct IpLimiter {
    map: Mutex<HashMap<IpAddr, IpState>>,
}

fn key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !((1u128 << 64) - 1))),
        v4 => v4,
    }
}

const WINDOW: Duration = Duration::from_secs(1);

impl IpLimiter {
    fn with<R>(&self, f: impl FnOnce(&mut HashMap<IpAddr, IpState>) -> R) -> R {
        let mut map = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut map)
    }

    /// A new connection from `ip`; the guard ends it.
    pub fn connect(
        self: &Arc<Self>,
        ip: IpAddr,
        limits: &LimitsConfig,
    ) -> Result<IpGuard, IpReject> {
        let k = key(ip);
        let now = Instant::now();
        self.with(|map| {
            let s = map.entry(k).or_insert(IpState {
                concurrent: 0,
                window: now,
                connections: 0,
                statuses: 0,
            });
            if now.duration_since(s.window) >= WINDOW {
                s.window = now;
                s.connections = 0;
                s.statuses = 0;
            }
            if s.connections >= limits.connections_per_ip_per_second {
                return Err(IpReject::Rate);
            }
            s.connections += 1;
            if s.concurrent >= limits.concurrent_per_ip {
                return Err(IpReject::Concurrent);
            }
            s.concurrent += 1;
            Ok(IpGuard {
                limiter: self.clone(),
                key: k,
            })
        })
    }

    /// A status ping from `ip` within its window.
    pub fn status(&self, ip: IpAddr, limits: &LimitsConfig) -> bool {
        self.with(|map| match map.get_mut(&key(ip)) {
            Some(s) if s.statuses < limits.status_per_ip_per_second => {
                s.statuses += 1;
                true
            }
            Some(_) => false,
            None => true,
        })
    }

    /// Drops entries without connections whose window has passed.
    pub fn sweep(&self) {
        let now = Instant::now();
        self.with(|map| {
            map.retain(|_, s| s.concurrent > 0 || now.duration_since(s.window) < WINDOW)
        });
    }

    pub fn tracked(&self) -> usize {
        self.with(|map| map.len())
    }
}

/// One open connection counted against its address.
#[derive(Debug)]
pub struct IpGuard {
    limiter: Arc<IpLimiter>,
    key: IpAddr,
}

impl Drop for IpGuard {
    fn drop(&mut self) {
        self.limiter.with(|map| {
            if let Some(s) = map.get_mut(&self.key) {
                s.concurrent = s.concurrent.saturating_sub(1);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_ip_limits() {
        let limits = LimitsConfig {
            connections_per_ip_per_second: 3,
            concurrent_per_ip: 2,
            status_per_ip_per_second: 1,
            ..LimitsConfig::default()
        };
        let l = Arc::new(IpLimiter::default());
        let ip: IpAddr = "203.0.113.1".parse().unwrap();
        let a = l.connect(ip, &limits).unwrap();
        let b = l.connect(ip, &limits).unwrap();
        assert_eq!(l.connect(ip, &limits).unwrap_err(), IpReject::Concurrent);
        drop(a);
        // Third connection this second: over the rate.
        assert_eq!(l.connect(ip, &limits).unwrap_err(), IpReject::Rate);
        assert!(l.connect("203.0.113.2".parse().unwrap(), &limits).is_ok());
        assert!(l.status(ip, &limits));
        assert!(!l.status(ip, &limits));
        drop(b);
        // IPv6 counts per /64.
        let v6a: IpAddr = "2001:db8::1".parse().unwrap();
        let v6b: IpAddr = "2001:db8::ffff:2".parse().unwrap();
        let _x = l.connect(v6a, &limits).unwrap();
        let _y = l.connect(v6b, &limits).unwrap();
        assert_eq!(l.connect(v6a, &limits).unwrap_err(), IpReject::Concurrent);
    }

    #[test]
    fn bucket_allows_burst_then_refuses() {
        let mut b = TokenBucket::new(10);
        let ok = (0..20).filter(|_| b.take(1.0)).count();
        assert!((10..=11).contains(&ok), "{ok}");
        let mut big = TokenBucket::new(100);
        assert!(big.take(1000.0));
        assert!(!big.take(1.0));
    }
}
