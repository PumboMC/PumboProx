//! The detector: one task that pings the servers of the config on a
//! schedule and puts what it learns into the runtime.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;
use tracing::warn;

use crate::server::{Proxy, Runtime};

/// Pings in flight at once.
const AT_ONCE: usize = 16;

/// The times of the detector; tests shorten them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How long one ping may take.
    pub ping_timeout: Duration,
    /// How long the start waits for the first answers.
    pub start_wait: Duration,
    /// The wait before asking a silent server again; it doubles up to `interval`.
    pub first_retry: Duration,
    /// The wait before asking a server that answered again.
    pub interval: Duration,
    /// The least time between two rounds a `wake` starts.
    pub wake_gap: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping_timeout: Duration::from_secs(3),
            start_wait: Duration::from_secs(1),
            first_retry: Duration::from_secs(5),
            interval: Duration::from_secs(60),
            wake_gap: Duration::from_secs(5),
        }
    }
}

/// Starts the detector and waits until every server answered its first
/// ping or `start_wait` passed; the answers it has by then are in the
/// runtime when it returns.
pub async fn start(proxy: &Arc<Proxy>) {
    start_with(proxy, Timing::default()).await;
}

/// [`start`] with other times.
pub async fn start_with(proxy: &Arc<Proxy>, timing: Timing) {
    if proxy.detect.started.swap(true, Ordering::Relaxed) {
        return;
    }
    let (first, done) = oneshot::channel();
    let signals = (proxy.detect.wake.clone(), proxy.detect.recheck.clone());
    let stop = proxy.shutdown_signal();
    tokio::spawn(run(Arc::downgrade(proxy), signals, stop, timing, first));
    let _ = tokio::time::timeout(timing.start_wait, done).await;
    proxy.apply_detection();
}

/// When a server is asked next.
struct Due {
    /// What it was asked about: its address and the protocol asked with.
    asked: (String, i32),
    at: Instant,
    /// Pings in a row without an answer.
    failures: u32,
}

async fn run(
    proxy: Weak<Proxy>,
    (wake, recheck): (Arc<Notify>, Arc<Notify>),
    mut stop: tokio::sync::watch::Receiver<bool>,
    timing: Timing,
    first: oneshot::Sender<()>,
) {
    let mut first = Some(first);
    let mut due: HashMap<String, Due> = HashMap::new();
    // The start counts as a wake: every server is asked.
    let (mut woken, mut last_wake) = (true, None::<Instant>);
    loop {
        let Some(p) = proxy.upgrade() else { return };
        let rt = p.runtime();
        let now = Instant::now();
        let all = wake_round(woken, last_wake, now, timing.wake_gap);
        if all {
            woken = false;
            last_wake = Some(now);
        }
        let round = targets(&rt, &mut due, all, now);
        drop((p, rt));
        if !round.is_empty() {
            // Until the start applies the first answers, without a warning.
            let quiet = first.is_some();
            let pings = ping_round(&proxy, round, &mut due, timing, quiet);
            tokio::pin!(pings);
            loop {
                tokio::select! {
                    biased;
                    () = crate::session::stopped(&mut stop) => return,
                    () = &mut pings => break,
                    // A brand or a bridge does not wait for the slowest ping.
                    () = recheck.notified() => if let Some(p) = proxy.upgrade() {
                        apply(&p, quiet);
                    },
                }
            }
        }
        // The start applies the first answers itself, once it stops waiting.
        let start_applies = first.take().is_some_and(|f| f.send(()).is_ok());
        if !start_applies && let Some(p) = proxy.upgrade() {
            apply(&p, false);
        }
        let mut next = due.values().map(|d| d.at).min();
        if woken && let Some(t) = last_wake {
            next = Some(next.map_or(t + timing.wake_gap, |n| n.min(t + timing.wake_gap)));
        }
        let sleep =
            tokio::time::sleep_until(next.unwrap_or_else(|| Instant::now() + timing.interval));
        tokio::select! {
            biased;
            () = crate::session::stopped(&mut stop) => return,
            () = wake.notified() => woken = true,
            () = recheck.notified() => {}
            () = sleep => {}
        }
    }
}

/// Whether a wake asks every server now: rounds a wake starts are `gap`
/// apart.
pub(super) fn wake_round(woken: bool, last: Option<Instant>, now: Instant, gap: Duration) -> bool {
    woken && last.is_none_or(|t| now >= t + gap)
}

/// The servers to ask in this round (`all`: every one), as name, address
/// and protocol to ask with. The schedule forgets servers no longer asked.
fn targets(
    rt: &Runtime,
    due: &mut HashMap<String, Due>,
    all: bool,
    now: Instant,
) -> Vec<(String, String, i32)> {
    let asked: HashMap<&String, (String, i32)> = if rt.config.detection.ping {
        rt.config
            .servers
            .iter()
            .filter(|(name, _)| !rt.managed.contains(name))
            .filter_map(|(name, s)| Some((name, (s.address.clone(), super::ask(s.protocol)?))))
            .collect()
    } else {
        HashMap::new()
    };
    due.retain(|name, d| asked.get(name).is_some_and(|a| *a == d.asked));
    asked
        .into_iter()
        .filter(|(name, _)| all || due.get(*name).is_none_or(|d| d.at <= now))
        .map(|(name, (address, ask))| (name.clone(), address, ask))
        .collect()
}

/// One round: `round` pinged, at most [`AT_ONCE`] at a time, and each
/// result recorded and applied as it comes.
async fn ping_round(
    proxy: &Weak<Proxy>,
    round: Vec<(String, String, i32)>,
    due: &mut HashMap<String, Due>,
    timing: Timing,
    quiet: bool,
) {
    let pings = futures::stream::iter(round).map(|(name, address, ask)| async move {
        let answer = crate::ping::status(&address, ask, timing.ping_timeout).await;
        (name, address, ask, answer)
    });
    let mut pings = pings.buffer_unordered(AT_ONCE);
    while let Some((name, address, ask, answer)) = pings.next().await {
        let failures = match &answer {
            Ok(_) => 0,
            Err(_) => due.get(&name).map_or(0, |d| d.failures) + 1,
        };
        let wait = match failures {
            0 => timing.interval,
            n => timing
                .first_retry
                .saturating_mul(1u32 << (n - 1).min(16))
                .min(timing.interval),
        };
        let at = Instant::now() + wait;
        let asked = (address, ask);
        let Some(p) = proxy.upgrade() else {
            return;
        };
        // A reload during the round may have moved the server or changed
        // the question: such an answer is about nothing the proxy has.
        let rt = p.runtime();
        let entry = rt.config.servers.get(&name);
        if entry.is_some_and(|s| (&s.address, super::ask(s.protocol)) == (&asked.0, Some(ask))) {
            p.detect
                .answered(&rt, &name, ask, answer.map(|a| a.protocol));
            apply(&p, quiet);
        }
        due.insert(
            name,
            Due {
                asked,
                at,
                failures,
            },
        );
    }
}

/// The facts into the runtime, and unless `quiet` the warning when that
/// leaves no listed version a way to any server.
fn apply(proxy: &Proxy, quiet: bool) {
    let reached = crate::versions::unreachable(&proxy.runtime()).is_none();
    if proxy.apply_detection()
        && !quiet
        && reached
        && let Some(w) = crate::versions::unreachable(&proxy.runtime())
    {
        warn!("{w}");
    }
}
