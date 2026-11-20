//! Picture area (owner requirements 2026-11-20): finds the active picture inside a captured window whose game or
//! emulator draws black bars around it (letterbox, pillarbox, a small game centred in a large window), keeps
//! following it while the app runs (the Auto mode), and holds the manual limits the user sets or tweaks.
//!
//! # Detection
//!
//! The analysis runs over a window of recent frames. Per frame it keeps, for every row and every column of the
//! frame, the running maximum of the brightest channel (max of red, green, blue: the 4-byte layout may be BGRA or
//! RGBA, alpha is ignored). A row or column is part of a bar when that maximum stays within
//! [`Config::tolerance`] codes of black in every frame of the window, so a dark game scene does not look like a
//! bar as long as one frame of the window lights the row up, and near-black noise, dithering or compression does
//! not hide a bar.
//!
//! From the four bar thicknesses (leading and trailing dark rows and columns, each side independently, so
//! symmetric and asymmetric bars both work):
//! - a bar thinner than [`Config::min_bar`] px is ignored (a 1 px window border, a rounding artefact);
//! - the guard rails, per axis: the bars of one axis may remove at most [`Config::max_crop`] of it (default 97 %),
//!   and the picture left must be at least [`MIN_SOURCE_PX`] source pixels and [`MIN_SHARE`] of the axis, so an
//!   all-black or nearly-black capture never collapses to a sliver. A blocked axis is left uncropped and the
//!   reason is reported ([`Blocked`]) instead of a silent no-op;
//! - a frame that is black or uniform everywhere is "nothing to detect", never a crop;
//! - with an integer divisor known (manual or detected, `scaling`), the picture size is snapped inward to whole
//!   source pixels: the picture keeps its detected origin and its width and height are reduced to multiples of
//!   the divisor (the remainder goes to the right and bottom bars), so the crop edges fall on source pixel
//!   boundaries.
//!
//! The first design (2026-11-20) refused to remove more than 60 % of an axis: a 256 x 224 game shown at x1 in a
//! 914 x 754 window loses 72 % of its width and 70 % of its height to the bars, so nothing was cropped (the live
//! failure of QUESTIONS CAP-6). The guard is now a near-total share plus a minimum picture size.
//!
//! # Auto mode ([`Auto`])
//!
//! A full detection ([`Session`], about one second) runs at attach, whenever the captured client size changes,
//! and on "Detect now"; its result applies at once. In between, the tracker samples a few frames per second
//! (adaptive, at most [`COST_SHARE`] of the time) into a window of [`EVAL_S`] seconds and evaluates it:
//! - a picture that grows or moves (the new area is not inside the current one) is followed once
//!   [`CONFIRM`] evaluations in a row agree (so a 1.5 s toast or overlay drawn over a bar is not followed);
//! - a picture that shrinks (a bar became dark) is followed only once the bars stayed dark for [`SHRINK_S`]
//!   seconds of frames: a dark scene, a fade or a black screen never shrinks the crop on its own;
//! - a black, uniform or blocked evaluation is no verdict and changes nothing.
//!
//! # Manual limits
//!
//! [`Bars`] also hold the manual limits (pixels cropped from the left, top, right and bottom of the client area);
//! [`edit_limit`] clamps and snaps one edit, [`set_manual_crop`], [`set_area_mode`] and [`reset_crop`] write the
//! choice to the parameters and the source profile, [`CropState`] undoes the last change.

use std::collections::VecDeque;

use crate::params::{Params, SOURCE_CROP_MODE_AUTO, SOURCE_CROP_MODE_MANUAL, SOURCE_CROP_MODE_NONE};
use crate::scaling::Rect;
use crate::settings::SourceProfile;

/// Length of the analysis window in seconds: the detection ends after it (with at least one frame).
pub const WINDOW_S: f64 = 1.0;
/// The detection ends early after this many frames (one second at 30 fps).
pub const MAX_FRAMES: u32 = 30;
/// Without any frame (a window that does not redraw) the detection gives up after this many seconds.
pub const TIMEOUT_S: f64 = 3.0;

/// The picture left by the bars is at least this many source pixels across and down.
pub const MIN_SOURCE_PX: u32 = 16;
/// The picture left by the bars is at least this share of the client width and height.
pub const MIN_SHARE: f64 = 0.02;
/// The smallest manual picture (px across and down).
pub const MIN_MANUAL_PX: u32 = 8;

/// Auto mode: seconds of frames per evaluation.
pub const EVAL_S: f64 = 2.0;
/// Auto mode: evaluations in a row that must see a larger or moved picture before the crop follows it.
pub const CONFIRM: usize = 3;
/// Auto mode: seconds the bars must stay dark (every sampled frame) before the crop shrinks.
pub const SHRINK_S: f64 = 30.0;
/// Auto mode: shortest interval between two sampled frames (s).
pub const SAMPLE_MIN_S: f64 = 0.25;
/// Auto mode: largest share of the time spent sampling (the interval grows with the cost of a frame).
pub const COST_SHARE: f64 = 0.02;
/// Evaluation windows kept for the shrink rule.
const RING: usize = (SHRINK_S / EVAL_S) as usize;

/// The detection thresholds (`[global.source]` `bars_tolerance`, `bars_min`, `bars_largest_crop`).
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
/// Default of the "Largest crop" guard: the bars may remove up to 97 % of an axis (a 256 px game in a 3840 px
/// wide window loses 93 %).
pub const DEFAULT_MAX_CROP: f64 = 0.97;
/// The range the settings and the menu allow for the "Largest crop" guard.
pub const MAX_CROP_RANGE: (f64, f64) = (0.10, 0.99);

impl Default for Config {
    fn default() -> Self {
        Config {
            tolerance: DEFAULT_TOLERANCE,
            min_bar: DEFAULT_MIN_BAR,
            max_crop: DEFAULT_MAX_CROP,
        }
    }
}

/// Bar thicknesses in captured pixels; also the manual limits (pixels cropped from each side of the client area).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bars {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Bars {
    /// The bars around `rect` inside a `client` frame (a rectangle reaching past the frame gives 0 there).
    pub fn around(rect: Rect, client: (u32, u32)) -> Bars {
        Bars {
            left: rect.x.min(client.0),
            top: rect.y.min(client.1),
            right: client.0.saturating_sub(rect.x + rect.w),
            bottom: client.1.saturating_sub(rect.y + rect.h),
        }
    }

    /// The picture these bars leave in a `client` frame, `None` when nothing is left.
    pub fn picture(&self, client: (u32, u32)) -> Option<Rect> {
        let w = client.0.checked_sub(self.left + self.right).filter(|&w| w > 0)?;
        let h = client.1.checked_sub(self.top + self.bottom).filter(|&h| h > 0)?;
        Some(Rect {
            x: self.left,
            y: self.top,
            w,
            h,
        })
    }

    pub fn get(&self, side: Side) -> u32 {
        match side {
            Side::Left => self.left,
            Side::Top => self.top,
            Side::Right => self.right,
            Side::Bottom => self.bottom,
        }
    }

    fn set(&mut self, side: Side, v: u32) {
        match side {
            Side::Left => self.left = v,
            Side::Top => self.top = v,
            Side::Right => self.right = v,
            Side::Bottom => self.bottom = v,
        }
    }
}

/// One side of the picture (a manual limit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Top,
    Right,
    Bottom,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Left, Side::Top, Side::Right, Side::Bottom];

    pub fn name(self) -> &'static str {
        match self {
            Side::Left => "Left",
            Side::Top => "Top",
            Side::Right => "Right",
            Side::Bottom => "Bottom",
        }
    }

    /// Horizontal (left, right) or vertical (top, bottom).
    pub fn horizontal(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }

    fn opposite(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }
}

/// Sets one manual limit to `value` (px cropped from that side of a `client` frame): clamped so that the limits
/// never cross (the picture stays at least [`MIN_MANUAL_PX`] across and down) and stay inside the frame, and with
/// a `snap` divisor above 1 on that axis, moved so that the picture extent is a whole number of source pixels (the
/// nearest such value; the opposite side stays where it is).
pub fn edit_limit(limits: Bars, side: Side, value: i64, client: (u32, u32), snap: (u32, u32)) -> Bars {
    let size = if side.horizontal() { client.0 } else { client.1 };
    let d = if side.horizontal() { snap.0 } else { snap.1 }.max(1);
    let other = limits.get(side.opposite()).min(size - MIN_MANUAL_PX.min(size));
    let room = size - other; // the extent with this side at 0
    let min = MIN_MANUAL_PX.min(room);
    let max_value = room - min;
    let mut v = value.clamp(0, i64::from(max_value)) as u32;
    if d > 1 {
        // The extent `room - v` to the nearest multiple of `d` within [min, room].
        let extent = room - v;
        let down = extent / d * d;
        let up = down + d;
        let pick = if extent - down <= up.saturating_sub(extent) {
            down
        } else {
            up
        };
        let lowest = min.div_ceil(d) * d;
        let pick = if pick < lowest { lowest } else { pick };
        let pick = if pick > room { room / d * d } else { pick };
        if pick >= min && pick <= room {
            v = room - pick;
        }
    }
    let mut out = limits;
    out.set(side, v);
    out.set(side.opposite(), other);
    out
}

/// Clamps all four limits to a `client` frame (the picture at least [`MIN_MANUAL_PX`] across and down when the
/// frame allows it). Used when the client size changes under manual limits.
pub fn clamp_limits(limits: Bars, client: (u32, u32)) -> Bars {
    let fix = |a: u32, b: u32, size: u32| -> (u32, u32) {
        let min = MIN_MANUAL_PX.min(size);
        let a = a.min(size - min);
        let b = b.min(size - min - a);
        (a, b)
    };
    let (left, right) = fix(limits.left, limits.right, client.0);
    let (top, bottom) = fix(limits.top, limits.bottom, client.1);
    Bars {
        left,
        top,
        right,
        bottom,
    }
}

/// An axis of the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Width,
    Height,
}

impl Axis {
    fn word(self) -> &'static str {
        match self {
            Axis::Width => "width",
            Axis::Height => "height",
        }
    }
    fn across(self) -> &'static str {
        match self {
            Axis::Width => "wide",
            Axis::Height => "high",
        }
    }
}

/// Why a guard rail left an axis uncropped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reason {
    /// The bars would remove more than [`Config::max_crop`] of the axis.
    LargestCrop,
    /// The picture left would be `px` px, less than the minimum `min` px.
    TooSmall { px: u32, min: u32 },
}

/// An axis a guard rail left uncropped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blocked {
    pub axis: Axis,
    /// The share of the axis the bars would remove (0 to 1).
    pub share: f64,
    pub reason: Reason,
}

impl Blocked {
    /// `the bars would remove 99 % of the width: raise Largest crop (Advanced)`.
    pub fn text(&self) -> String {
        let pct = (self.share * 100.0).round().min(99.0);
        match self.reason {
            Reason::LargestCrop => format!(
                "the bars would remove {pct:.0} % of the {}, more than Largest crop allows (raise it in Advanced if \
                 this is right)",
                self.axis.word()
            ),
            Reason::TooSmall { px, min } => format!(
                "the picture would be only {px} px {} (less than {MIN_SOURCE_PX} source pixels or 2 % of the \
                 window, {min} px here)",
                self.axis.across()
            ),
        }
    }
}

/// A detection that found bars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Found {
    pub bars: Bars,
    /// The active picture inside the analysed frame (the new crop rectangle).
    pub picture: Rect,
    /// An axis left uncropped by a guard rail, and why.
    pub limited: Option<Blocked>,
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
    /// Bars were found but a guard rail blocked every axis that had some: nothing is cropped.
    Blocked(Blocked),
    /// No frame arrived during the detection.
    NoFrames,
}

impl Outcome {
    /// The message the Source section shows: (good news, text).
    pub fn message(&self) -> (bool, String) {
        match self {
            Outcome::Found(f) => {
                let p = f.picture;
                let mut text = format!(
                    "Picture found inside the black bars: {} \u{00D7} {} at {}, {}.",
                    p.w, p.h, p.x, p.y
                );
                if let Some(b) = f.limited {
                    text.push_str(&format!(" One axis was left uncropped: {}.", b.text()));
                }
                (true, text)
            }
            Outcome::NoBars => (true, "No black bars found: the picture fills the window.".to_string()),
            Outcome::AllBlack => (
                false,
                "Nothing to detect: the window stayed black. The picture area is found as soon as the game shows \
                 a picture."
                    .to_string(),
            ),
            Outcome::Uniform => (
                false,
                "Nothing to detect: the window showed one flat colour. The picture area is found as soon as the \
                 game shows a picture."
                    .to_string(),
            ),
            Outcome::Blocked(b) => (false, format!("Nothing cropped: {}.", b.text())),
            Outcome::NoFrames => (
                false,
                "Nothing to detect: the window sent no picture. Make it redraw (move it or play) and try again."
                    .to_string(),
            ),
        }
    }

    /// The picture area this outcome gives in a `client` frame: `Some(rect)` for a verdict (the whole frame when
    /// there are no bars), `None` for no verdict (black, uniform, blocked, no frames).
    pub fn area(&self, client: (u32, u32)) -> Option<Rect> {
        match self {
            Outcome::Found(f) => Some(f.picture),
            Outcome::NoBars => Some(full(client)),
            _ => None,
        }
    }
}

fn full(client: (u32, u32)) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: client.0,
        h: client.1,
    }
}

/// `a` lies inside `b`.
fn within(a: Rect, b: Rect) -> bool {
    a.x >= b.x && a.y >= b.y && a.x + a.w <= b.x + b.w && a.y + a.h <= b.y + b.h
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

    /// The frame size of the frames accumulated.
    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
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

    /// Adds the maxima of `other` (same frame size; another size or an empty one is ignored).
    pub fn merge(&mut self, other: &Accumulator) {
        if other.frames == 0 {
            return;
        }
        if self.frames == 0 {
            *self = other.clone();
            return;
        }
        if (other.w, other.h) != (self.w, self.h) {
            return;
        }
        for (a, b) in self.rows.iter_mut().zip(&other.rows) {
            *a = (*a).max(*b);
        }
        for (a, b) in self.cols.iter_mut().zip(&other.cols) {
            *a = (*a).max(*b);
        }
        self.lo = self.lo.min(other.lo);
        self.hi = self.hi.max(other.hi);
        self.frames += other.frames;
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
        let max_crop = if cfg.max_crop.is_finite() {
            cfg.max_crop.clamp(0.0, 1.0)
        } else {
            DEFAULT_MAX_CROP
        };
        let mut limited: Option<Blocked> = None;
        let mut guard = |a: &mut u32, b: &mut u32, size: u32, d: u32, axis: Axis| {
            let cut = *a + *b;
            if cut == 0 {
                return;
            }
            let left_px = size - cut;
            let min_px = (MIN_SOURCE_PX * d.max(1))
                .max((MIN_SHARE * f64::from(size)).ceil() as u32)
                .min(size);
            let share = f64::from(cut) / f64::from(size);
            let reason = if left_px < min_px {
                Some(Reason::TooSmall {
                    px: left_px,
                    min: min_px,
                })
            } else if share > max_crop {
                Some(Reason::LargestCrop)
            } else {
                None
            };
            if let Some(reason) = reason {
                limited.get_or_insert(Blocked { axis, share, reason });
                (*a, *b) = (0, 0);
            }
        };
        guard(&mut left, &mut right, self.w, divisor.0, Axis::Width);
        guard(&mut top, &mut bottom, self.h, divisor.1, Axis::Height);
        if top + bottom + left + right == 0 {
            return match limited {
                Some(b) => Outcome::Blocked(b),
                None => Outcome::NoBars,
            };
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

// ---- Auto mode ----

/// What started a full detection of the Auto mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// A window was attached (or the Auto mode chosen).
    Attach,
    /// The captured client size changed.
    Resize,
    /// The Source section's "Detect now".
    DetectNow,
}

/// How the Auto mode changed the picture area between full detections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follow {
    /// The picture grew or moved (confirmed by [`CONFIRM`] evaluations).
    Grew,
    /// The bars stayed dark for [`SHRINK_S`]: the picture shrank.
    Shrank,
    /// The first verdict after a detection that found nothing (a black screen at attach).
    Found,
}

/// What the Auto mode did in one [`Auto::tick`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AutoEvent {
    /// A full detection ended (its outcome; the area changed when it was a verdict).
    Detected(Trigger, Outcome),
    /// The periodic check moved the picture area.
    Followed(Follow, Rect),
}

/// The Auto mode's tracker: the full detections and the cheap periodic check with its hysteresis. Pure (the
/// caller gives the time and the frames); only the cost meter reads the clock.
#[derive(Clone, Debug)]
pub struct Auto {
    config: Config,
    /// Client size of the frames seen (0 x 0 before the first).
    size: (u32, u32),
    /// The full detection running and what started it.
    session: Option<(Session, Trigger)>,
    /// The evaluation window being filled and when it started.
    bucket: Accumulator,
    bucket_start: Option<f64>,
    /// Completed evaluation windows (with frames), oldest first, at most [`RING`].
    ring: VecDeque<Accumulator>,
    /// The picture area in force, `None` = the whole client area.
    current: Option<Rect>,
    /// `current` comes from a verdict (a detection that found bars or none); false after a black start.
    settled: bool,
    /// Evaluations in a row whose area is larger than or moved from `current`.
    pending: usize,
    next_sample: f64,
    /// Sampled frames and seconds spent sampling since the last [`Auto::take_cost`].
    cost: (u32, f64),
}

impl Auto {
    pub fn new(config: Config) -> Auto {
        Auto {
            config,
            size: (0, 0),
            session: None,
            bucket: Accumulator::default(),
            bucket_start: None,
            ring: VecDeque::new(),
            current: None,
            settled: false,
            pending: 0,
            next_sample: 0.0,
            cost: (0, 0.0),
        }
    }

    pub fn set_config(&mut self, config: Config) {
        self.config = config;
    }

    /// The picture area in force, `None` = the whole client area.
    pub fn rect(&self) -> Option<Rect> {
        self.current
    }

    /// A full detection is running.
    pub fn running(&self) -> bool {
        self.session.is_some()
    }

    /// Forgets everything (another window): the whole client area until the next detection.
    pub fn reset(&mut self) {
        *self = Auto::new(self.config);
    }

    /// Takes `area` (in a `client` frame, `None` = the whole client area) as the area in force until the next
    /// verdict: switching to Auto keeps the picture where it was while the detection runs.
    pub fn seed(&mut self, area: Option<Rect>, client: (u32, u32)) {
        self.size = client;
        self.current = area;
        self.settled = false;
    }

    /// Starts a full detection now. The periodic check restarts after it (the hysteresis is reset). `frame`, the
    /// frame on screen, counts (a game that redraws rarely still gives one).
    pub fn start(&mut self, now: f64, trigger: Trigger, frame: Option<(&[u8], u32, u32)>) {
        let mut s = Session::new(now, self.config);
        if let Some((data, w, h)) = frame {
            s.feed(data, w, h);
            self.size = (w, h);
        }
        self.session = Some((s, trigger));
        self.restart_check(now);
    }

    fn restart_check(&mut self, now: f64) {
        self.bucket = Accumulator::default();
        self.bucket_start = Some(now);
        self.ring.clear();
        self.pending = 0;
    }

    /// One captured frame (the client area). A new client size starts a full detection ([`Trigger::Resize`]) and
    /// drops the area in force (the picture moved with the window). While a detection runs every frame counts;
    /// otherwise frames are sampled at most every [`SAMPLE_MIN_S`] s, less often when a frame is expensive.
    pub fn feed(&mut self, now: f64, data: &[u8], w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        if (w, h) != self.size {
            let first = self.size == (0, 0);
            self.size = (w, h);
            if !first || self.session.is_none() {
                self.current = None;
                self.settled = false;
                self.start(
                    now,
                    if first { Trigger::Attach } else { Trigger::Resize },
                    Some((data, w, h)),
                );
                return;
            }
        }
        if let Some((s, _)) = self.session.as_mut() {
            s.feed(data, w, h);
            return;
        }
        if now < self.next_sample {
            return;
        }
        let t0 = std::time::Instant::now();
        self.bucket.feed(data, w, h);
        let spent = t0.elapsed().as_secs_f64();
        self.cost = (self.cost.0 + 1, self.cost.1 + spent);
        self.next_sample = now + SAMPLE_MIN_S.max(spent / COST_SHARE);
    }

    /// Frames sampled and seconds spent since the last call (the `--stats` line).
    pub fn take_cost(&mut self) -> (u32, f64) {
        std::mem::take(&mut self.cost)
    }

    /// Advances the tracker to `now`: ends a full detection, or evaluates the periodic check every [`EVAL_S`]
    /// (only when frames arrived). `divisor` snaps the picture to whole source pixels.
    pub fn tick(&mut self, now: f64, divisor: (u32, u32)) -> Option<AutoEvent> {
        if let Some((s, trigger)) = &self.session {
            if !s.done(now) {
                return None;
            }
            let (outcome, trigger) = (s.outcome(divisor), *trigger);
            self.session = None;
            if let Some(area) = outcome.area(self.size) {
                self.set_current(area);
                self.settled = true;
            }
            self.restart_check(now);
            return Some(AutoEvent::Detected(trigger, outcome));
        }
        let start = *self.bucket_start.get_or_insert(now);
        if now - start < EVAL_S {
            return None;
        }
        self.bucket_start = Some(now);
        let bucket = std::mem::take(&mut self.bucket);
        if bucket.frames() == 0 || bucket.size() != self.size {
            return None;
        }
        // Black, uniform or blocked: no verdict, the area stays, and the window does not count towards the
        // shrink rule (a fade to black is not evidence that the bars grew).
        let short = bucket.outcome(&self.config, divisor).area(self.size);
        if short.is_none() {
            self.pending = 0;
            return None;
        }
        self.ring.push_back(bucket);
        while self.ring.len() > RING {
            self.ring.pop_front();
        }
        let merged = |n: usize| {
            let mut acc = Accumulator::default();
            for b in self.ring.iter().rev().take(n) {
                acc.merge(b);
            }
            acc.outcome(&self.config, divisor).area(self.size)
        };
        let cur = self.current.unwrap_or(full(self.size));
        if !self.settled {
            // Nothing found yet (a black start): the first verdict over every frame since then applies.
            let area = merged(RING)?;
            self.set_current(area);
            self.settled = true;
            self.restart_check(now);
            return Some(AutoEvent::Followed(Follow::Found, area));
        }
        match short {
            None => None,
            Some(a) if !within(a, cur) => {
                // Larger or moved: followed once CONFIRM evaluations in a row agree; the new area is the one of
                // the last evaluations together (the first may still hold frames of the old picture).
                self.pending += 1;
                if self.pending < CONFIRM {
                    return None;
                }
                let area = merged(CONFIRM - 1).filter(|r| !within(*r, cur))?;
                self.set_current(area);
                self.restart_check(now);
                Some(AutoEvent::Followed(Follow::Grew, area))
            }
            Some(_) => {
                self.pending = 0;
                // Smaller: only when the bars stayed dark over the whole shrink window.
                if self.ring.len() < RING {
                    return None;
                }
                let area = merged(RING).filter(|r| within(*r, cur) && *r != cur)?;
                self.set_current(area);
                self.restart_check(now);
                Some(AutoEvent::Followed(Follow::Shrank, area))
            }
        }
    }

    fn set_current(&mut self, area: Rect) {
        self.current = (area != full(self.size)).then_some(area);
    }
}

// ---- The picture area mode and the manual limits in the parameters and the source profile ----

/// The Source section's "Picture area" choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AreaMode {
    /// Detected and followed by the app (the default).
    #[default]
    Auto,
    /// The user's limits (the source profile's crop rectangle, or `--crop`).
    Manual,
    /// The whole client area, no crop.
    Client,
}

impl AreaMode {
    pub const ALL: [AreaMode; 3] = [AreaMode::Auto, AreaMode::Manual, AreaMode::Client];

    pub fn label(self) -> &'static str {
        match self {
            AreaMode::Auto => "Auto",
            AreaMode::Manual => "Manual",
            AreaMode::Client => "Whole client area",
        }
    }

    /// The profile value (`picture_area`).
    pub fn key(self) -> &'static str {
        match self {
            AreaMode::Auto => "auto",
            AreaMode::Manual => "manual",
            AreaMode::Client => "client",
        }
    }

    /// The mode the parameters and the profile give: crop mode manual is Manual; crop mode auto is Auto unless the
    /// profile says `picture_area = "client"`. Crop mode none (the whole window, frame included) reads as Client.
    pub fn of(params: &Params, profile: &SourceProfile) -> AreaMode {
        match params.source_crop_mode {
            SOURCE_CROP_MODE_MANUAL => AreaMode::Manual,
            SOURCE_CROP_MODE_NONE => AreaMode::Client,
            _ if profile.picture_area.as_deref() == Some("client") => AreaMode::Client,
            _ => AreaMode::Auto,
        }
    }
}

/// The crop as the parameters, the source profile and the command line hold it: what [`CropState::restore`]
/// puts back (the Undo of the Source section).
#[derive(Clone, Debug, PartialEq)]
pub struct CropState {
    /// `source_crop_mode` (enum index).
    pub mode: u32,
    pub profile_mode: Option<String>,
    pub profile_crop: Option<[u32; 4]>,
    pub profile_area: Option<String>,
    pub profile_client: Option<[u32; 2]>,
    /// The crop rectangle that wins over the profile's (`--crop`), if any.
    pub override_crop: Option<Rect>,
}

impl CropState {
    pub fn capture(params: &Params, profile: &SourceProfile, override_crop: Option<Rect>) -> CropState {
        CropState {
            mode: params.source_crop_mode,
            profile_mode: profile.crop_mode.clone(),
            profile_crop: profile.crop,
            profile_area: profile.picture_area.clone(),
            profile_client: profile.crop_client,
            override_crop,
        }
    }

    pub fn restore(&self, params: &mut Params, profile: &mut SourceProfile, override_crop: &mut Option<Rect>) {
        params.source_crop_mode = self.mode;
        profile.crop_mode = self.profile_mode.clone();
        profile.crop = self.profile_crop;
        profile.picture_area = self.profile_area.clone();
        profile.crop_client = self.profile_client;
        *override_crop = self.override_crop;
    }
}

/// Manual mode with the crop `rect` (captured pixels of a `client` frame): crop mode manual, the rectangle stored
/// in the source profile (left, top, right, bottom) with the client size it was set for; a command-line crop no
/// longer wins.
pub fn set_manual_crop(
    params: &mut Params,
    profile: &mut SourceProfile,
    override_crop: &mut Option<Rect>,
    rect: Rect,
    client: (u32, u32),
) {
    params.source_crop_mode = SOURCE_CROP_MODE_MANUAL;
    profile.crop_mode = Some("manual".to_string());
    profile.crop = Some([rect.x, rect.y, rect.x + rect.w, rect.y + rect.h]);
    profile.picture_area = Some(AreaMode::Manual.key().to_string());
    profile.crop_client = Some([client.0, client.1]);
    *override_crop = None;
}

/// Auto or Client mode (Manual goes through [`set_manual_crop`]): crop mode auto (the client area is captured), no
/// crop rectangle in the profile, a command-line crop no longer wins.
pub fn set_area_mode(
    params: &mut Params,
    profile: &mut SourceProfile,
    override_crop: &mut Option<Rect>,
    mode: AreaMode,
) {
    params.source_crop_mode = SOURCE_CROP_MODE_AUTO;
    profile.crop_mode = Some("auto".to_string());
    profile.crop = None;
    profile.crop_client = None;
    profile.picture_area = Some(mode.key().to_string());
    *override_crop = None;
}

/// "Reset (whole client area)": Client mode.
pub fn reset_crop(params: &mut Params, profile: &mut SourceProfile, override_crop: &mut Option<Rect>) {
    set_area_mode(params, profile, override_crop, AreaMode::Client);
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

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    /// The bars that centre a `pw` x `ph` picture in a `w` x `h` window (odd remainders to the right and bottom).
    fn centred(w: u32, h: u32, pw: u32, ph: u32) -> Bars {
        let (l, t) = ((w - pw) / 2, (h - ph) / 2);
        bars(l, t, w - pw - l, h - ph - t)
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
        assert_eq!(f.picture, rect(16, 0, 96, 72));
        assert_eq!(f.limited, None);
    }

    #[test]
    fn letterbox() {
        let b = bars(0, 10, 0, 10);
        let f = found(detect(&[framed(128, 72, b, busy)], 128, 72, Config::default(), (1, 1)));
        assert_eq!(f.bars, b);
        assert_eq!(f.picture, rect(0, 10, 128, 52));
    }

    #[test]
    fn four_sides_asymmetric() {
        let b = bars(3, 7, 12, 5);
        let f = found(detect(&[framed(100, 80, b, busy)], 100, 80, Config::default(), (1, 1)));
        assert_eq!(f.bars, b);
        assert_eq!(f.picture, rect(3, 7, 85, 68));
        // RGBA input gives the same answer (alpha ignored, max of the three colour bytes).
        let mut rgba = framed(100, 80, b, busy);
        rgba.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p.swap(0, 2));
        assert_eq!(found(detect(&[rgba], 100, 80, Config::default(), (1, 1))).bars, b);
    }

    // ---- The owner's live geometry (2026-11-20) and its relatives ----

    /// The live failure: pixel-demo 256 x 224 at x1 centred in a 914 x 754 client area. The first design's 60 %
    /// guard refused both axes (72 % of the width, 70 % of the height); the new default finds the exact picture.
    #[test]
    fn small_game_centred_in_a_large_window() {
        let (w, h) = (914, 754);
        let b = centred(w, h, 256, 224);
        assert_eq!(b, bars(329, 265, 329, 265));
        let f = framed(w, h, b, busy);
        let old = Config {
            max_crop: 0.6,
            ..Config::default()
        };
        match detect(std::slice::from_ref(&f), w, h, old, (1, 1)) {
            Outcome::Blocked(bl) => {
                assert_eq!(bl.axis, Axis::Width);
                assert_eq!(bl.reason, Reason::LargestCrop);
                assert!((bl.share - 658.0 / 914.0).abs() < 1e-9);
            }
            other => panic!("the old guard should block, got {other:?}"),
        }
        let got = found(detect(&[f], w, h, Config::default(), (1, 1)));
        assert_eq!(got.picture, rect(329, 265, 256, 224));
        assert_eq!(got.bars, b);
        assert_eq!(got.limited, None);
    }

    /// The same game at x3 (768 x 672) in the same window, also snapped to the detected x3.
    #[test]
    fn same_game_at_x3() {
        let (w, h) = (914, 754);
        let b = centred(w, h, 768, 672);
        let f = framed(w, h, b, |x, y| busy(x / 3, y / 3));
        for div in [(1, 1), (3, 3)] {
            let got = found(detect(std::slice::from_ref(&f), w, h, Config::default(), div));
            assert_eq!(got.picture, rect(73, 41, 768, 672), "{div:?}");
        }
    }

    /// The demo's test card: a black picture with bright corner markers at the picture corners and a frame
    /// counter near its top-left corner. The markers light the first and last rows and columns of the picture.
    #[test]
    fn markers_and_counter_on_a_black_picture() {
        let (w, h, pw, ph) = (914, 754, 256, 224);
        let b = centred(w, h, pw, ph);
        let pic = |x: u32, y: u32| {
            let corner = (x < 6 || x >= pw - 6) && (y < 6 || y >= ph - 6);
            let counter = (8..72).contains(&x) && (8..16).contains(&y) && (x / 2 + y).is_multiple_of(3);
            if corner {
                [255, 255, 255]
            } else if counter {
                [255, 255, 0]
            } else {
                [0, 0, 0]
            }
        };
        let got = found(detect(&[framed(w, h, b, pic)], w, h, Config::default(), (1, 1)));
        assert_eq!(got.picture, rect(329, 265, 256, 224));
    }

    /// Bright pixels exactly on the picture edges (a 1 px frame): the edges belong to the picture.
    #[test]
    fn bright_pixels_on_the_picture_edges() {
        let (w, h, pw, ph) = (914, 754, 256, 224);
        let b = centred(w, h, pw, ph);
        let pic = |x: u32, y: u32| {
            if x == 0 || y == 0 || x == pw - 1 || y == ph - 1 {
                [200, 0, 0]
            } else {
                [0, 0, 0]
            }
        };
        let got = found(detect(&[framed(w, h, b, pic)], w, h, Config::default(), (1, 1)));
        assert_eq!(got.picture, rect(329, 265, 256, 224));
    }

    /// A dark game scene: only the middle of the picture is lit, until one bright frame lights all of it.
    #[test]
    fn dark_scene_with_a_single_bright_frame() {
        let (w, h) = (914, 754);
        let b = centred(w, h, 256, 224);
        let dark = framed(w, h, b, |x, y| {
            if (100..150).contains(&x) && (90..130).contains(&y) {
                [180, 180, 180]
            } else {
                [0, 0, 0]
            }
        });
        let wrong = found(detect(std::slice::from_ref(&dark), w, h, Config::default(), (1, 1)));
        assert!(wrong.picture.w < 256, "the dark frames alone see a smaller picture");
        let bright = framed(w, h, b, |_, _| [60, 60, 60]);
        let frames = vec![dark.clone(), dark.clone(), bright, dark];
        assert_eq!(
            found(detect(&frames, w, h, Config::default(), (1, 1))).picture,
            rect(329, 265, 256, 224)
        );
    }

    /// A 4:3 picture in a 1280 x 720 window (pillarbox 160 px each side).
    #[test]
    fn four_by_three_in_a_wide_window() {
        let b = centred(1280, 720, 960, 720);
        let got = found(detect(
            &[framed(1280, 720, b, busy)],
            1280,
            720,
            Config::default(),
            (1, 1),
        ));
        assert_eq!(got.picture, rect(160, 0, 960, 720));
        assert_eq!(got.bars, bars(160, 0, 160, 0));
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
        let blocked = Outcome::Blocked(Blocked {
            axis: Axis::Width,
            share: 0.99,
            reason: Reason::LargestCrop,
        });
        for o in [Outcome::AllBlack, Outcome::Uniform, Outcome::NoFrames, blocked] {
            assert!(!o.message().0);
            assert_eq!(o.area((64, 48)), None);
        }
    }

    #[test]
    fn snaps_to_the_divisor_grid() {
        // A 3x upscale whose detected picture is 97 x 70 px: snapped inward to 96 x 69, the origin kept.
        let b = bars(10, 5, 13, 5);
        let f = found(detect(&[framed(120, 80, b, busy)], 120, 80, Config::default(), (3, 3)));
        assert_eq!(f.picture, rect(10, 5, 96, 69));
        assert_eq!(f.bars, bars(10, 5, 14, 6));
        assert_eq!(f.snapped_to, (3, 3));
        // Already on the grid: unchanged.
        let g = found(detect(
            &[framed(120, 80, bars(12, 4, 12, 4), busy)],
            120,
            80,
            Config::default(),
            (4, 4),
        ));
        assert_eq!(g.picture, rect(12, 4, 96, 72));
    }

    /// The guard rails: Largest crop (now 97 % by default) and the minimum picture (16 source px, 2 % of the
    /// axis), each axis on its own, with the reason in the message.
    #[test]
    fn guard_rails_report_their_reason() {
        // 99 % of the width: more than Largest crop; the letterbox still applies.
        let (w, h) = (1000, 200);
        let b = bars(495, 20, 495, 20); // a 10 px wide picture... also below the minimum
        let o = found(detect(&[framed(w, h, b, busy)], w, h, Config::default(), (1, 1)));
        assert_eq!(o.bars, bars(0, 20, 0, 20));
        let lim = o.limited.expect("the width was blocked");
        assert_eq!(lim.axis, Axis::Width);
        assert!(matches!(lim.reason, Reason::TooSmall { px: 10, min: 20 }), "{lim:?}");
        // Only the blocked axis has bars: nothing cropped, the reason is the message.
        let only = detect(
            &[framed(w, h, bars(495, 0, 495, 0), busy)],
            w,
            h,
            Config::default(),
            (1, 1),
        );
        let Outcome::Blocked(bl) = only else { panic!("{only:?}") };
        assert!(only.message().1.contains("only 10 px wide"), "{}", only.message().1);
        assert_eq!(bl.reason, Reason::TooSmall { px: 10, min: 20 });
        // A 40 px picture in 1000 px (96 %) passes the default; a 90 % Largest crop blocks it and says so.
        let ok = detect(
            &[framed(w, h, bars(480, 0, 480, 0), busy)],
            w,
            h,
            Config::default(),
            (1, 1),
        );
        assert_eq!(found(ok).picture, rect(480, 0, 40, 200));
        let tight = Config {
            max_crop: 0.9,
            ..Config::default()
        };
        let o = detect(&[framed(w, h, bars(480, 0, 480, 0), busy)], w, h, tight, (1, 1));
        assert!(matches!(
            o,
            Outcome::Blocked(Blocked {
                reason: Reason::LargestCrop,
                ..
            })
        ));
        assert!(o.message().1.contains("96 % of the width"), "{}", o.message().1);
        // The minimum counts source pixels: 40 px at x3 is 13 source px, below 16.
        let o = detect(
            &[framed(w, h, bars(480, 0, 480, 0), busy)],
            w,
            h,
            Config::default(),
            (3, 3),
        );
        assert!(
            matches!(
                o,
                Outcome::Blocked(Blocked {
                    reason: Reason::TooSmall { .. },
                    ..
                })
            ),
            "{o:?}"
        );
        // A nearly black capture with one bright pixel column never collapses to a sliver.
        let sliver = framed(914, 754, bars(500, 0, 413, 0), busy);
        assert!(matches!(
            detect(&[sliver], 914, 754, Config::default(), (1, 1)),
            Outcome::Blocked(_)
        ));
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

    // ---- The Auto mode's state machine ----

    const W: u32 = 320;
    const H: u32 = 240;

    /// Drives an [`Auto`] at 30 fps from `t0` for `secs` seconds with frames from `frame(t)`; returns the events.
    fn run(a: &mut Auto, t0: f64, secs: f64, frame: impl Fn(f64) -> Vec<u8>) -> Vec<AutoEvent> {
        let mut events = Vec::new();
        let n = (secs * 30.0).round() as u32;
        for i in 0..n {
            let t = t0 + f64::from(i) / 30.0;
            a.feed(t, &frame(t), W, H);
            events.extend(a.tick(t, (1, 1)));
        }
        events
    }

    fn picture_in(b: Bars) -> impl Fn(f64) -> Vec<u8> {
        move |_| framed(W, H, b, busy)
    }

    /// An Auto tracker attached at `t = 0` to a 320 x 240 window with the picture inside `b`, after its detection.
    fn settled(b: Bars) -> Auto {
        let mut a = Auto::new(Config::default());
        a.start(0.0, Trigger::Attach, None);
        let ev = run(&mut a, 0.0, 1.5, picture_in(b));
        assert!(
            matches!(ev[..], [AutoEvent::Detected(Trigger::Attach, Outcome::Found(_))]),
            "{ev:?}"
        );
        assert_eq!(a.rect(), b.picture((W, H)));
        a
    }

    #[test]
    fn auto_detects_at_attach_and_on_resize() {
        let b = bars(40, 20, 40, 20);
        let mut a = settled(b);
        assert!(!a.running());
        // The window is resized: the area is dropped and a detection runs on the new size.
        let (w2, h2) = (400, 300);
        let b2 = centred(w2, h2, 240, 200);
        a.feed(10.0, &framed(w2, h2, b2, busy), w2, h2);
        assert!(a.running());
        assert_eq!(a.rect(), None);
        let mut events = Vec::new();
        for i in 1..40 {
            let t = 10.0 + f64::from(i) / 30.0;
            a.feed(t, &framed(w2, h2, b2, busy), w2, h2);
            events.extend(a.tick(t, (1, 1)));
        }
        assert!(
            matches!(events[..], [AutoEvent::Detected(Trigger::Resize, Outcome::Found(_))]),
            "{events:?}"
        );
        assert_eq!(a.rect(), b2.picture((w2, h2)));
        // Detect now restarts a detection (and the hysteresis) at once.
        a.start(20.0, Trigger::DetectNow, Some((&framed(w2, h2, b2, busy), w2, h2)));
        assert!(a.running());
        assert!(matches!(
            a.tick(21.0, (1, 1)),
            Some(AutoEvent::Detected(Trigger::DetectNow, _))
        ));
    }

    #[test]
    fn auto_black_start_then_the_picture_appears() {
        let mut a = Auto::new(Config::default());
        a.start(0.0, Trigger::Attach, None);
        let black = |_: f64| vec![0u8; (W * H * 4) as usize];
        let ev = run(&mut a, 0.0, 1.5, black);
        assert!(matches!(ev[..], [AutoEvent::Detected(_, Outcome::AllBlack)]), "{ev:?}");
        assert_eq!(a.rect(), None);
        // Still black: nothing changes.
        assert!(run(&mut a, 1.5, 10.0, black).is_empty());
        assert_eq!(a.rect(), None);
        // The game shows its picture: found at the next evaluation (no 30 s wait).
        let b = bars(50, 30, 50, 30);
        let ev = run(&mut a, 11.5, 2.5, picture_in(b));
        assert!(matches!(ev[..], [AutoEvent::Followed(Follow::Found, _)]), "{ev:?}");
        assert_eq!(a.rect(), b.picture((W, H)));
    }

    #[test]
    fn auto_hysteresis_ignores_a_short_overlay_in_a_bar() {
        let b = bars(40, 20, 40, 20);
        let mut a = settled(b);
        // A 1.5 s toast drawn over the bottom bar (as pixel-demo shows after a key): never followed.
        let toast = move |t: f64| {
            let mut f = framed(W, H, b, busy);
            if (5.0..6.5).contains(&t) {
                for y in H - 15..H - 5 {
                    for x in 100..220 {
                        let i = ((y * W + x) * 4) as usize;
                        f[i..i + 3].copy_from_slice(&[32, 32, 32]);
                    }
                }
            }
            f
        };
        let ev = run(&mut a, 1.5, 20.0, toast);
        assert!(ev.is_empty(), "{ev:?}");
        assert_eq!(a.rect(), b.picture((W, H)));
    }

    #[test]
    fn auto_follows_a_picture_that_really_grows_or_moves() {
        let b = bars(40, 20, 40, 20);
        let mut a = settled(b);
        let bigger = bars(20, 10, 20, 10);
        let ev = run(&mut a, 1.5, 10.0, picture_in(bigger));
        assert!(matches!(ev[..], [AutoEvent::Followed(Follow::Grew, _)]), "{ev:?}");
        assert_eq!(a.rect(), bigger.picture((W, H)));
        // A move to the left (same size, not inside the current area): followed too.
        let moved = bars(0, 10, 40, 10);
        let ev = run(&mut a, 11.5, 10.0, picture_in(moved));
        assert!(matches!(ev[..], [AutoEvent::Followed(Follow::Grew, _)]), "{ev:?}");
        assert_eq!(a.rect(), moved.picture((W, H)));
    }

    #[test]
    fn auto_dark_scenes_and_fades_never_shrink_the_crop() {
        let b = bars(40, 20, 40, 20);
        let mut a = settled(b);
        // A fade to black for a minute: no verdict, nothing changes.
        let ev = run(&mut a, 1.5, 60.0, |_| vec![0u8; (W * H * 4) as usize]);
        assert!(ev.is_empty(), "{ev:?}");
        // A dark scene (only the middle lit) for less than the shrink window: nothing changes.
        let dark = move |_: f64| {
            framed(W, H, b, |x, y| {
                if (60..180).contains(&x) && (60..140).contains(&y) {
                    [150, 150, 150]
                } else {
                    [0, 0, 0]
                }
            })
        };
        let ev = run(&mut a, 61.5, SHRINK_S - 6.0, dark);
        assert!(ev.is_empty(), "{ev:?}");
        // A long dark scene lit by one bright moment every 20 s: never shrinks.
        let flashes = move |t: f64| {
            if t.rem_euclid(20.0) < 0.5 {
                framed(W, H, b, busy)
            } else {
                dark(t)
            }
        };
        let ev = run(&mut a, 100.0, 90.0, flashes);
        assert!(ev.is_empty(), "{ev:?}");
        assert_eq!(a.rect(), b.picture((W, H)));
    }

    #[test]
    fn auto_follows_a_real_shrink_after_the_dark_bars_rule() {
        let b = bars(40, 20, 40, 20);
        let mut a = settled(b);
        let smaller = bars(80, 40, 80, 40);
        let ev = run(&mut a, 1.5, SHRINK_S - 3.0, picture_in(smaller));
        assert!(ev.is_empty(), "not before the shrink window: {ev:?}");
        let ev = run(&mut a, SHRINK_S - 1.5, 8.0, picture_in(smaller));
        assert!(matches!(ev[..], [AutoEvent::Followed(Follow::Shrank, _)]), "{ev:?}");
        assert_eq!(a.rect(), smaller.picture((W, H)));
    }

    #[test]
    fn auto_sampling_is_rate_limited() {
        let b = bars(40, 20, 40, 20);
        let mut a = settled(b);
        a.take_cost();
        run(&mut a, 1.5, 10.0, picture_in(b));
        let (samples, secs) = a.take_cost();
        assert!(samples <= (10.0 / SAMPLE_MIN_S) as u32 + 1, "{samples}");
        assert!(samples >= 10, "{samples}");
        assert!(secs >= 0.0);
        // No frames: no evaluation, nothing changes.
        assert_eq!(a.tick(100.0, (1, 1)), None);
        assert_eq!(a.tick(110.0, (1, 1)), None);
        assert_eq!(a.rect(), b.picture((W, H)));
    }

    // ---- Manual limits ----

    #[test]
    fn manual_limits_clamp_and_never_cross() {
        let client = (914, 754);
        let l = bars(329, 265, 329, 265);
        // A plain edit.
        assert_eq!(edit_limit(l, Side::Left, 300, client, (1, 1)).left, 300);
        // Negative and past the opposite limit: clamped, the picture stays 8 px.
        assert_eq!(edit_limit(l, Side::Left, -5, client, (1, 1)).left, 0);
        let e = edit_limit(l, Side::Left, 2000, client, (1, 1));
        assert_eq!(e.left, 914 - 329 - MIN_MANUAL_PX);
        assert_eq!(e.picture(client).unwrap().w, MIN_MANUAL_PX);
        let e = edit_limit(l, Side::Bottom, 10_000, client, (1, 1));
        assert_eq!(e.picture(client).unwrap().h, MIN_MANUAL_PX);
        // An opposite limit already past the frame is clamped too.
        let odd = bars(0, 0, 5000, 0);
        let e = edit_limit(odd, Side::Left, 10, client, (1, 1));
        assert!(e.picture(client).is_some(), "{e:?}");
        // Clamp of all four limits to a smaller client area.
        let c = clamp_limits(bars(600, 500, 600, 500), (640, 480));
        assert!(c
            .picture((640, 480))
            .is_some_and(|p| p.w >= MIN_MANUAL_PX && p.h >= MIN_MANUAL_PX));
        // The rectangle round trip.
        let r = rect(329, 265, 256, 224);
        assert_eq!(Bars::around(r, client), l);
        assert_eq!(l.picture(client), Some(r));
    }

    #[test]
    fn manual_limits_snap_to_source_pixels() {
        let client = (914, 754);
        // x3: the picture 768 x 672 at 73, 41 (bars 73, 41, 73, 41).
        let l = bars(73, 41, 73, 41);
        // Nudges of one source pixel keep the extent a multiple of 3.
        let e = edit_limit(l, Side::Left, 76, client, (3, 3));
        assert_eq!((e.left, e.picture(client).unwrap().w % 3), (76, 0));
        // A drag to an off-grid value goes to the nearest whole source pixel.
        let e = edit_limit(l, Side::Right, 80, client, (3, 3));
        assert_eq!(e.right, 79);
        assert_eq!(e.picture(client).unwrap().w, 762);
        let e = edit_limit(l, Side::Top, 43, client, (3, 3));
        assert_eq!(e.top, 44);
        // No snapping without a multiplier.
        assert_eq!(edit_limit(l, Side::Right, 80, client, (1, 1)).right, 80);
        // Snapping never crosses: the smallest picture is rounded up to whole source pixels.
        let e = edit_limit(l, Side::Left, 5000, client, (3, 3));
        let w = e.picture(client).unwrap().w;
        assert!(w >= MIN_MANUAL_PX && w.is_multiple_of(3), "{w}");
    }

    #[test]
    fn area_mode_apply_reset_and_undo() {
        let mut p = Params::default();
        let mut profile = SourceProfile::default();
        assert_eq!(AreaMode::of(&p, &profile), AreaMode::Auto, "Auto is the default");
        let mut cli = Some(rect(1, 1, 10, 10));
        let before = CropState::capture(&p, &profile, cli);
        let r = rect(8, 4, 320, 240);
        set_manual_crop(&mut p, &mut profile, &mut cli, r, (336, 248));
        assert_eq!(AreaMode::of(&p, &profile), AreaMode::Manual);
        assert_eq!(p.source_crop_mode, SOURCE_CROP_MODE_MANUAL);
        assert_eq!(profile.crop, Some([8, 4, 328, 244]));
        assert_eq!(profile.crop_client, Some([336, 248]));
        assert_eq!(profile.crop_rect(), Some(r));
        assert_eq!(cli, None);
        // The profile alone reproduces it (as at the next attach).
        let mut fresh = Params::default();
        assert!(profile.apply_to(&mut fresh).is_empty());
        assert_eq!(AreaMode::of(&fresh, &profile), AreaMode::Manual);
        // Reset: the whole client area, no rectangle.
        let manual = CropState::capture(&p, &profile, cli);
        reset_crop(&mut p, &mut profile, &mut cli);
        assert_eq!((p.source_crop_mode, profile.crop), (SOURCE_CROP_MODE_AUTO, None));
        assert_eq!(AreaMode::of(&p, &profile), AreaMode::Client);
        // Back to Auto.
        set_area_mode(&mut p, &mut profile, &mut cli, AreaMode::Auto);
        assert_eq!(AreaMode::of(&p, &profile), AreaMode::Auto);
        let mut fresh = Params::default();
        assert!(profile.apply_to(&mut fresh).is_empty());
        assert_eq!(AreaMode::of(&fresh, &profile), AreaMode::Auto);
        // Undo to the manual limits, then to the start.
        manual.restore(&mut p, &mut profile, &mut cli);
        assert_eq!(profile.crop_rect(), Some(r));
        assert_eq!(AreaMode::of(&p, &profile), AreaMode::Manual);
        before.restore(&mut p, &mut profile, &mut cli);
        assert_eq!(CropState::capture(&p, &profile, cli), before);
    }
}
