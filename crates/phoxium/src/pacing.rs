//! Frame pacing of the live loop (OUT-6): each frame starts after the next desktop composition.
//!
//! The FIFO swapchain alone is not a steady clock on the development laptop: once the GPU lowers its clocks (a light
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

#[cfg(test)]
mod tests {
    use super::*;

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
