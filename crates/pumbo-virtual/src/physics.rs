//! Gravity check: compares reported positions with the free-fall curve of the
//! vanilla client. Taken over from Pumbo (`pumbo-limbo/src/core/physics.rs`,
//! our own code); PumboFilter (E7) builds its fall test on it, the test gate
//! and the tests of E6 use it to check the movement the proxy decodes.
//!
//! Each client tick the vanilla client first moves by its vertical velocity and
//! then updates it: `v = (v - 0.08) * 0.98` (the drag factor is a 32-bit float in
//! the client). Starting from rest after a teleport, a real client therefore
//! reports a well-known sequence of heights. Packets may be skipped (the client
//! only sends when it moved, and packets get merged under lag), so a report may
//! match the curve a few ticks ahead.

pub const GRAVITY: f64 = 0.08;
pub const DRAG: f64 = 0.98f32 as f64;
/// Velocities below this are zeroed by the client before moving.
const MIN_VELOCITY: f64 = 0.003;
/// How many unreported ticks one packet may skip.
const MAX_SKIP: u32 = 6;
/// The first reports after a teleport are often lost (the server ignores movement
/// until the teleport is confirmed and the chunks are sent). The first accepted
/// report may therefore lie this many ticks down the curve.
const MAX_ARM_TICKS: u32 = 240;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FallParams {
    pub ticks: u32,
    pub max_y_difference: f64,
    pub max_y_errors: u32,
    pub max_xz_errors: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallStatus {
    /// Waiting for the first position at the start point.
    Waiting,
    InProgress,
    Passed,
    FailedY,
    FailedXz,
}

#[derive(Debug, Clone)]
pub struct FallCheck {
    params: FallParams,
    start: (f64, f64, f64),
    armed: bool,
    y: f64,
    v: f64,
    step: u32,
    samples: u32,
    y_errors: u32,
    xz_errors: u32,
    status: FallStatus,
}

/// Advances the client physics by one tick.
pub fn tick(y: f64, v: f64) -> (f64, f64) {
    let v = if v.abs() < MIN_VELOCITY { 0.0 } else { v };
    let y = y + v;
    (y, (v - GRAVITY) * DRAG)
}

impl FallCheck {
    pub fn new(start: (f64, f64, f64), params: FallParams) -> Self {
        Self {
            params,
            start,
            armed: false,
            y: start.1,
            v: 0.0,
            step: 0,
            samples: 0,
            y_errors: 0,
            xz_errors: 0,
            status: FallStatus::Waiting,
        }
    }

    pub fn status(&self) -> FallStatus {
        self.status
    }

    /// Fraction of the check completed (for the experience bar).
    pub fn progress(&self) -> f32 {
        (self.step as f32 / self.params.ticks.max(1) as f32).clamp(0.0, 1.0)
    }

    pub fn errors(&self) -> (u32, u32) {
        (self.y_errors, self.xz_errors)
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    pub fn step(&self) -> u32 {
        self.step
    }

    /// One-line summary for debug logs.
    pub fn summary(&self) -> String {
        format!(
            "status={:?} matched_ticks={}/{} samples={} y_errors={} xz_errors={} expected_y={:.4}",
            self.status,
            self.step,
            self.params.ticks,
            self.samples,
            self.y_errors,
            self.xz_errors,
            self.y
        )
    }

    /// Feeds one reported position.
    pub fn on_move(&mut self, x: f64, y: f64, z: f64) -> FallStatus {
        if matches!(
            self.status,
            FallStatus::Passed | FallStatus::FailedY | FallStatus::FailedXz
        ) {
            return self.status;
        }
        if !(x.is_finite() && y.is_finite() && z.is_finite()) {
            self.y_errors += 1;
            return self.finish_errors();
        }
        let (sx, sy, sz) = self.start;
        if !self.armed {
            // Ignore positions from before the teleport took effect (other column).
            if (x - sx).abs() > 1.0 || (z - sz).abs() > 1.0 || y > sy + 0.01 {
                return self.status;
            }
            // Find where on the curve the first report lies.
            let (mut cy, mut cv) = (sy, 0.0);
            let mut found = None;
            for n in 0..=MAX_ARM_TICKS {
                if (y - cy).abs() <= self.params.max_y_difference {
                    found = Some((n, cy, cv));
                    break;
                }
                (cy, cv) = tick(cy, cv);
            }
            let Some((n, fy, fv)) = found else {
                // Not on the curve at all: counts as a wrong height.
                self.y_errors += 1;
                return self.finish_errors();
            };
            self.armed = true;
            self.status = FallStatus::InProgress;
            self.step = n;
            self.y = fy;
            self.v = fv;
            self.samples = 1;
            return self.status;
        }
        self.samples += 1;
        if (x - sx).abs() > 1e-3 || (z - sz).abs() > 1e-3 {
            self.xz_errors += 1;
        }
        // Find how many ticks ahead of the last match this report lies.
        let tol = self.params.max_y_difference;
        let (mut cy, mut cv) = (self.y, self.v);
        let mut matched = None;
        if (y - cy).abs() <= tol {
            matched = Some((0, cy, cv));
        } else {
            for n in 1..=MAX_SKIP {
                let (ny, nv) = tick(cy, cv);
                cy = ny;
                cv = nv;
                if (y - cy).abs() <= tol {
                    matched = Some((n, cy, cv));
                    break;
                }
            }
        }
        match matched {
            Some((n, my, mv)) => {
                self.step += n;
                self.y = my;
                self.v = mv;
            }
            None => self.y_errors += 1,
        }
        if let Some(fail) = self.check_errors() {
            self.status = fail;
            return fail;
        }
        // Require a reasonable number of real reports, not just a few lucky ones.
        if self.step >= self.params.ticks && self.samples >= self.params.ticks / 2 {
            self.status = FallStatus::Passed;
        }
        self.status
    }

    fn check_errors(&self) -> Option<FallStatus> {
        if self.y_errors > self.params.max_y_errors {
            Some(FallStatus::FailedY)
        } else if self.xz_errors > self.params.max_xz_errors {
            Some(FallStatus::FailedXz)
        } else {
            None
        }
    }

    fn finish_errors(&mut self) -> FallStatus {
        if let Some(f) = self.check_errors() {
            self.status = f;
        }
        self.status
    }
}

/// Positions a correct client reports, one per tick, starting at rest at `y0`.
/// The first tick does not move, so the list starts with the second tick.
pub fn ideal_reports(y0: f64, ticks: u32) -> Vec<f64> {
    let mut out = Vec::new();
    let (mut y, mut v) = (y0, 0.0);
    for _ in 0..ticks {
        let (ny, nv) = tick(y, v);
        y = ny;
        v = nv;
        if (y - y0).abs() > 1e-9 || !out.is_empty() {
            out.push(y);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> FallParams {
        FallParams {
            ticks: 128,
            max_y_difference: 0.01,
            max_y_errors: 10,
            max_xz_errors: 10,
        }
    }

    const START: (f64, f64, f64) = (0.5, 512.0, 0.5);

    #[test]
    fn known_curve_values() {
        // tick 1 does not move, tick 2 moves by -0.0784, tick 3 by -0.155232...
        let r = ideal_reports(512.0, 4);
        assert!((r[0] - (512.0 - 0.0784)).abs() < 1e-6);
        assert!((r[1] - (512.0 - 0.0784 - 0.155_232)).abs() < 1e-5);
        // terminal velocity is about 3.92 blocks per tick
        let (mut y, mut v) = (0.0, 0.0);
        for _ in 0..1000 {
            (y, v) = tick(y, v);
        }
        assert!((v + 3.92).abs() < 0.01, "{v}");
        assert!(y < 0.0);
    }

    #[test]
    fn real_client_passes() {
        let mut c = FallCheck::new(START, params());
        let mut st = FallStatus::Waiting;
        for y in ideal_reports(START.1, 140) {
            st = c.on_move(START.0, y, START.2);
            if st == FallStatus::Passed {
                break;
            }
        }
        assert_eq!(st, FallStatus::Passed);
        assert_eq!(c.errors(), (0, 0));
    }

    #[test]
    fn skipped_packets_are_tolerated() {
        let mut c = FallCheck::new(START, params());
        let mut st = FallStatus::Waiting;
        for (i, y) in ideal_reports(START.1, 200).into_iter().enumerate() {
            if i % 3 == 1 {
                continue; // lose every third packet
            }
            st = c.on_move(START.0, y, START.2);
            if st == FallStatus::Passed {
                break;
            }
        }
        assert_eq!(st, FallStatus::Passed);
    }

    #[test]
    fn hovering_bot_fails() {
        let mut c = FallCheck::new(START, params());
        let mut st = FallStatus::Waiting;
        for i in 0..40 {
            st = c.on_move(START.0, START.1 - 0.5 - f64::from(i) * 0.1, START.2);
        }
        assert_eq!(st, FallStatus::FailedY);
    }

    #[test]
    fn standing_still_never_passes() {
        let mut c = FallCheck::new(START, params());
        for _ in 0..500 {
            assert_ne!(c.on_move(START.0, START.1, START.2), FallStatus::Passed);
        }
        assert_eq!(c.progress(), 0.0);
    }

    #[test]
    fn linear_fall_fails() {
        let mut c = FallCheck::new(START, params());
        let mut st = FallStatus::Waiting;
        for i in 1..100 {
            st = c.on_move(START.0, START.1 - f64::from(i) * 0.5, START.2);
        }
        assert_eq!(st, FallStatus::FailedY);
    }

    #[test]
    fn horizontal_movement_fails() {
        let mut c = FallCheck::new(START, params());
        let mut st = FallStatus::Waiting;
        for (i, y) in ideal_reports(START.1, 60).into_iter().enumerate() {
            st = c.on_move(START.0 + i as f64 * 0.2, y, START.2);
        }
        assert_eq!(st, FallStatus::FailedXz);
    }

    #[test]
    fn lost_first_reports_are_tolerated() {
        let mut c = FallCheck::new(START, params());
        let mut st = FallStatus::Waiting;
        // the first 40 ticks never reach the server
        for y in ideal_reports(START.1, 220).into_iter().skip(40) {
            st = c.on_move(START.0, y, START.2);
            if st == FallStatus::Passed {
                break;
            }
        }
        assert_eq!(st, FallStatus::Passed);
    }

    #[test]
    fn off_curve_first_report_counts_as_error() {
        let mut c = FallCheck::new(START, params());
        let mut st = FallStatus::Waiting;
        for _ in 0..20 {
            st = c.on_move(START.0, 300.123, START.2);
        }
        assert_eq!(st, FallStatus::FailedY);
    }

    #[test]
    fn positions_before_teleport_are_ignored() {
        let mut c = FallCheck::new(START, params());
        assert_eq!(c.on_move(100.0, 64.0, 100.0), FallStatus::Waiting);
        assert_eq!(c.errors(), (0, 0));
    }

    #[test]
    fn sparse_lucky_reports_do_not_pass() {
        let mut c = FallCheck::new(START, params());
        let reports = ideal_reports(START.1, 200);
        let mut st = FallStatus::Waiting;
        for y in reports.iter().step_by(6) {
            st = c.on_move(START.0, *y, START.2);
        }
        // every report matches, but there are too few of them
        assert_ne!(st, FallStatus::Passed);
    }
}
