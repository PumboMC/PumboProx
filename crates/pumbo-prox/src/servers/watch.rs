//! A download as its starter sees it: progress on a boss bar and in the log.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use pumbo_servers::Progress;
use pumbo_text::template::escape_mini as esc;
use tracing::info;
use uuid::Uuid;

use crate::server::{Proxy, SessionCmd};
use crate::ui::{row, title_case};
use crate::world::BossbarOp;

/// Bar colours (vanilla IDs).
const YELLOW: i32 = 4;
const BYTES_PER_MB: f64 = 1_000_000.0;
const SPEED_WINDOW: Duration = Duration::from_secs(3);
const DRAW_EVERY: Duration = Duration::from_millis(250);

/// One download as its starter sees it: a boss bar for a player (it ends
/// with the player), the log every 10 % for everyone.
pub(super) struct Watch {
    proxy: Weak<Proxy>,
    player: Option<Uuid>,
    bar: Uuid,
    software: String,
    st: Mutex<WatchState>,
}

#[derive(Default)]
struct WatchState {
    tag: Option<String>,
    shown: bool,
    drawn: Option<Instant>,
    samples: VecDeque<(Instant, u64)>,
    tenths: u64,
    retry: Option<(u32, u32)>,
    unverified: bool,
}

impl Watch {
    pub(super) fn new(proxy: &Arc<Proxy>, player: Option<Uuid>, src: &str) -> Self {
        Self {
            proxy: Arc::downgrade(proxy),
            player,
            bar: {
                let mut b = [0u8; 16];
                let _ = aws_lc_rs::rand::fill(&mut b);
                Uuid::from_bytes(b)
            },
            software: src.to_string(),
            st: Mutex::new(WatchState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.st
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn unverified(&self) -> bool {
        self.lock().unverified
    }

    /// The tag once known, else what was asked for (`#2`).
    pub(super) fn what(&self, which: &str) -> String {
        self.lock().tag.clone().unwrap_or_else(|| esc(which))
    }

    fn send(&self, op: BossbarOp) {
        if let (Some(id), Some(p)) = (self.player, self.proxy.upgrade()) {
            // A player who left: nothing to show, the download goes on.
            p.send_to(
                id,
                SessionCmd::Bossbar {
                    id: self.bar,
                    op: Box::new(op),
                },
            );
        }
    }

    pub(super) fn on(&self, p: Progress) {
        let mut st = self.lock();
        let (done, total) = match p {
            Progress::Started { tag } => {
                st.tag = Some(tag);
                return;
            }
            Progress::Unverified => {
                st.unverified = true;
                return;
            }
            Progress::Retry { attempt, of } => {
                st.retry = Some((attempt, of));
                st.samples.clear();
                st.tenths = 0;
                st.drawn = None;
                info!(
                    "downloading {} {}: retrying ({attempt}/{of})",
                    self.software,
                    st.tag.as_deref().unwrap_or("")
                );
                (0, None)
            }
            Progress::Bytes { done, total } => (done, total),
        };
        let now = Instant::now();
        st.samples.push_back((now, done));
        while st.samples.len() > 2
            && st
                .samples
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > SPEED_WINDOW)
        {
            st.samples.pop_front();
        }
        let speed = match st.samples.front() {
            Some((t, d)) if now.duration_since(*t) > Duration::from_millis(200) => {
                done.saturating_sub(*d) as f64 / now.duration_since(*t).as_secs_f64()
            }
            _ => 0.0,
        };
        let mb = |b: u64| b as f64 / BYTES_PER_MB;
        let tag = st.tag.clone().unwrap_or_default();
        if let Some(t) = total.filter(|t| *t > 0) {
            let tenths = done * 10 / t;
            if tenths > st.tenths {
                st.tenths = tenths;
                info!(
                    "downloading {} {tag}: {}% ({:.0}/{:.0} MB, {:.1} MB/s)",
                    self.software,
                    tenths * 10,
                    mb(done),
                    mb(t),
                    speed / BYTES_PER_MB
                );
            }
        }
        let last = total == Some(done);
        if self.player.is_none() || (!last && st.drawn.is_some_and(|d| d.elapsed() < DRAW_EVERY)) {
            return;
        }
        st.drawn = Some(now);
        let (progress, size) = match total.filter(|t| *t > 0) {
            Some(t) => (
                done as f32 / t as f32,
                format!(
                    " · {}% · {:.1} MB/s · {:.0}/{:.0} MB",
                    done * 100 / t,
                    speed / BYTES_PER_MB,
                    mb(done),
                    mb(t)
                ),
            ),
            None => (
                0.0,
                format!(" · {:.1} MB/s · {:.0} MB", speed / BYTES_PER_MB, mb(done)),
            ),
        };
        let retry = st
            .retry
            .map(|(a, of)| format!(" <yellow>· retrying ({a}/{of})"))
            .unwrap_or_default();
        let title = row(&format!(
            "<white>Downloading {} {tag}<gray>{size}{retry}",
            title_case(&self.software)
        ));
        if st.shown {
            self.send(BossbarOp::Title(title));
            self.send(BossbarOp::Progress(progress));
        } else {
            st.shown = true;
            self.send(BossbarOp::Show {
                title,
                progress,
                color: YELLOW,
                overlay: 0,
            });
        }
    }

    /// The bar in its end colour (a new bar: colours do not change in place),
    /// gone after `linger`.
    pub(super) fn finish(self: &Arc<Self>, mini: &str, color: i32, linger: Duration) {
        if self.player.is_none() {
            return;
        }
        if self.lock().shown {
            self.send(BossbarOp::Hide);
        }
        self.send(BossbarOp::Show {
            title: row(mini),
            progress: 1.0,
            color,
            overlay: 0,
        });
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(linger).await;
            me.send(BossbarOp::Hide);
        });
    }
}
