//! Frame pacing of the live loop (OUT-6): each frame starts after the next desktop composition.
//!
//! The FIFO swapchain alone is not a steady clock on the reference PC: once the GPU lowers its clocks (a light
//! chain at 60 fps, after about 5 s), the presentation engine releases the swapchain images in pairs about every
//! 33 ms, the loop waits for them in `wait_slot`, and frames start in pairs about 2 ms apart then 30 ms apart; with a
//! captured source only one frame of each pair has a new capture, so motion steps like 30 fps (`QUESTIONS.md` OUT-6,
//! 2026-11-21). Waiting for the compositor (`DwmFlush`) before each frame gives one frame start per refresh in the
//! framed and the fullscreen output alike; the swapchain stays FIFO.
//!
//! The wait is best effort: a failed wait returns at once and the swapchain paces that frame alone, as before. The
//! pacer only tells the loop when the waits start failing (several in a row) and when they work again, so it can say
//! so once. Pure apart from the call it is given.

/// Failed waits in a row after which the pacer reports that the composition does not pace the frames.
pub const FAILURES_TO_REPORT: u32 = 3;

/// A change of the pacing the loop reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// The waits fail ([`FAILURES_TO_REPORT`] in a row): the swapchain paces the frames alone.
    Lost,
    /// A wait worked again after [`Change::Lost`].
    Back,
}

/// Counts the failed composition waits of the loop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pacer {
    failures: u32,
    lost: bool,
}

impl Pacer {
    /// Calls `wait` (true when the composition was waited for) before a frame; returns the change to report, if any.
    pub fn before_frame(&mut self, wait: impl FnOnce() -> bool) -> Option<Change> {
        if wait() {
            self.failures = 0;
            return std::mem::take(&mut self.lost).then_some(Change::Back);
        }
        self.failures = self.failures.saturating_add(1);
        if self.failures >= FAILURES_TO_REPORT && !self.lost {
            self.lost = true;
            return Some(Change::Lost);
        }
        None
    }

    /// The composition paces the frames (no run of failures is in force).
    pub fn paced(&self) -> bool {
        !self.lost
    }
}

/// Waits for the next desktop composition pass (`DwmFlush`); false when it failed (it returns at once then).
pub fn wait_for_composition() -> bool {
    // SAFETY: no arguments, no pointers; blocks the calling thread until the next composition.
    unsafe { windows::Win32::Graphics::Dwm::DwmFlush() }.is_ok()
}

/// A frame may start once this share of the cap interval has passed since the previous start: composition ticks
/// come a little early or late, and a tick just short of the interval must not cost a whole extra refresh.
const CAP_TOLERANCE: f64 = 0.9;

/// The most waits one held frame makes (a guard: a cap is at least 10 fps, so at most a few refreshes are needed).
const MAX_HOLD_WAITS: u32 = 64;

/// Frame rate cap of the output loop: holds a frame back, in whole display refreshes, until the cap interval has
/// passed since the previous frame started. The swapchain stays in its present mode; the cap only decides how many
/// composition waits come before a frame, so the rate in force is the display rate divided by a whole number
/// (never above the cap, but a cap of 60 on a 144 Hz display gives 48).
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCap {
    last: Option<std::time::Instant>,
}

impl FrameCap {
    /// Holds the frame until `1 / cap_hz` (less [`CAP_TOLERANCE`]) has passed since the previous frame started,
    /// calling `wait` (one composition, true when it worked) and, when a wait fails and returns at once, `sleep` for
    /// the time left. A `cap_hz` of 0 holds nothing. Returns the number of waits made.
    pub fn hold(
        &mut self,
        cap_hz: u32,
        mut now: impl FnMut() -> std::time::Instant,
        mut wait: impl FnMut() -> bool,
        mut sleep: impl FnMut(std::time::Duration),
    ) -> u32 {
        if cap_hz == 0 {
            self.last = None;
            return 0;
        }
        let interval = std::time::Duration::from_secs_f64(CAP_TOLERANCE / f64::from(cap_hz));
        let mut waits = 0;
        if let Some(last) = self.last {
            while waits < MAX_HOLD_WAITS {
                let elapsed = now().saturating_duration_since(last);
                if elapsed >= interval {
                    break;
                }
                if !wait() {
                    sleep(interval - elapsed);
                }
                waits += 1;
            }
        }
        self.last = Some(now());
        waits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A fake clock: `tick` is the refresh period, each wait advances by it.
    fn run(cap: u32, tick_ms: f64, frames: usize) -> Vec<f64> {
        let start = Instant::now();
        let t = std::cell::Cell::new(0.0f64); // ms since start
        let mut c = FrameCap::default();
        let mut starts = Vec::new();
        for _ in 0..frames {
            // The loop's own composition wait comes first, then the hold.
            t.set(t.get() + tick_ms);
            c.hold(
                cap,
                || start + Duration::from_secs_f64(t.get() / 1000.0),
                || {
                    t.set(t.get() + tick_ms);
                    true
                },
                |d| t.set(t.get() + d.as_secs_f64() * 1000.0),
            );
            starts.push(t.get());
        }
        starts.windows(2).map(|w| w[1] - w[0]).collect()
    }

    #[test]
    fn no_cap_holds_nothing() {
        let intervals = run(0, 16.667, 20);
        assert!(intervals.iter().all(|i| (i - 16.667).abs() < 1e-6), "{intervals:?}");
    }

    #[test]
    fn a_cap_at_the_display_rate_holds_nothing() {
        let intervals = run(60, 16.667, 20);
        assert!(intervals.iter().all(|i| (i - 16.667).abs() < 1e-6), "{intervals:?}");
    }

    #[test]
    fn half_the_display_rate_skips_every_other_refresh() {
        let intervals = run(30, 16.667, 20);
        assert!(intervals.iter().all(|i| (i - 33.334).abs() < 1e-3), "{intervals:?}");
    }

    #[test]
    fn a_cap_between_two_divisors_never_exceeds_the_cap() {
        // 144 Hz display, cap 60: whole refreshes only, so 3 refreshes (48 fps), never 2 (72 fps).
        let intervals = run(60, 1000.0 / 144.0, 40);
        assert!(
            intervals.iter().all(|i| (i - 3000.0 / 144.0).abs() < 1e-3),
            "{intervals:?}"
        );
        // A cap of 72 is met exactly by 2 refreshes.
        let intervals = run(72, 1000.0 / 144.0, 40);
        assert!(
            intervals.iter().all(|i| (i - 2000.0 / 144.0).abs() < 1e-3),
            "{intervals:?}"
        );
    }

    #[test]
    fn a_failing_composition_wait_sleeps_instead_of_spinning() {
        let start = Instant::now();
        let t = std::cell::Cell::new(0.0f64);
        let mut c = FrameCap::default();
        let slept = std::cell::Cell::new(0.0f64);
        let hold = |c: &mut FrameCap| {
            c.hold(
                30,
                || start + Duration::from_secs_f64(t.get() / 1000.0),
                || false,
                |d| {
                    slept.set(slept.get() + d.as_secs_f64() * 1000.0);
                    t.set(t.get() + d.as_secs_f64() * 1000.0);
                },
            )
        };
        assert_eq!(hold(&mut c), 0, "the first frame is never held");
        assert_eq!(hold(&mut c), 1);
        assert!(
            (slept.get() - 30.0).abs() < 1e-3,
            "slept {} ms for a 30 fps cap",
            slept.get()
        );
    }

    #[test]
    fn changing_the_cap_off_forgets_the_last_frame() {
        let start = Instant::now();
        let mut c = FrameCap::default();
        assert_eq!(c.hold(30, || start, || true, |_| {}), 0);
        assert_eq!(c.hold(0, || start, || true, |_| {}), 0);
        // After an uncapped frame the next capped one is not held either.
        assert_eq!(c.hold(30, || start, || true, |_| {}), 0);
    }

    #[test]
    fn waits_before_every_frame_and_reports_nothing_while_it_works() {
        let mut p = Pacer::default();
        let mut calls = 0;
        for _ in 0..100 {
            let change = p.before_frame(|| {
                calls += 1;
                true
            });
            assert_eq!(change, None);
        }
        assert_eq!(calls, 100);
        assert!(p.paced());
    }

    #[test]
    fn isolated_failures_are_not_reported() {
        let mut p = Pacer::default();
        for i in 0..50 {
            // Two failures, then a success, repeated: never three in a row.
            assert_eq!(p.before_frame(|| i % 3 == 2), None);
        }
        assert!(p.paced());
    }

    #[test]
    fn a_run_of_failures_is_reported_once_and_the_recovery_once() {
        let mut p = Pacer::default();
        assert_eq!(p.before_frame(|| false), None);
        assert_eq!(p.before_frame(|| false), None);
        assert_eq!(p.before_frame(|| false), Some(Change::Lost));
        assert!(!p.paced());
        let mut calls = 0;
        for _ in 0..10 {
            assert_eq!(
                p.before_frame(|| {
                    calls += 1;
                    false
                }),
                None
            );
        }
        assert_eq!(calls, 10, "the wait is still tried before every frame");
        assert_eq!(p.before_frame(|| true), Some(Change::Back));
        assert!(p.paced());
        assert_eq!(p.before_frame(|| true), None);
        // A new run is reported again.
        for _ in 0..2 {
            assert_eq!(p.before_frame(|| false), None);
        }
        assert_eq!(p.before_frame(|| false), Some(Change::Lost));
    }
}
