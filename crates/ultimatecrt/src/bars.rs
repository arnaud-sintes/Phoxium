//! Black-bar detection (owner requirement 2026-11-20): finds the active picture inside a captured window whose
//! game or emulator draws black bars around it (letterbox, pillarbox, a border area), so the capture crop can be
//! set to the picture.
//!
//! The analysis runs over a short window of recent frames (about one second, at most [`MAX_FRAMES`]), only while
//! a detection is running. Per frame it keeps, for every row and every column of the frame, the running maximum
//! of the brightest channel (max of red, green, blue: the 4-byte layout may be BGRA or RGBA, alpha is ignored).
//! A row or column is part of a bar when that maximum stays within [`Config::tolerance`] codes of black in every
//! frame of the window, so a dark game scene does not look like a bar as long as one frame of the window lights
//! the row up, and near-black noise, dithering or compression does not hide a bar.
//!
//! From the four bar thicknesses (leading and trailing dark rows and columns, each side independently, so
//! symmetric and asymmetric bars both work):
//! - a bar thinner than [`Config::min_bar`] px is ignored (a 1 px window border, a rounding artefact);
//! - an axis whose bars would remove more than [`Config::max_crop`] of it is left uncropped (the guard rail
//!   against a mostly dark scene);
//! - a frame that is black or uniform everywhere is "nothing to detect", never a crop;
//! - with an integer divisor known (manual or detected, `scaling`), the picture size is snapped inward to whole
//!   source pixels: the picture keeps its detected origin and its width and height are reduced to multiples of
//!   the divisor (the remainder goes to the right and bottom bars), so the crop edges fall on source pixel
//!   boundaries.
//!
//! The second half of the module applies a detection to the parameters and the source profile (manual crop),
//! resets the crop to the client area, and undoes the last change ([`CropState`]).

use crate::params::{Params, SOURCE_CROP_MODE_AUTO, SOURCE_CROP_MODE_MANUAL};
use crate::scaling::Rect;
use crate::settings::SourceProfile;

/// Length of the analysis window in seconds: the detection ends after it (with at least one frame).
pub const WINDOW_S: f64 = 1.0;
/// The detection ends early after this many frames (one second at 30 fps).
pub const MAX_FRAMES: u32 = 30;
/// Without any frame (a window that does not redraw) the detection gives up after this many seconds.
pub const TIMEOUT_S: f64 = 3.0;

/// The detection thresholds (`[global.source]` `bars_tolerance`, `bars_min`, `bars_max_crop`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    /// Largest code value (0 to 255, brightest channel) still counted as black.
    pub tolerance: u8,
    /// Thinner bars are ignored (px, at least 1).
    pub min_bar: u32,
    /// Largest fraction of an axis the bars of that axis may remove (0 to 1).
    pub max_crop: f64,
}

pub const DEFAULT_TOLERANCE: u8 = 8;
pub const DEFAULT_MIN_BAR: u32 = 2;
pub const DEFAULT_MAX_CROP: f64 = 0.6;

impl Default for Config {
    fn default() -> Self {
        Config {
            tolerance: DEFAULT_TOLERANCE,
            min_bar: DEFAULT_MIN_BAR,
            max_crop: DEFAULT_MAX_CROP,
        }
    }
}

/// Bar thicknesses in captured pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bars {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

/// A detection that found bars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Found {
    pub bars: Bars,
    /// The active picture inside the analysed frame (the new crop rectangle).
    pub picture: Rect,
    /// An axis was left uncropped by the [`Config::max_crop`] guard.
    pub limited: bool,
    /// The divisor the picture size was snapped to (1 x 1: no snapping).
    pub snapped_to: (u32, u32),
}

/// The outcome of a detection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    Found(Found),
    /// The picture fills the frame (no bar thicker than the minimum).
    NoBars,
    /// Every frame of the window was black: nothing to detect.
    AllBlack,
    /// Every frame of the window was one flat colour: nothing to detect.
    Uniform,
    /// Bars were found but each axis would lose more than the allowed fraction: nothing is cropped.
    TooLarge,
    /// No frame arrived during the detection.
    NoFrames,
}

impl Outcome {
    /// The message the Source section shows: (good news, text).
    pub fn message(&self) -> (bool, String) {
        match self {
            Outcome::Found(f) => {
                let mut text = "Cropped to the picture inside the black bars (stored for this program).".to_string();
                if f.limited {
                    text.push_str(" One axis was left uncropped (its bars were larger than the limit).");
                }
                (true, text)
            }
            Outcome::NoBars => (true, "No black bars found: the picture fills the window.".to_string()),
            Outcome::AllBlack => (
                false,
                "Nothing to detect: the window stayed black. Try again while the game shows a picture.".to_string(),
            ),
            Outcome::Uniform => (
                false,
                "Nothing to detect: the window showed one flat colour. Try again while the game shows a picture."
                    .to_string(),
            ),
            Outcome::TooLarge => (
                false,
                "Nothing cropped: the dark borders found were larger than the limit (probably a dark scene). Try \
                 again on a brighter scene."
                    .to_string(),
            ),
            Outcome::NoFrames => (
                false,
                "Nothing to detect: the window sent no picture. Make it redraw (move it or play) and try again."
                    .to_string(),
            ),
        }
    }
}

/// Per-row and per-column running maxima of the brightest channel over the frames fed so far.
#[derive(Clone, Debug, Default)]
pub struct Accumulator {
    w: u32,
    h: u32,
    rows: Vec<u8>,
    cols: Vec<u8>,
    /// Darkest and brightest value seen anywhere (the uniform-frame test).
    lo: u8,
    hi: u8,
    frames: u32,
}

impl Accumulator {
    /// Frames accumulated since the start (or since the last size change).
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Adds one `w` x `h` frame of 4-byte pixels (BGRA or RGBA). A frame of another size restarts the
    /// accumulation (the window was resized); a short buffer is ignored.
    pub fn feed(&mut self, data: &[u8], w: u32, h: u32) {
        let n = w as usize * h as usize;
        if n == 0 || data.len() < n * 4 {
            return;
        }
        if (w, h) != (self.w, self.h) || self.frames == 0 {
            *self = Accumulator {
                w,
                h,
                rows: vec![0; h as usize],
                cols: vec![0; w as usize],
                lo: u8::MAX,
                hi: 0,
                frames: 0,
            };
        }
        let (mut lo, mut hi) = (self.lo, self.hi);
        for (y, row) in data[..n * 4].chunks_exact(w as usize * 4).enumerate() {
            let mut row_max = 0u8;
            let mut row_min = u8::MAX;
            for (c, px) in self.cols.iter_mut().zip(row.as_chunks::<4>().0) {
                let v = px[0].max(px[1]).max(px[2]);
                row_max = row_max.max(v);
                row_min = row_min.min(v);
                *c = (*c).max(v);
            }
            self.rows[y] = self.rows[y].max(row_max);
            lo = lo.min(row_min);
            hi = hi.max(row_max);
        }
        (self.lo, self.hi) = (lo, hi);
        self.frames += 1;
    }

    /// The bars found in the frames fed so far, the picture snapped to `divisor` (horizontal, vertical; 1 =
    /// no snapping).
    pub fn outcome(&self, cfg: &Config, divisor: (u32, u32)) -> Outcome {
        if self.frames == 0 {
            return Outcome::NoFrames;
        }
        let tol = cfg.tolerance;
        if self.hi <= tol {
            return Outcome::AllBlack;
        }
        if self.hi - self.lo <= tol {
            return Outcome::Uniform;
        }
        let min_bar = cfg.min_bar.max(1);
        let dark = |v: &&u8| **v <= tol;
        let lead = |v: &[u8]| v.iter().take_while(dark).count() as u32;
        let trail = |v: &[u8]| v.iter().rev().take_while(dark).count() as u32;
        let keep = |t: u32| if t >= min_bar { t } else { 0 };
        // Not every row is dark (hi > tol), so the leading and trailing runs never overlap.
        let (mut top, mut bottom) = (keep(lead(&self.rows)), keep(trail(&self.rows)));
        let (mut left, mut right) = (keep(lead(&self.cols)), keep(trail(&self.cols)));
        let mut limited = false;
        let mut guard = |a: &mut u32, b: &mut u32, size: u32| {
            if f64::from(*a + *b) > cfg.max_crop.clamp(0.0, 1.0) * f64::from(size) {
                limited |= *a + *b > 0;
                (*a, *b) = (0, 0);
            }
        };
        guard(&mut top, &mut bottom, self.h);
        guard(&mut left, &mut right, self.w);
        if top + bottom + left + right == 0 {
            return if limited { Outcome::TooLarge } else { Outcome::NoBars };
        }
        // Snap inward to whole source pixels: the origin stays, the size drops to a multiple of the divisor.
        let snap = |size: u32, d: u32| if d > 1 && size >= d { size / d * d } else { size };
        let (w, h) = (self.w - left - right, self.h - top - bottom);
        let (sw, sh) = (snap(w, divisor.0), snap(h, divisor.1));
        right += w - sw;
        bottom += h - sh;
        Outcome::Found(Found {
            bars: Bars {
                left,
                top,
                right,
                bottom,
            },
            picture: Rect {
                x: left,
                y: top,
                w: sw,
                h: sh,
            },
            limited,
            snapped_to: (
                if sw != w || divisor.0 > 1 { divisor.0.max(1) } else { 1 },
                if sh != h || divisor.1 > 1 { divisor.1.max(1) } else { 1 },
            ),
        })
    }
}

/// One running detection: the accumulator and when it started. The app feeds the captured frames while it
/// runs ([`Session::feed`]) and asks every loop whether it is over ([`Session::done`]).
#[derive(Clone, Debug)]
pub struct Session {
    start: f64,
    pub config: Config,
    acc: Accumulator,
}

impl Session {
    pub fn new(now: f64, config: Config) -> Session {
        Session {
            start: now,
            config,
            acc: Accumulator::default(),
        }
    }

    pub fn feed(&mut self, data: &[u8], w: u32, h: u32) {
        self.acc.feed(data, w, h);
    }

    pub fn frames(&self) -> u32 {
        self.acc.frames()
    }

    /// The window is complete: [`MAX_FRAMES`] frames, or [`WINDOW_S`] elapsed with at least one frame, or
    /// [`TIMEOUT_S`] elapsed without any.
    pub fn done(&self, now: f64) -> bool {
        let t = now - self.start;
        self.acc.frames() >= MAX_FRAMES || (t >= WINDOW_S && self.acc.frames() > 0) || t >= TIMEOUT_S
    }

    pub fn outcome(&self, divisor: (u32, u32)) -> Outcome {
        self.acc.outcome(&self.config, divisor)
    }
}

// ---- Applying a detection to the parameters and the source profile ----

/// The crop as the parameters, the source profile and the command line hold it: what [`CropState::restore`]
/// puts back (the Undo of the Source section).
#[derive(Clone, Debug, PartialEq)]
pub struct CropState {
    /// `source_crop_mode` (enum index).
    pub mode: u32,
    pub profile_mode: Option<String>,
    pub profile_crop: Option<[u32; 4]>,
    /// The crop rectangle that wins over the profile's (`--crop`), if any.
    pub override_crop: Option<Rect>,
}

impl CropState {
    pub fn capture(params: &Params, profile: &SourceProfile, override_crop: Option<Rect>) -> CropState {
        CropState {
            mode: params.source_crop_mode,
            profile_mode: profile.crop_mode.clone(),
            profile_crop: profile.crop,
            override_crop,
        }
    }

    pub fn restore(&self, params: &mut Params, profile: &mut SourceProfile, override_crop: &mut Option<Rect>) {
        params.source_crop_mode = self.mode;
        profile.crop_mode = self.profile_mode.clone();
        profile.crop = self.profile_crop;
        *override_crop = self.override_crop;
    }
}

/// Sets the crop to `rect` (captured pixels of the client area): crop mode manual, the rectangle stored in the
/// source profile (left, top, right, bottom) like a manual crop; a command-line crop no longer wins.
pub fn set_manual_crop(params: &mut Params, profile: &mut SourceProfile, override_crop: &mut Option<Rect>, rect: Rect) {
    params.source_crop_mode = SOURCE_CROP_MODE_MANUAL;
    profile.crop_mode = Some("manual".to_string());
    profile.crop = Some([rect.x, rect.y, rect.x + rect.w, rect.y + rect.h]);
    *override_crop = None;
}

/// Back to the whole client area: crop mode auto, no crop rectangle.
pub fn reset_crop(params: &mut Params, profile: &mut SourceProfile, override_crop: &mut Option<Rect>) {
    params.source_crop_mode = SOURCE_CROP_MODE_AUTO;
    profile.crop_mode = Some("auto".to_string());
    profile.crop = None;
    *override_crop = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w` x `h` BGRA frame: black bars of the given thicknesses around a picture whose pixels come from
    /// `pic(x, y)` (picture coordinates).
    fn framed(w: u32, h: u32, b: Bars, pic: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut d = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let inside = x >= b.left && x < w - b.right && y >= b.top && y < h - b.bottom;
                let c = if inside { pic(x - b.left, y - b.top) } else { [0, 0, 0] };
                d.extend_from_slice(&[c[2], c[1], c[0], 255]);
            }
        }
        d
    }

    /// A busy picture (never near black).
    fn busy(x: u32, y: u32) -> [u8; 3] {
        [
            (40 + (x * 7 + y * 3) % 200) as u8,
            (60 + (x * 5) % 150) as u8,
            (30 + (y * 11) % 180) as u8,
        ]
    }

    fn bars(left: u32, top: u32, right: u32, bottom: u32) -> Bars {
        Bars {
            left,
            top,
            right,
            bottom,
        }
    }

    fn detect(frames: &[Vec<u8>], w: u32, h: u32, cfg: Config, div: (u32, u32)) -> Outcome {
        let mut a = Accumulator::default();
        for f in frames {
            a.feed(f, w, h);
        }
        a.outcome(&cfg, div)
    }

    fn found(o: Outcome) -> Found {
        match o {
            Outcome::Found(f) => f,
            other => panic!("expected bars, got {other:?}"),
        }
    }

    #[test]
    fn no_bars() {
        let f = framed(64, 48, Bars::default(), busy);
        assert_eq!(detect(&[f], 64, 48, Config::default(), (1, 1)), Outcome::NoBars);
    }

    #[test]
    fn pillarbox() {
        let b = bars(16, 0, 16, 0);
        let f = found(detect(&[framed(128, 72, b, busy)], 128, 72, Config::default(), (1, 1)));
        assert_eq!(f.bars, b);
        assert_eq!(
            f.picture,
            Rect {
                x: 16,
                y: 0,
                w: 96,
                h: 72
            }
        );
        assert!(!f.limited);
    }

    #[test]
    fn letterbox() {
        let b = bars(0, 10, 0, 10);
        let f = found(detect(&[framed(128, 72, b, busy)], 128, 72, Config::default(), (1, 1)));
        assert_eq!(f.bars, b);
        assert_eq!(
            f.picture,
            Rect {
                x: 0,
                y: 10,
                w: 128,
                h: 52
            }
        );
    }

    #[test]
    fn four_sides_asymmetric() {
        let b = bars(3, 7, 12, 5);
        let f = found(detect(&[framed(100, 80, b, busy)], 100, 80, Config::default(), (1, 1)));
        assert_eq!(f.bars, b);
        assert_eq!(
            f.picture,
            Rect {
                x: 3,
                y: 7,
                w: 85,
                h: 68
            }
        );
        // RGBA input gives the same answer (alpha ignored, max of the three colour bytes).
        let mut rgba = framed(100, 80, b, busy);
        rgba.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p.swap(0, 2));
        assert_eq!(found(detect(&[rgba], 100, 80, Config::default(), (1, 1))).bars, b);
    }

    #[test]
    fn dark_game_content_with_one_bright_frame() {
        let b = bars(8, 4, 8, 4);
        // A dark scene: only the middle of the picture is lit, its edges are black like the bars.
        let dark = framed(96, 64, b, |x, y| {
            if (20..60).contains(&x) && (20..36).contains(&y) {
                [200, 200, 200]
            } else {
                [0, 0, 0]
            }
        });
        // On the dark frames alone the picture edges look like bars (larger than the real ones).
        let wrong = found(detect(std::slice::from_ref(&dark), 96, 64, Config::default(), (1, 1)));
        assert!(wrong.bars.left > b.left);
        // One bright frame in the window lights every picture row and column: the real bars.
        let bright = framed(96, 64, b, |_, _| [90, 90, 90]);
        let frames = vec![dark.clone(), dark.clone(), bright, dark];
        assert_eq!(found(detect(&frames, 96, 64, Config::default(), (1, 1))).bars, b);
    }

    #[test]
    fn noise_within_tolerance_is_still_a_bar() {
        let b = bars(10, 6, 10, 6);
        let mut f = framed(80, 60, b, busy);
        // Near-black noise and dithering in the bars (codes up to 8).
        for (i, p) in f.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = ((i % 80) as u32, (i / 80) as u32);
            if !(10..70).contains(&x) || !(6..54).contains(&y) {
                let v = ((i * 37) % 9) as u8;
                *p = [v, v / 2, v, 255];
            }
        }
        assert_eq!(found(detect(&[f.clone()], 80, 60, Config::default(), (1, 1))).bars, b);
        // A stricter tolerance sees the noise as picture.
        let strict = Config {
            tolerance: 2,
            ..Config::default()
        };
        assert_eq!(detect(&[f], 80, 60, strict, (1, 1)), Outcome::NoBars);
    }

    #[test]
    fn thin_border_is_ignored() {
        // A 1 px black border (a window frame line) is not a bar by default; a thicker bottom bar still is.
        let f = framed(64, 48, bars(1, 1, 1, 6), busy);
        let got = found(detect(std::slice::from_ref(&f), 64, 48, Config::default(), (1, 1)));
        assert_eq!(got.bars, bars(0, 0, 0, 6));
        let one = Config {
            min_bar: 1,
            ..Config::default()
        };
        assert_eq!(found(detect(&[f], 64, 48, one, (1, 1))).bars, bars(1, 1, 1, 6));
    }

    #[test]
    fn all_black_and_uniform_frames_detect_nothing() {
        let black = vec![0u8; 64 * 48 * 4];
        assert_eq!(
            detect(std::slice::from_ref(&black), 64, 48, Config::default(), (1, 1)),
            Outcome::AllBlack
        );
        let grey: Vec<u8> = [120u8, 120, 120, 255].repeat(64 * 48);
        assert_eq!(detect(&[grey], 64, 48, Config::default(), (1, 1)), Outcome::Uniform);
        assert_eq!(detect(&[], 64, 48, Config::default(), (1, 1)), Outcome::NoFrames);
        for o in [
            Outcome::AllBlack,
            Outcome::Uniform,
            Outcome::NoFrames,
            Outcome::TooLarge,
        ] {
            assert!(!o.message().0);
        }
    }

    #[test]
    fn snaps_to_the_divisor_grid() {
        // A 3x upscale whose detected picture is 97 x 70 px: snapped inward to 96 x 69, the origin kept.
        let b = bars(10, 5, 13, 5);
        let f = found(detect(&[framed(120, 80, b, busy)], 120, 80, Config::default(), (3, 3)));
        assert_eq!(
            f.picture,
            Rect {
                x: 10,
                y: 5,
                w: 96,
                h: 69
            }
        );
        assert_eq!(f.bars, bars(10, 5, 14, 6));
        assert_eq!(f.picture.w % 3, 0);
        assert_eq!(f.picture.h % 3, 0);
        assert_eq!(f.snapped_to, (3, 3));
        // Already on the grid: unchanged.
        let g = found(detect(
            &[framed(120, 80, bars(12, 4, 12, 4), busy)],
            120,
            80,
            Config::default(),
            (4, 4),
        ));
        assert_eq!(
            g.picture,
            Rect {
                x: 12,
                y: 4,
                w: 96,
                h: 72
            }
        );
    }

    #[test]
    fn max_crop_guard() {
        // Bars eating 75 % of the width: that axis is left alone, the letterbox still applies.
        let b = bars(40, 6, 40, 6);
        let f = found(detect(&[framed(106, 60, b, busy)], 106, 60, Config::default(), (1, 1)));
        assert_eq!(f.bars, bars(0, 6, 0, 6));
        assert!(f.limited);
        // Only a too-large axis: nothing is cropped.
        let o = detect(
            &[framed(106, 60, bars(40, 0, 40, 0), busy)],
            106,
            60,
            Config::default(),
            (1, 1),
        );
        assert_eq!(o, Outcome::TooLarge);
        // A looser guard accepts it.
        let loose = Config {
            max_crop: 0.8,
            ..Config::default()
        };
        let o = detect(&[framed(106, 60, bars(40, 0, 40, 0), busy)], 106, 60, loose, (1, 1));
        assert_eq!(found(o).bars, bars(40, 0, 40, 0));
    }

    #[test]
    fn accumulation_over_frames_and_size_changes() {
        let b = bars(4, 4, 4, 4);
        let mut s = Session::new(10.0, Config::default());
        assert!(!s.done(10.5));
        // Moving content: each frame lights a different part of the picture; together they cover it.
        for i in 0..4u32 {
            let f = framed(
                48,
                40,
                b,
                |x, y| if (x + y + i) % 4 == 0 { [255, 0, 0] } else { [0, 0, 0] },
            );
            s.feed(&f, 48, 40);
        }
        assert_eq!(s.frames(), 4);
        assert!(!s.done(10.5));
        assert!(s.done(11.0), "one second with frames");
        assert_eq!(found(s.outcome((1, 1))).bars, b);
        // A resize restarts the accumulation on the new size.
        s.feed(&framed(32, 32, bars(0, 2, 0, 2), busy), 32, 32);
        assert_eq!(s.frames(), 1);
        assert_eq!(found(s.outcome((1, 1))).bars, bars(0, 2, 0, 2));
        // The window ends after MAX_FRAMES frames, or after the timeout without frames.
        let mut t = Session::new(0.0, Config::default());
        for _ in 0..MAX_FRAMES {
            t.feed(&framed(16, 16, Bars::default(), busy), 16, 16);
        }
        assert!(t.done(0.1));
        let idle = Session::new(0.0, Config::default());
        assert!(!idle.done(2.0));
        assert!(idle.done(TIMEOUT_S));
        assert_eq!(idle.outcome((1, 1)), Outcome::NoFrames);
    }

    #[test]
    fn crop_apply_reset_and_undo() {
        let mut p = Params::default();
        let mut profile = SourceProfile::default();
        let mut cli = Some(Rect {
            x: 1,
            y: 1,
            w: 10,
            h: 10,
        });
        let before = CropState::capture(&p, &profile, cli);
        let r = Rect {
            x: 8,
            y: 4,
            w: 320,
            h: 240,
        };
        set_manual_crop(&mut p, &mut profile, &mut cli, r);
        assert_eq!(p.source_crop_mode, SOURCE_CROP_MODE_MANUAL);
        assert_eq!(profile.crop, Some([8, 4, 328, 244]));
        assert_eq!(profile.crop_rect(), Some(r));
        assert_eq!(cli, None);
        // The profile alone reproduces it (as at the next attach).
        let mut fresh = Params::default();
        assert!(profile.apply_to(&mut fresh).is_empty());
        assert_eq!(fresh.source_crop_mode, SOURCE_CROP_MODE_MANUAL);
        // Reset: client area, no rectangle.
        let detected = CropState::capture(&p, &profile, cli);
        reset_crop(&mut p, &mut profile, &mut cli);
        assert_eq!((p.source_crop_mode, profile.crop), (SOURCE_CROP_MODE_AUTO, None));
        // Undo of the reset, then of the detection.
        detected.restore(&mut p, &mut profile, &mut cli);
        assert_eq!(profile.crop_rect(), Some(r));
        before.restore(&mut p, &mut profile, &mut cli);
        assert_eq!(CropState::capture(&p, &profile, cli), before);
    }
}
