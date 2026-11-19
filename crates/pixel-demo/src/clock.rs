//! Pure frame clock arithmetic for the fixed update rate. The Win32 loop waits on a
//! high-resolution waitable timer until `deadline_ns(n + 1)`; the frame index is derived from the
//! elapsed time, so a late wake-up skips nothing silently (the counter jumps, which the capture
//! side can see) and the long-run rate is exactly `fps`.

/// Frame index shown at `elapsed_ns` after the start: `floor(elapsed * fps)`.
pub fn frame_index(elapsed_ns: u64, fps: u32) -> u64 {
    (elapsed_ns as u128 * fps as u128 / 1_000_000_000) as u64
}

/// Time of the start of frame `n`, in ns after the start (rounded up so the frame has begun).
pub fn deadline_ns(n: u64, fps: u32) -> u64 {
    (n as u128 * 1_000_000_000).div_ceil(fps as u128) as u64
}

/// Animation time in the 60 Hz units used by the existing scene and card art.
pub fn art_time(frame: u64, fps: u32) -> u32 {
    (frame * 60 / fps as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_index_at_known_instants() {
        assert_eq!(frame_index(0, 60), 0);
        assert_eq!(frame_index(16_666_666, 60), 0);
        assert_eq!(frame_index(16_666_667, 60), 1);
        assert_eq!(frame_index(1_000_000_000, 120), 120);
        assert_eq!(frame_index(999_999_999, 50), 49);
    }

    #[test]
    fn deadline_is_the_first_instant_of_the_frame() {
        for fps in [30, 50, 60, 75, 120] {
            for n in [0u64, 1, 2, 59, 1000] {
                let d = deadline_ns(n, fps);
                assert_eq!(frame_index(d, fps), n, "fps {fps} frame {n}");
                if n > 0 {
                    assert_eq!(frame_index(d - 1, fps), n - 1);
                }
            }
        }
        assert_eq!(deadline_ns(1, 120), 8_333_334);
        assert_eq!(deadline_ns(60, 60), 1_000_000_000);
    }

    #[test]
    fn art_time_runs_at_60_per_second() {
        assert_eq!(art_time(60, 60), 60);
        assert_eq!(art_time(120, 120), 60);
        assert_eq!(art_time(30, 30), 60);
        assert_eq!(art_time(1, 120), 0);
        assert_eq!(art_time(2, 120), 1);
    }
}
