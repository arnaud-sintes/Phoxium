//! The main loop's CPU analyses of the captured frames: the black-frame health check, the Auto picture area, the
//! integer-scale detection and the smoothed-source watch.
//!
//! They read the frame's pixels only through [`Frame::sample`] (this frame's own pixels) or [`Frame::last_sample`]
//! (the newest picture of this frame's size), never `Frame::data` directly. On the zero-copy path (A29) a frame
//! carries its CPU pixels only when a sample was taken for it; a frame without one, or whose buffer is an earlier
//! sample of another size, is simply not analysed ("not ready"), and the analyses wait for the next sample. The
//! detectors validate their input as well (defence in depth): no slice of any length can make them index out of
//! bounds.

use crate::{
    bars,
    capture::{Frame, Sample},
    lifecycle,
    scaling::{self, Rect},
};

/// The picture as one packed slice with its width and height (capture frames are always packed).
fn packed(s: Sample<'_>) -> Option<(&[u8], u32, u32)> {
    s.packed().map(|d| (d, s.width(), s.height()))
}

/// True when every pixel of the sample is black ([`lifecycle::is_black`], row by row).
pub fn is_black(s: Sample<'_>) -> bool {
    (0..s.height()).all(|y| lifecycle::is_black(s.row(y)))
}

/// A frame just taken from the capture: the black-frame scan when it is due and the frame carries its own pixels;
/// the arrival is noted either way.
pub fn note_health(health: &mut lifecycle::Health, now: f64, frame: &Frame) {
    let black = frame.sample().filter(|_| health.wants_sample(now)).map(is_black);
    health.note_frame(now, black);
}

/// A frame just taken from the capture, for the Auto picture area: fed only with its own pixels.
pub fn feed_area(auto: &mut bars::Auto, now: f64, frame: &Frame) {
    if let Some((d, w, h)) = frame.sample().and_then(packed) {
        auto.feed(now, d, w, h);
    }
}

/// The frame on screen for a picture-area detection that starts now (it counts as its first frame): the newest
/// picture of the frame's size, `None` when there is none yet.
pub fn shown(frame: &Frame) -> Option<(&[u8], u32, u32)> {
    if frame.seq == 0 {
        return None;
    }
    frame.last_sample().and_then(packed)
}

/// The integer-scale detection on `region` of the frame. A frame just taken (`got`) is analysed on its own pixels
/// only; the frame on screen analysed again (the source map changed) on the newest picture of its size. Returns
/// true when the detection locked with this frame.
pub fn feed_scale(detector: &mut scaling::Detector, frame: &Frame, got: bool, region: Rect) -> bool {
    let pixels = if got { frame.sample() } else { frame.last_sample() };
    let Some((d, w, _)) = pixels.and_then(packed) else {
        return false;
    };
    let was_locked = detector.is_locked();
    detector.feed(d, w, region);
    detector.is_locked() && !was_locked
}

/// The smoothed-source watch on a frame just taken (its own pixels only). True when the warning has just been
/// raised.
pub fn feed_smoothing(
    smoothing: &mut lifecycle::SmoothingWatch,
    frame: &Frame,
    got: bool,
    region: Rect,
    eligible: bool,
) -> bool {
    got && frame
        .sample()
        .and_then(packed)
        .is_some_and(|(d, w, _)| smoothing.feed(d, w, region, eligible))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic generator (no dependency): xorshift64.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
        /// A dimension: often small or 0, sometimes large, sometimes huge.
        fn dim(&mut self) -> u32 {
            match self.below(10) {
                0 => 0,
                1 => u32::MAX - self.below(4) as u32,
                2 => self.below(1 << 20) as u32,
                _ => self.below(200) as u32,
            }
        }
    }

    /// A frame with x3 content, 192 x 144.
    fn x3_frame() -> (Vec<u8>, u32, u32) {
        let (w, h) = (192u32, 144u32);
        let mut d = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = (((x / 3) * 7 + (y / 3) * 13) % 251) as u8;
                d.extend_from_slice(&[v, v ^ 0x55, v.wrapping_mul(3), 255]);
            }
        }
        (d, w, h)
    }

    /// Defence in depth: every detector's feed takes any buffer length with any dimensions and any region (0 and
    /// huge included) without panicking; an input that does not hold the region is not analysed.
    #[test]
    fn detectors_never_index_out_of_bounds() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let pool: Vec<u8> = (0..64 * 1024).map(|i| (i * 31 % 251) as u8).collect();
        let mut detector = scaling::Detector::default();
        let mut smoothing = lifecycle::SmoothingWatch::default();
        let mut auto = bars::Auto::new(bars::Config::default());
        let mut health = lifecycle::Health::new(0.0);
        for i in 0..20_000u32 {
            let len = match rng.below(4) {
                0 => 0,
                1 => rng.below(64) as usize,
                _ => rng.below(pool.len() as u64 + 1) as usize,
            };
            let data = &pool[..len];
            let (w, h) = (rng.dim(), rng.dim());
            let region = Rect {
                x: if rng.below(3) == 0 { rng.dim() } else { 0 },
                y: if rng.below(3) == 0 { rng.dim() } else { 0 },
                w: if rng.below(2) == 0 { w } else { rng.dim() },
                h: if rng.below(2) == 0 { h } else { rng.dim() },
            };
            let fits = scaling::region_fits(len, w, region);
            if i % 64 == 0 {
                detector.reset();
            }
            let usable = detector.feed(data, w, region);
            assert_eq!(usable, fits, "detector {w}x{h} {region:?} len {len}");
            smoothing.reset();
            let _ = smoothing.feed(data, w, region, true);
            let _ = lifecycle::edge_stats(data, w, region);
            let now = f64::from(i) * 0.01;
            auto.feed(now, data, w, h);
            if i % 97 == 0 {
                auto.start(now, bars::Trigger::DetectNow, Some((data, w, h)));
            }
            let _ = auto.tick(now, (1, 1));
            let _ = bars::report(data, w.min(4096), h.min(4096), &bars::Config::default());
            // The frame accessors: a sample exists only when the buffer holds the frame's picture.
            let frame = Frame {
                width: w,
                height: h,
                data: data.to_vec(),
                data_size: if rng.below(4) == 0 { (h, w) } else { (w, h) },
                pixels_old: rng.below(3) == 0,
                seq: 1,
                ..Frame::default()
            };
            let consistent = frame.data_size == (w, h) && w > 0 && h > 0 && (w as u128 * h as u128 * 4) <= len as u128;
            assert_eq!(frame.last_sample().is_some(), consistent);
            assert_eq!(frame.sample().is_some(), consistent && !frame.pixels_old);
            note_health(&mut health, now, &frame);
            feed_area(&mut auto, now, &frame);
            let _ = shown(&frame);
            let _ = feed_scale(&mut detector, &frame, rng.below(2) == 0, region);
            let _ = feed_smoothing(&mut smoothing, &frame, true, region, true);
        }
    }

    /// The crash of the live review: a frame whose `data` is empty (a zero-copy frame taken while its sample was
    /// being replaced) with the frame's size. Every consumer skips it; nothing panics, nothing is counted.
    #[test]
    fn a_frame_without_its_pixels_is_not_analysed() {
        let frame = Frame {
            width: 768,
            height: 672,
            data: Vec::new(),
            data_size: (768, 672),
            seq: 7,
            ..Frame::default()
        };
        let region = Rect {
            x: 0,
            y: 0,
            w: 768,
            h: 672,
        };
        let mut detector = scaling::Detector::default();
        assert!(!feed_scale(&mut detector, &frame, true, region));
        assert!(!feed_scale(&mut detector, &frame, false, region));
        assert!(
            !detector.feed(&frame.data, frame.width, region),
            "the raw call is refused too"
        );
        let mut auto = bars::Auto::new(bars::Config::default());
        feed_area(&mut auto, 0.0, &frame);
        assert!(!auto.running(), "an empty frame starts no detection");
        assert_eq!(shown(&frame), None);
        let mut health = lifecycle::Health::new(0.0);
        note_health(&mut health, 0.0, &frame);
        assert!(health.wants_sample(0.0), "no scan was counted");
    }

    /// A sample of another size (an earlier frame before a resize) is never read as this frame's pixels, even when
    /// its length would be enough.
    #[test]
    fn a_sample_of_another_size_is_not_read() {
        let (d, w, h) = x3_frame();
        let frame = Frame {
            width: h,
            height: w,
            data: d.clone(),
            data_size: (w, h),
            seq: 3,
            ..Frame::default()
        };
        assert!(frame.sample().is_none() && frame.last_sample().is_none());
        let mut detector = scaling::Detector::default();
        let region = Rect { x: 0, y: 0, w: h, h: w };
        for _ in 0..40 {
            assert!(!feed_scale(&mut detector, &frame, true, region));
        }
        assert!(!detector.is_locked());
        // The same pixels with their own size are analysed and lock to x3.
        let frame = Frame::cpu(w, h, d);
        let region = Rect { x: 0, y: 0, w, h };
        let locked = (0..40)
            .filter(|_| feed_scale(&mut detector, &frame, true, region))
            .count();
        assert_eq!(locked, 1);
        assert_eq!(detector.divisor(), (3, 3));
    }

    /// An older sample of the same size: skipped as a new frame's pixels, used for a re-analysis of the frame on
    /// screen.
    #[test]
    fn an_older_sample_of_the_same_size_serves_only_a_reanalysis() {
        let (d, w, h) = x3_frame();
        let frame = Frame {
            pixels_old: true,
            seq: 5,
            ..Frame::cpu(w, h, d)
        };
        let region = Rect { x: 0, y: 0, w, h };
        let mut detector = scaling::Detector::default();
        for _ in 0..40 {
            feed_scale(&mut detector, &frame, true, region);
        }
        assert!(!detector.is_locked(), "an old sample counted as a new frame");
        for _ in 0..40 {
            feed_scale(&mut detector, &frame, false, region);
        }
        assert_eq!(detector.divisor(), (3, 3));
        assert!(shown(&frame).is_some());
    }

    #[test]
    fn samples_are_consistent_by_construction() {
        assert!(Sample::new(&[], 0, 0).is_none());
        assert!(Sample::new(&[0; 16], 2, 2).is_some());
        assert!(Sample::new(&[0; 15], 2, 2).is_none());
        assert!(Sample::new(&[0; 16], 0, 4).is_none());
        assert!(Sample::new(&[0; 16], u32::MAX, u32::MAX).is_none());
        let padded = [1u8; 2 * 12 + 8];
        let s = Sample::with_stride(&padded, 2, 3, 12).expect("padded rows");
        assert_eq!((s.row(2).len(), s.packed()), (8, None));
        assert!(
            Sample::with_stride(&padded, 2, 3, 4).is_none(),
            "a stride shorter than a row"
        );
        assert!(Sample::with_stride(&padded[..31], 2, 3, 12).is_none());
        assert!(is_black(Sample::new(&[0, 0, 0, 255, 1, 2, 0, 0], 2, 1).unwrap()));
    }
}
