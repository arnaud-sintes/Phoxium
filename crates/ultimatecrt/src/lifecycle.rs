//! Target lifecycle (T-56, CAP-5, CAP-7, SRC-2, SRC-8): the decisions around the captured window,
//! kept free of OS calls so they are unit tested on synthetic data.
//!
//! - [`Reattacher`]: when the target closes (or its capture session is lost) and a window with the
//!   same executable and class appears, say which window to capture next. The key is the one of the
//!   source profiles (T-51): executable file name and window class, case insensitive. Only windows
//!   that appear after the loss are candidates (the handles that existed then are ignored), so a
//!   sibling window of a shared host (browser, UWP) is not taken for the target.
//! - [`LostWatch`]: while the source is lost, console reminders, and the drop of a fullscreen output
//!   to a window after a timeout (or an exit) so the only monitor is not held by a frozen frame.
//! - [`Health`] and [`is_black`]: a protected window, an excluded window or one that does not draw
//!   captures as black frames or as no frames at all (CAP-5): a message instead of a silent black.
//! - [`SmoothingWatch`]: warns when no integer scale was found and the picture looks like a
//!   low-resolution source upscaled with filtering (SRC-2), advisory only.
//! - [`SizeWatch`]: flags a change of the source size (SRC-8). Resolution-dependent resources are
//!   rebuilt by the renderer on its own; the history clear is a flag for the persistence stage (T-41).
//!
//! Times are seconds on any monotonic clock, passed in, so the logic is deterministic.

use std::collections::HashSet;

use crate::{scaling::Rect, sources::WindowEntry};

// ---------------------------------------------------------------------------------------------
// Reattach
// ---------------------------------------------------------------------------------------------

/// Seconds between two scans of the window list while the target is lost. The scan enumerates every
/// window and queries each owning process, so it is not done every frame; 0.25 s keeps a relaunch
/// well inside the 2 s of the acceptance test.
pub const REATTACH_POLL_S: f64 = 0.25;
/// Seconds before a failed capture start is tried again (the new window may not be ready yet).
pub const REATTACH_RETRY_S: f64 = 0.5;
/// Windows smaller than this are not taken for the target (splash screens, tool tips).
const MIN_TARGET_SIZE: i32 = 32;

/// What identifies the target across relaunches: executable and class (the source-profile key of
/// `Settings::find_source`), plus the title as a tie-breaker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetKey {
    pub exe: Option<String>,
    pub class: String,
    pub title: String,
}

impl TargetKey {
    pub fn of(entry: &WindowEntry) -> Self {
        TargetKey {
            exe: entry.exe.clone(),
            class: entry.class.clone(),
            title: entry.title.clone(),
        }
    }

    /// The `(exe, class)` pair for `Settings::find_source`, `None` when the executable is unknown.
    pub fn profile_key(&self) -> Option<(&str, &str)> {
        self.exe.as_deref().map(|e| (e, self.class.as_str()))
    }

    /// Same executable and class, case insensitive. When the executable of the target could not be
    /// queried (a protected or anti-cheat process) both sides must lack it and the title must be identical, otherwise
    /// any window of that class would match.
    pub fn matches(&self, e: &WindowEntry) -> bool {
        if !self.class.eq_ignore_ascii_case(&e.class) {
            return false;
        }
        match (&self.exe, &e.exe) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            (None, None) => self.title == e.title,
            _ => false,
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} ({})",
            self.exe.as_deref().unwrap_or("unknown executable"),
            self.class
        )
    }
}

/// The window to reattach to among `windows`: a match of the key, preferring the previous handle
/// (the window is still there, only the session was lost), then the same title, then the largest.
///
/// Only the previous handle or a window that was NOT in `known` (the handles that existed when the
/// target was lost) is accepted. Shared hosts break a bare key match: every UWP app is
/// `ApplicationFrameHost.exe` / `ApplicationFrameWindow` and every Chrome window is
/// `chrome.exe` / `Chrome_WidgetWin_1`, so closing a browser game tab or a Game Pass game must not
/// move the capture to a sibling window that was already open. Known limit: a sibling that was
/// hidden or minimised (not in the list) when the target closed and is shown later counts as new.
pub fn choose_reattach<'a>(
    target: &TargetKey,
    last_hwnd: isize,
    known: &HashSet<isize>,
    windows: &'a [WindowEntry],
) -> Option<&'a WindowEntry> {
    windows
        .iter()
        .filter(|w| {
            target.matches(w)
                && w.width >= MIN_TARGET_SIZE
                && w.height >= MIN_TARGET_SIZE
                && (w.hwnd == last_hwnd || !known.contains(&w.hwnd))
        })
        .max_by_key(|w| {
            (
                w.hwnd == last_hwnd,
                w.title == target.title,
                w.width as i64 * w.height as i64,
            )
        })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Attached,
    Lost { next_poll: f64 },
}

/// What the main loop must do after [`Reattacher::update`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do.
    Idle,
    /// The target was just lost: drop the capture session.
    Lost,
    /// Start a capture session on this window, then report with [`Reattacher::attach_result`].
    TryAttach(isize),
}

/// Follows the target window: attached, or lost and waiting for a window with the same key.
pub struct Reattacher {
    target: TargetKey,
    hwnd: isize,
    phase: Phase,
    failures: u32,
    /// Handles of the windows that existed when the target was lost: not candidates (see
    /// [`choose_reattach`]). Empty while attached.
    known: HashSet<isize>,
    /// Handle and title of the window of the last [`Step::TryAttach`], so a successful attach can
    /// refresh the title used as tie-breaker.
    pending: Option<(isize, String)>,
}

impl Reattacher {
    /// `hwnd` is the window currently captured, `target` its key.
    pub fn new(target: TargetKey, hwnd: isize) -> Self {
        Reattacher {
            target,
            hwnd,
            phase: Phase::Attached,
            failures: 0,
            known: HashSet::new(),
            pending: None,
        }
    }

    pub fn target(&self) -> &TargetKey {
        &self.target
    }

    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    pub fn is_attached(&self) -> bool {
        self.phase == Phase::Attached
    }

    /// Consecutive failed capture starts since the last success.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Called every loop iteration. `capture_closed` is the closed flag of the session. `alive`
    /// tells whether a handle still designates a window (the closed event can be missed), `list`
    /// returns the current window list; both are only called when needed (`list` once when the
    /// target is lost, to record the windows that already exist, then at every poll).
    pub fn update(
        &mut self,
        now: f64,
        capture_closed: bool,
        alive: impl FnOnce(isize) -> bool,
        list: impl FnOnce() -> Vec<WindowEntry>,
    ) -> Step {
        match self.phase {
            Phase::Attached => {
                if capture_closed || !alive(self.hwnd) {
                    self.known = list().iter().map(|w| w.hwnd).collect();
                    self.phase = Phase::Lost { next_poll: now };
                    Step::Lost
                } else {
                    Step::Idle
                }
            }
            Phase::Lost { next_poll } => {
                if now < next_poll {
                    return Step::Idle;
                }
                self.phase = Phase::Lost {
                    next_poll: now + REATTACH_POLL_S,
                };
                let windows = list();
                match choose_reattach(&self.target, self.hwnd, &self.known, &windows) {
                    Some(w) => {
                        self.pending = Some((w.hwnd, w.title.clone()));
                        Step::TryAttach(w.hwnd)
                    }
                    None => Step::Idle,
                }
            }
        }
    }

    /// Reports the outcome of a [`Step::TryAttach`]. On failure the next attempt waits
    /// [`REATTACH_RETRY_S`]. On success the title of the new window becomes the tie-breaker of the
    /// next loss.
    pub fn attach_result(&mut self, now: f64, hwnd: isize, ok: bool) {
        if ok {
            if let Some((_, title)) = self.pending.take().filter(|(h, _)| *h == hwnd) {
                self.target.title = title;
            }
            self.hwnd = hwnd;
            self.phase = Phase::Attached;
            self.failures = 0;
            self.known.clear();
        } else {
            self.failures += 1;
            self.phase = Phase::Lost {
                next_poll: now + REATTACH_RETRY_S,
            };
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Lost source: reminders, timeout, exit
// ---------------------------------------------------------------------------------------------

/// Seconds between two console reminders while the source is lost.
pub const LOST_REMIND_S: f64 = 10.0;
/// Default of `--wait-source`: seconds a fullscreen output waits for the source before it drops to a
/// window.
pub const DEFAULT_WAIT_SOURCE_S: f64 = 30.0;
/// The console reminder.
pub const LOST_REMINDER: &str = "source lost; the Quit hotkey (Ctrl+Alt+End by default) quits";

/// What to do when the source is lost (command line: `--wait-source`, `--exit-on-lost`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LostPolicy {
    /// Seconds a fullscreen output waits before dropping to a window; 0 waits forever.
    pub wait_s: f64,
    /// Exit when the target closes instead of waiting for it.
    pub exit_on_lost: bool,
}

impl Default for LostPolicy {
    fn default() -> Self {
        LostPolicy {
            wait_s: DEFAULT_WAIT_SOURCE_S,
            exit_on_lost: false,
        }
    }
}

/// What the main loop must do after [`LostWatch::update`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LostAction {
    None,
    /// Print [`LOST_REMINDER`].
    Remind,
    /// The wait is over and the output is fullscreen: give the source up (the app shows the window picker,
    /// A23) and replace the fullscreen output by a normal window (once per lost period).
    GoWindowed,
    /// The wait is over and the output is a window: give the source up (the app shows the window picker, A23),
    /// once per lost period.
    GiveUp,
    /// Leave the main loop.
    Exit,
}

/// Decides what happens while the source stays lost. Without it a fullscreen, topmost,
/// non-activating, taskbar-less output over the only monitor stays up with a frozen frame and only
/// The Quit hotkey closes it. Reattaching is not decided here ([`Reattacher`] keeps going in every mode).
#[derive(Debug)]
pub struct LostWatch {
    policy: LostPolicy,
    fullscreen: bool,
    lost_since: Option<f64>,
    next_remind: f64,
    /// The wait of this lost period is over (`GoWindowed` or `GiveUp` returned).
    gave_up: bool,
}

impl LostWatch {
    /// `fullscreen` is true when the output currently is a fullscreen one.
    pub fn new(policy: LostPolicy, fullscreen: bool) -> Self {
        LostWatch {
            policy,
            fullscreen,
            lost_since: None,
            next_remind: 0.0,
            gave_up: false,
        }
    }

    /// The policy in force.
    pub fn policy(&self) -> LostPolicy {
        self.policy
    }

    /// Changes the wait (the menu's setting, A23); applies to the current lost period too.
    pub fn set_wait(&mut self, wait_s: f64) {
        self.policy.wait_s = wait_s.max(0.0);
    }

    /// True while the output is still the fullscreen one.
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// The user switched the output between fullscreen and a window (F11, T-57): only a fullscreen output
    /// drops to a window when the source stays lost.
    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        self.fullscreen = fullscreen;
    }

    /// Called every loop iteration with `attached` from [`Reattacher::is_attached`]. The first call
    /// with `attached == false` starts the lost period: reminder at once, then every
    /// [`LOST_REMIND_S`]; after `wait_s` the source is given up once: [`LostAction::GoWindowed`] for a
    /// fullscreen output (it also drops to a window), [`LostAction::GiveUp`] for a window. Becoming
    /// attached again ends the period.
    pub fn update(&mut self, now: f64, attached: bool) -> LostAction {
        if attached {
            self.lost_since = None;
            self.gave_up = false;
            return LostAction::None;
        }
        let since = match self.lost_since {
            Some(t) => t,
            None => {
                self.lost_since = Some(now);
                self.next_remind = now;
                if self.policy.exit_on_lost {
                    return LostAction::Exit;
                }
                now
            }
        };
        if !self.gave_up && self.policy.wait_s > 0.0 && now - since >= self.policy.wait_s {
            self.gave_up = true;
            self.next_remind = now + LOST_REMIND_S;
            if self.fullscreen {
                self.fullscreen = false;
                return LostAction::GoWindowed;
            }
            return LostAction::GiveUp;
        }
        if now >= self.next_remind {
            self.next_remind = now + LOST_REMIND_S;
            return LostAction::Remind;
        }
        LostAction::None
    }
}

// ---------------------------------------------------------------------------------------------
// Black or stale frames
// ---------------------------------------------------------------------------------------------

/// The message of CAP-5.
pub const PROTECTED_OR_NOT_DRAWING: &str = "protected or not drawing";
/// Seconds of black frames before the message shows (a fade or a black boot screen is shorter).
pub const BLACK_AFTER_S: f64 = 3.0;
/// Seconds without any new frame before the message shows. The capture API only delivers a frame
/// when the window content changes, so a static picture is silent too: the message says so.
pub const STALE_AFTER_S: f64 = 10.0;
/// Minimum seconds between two black scans of a frame.
const SAMPLE_EVERY_S: f64 = 0.1;
/// A channel value up to this counts as black (capture noise, near-black dithering).
const BLACK_LEVEL: u8 = 2;

/// True when every pixel of a BGRA8 frame is black (alpha ignored). An empty frame carries no
/// information and is not black. Stops at the first lit pixel, so a normal frame is cheap.
pub fn is_black(data: &[u8]) -> bool {
    !data.is_empty()
        && data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[0] <= BLACK_LEVEL && p[1] <= BLACK_LEVEL && p[2] <= BLACK_LEVEL)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthStatus {
    Ok,
    /// Frames keep arriving but they are black: protected, excluded from capture, or not drawing.
    Black,
    /// No new frame for a long time: static picture, or the window does not draw.
    Stale,
}

impl HealthStatus {
    /// Text for the user, `None` when all is well. Both cases carry the CAP-5 message.
    pub fn message(self) -> Option<String> {
        match self {
            HealthStatus::Ok => None,
            HealthStatus::Black => Some(format!("{PROTECTED_OR_NOT_DRAWING}: the captured frames are black")),
            HealthStatus::Stale => Some(format!(
                "{PROTECTED_OR_NOT_DRAWING}: no new frame for {STALE_AFTER_S:.0} s (or the picture is static)"
            )),
        }
    }
}

/// Watches the frames of one capture session.
#[derive(Debug)]
pub struct Health {
    started: f64,
    last_frame: Option<f64>,
    black_since: Option<f64>,
    last_sample: f64,
}

impl Health {
    pub fn new(now: f64) -> Self {
        Health {
            started: now,
            last_frame: None,
            black_since: None,
            last_sample: f64::NEG_INFINITY,
        }
    }

    /// Forget everything: a new capture session starts.
    pub fn reset(&mut self, now: f64) {
        *self = Health::new(now);
    }

    /// Whether the black scan of the next new frame is due (it is rate limited).
    pub fn wants_sample(&self, now: f64) -> bool {
        now - self.last_sample >= SAMPLE_EVERY_S
    }

    /// A new frame arrived. `black` is the result of [`is_black`] when the frame was scanned, `None`
    /// when it was not (the black state is then unchanged).
    pub fn note_frame(&mut self, now: f64, black: Option<bool>) {
        self.last_frame = Some(now);
        match black {
            Some(true) => {
                self.last_sample = now;
                self.black_since.get_or_insert(now);
            }
            Some(false) => {
                self.last_sample = now;
                self.black_since = None;
            }
            None => {}
        }
    }

    pub fn status(&self, now: f64) -> HealthStatus {
        if self.black_since.is_some_and(|t| now - t >= BLACK_AFTER_S) {
            return HealthStatus::Black;
        }
        let silent_since = self.last_frame.unwrap_or(self.started);
        if now - silent_since >= STALE_AFTER_S {
            return HealthStatus::Stale;
        }
        HealthStatus::Ok
    }
}

// ---------------------------------------------------------------------------------------------
// Smoothed source
// ---------------------------------------------------------------------------------------------
//
// SRC-2 verdict: "the window shows a low-resolution source that was upscaled with filtering".
// Advisory only (a message, nothing is changed). Two pieces of evidence are required:
// - most edges between flat areas are filtered ones (a few in-between pixels that change
//   monotonically, overshoot of up to a quarter of the step tolerated for bicubic and Lanczos);
// - the edges sit on a regular grid: an upscale by a factor s puts every edge at a
//   multiple of s (integer or not), whereas the edges of text or of a drawing at 1:1 are at
//   arbitrary positions. The grid is searched as a period in [PERIOD_MIN, PERIOD_MAX] px, by the
//   phase concentration of the edge positions inside short windows (so a long image does not need a
//   fine period resolution).
// Known cases it cannot distinguish (documented in README.md): monospaced text with heavy strokes
// at 1:1 (its stems repeat at the character pitch), and an upscale whose flat areas are all shorter
// than the filter support (for example bicubic x2 of art without runs of four native pixels):
// no flat area survives, so there is no edge to count.

/// Pixels ignored on every side (rounded window corners), as in the scale detector.
const MARGIN: u32 = 16;
/// One line in this many is analysed, in both directions.
const LINE_STRIDE: u32 = 4;
/// Longest run of in-between pixels taken for a filtered edge (an upscale of up to x9).
const MAX_RAMP: usize = 8;
/// Edges whose flat sides differ by less than this (largest channel) are noise, not edges.
const MIN_EDGE_STEP: u8 = 28;
/// Pixels whose channels all differ by at most this from the first of a run belong to the run
/// (the ringing of bicubic and Lanczos filters and capture noise leave flat areas a few levels off).
const FLAT_TOLERANCE: u8 = 8;
/// Overshoot allowed beyond the two flat levels in a filtered edge, as a fraction of the step
/// (at least `MIN_OVERSHOOT` levels).
const OVERSHOOT_FRACTION: f32 = 0.25;
const MIN_OVERSHOOT: f32 = 6.0;
/// Edges needed before the verdict means anything.
const MIN_EDGES: u64 = 64;
/// Fraction of filtered edges above which the source looks smoothed.
const SMOOTHED_FRACTION: f64 = 0.3;
/// Frames between two analyses, and the analyses taken before giving up.
const SMOOTH_FRAME_GAP: u32 = 15;
const SMOOTH_MAX_CHECKS: u32 = 8;
/// Edge-grid search: periods in px, search step, window length in px, positions a window needs, windows
/// needed, and the excess phase concentration above which the edges are on a grid (random positions give
/// about 0.05, text at 1:1 0.04, a true upscale 0.25 to 1 depending on the filter).
const PERIOD_MIN: f64 = 1.5;
const PERIOD_MAX: f64 = 16.0;
const PERIOD_STEP: f64 = 0.01;
const PERIOD_WINDOW: usize = 96;
const WINDOW_MIN_POSITIONS: usize = 4;
const MIN_WINDOWS: usize = 4;
const GRID_STRENGTH: f64 = 0.15;

/// Edges between two flat areas (runs of at least two near-identical pixels), counted by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EdgeCounts {
    /// The flat areas touch: a nearest-neighbour edge.
    pub hard: u64,
    /// One or more in-between pixels that change monotonically: a filtered (bilinear, bicubic) edge.
    pub soft: u64,
}

impl EdgeCounts {
    pub fn total(&self) -> u64 {
        self.hard + self.soft
    }

    /// Mostly filtered edges, enough of them to judge.
    pub fn looks_smoothed(&self) -> bool {
        self.total() >= MIN_EDGES && self.soft as f64 >= SMOOTHED_FRACTION * self.total() as f64
    }

    fn add(&mut self, other: EdgeCounts) {
        self.hard += other.hard;
        self.soft += other.soft;
    }
}

/// Edge counts and the positions of the edges: `cols[i]` is set when an edge of a
/// horizontal scan lies at x = i / 2 (px from the left of the region), `rows` the same
/// for y.
#[derive(Clone, Debug, Default)]
pub struct EdgeStats {
    pub counts: EdgeCounts,
    cols: Vec<bool>,
    rows: Vec<bool>,
}

impl EdgeStats {
    fn add(&mut self, other: &EdgeStats) {
        self.counts.add(other.counts);
        for (mine, theirs) in [(&mut self.cols, &other.cols), (&mut self.rows, &other.rows)] {
            if mine.len() < theirs.len() {
                mine.resize(theirs.len(), false);
            }
            for (m, t) in mine.iter_mut().zip(theirs) {
                *m |= *t;
            }
        }
    }

    /// Best phase concentration of the filtered edges on either axis (0 when there is no evidence).
    pub fn grid_strength(&self) -> f64 {
        grid_strength(&self.cols).max(grid_strength(&self.rows))
    }

    /// The verdict: mostly filtered edges, lying on a regular grid.
    pub fn looks_like_filtered_upscale(&self) -> bool {
        self.counts.looks_smoothed() && self.grid_strength() >= GRID_STRENGTH
    }
}

/// Best excess phase concentration over the periods of the grid search, 0 without enough evidence.
/// `present[i]` marks a position of i / 2 px. Per window of N positions with phase sum Z the excess
/// is `|Z|^2 - N` (zero on average for random positions, N^2 - N when all are in phase); the score
/// is the sum over windows divided by the sum of the maxima, so it is 0 for no grid and 1 for a
/// perfect one, and windows with few positions weigh little.
fn grid_strength(present: &[bool]) -> f64 {
    // Positions per window, relative to the window start.
    let mut windows: Vec<Vec<f64>> = Vec::new();
    for chunk in present.chunks(PERIOD_WINDOW * 2) {
        let pos: Vec<f64> = chunk
            .iter()
            .enumerate()
            .filter(|(_, p)| **p)
            .map(|(i, _)| i as f64 / 2.0)
            .collect();
        if pos.len() >= WINDOW_MIN_POSITIONS {
            windows.push(pos);
        }
    }
    if windows.len() < MIN_WINDOWS {
        return 0.0;
    }
    let max_excess: f64 = windows.iter().map(|p| (p.len() * (p.len() - 1)) as f64).sum();
    let mut best = 0.0f64;
    let mut s = PERIOD_MIN;
    while s <= PERIOD_MAX {
        let k = std::f64::consts::TAU / s;
        let excess: f64 = windows
            .iter()
            .map(|pos| {
                let (mut c, mut sn) = (0.0, 0.0);
                for p in pos {
                    let (a, b) = (k * p).sin_cos();
                    sn += a;
                    c += b;
                }
                c * c + sn * sn - pos.len() as f64
            })
            .sum();
        best = best.max(excess / max_excess);
        s += PERIOD_STEP;
    }
    best
}
fn same_run(a: &[u8; 4], b: &[u8; 4]) -> bool {
    max_diff(a, b) <= FLAT_TOLERANCE
}

fn max_diff(a: &[u8; 4], b: &[u8; 4]) -> u8 {
    (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0)
}

/// True when each of B, G, R goes monotonically (up or down) from `from` through `ramp` to `to`,
/// after clamping a small overshoot beyond the two levels (bicubic and Lanczos ringing).
fn monotone(from: &[u8; 4], ramp: &[[u8; 4]], to: &[u8; 4]) -> bool {
    (0..3).all(|c| {
        let (a, b) = (from[c] as f32, to[c] as f32);
        let (lo, hi) = (a.min(b), a.max(b));
        let tol = ((hi - lo) * OVERSHOOT_FRACTION).max(MIN_OVERSHOOT);
        let mut up = true;
        let mut down = true;
        let mut prev = a;
        for v in ramp.iter().map(|p| p[c] as f32).chain(std::iter::once(b)) {
            if v < lo - tol || v > hi + tol {
                return false;
            }
            let v = v.clamp(lo, hi);
            up &= v >= prev;
            down &= v <= prev;
            prev = v;
        }
        up || down
    })
}

/// Position of an edge in half pixels from the start of the line: where the channel with the
/// largest step crosses the middle of the two flat levels, interpolated between pixel centres. The
/// right flat run starts at pixel `i`, the ramp is the `ramp.len()` pixels before it. Taking the
/// crossing instead of the middle of the ramp keeps the position stable when a bicubic or Lanczos
/// filter rings (the ramp then also holds the overshoot pixels).
fn edge_centre(from: &[u8; 4], ramp: &[[u8; 4]], to: &[u8; 4], i: usize) -> usize {
    let c = (0..3).max_by_key(|&c| from[c].abs_diff(to[c])).unwrap_or(0);
    let seq: Vec<f64> = std::iter::once(from[c])
        .chain(ramp.iter().map(|p| p[c]))
        .chain(std::iter::once(to[c]))
        .map(f64::from)
        .collect();
    let mid = (seq[0] + seq[seq.len() - 1]) / 2.0;
    // Pixel index of seq[0] is i - ramp.len() - 1.
    let first = (i - ramp.len() - 1) as f64;
    let mut p = first + (seq.len() - 1) as f64 / 2.0;
    for k in 0..seq.len() - 1 {
        let (a, b) = (seq[k] - mid, seq[k + 1] - mid);
        if a * b <= 0.0 && a != b {
            p = first + k as f64 + a / (a - b);
            break;
        }
    }
    // The pixel centre is at +0.5, in half pixels that is 2p + 1.
    (2.0 * p + 1.0).round().max(0.0) as usize
}
/// Counts the edges of one line of pixels. Pixels are grouped in runs of near-identical values; a
/// run of two or more pixels is flat. Between two consecutive flat runs there are either no pixels
/// (hard edge) or a few single pixels. If those go monotonically from one side to the other it is a
/// filtered edge, whose position (in half pixels from the start of the line) is
/// pushed on `centres`. Anything else (texture, a thin line, a gradient) says nothing and is skipped.
fn scan_line(line: &[[u8; 4]], counts: &mut EdgeCounts, centres: &mut Vec<usize>) {
    let mut prev_flat: Option<[u8; 4]> = None;
    let mut ramp: Vec<[u8; 4]> = Vec::new();
    let mut ramp_overflow = false;
    let mut i = 0;
    while i < line.len() {
        let px = line[i];
        let mut j = i + 1;
        while j < line.len() && same_run(&line[j], &px) {
            j += 1;
        }
        if j - i >= 2 {
            if let Some(from) = prev_flat {
                if max_diff(&from, &px) >= MIN_EDGE_STEP && !ramp_overflow {
                    if ramp.is_empty() {
                        counts.hard += 1;
                        centres.push(edge_centre(&from, &ramp, &px, i));
                    } else if monotone(&from, &ramp, &px) {
                        counts.soft += 1;
                        centres.push(edge_centre(&from, &ramp, &px, i));
                    }
                }
            }
            prev_flat = Some(px);
            ramp.clear();
            ramp_overflow = false;
        } else if prev_flat.is_some() {
            if ramp.len() < MAX_RAMP {
                ramp.push(px);
            } else {
                ramp_overflow = true;
            }
        }
        i = j;
    }
}

/// Counts the edges of `region` of a BGRA8 frame `frame_w` pixels wide, on one line in
/// [`LINE_STRIDE`], horizontally and vertically, and records where the filtered ones are.
pub fn edge_stats(data: &[u8], frame_w: u32, region: Rect) -> EdgeStats {
    let mut stats = EdgeStats::default();
    let m = MARGIN.min(region.w / 8).min(region.h / 8);
    if region.w <= 2 * m + 2 || region.h <= 2 * m + 2 {
        return stats;
    }
    stats.cols = vec![false; 2 * region.w as usize + 2];
    stats.rows = vec![false; 2 * region.h as usize + 2];
    let (x0, x1) = (region.x + m, region.x + region.w - m);
    let (y0, y1) = (region.y + m, region.y + region.h - m);
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * frame_w + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    };
    let mut line: Vec<[u8; 4]> = Vec::new();
    let mut centres: Vec<usize> = Vec::new();
    for y in (y0..y1).step_by(LINE_STRIDE as usize) {
        line.clear();
        centres.clear();
        line.extend((x0..x1).map(|x| at(x, y)));
        scan_line(&line, &mut stats.counts, &mut centres);
        for c in &centres {
            stats.cols[c + 2 * (x0 - region.x) as usize] = true;
        }
    }
    for x in (x0..x1).step_by(LINE_STRIDE as usize) {
        line.clear();
        centres.clear();
        line.extend((y0..y1).map(|y| at(x, y)));
        scan_line(&line, &mut stats.counts, &mut centres);
        for c in &centres {
            stats.rows[c + 2 * (y0 - region.y) as usize] = true;
        }
    }
    stats
}

/// The edge counts of [`edge_stats`].
pub fn edge_counts(data: &[u8], frame_w: u32, region: Rect) -> EdgeCounts {
    edge_stats(data, frame_w, region).counts
}

/// Accumulates [`EdgeStats`] over a few frames while no integer scale is in use, and raises the
/// SRC-2 warning when the picture looks like a filtered upscale of a low-resolution source.
#[derive(Debug, Default)]
pub struct SmoothingWatch {
    frames: u32,
    checks: u32,
    stats: EdgeStats,
    warned: bool,
}

impl SmoothingWatch {
    pub fn reset(&mut self) {
        *self = SmoothingWatch::default();
    }

    /// True while the warning is active.
    pub fn warning(&self) -> bool {
        self.warned
    }

    /// Feeds one new frame. `eligible` is true when the divisor was detected (not given by the
    /// user) and the locked detection found no integer scale; otherwise the watch is reset. That
    /// alone also holds for any text-heavy window at 1:1, so the verdict itself requires filtered
    /// edges on a regular grid ([`EdgeStats::looks_like_filtered_upscale`]).
    /// Returns true when the warning has just been raised.
    pub fn feed(&mut self, data: &[u8], frame_w: u32, region: Rect, eligible: bool) -> bool {
        if !eligible {
            self.reset();
            return false;
        }
        if self.warned || self.checks >= SMOOTH_MAX_CHECKS {
            return false;
        }
        let due = self.frames == 0;
        self.frames = (self.frames + 1) % SMOOTH_FRAME_GAP;
        if !due {
            return false;
        }
        self.checks += 1;
        self.stats.add(&edge_stats(data, frame_w, region));
        self.warned = self.stats.looks_like_filtered_upscale();
        self.warned
    }
}

/// The SRC-2 warning text.
pub const SMOOTHED_WARNING: &str = "no integer scale found and the picture looks like a low-resolution source \
    scaled with filtering, which cannot be undone (use the game at its native size or an integer scale)";

// ---------------------------------------------------------------------------------------------
// Source size change
// ---------------------------------------------------------------------------------------------

/// Notices a change of the source size (a mode switch, a window resize, a relaunch with another
/// size). The renderer rebuilds its resolution-dependent resources by itself when it is given a
/// frame of another size; what remains is the history, which the main loop clears with
/// `Renderer::clear_history` when [`SizeWatch::take_history_clear`] says so. That call reaches
/// every stage that implements `Stage::clear_history` (today the phosphor persistence stage resets its
/// afterglow); a stage that keeps no history, or does not implement it, is left as it is.
#[derive(Debug, Default)]
pub struct SizeWatch {
    last: Option<(u32, u32)>,
    generation: u64,
    history_clear: bool,
}

impl SizeWatch {
    /// Records the size of a new frame. True when it differs from the previous one (the first
    /// frame is not a change: there is no history yet).
    pub fn observe(&mut self, w: u32, h: u32) -> bool {
        let changed = self.last.is_some_and(|l| l != (w, h));
        self.last = Some((w, h));
        if changed {
            self.generation += 1;
            self.history_clear = true;
        }
        changed
    }

    /// Asks for a history clear without a size change (a new capture session).
    pub fn request_history_clear(&mut self) {
        self.history_clear = true;
    }

    /// True once per request: the history must be cleared now.
    pub fn take_history_clear(&mut self) -> bool {
        std::mem::take(&mut self.history_clear)
    }

    /// Counts the size changes, for resources keyed on the source size.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

// ---------------------------------------------------------------------------------------------
// Minimised or hidden source
// ---------------------------------------------------------------------------------------------

/// Whether the attached window can deliver frames: Windows.Graphics.Capture sends none for a minimised window,
/// nor for a window cloaked by the compositor (on another virtual desktop). The capture session stays open and
/// the frames come back when the window is restored or brought to the current desktop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceVisibility {
    #[default]
    Visible,
    /// `IsIconic`.
    Minimised,
    /// `DWMWA_CLOAKED`: on another virtual desktop (or hidden by the shell).
    Cloaked,
}

impl SourceVisibility {
    /// From the two window queries; minimised wins (a minimised window on another desktop is restored first).
    pub fn of(minimised: bool, cloaked: bool) -> Self {
        if minimised {
            SourceVisibility::Minimised
        } else if cloaked {
            SourceVisibility::Cloaked
        } else {
            SourceVisibility::Visible
        }
    }

    /// The notice for the title bar and the menu, `None` while the window is visible.
    pub fn notice(self) -> Option<&'static str> {
        match self {
            SourceVisibility::Visible => None,
            SourceVisibility::Minimised => Some("source minimised: no frames (restore it to resume)"),
            SourceVisibility::Cloaked => {
                Some("source on another virtual desktop: no frames (bring it to this desktop to resume)")
            }
        }
    }
}

/// The notice of an attached source: why no frames come (minimised, on another desktop) before what the frames
/// show ([`HealthStatus`]: black, or none for a while, which a minimised window would otherwise report as
/// "protected or not drawing" after [`STALE_AFTER_S`]). `None` while all is well. Pure.
pub fn source_notice(visibility: SourceVisibility, health: HealthStatus) -> Option<String> {
    visibility.notice().map(str::to_string).or_else(|| health.message())
}

// ---------------------------------------------------------------------------------------------
// Capture restart on the renderer's adapter (HW-3)
// ---------------------------------------------------------------------------------------------

/// What [`follow_renderer_adapter`] needs of a capture session (`capture::Capture`; a fake in the tests).
pub trait CaptureSession {
    /// The adapter the session's Direct3D 11 device runs on.
    fn adapter_luid(&self) -> crate::adapters::Luid;
    /// Frames delivered by the session so far.
    fn frames_captured(&self) -> u64;
}

/// The outcome of [`follow_renderer_adapter`].
#[derive(Debug, PartialEq, Eq)]
pub enum Restart {
    /// No session, no wanted adapter, or the session already runs on it: nothing changed.
    Kept,
    /// The session runs on the renderer's adapter now (a new session: its frames count from 1).
    Restarted,
    /// The session could not start on the renderer's adapter and runs again on its first adapter (a new session
    /// too): every frame crosses adapters, the zero-copy path refuses (`zerocopy::Refusal::CrossAdapter`) and the
    /// CPU copy runs. The text says why.
    Fallback(String),
    /// Neither start worked: no session any more (the caller shows the window picker, or stops on the
    /// developer path). The text says why.
    Lost(String),
}

/// The capture session follows the adapter the renderer really got (HW-3): the first session starts on a guess
/// before the output window exists; when the renderer's adapter `wanted` differs, the session is closed first
/// (frames counted into `captured_base`), then started again on `wanted` through `start`, else on its first
/// adapter, else given up. A session started on another adapter makes new shared pools (new generations,
/// `zerocopy::next_id`), so a zero-copy renderer can never keep sampling the old session's textures.
pub fn follow_renderer_adapter<C: CaptureSession, E: std::fmt::Display>(
    capture: &mut Option<C>,
    wanted: Option<crate::adapters::Luid>,
    captured_base: &mut u64,
    mut start: impl FnMut(Option<crate::adapters::Luid>) -> Result<C, E>,
) -> Restart {
    let (Some(w), Some(old)) = (wanted, capture.as_ref()) else {
        return Restart::Kept;
    };
    let first = old.adapter_luid();
    if first == w {
        return Restart::Kept;
    }
    if let Some(old) = capture.take() {
        *captured_base += old.frames_captured();
        drop(old); // the old session closes before the new one starts
    }
    match start(Some(w)) {
        Ok(c) => {
            *capture = Some(c);
            Restart::Restarted
        }
        Err(e) => match start(Some(first)) {
            Ok(c) => {
                *capture = Some(c);
                Restart::Fallback(format!(
                    "the capture could not start on the renderer's adapter ({e}); it runs on its first adapter, \
                     frames cross adapters (CPU copy)"
                ))
            }
            Err(e2) => Restart::Lost(format!(
                "the capture could not start on the renderer's adapter ({e}) nor on its first one ({e2})"
            )),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Monitor and DPI of the output
// ---------------------------------------------------------------------------------------------

/// Follows the monitor the output window is on and its DPI, sampled about once a second: a move onto another
/// monitor (by a drag, F11, a display change) re-evaluates the GPU choice (`adapters::moved_notice`), resets the
/// automatic quality's streaks (`quality::AutoQuality::monitor_changed`), and a DPI change missed by
/// `WM_DPICHANGED` still reaches the UI scale. The persistence's automatic rate measures the real frame period
/// and follows by itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitorWatch {
    last: Option<(crate::modes::IRect, u32)>,
}

/// What changed since the last [`MonitorWatch::observe`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitorChange {
    /// The output is on another monitor (not reported for the first sample).
    pub moved: bool,
    /// The new DPI, when it changed (not reported for the first sample).
    pub dpi: Option<u32>,
}

impl MonitorWatch {
    /// One sample: the monitor rectangle the output is on and its DPI (0 when unknown: ignored).
    pub fn observe(&mut self, monitor: crate::modes::IRect, dpi: u32) -> MonitorChange {
        let dpi = if dpi == 0 { self.last.map_or(0, |(_, d)| d) } else { dpi };
        let change = match self.last {
            None => MonitorChange::default(),
            Some((m, d)) => MonitorChange {
                moved: m != monitor,
                dpi: (d != dpi && dpi != 0).then_some(dpi),
            },
        };
        self.last = Some((monitor, dpi));
        change
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hwnd: isize, title: &str, exe: Option<&str>, class: &str, w: i32, h: i32) -> WindowEntry {
        WindowEntry {
            hwnd,
            title: title.into(),
            exe: exe.map(Into::into),
            class: class.into(),
            width: w,
            height: h,
        }
    }

    fn game(hwnd: isize) -> WindowEntry {
        entry(hwnd, "Mina", Some("Mina.exe"), "UnityWndClass", 1280, 720)
    }

    fn reattacher() -> Reattacher {
        let g = game(10);
        Reattacher::new(TargetKey::of(&g), 10)
    }

    // --- reattach ---------------------------------------------------------------------------

    #[test]
    fn key_matches_executable_and_class_ignoring_case() {
        let key = TargetKey::of(&game(10));
        assert!(key.matches(&entry(11, "Other title", Some("mina.EXE"), "unitywndclass", 640, 480)));
        assert!(!key.matches(&entry(11, "Mina", Some("Other.exe"), "UnityWndClass", 640, 480)));
        assert!(!key.matches(&entry(11, "Mina", Some("Mina.exe"), "Other", 640, 480)));
        assert!(!key.matches(&entry(11, "Mina", None, "UnityWndClass", 640, 480)));
        assert_eq!(key.profile_key(), Some(("Mina.exe", "UnityWndClass")));
    }

    #[test]
    fn unknown_executable_needs_the_same_title() {
        let key = TargetKey::of(&entry(10, "Tool", None, "Cls", 400, 300));
        assert!(key.matches(&entry(11, "Tool", None, "Cls", 400, 300)));
        assert!(!key.matches(&entry(11, "Another", None, "Cls", 400, 300)));
        assert!(!key.matches(&entry(11, "Tool", Some("x.exe"), "Cls", 400, 300)));
        assert_eq!(key.profile_key(), None);
    }

    #[test]
    fn choice_prefers_previous_handle_then_title_then_size() {
        let key = TargetKey::of(&game(10));
        let big = entry(20, "Mina", Some("Mina.exe"), "UnityWndClass", 1920, 1080);
        let small = entry(21, "Mina", Some("Mina.exe"), "UnityWndClass", 640, 480);
        let retitled = entry(22, "Mina - menu", Some("Mina.exe"), "UnityWndClass", 3000, 2000);
        let same_handle = entry(10, "Mina - menu", Some("Mina.exe"), "UnityWndClass", 200, 100);
        let list = [small.clone(), retitled.clone(), big.clone()];
        assert_eq!(choose_reattach(&key, 10, &HashSet::new(), &list).unwrap().hwnd, 20);
        let list = [small.clone(), retitled.clone()];
        assert_eq!(choose_reattach(&key, 10, &HashSet::new(), &list).unwrap().hwnd, 21);
        let list = [small, retitled, big, same_handle];
        assert_eq!(choose_reattach(&key, 10, &HashSet::new(), &list).unwrap().hwnd, 10);
    }

    #[test]
    fn choice_ignores_other_programs_and_tiny_windows() {
        let key = TargetKey::of(&game(10));
        let list = [
            entry(30, "Mina", Some("Mina.exe"), "UnityWndClass", 16, 16),
            entry(31, "Mina", Some("Editor.exe"), "UnityWndClass", 1280, 720),
            entry(32, "Mina", Some("Mina.exe"), "SplashClass", 1280, 720),
        ];
        assert!(choose_reattach(&key, 10, &HashSet::new(), &list).is_none());
    }

    #[test]
    fn attached_target_is_left_alone() {
        let mut r = reattacher();
        let step = r.update(1.0, false, |_| true, || panic!("no scan while attached"));
        assert_eq!(step, Step::Idle);
        assert!(r.is_attached());
    }

    #[test]
    fn closed_capture_or_dead_window_means_lost() {
        let mut r = reattacher();
        assert_eq!(r.update(1.0, true, |_| true, Vec::new), Step::Lost);
        assert!(!r.is_attached());
        let mut r = reattacher();
        assert_eq!(r.update(1.0, false, |h| h != 10, Vec::new), Step::Lost);
    }

    #[test]
    fn reattaches_when_a_matching_window_appears() {
        let mut r = reattacher();
        assert_eq!(r.update(1.0, true, |_| false, Vec::new), Step::Lost);
        // Nothing yet: polls at most every REATTACH_POLL_S.
        assert_eq!(r.update(1.0, true, |_| false, Vec::new), Step::Idle);
        assert_eq!(
            r.update(1.1, true, |_| false, || panic!("too early to scan")),
            Step::Idle
        );
        let relaunched = vec![game(77)];
        assert_eq!(
            r.update(1.0 + REATTACH_POLL_S, true, |_| false, || relaunched),
            Step::TryAttach(77)
        );
        r.attach_result(1.3, 77, true);
        assert!(r.is_attached());
        assert_eq!(r.hwnd(), 77);
        assert_eq!(r.update(1.4, false, |h| h == 77, || panic!("attached")), Step::Idle);
    }

    #[test]
    fn failed_start_is_retried_after_the_backoff() {
        let mut r = reattacher();
        r.update(0.0, true, |_| false, Vec::new);
        let win = vec![game(5)];
        assert_eq!(r.update(0.0, true, |_| false, || win.clone()), Step::TryAttach(5));
        r.attach_result(0.0, 5, false);
        assert_eq!(r.failures(), 1);
        assert_eq!(r.update(0.3, true, |_| false, || panic!("backoff")), Step::Idle);
        assert_eq!(
            r.update(REATTACH_RETRY_S, true, |_| false, || win.clone()),
            Step::TryAttach(5)
        );
        r.attach_result(REATTACH_RETRY_S, 5, true);
        assert_eq!(r.failures(), 0);
    }

    #[test]
    fn relaunch_during_a_frame_loop_reattaches_within_two_seconds() {
        // 60 Hz loop. The game closes at t = 1.0 and a new window shows up at t = 1.7.
        let mut r = reattacher();
        let mut reattached_at = None;
        let mut t = 0.0;
        while t < 4.0 {
            let closed = t >= 1.0;
            let windows = || {
                if t >= 1.7 {
                    vec![game(99)]
                } else {
                    vec![entry(1, "Explorer", Some("explorer.exe"), "CabinetWClass", 800, 600)]
                }
            };
            let attached_now = r.is_attached();
            if let Step::TryAttach(h) = r.update(t, closed && attached_now, |h| h != 10, windows) {
                r.attach_result(t, h, true);
                reattached_at = Some(t);
                break;
            }
            t += 1.0 / 60.0;
        }
        let at = reattached_at.expect("reattached");
        assert!(at >= 1.7 && at - 1.0 < 2.0, "reattached at {at}");
    }

    // --- reattach to new windows only -------------------------------------------------------

    fn chrome(hwnd: isize, title: &str) -> WindowEntry {
        entry(hwnd, title, Some("chrome.exe"), "Chrome_WidgetWin_1", 1280, 720)
    }

    #[test]
    fn windows_that_existed_when_the_target_was_lost_are_ignored() {
        let key = TargetKey::of(&game(10));
        let known: HashSet<isize> = [20, 21].into();
        let old = entry(20, "Mina", Some("Mina.exe"), "UnityWndClass", 3000, 2000);
        let old_same_title = entry(21, "Mina", Some("Mina.exe"), "UnityWndClass", 1280, 720);
        let list = [old.clone(), old_same_title.clone()];
        assert!(choose_reattach(&key, 10, &known, &list).is_none());
        // A new window with the same key is taken, with the title then size preference among the new.
        let new_small = entry(22, "Mina", Some("Mina.exe"), "UnityWndClass", 640, 480);
        let new_big = entry(23, "Other", Some("Mina.exe"), "UnityWndClass", 1920, 1080);
        let list = [old.clone(), new_big.clone(), new_small.clone()];
        assert_eq!(choose_reattach(&key, 10, &known, &list).unwrap().hwnd, 22);
        let list = [old.clone(), new_big];
        assert_eq!(choose_reattach(&key, 10, &known, &list).unwrap().hwnd, 23);
        // The old handle coming back is taken even though it is "known".
        let back = entry(10, "Mina", Some("Mina.exe"), "UnityWndClass", 200, 100);
        let known: HashSet<isize> = [10, 20, 21].into();
        let list = [old, old_same_title, back];
        assert_eq!(choose_reattach(&key, 10, &known, &list).unwrap().hwnd, 10);
    }

    #[test]
    fn shared_host_siblings_are_not_taken_for_the_target() {
        // A browser game tab is closed: the window of the tab is gone, other browser windows stay.
        let target = chrome(10, "Game - Chrome");
        let mut r = Reattacher::new(TargetKey::of(&target), 10);
        let siblings = vec![chrome(11, "Mail - Chrome"), chrome(12, "Docs - Chrome")];
        assert_eq!(r.update(1.0, true, |_| false, || siblings.clone()), Step::Lost);
        // Nothing new for as long as the siblings are all there.
        for k in 1..10 {
            let t = 1.0 + k as f64 * REATTACH_POLL_S;
            assert_eq!(r.update(t, true, |_| false, || siblings.clone()), Step::Idle, "t={t}");
        }
        // A new browser window with the same key appears: taken.
        let mut with_new = siblings.clone();
        with_new.push(chrome(40, "Game - Chrome"));
        assert_eq!(r.update(5.0, true, |_| false, || with_new.clone()), Step::TryAttach(40));
        // UWP hosts behave the same.
        let uwp = |h, t: &str| {
            entry(
                h,
                t,
                Some("ApplicationFrameHost.exe"),
                "ApplicationFrameWindow",
                1280,
                720,
            )
        };
        let mut r = Reattacher::new(TargetKey::of(&uwp(1, "Game")), 1);
        let others = vec![uwp(2, "Calculator"), uwp(3, "Settings")];
        assert_eq!(r.update(0.0, true, |_| false, || others.clone()), Step::Lost);
        assert_eq!(r.update(0.5, true, |_| false, || others.clone()), Step::Idle);
    }

    #[test]
    fn the_old_handle_returning_is_taken_after_a_session_loss() {
        // The session was lost but the window never went away: it is in the list at once.
        let mut r = reattacher();
        let list = vec![
            game(10),
            entry(1, "Explorer", Some("explorer.exe"), "CabinetWClass", 800, 600),
        ];
        assert_eq!(r.update(0.0, true, |_| true, || list.clone()), Step::Lost);
        assert_eq!(r.update(0.0, true, |_| true, || list.clone()), Step::TryAttach(10));
    }

    #[test]
    fn the_title_follows_the_attached_window() {
        let mut r = reattacher();
        assert_eq!(r.target().title, "Mina");
        r.update(0.0, true, |_| false, Vec::new);
        let relaunched = vec![entry(
            50,
            "Mina - level 2",
            Some("Mina.exe"),
            "UnityWndClass",
            1280,
            720,
        )];
        assert_eq!(r.update(0.0, true, |_| false, || relaunched), Step::TryAttach(50));
        // A failed attach does not change the title.
        r.attach_result(0.0, 50, false);
        assert_eq!(r.target().title, "Mina");
        r.attach_result(0.0, 50, true);
        assert_eq!(r.target().title, "Mina - level 2");
        // The next loss prefers the new title over the bigger window.
        let before = vec![entry(
            60,
            "Mina - level 2",
            Some("Mina.exe"),
            "UnityWndClass",
            1280,
            720,
        )];
        assert_eq!(r.update(1.0, true, |_| false, || before), Step::Lost);
        let list = vec![
            entry(61, "Mina", Some("Mina.exe"), "UnityWndClass", 1920, 1080),
            entry(62, "Mina - level 2", Some("Mina.exe"), "UnityWndClass", 800, 600),
        ];
        assert_eq!(r.update(2.0, true, |_| false, || list), Step::TryAttach(62));
    }

    // --- lost source: reminders, timeout, exit ----------------------------------------------

    #[test]
    fn lost_source_reminds_at_once_then_every_ten_seconds() {
        let mut w = LostWatch::new(LostPolicy::default(), false);
        assert_eq!(w.update(0.0, true), LostAction::None);
        assert_eq!(w.update(5.0, false), LostAction::Remind);
        assert_eq!(w.update(5.5, false), LostAction::None);
        assert_eq!(w.update(14.9, false), LostAction::None);
        assert_eq!(w.update(15.0, false), LostAction::Remind);
        assert_eq!(w.update(25.0, false), LostAction::Remind);
        assert_eq!(w.update(34.9, false), LostAction::None);
        // A23: a windowed output gives the source up after the wait (the picker comes back), once.
        assert_eq!(w.update(35.0, false), LostAction::GiveUp);
        assert_eq!(w.update(45.0, false), LostAction::Remind);
        assert_eq!(w.update(500.0, false), LostAction::Remind);
        assert!(!w.is_fullscreen());
    }

    #[test]
    fn fullscreen_drops_to_a_window_after_the_timeout_once() {
        let mut w = LostWatch::new(LostPolicy::default(), true);
        assert_eq!(w.update(100.0, false), LostAction::Remind);
        assert_eq!(w.update(110.0, false), LostAction::Remind);
        assert_eq!(w.update(120.0, false), LostAction::Remind);
        assert_eq!(w.update(129.9, false), LostAction::None);
        assert_eq!(w.update(130.0, false), LostAction::GoWindowed);
        assert!(!w.is_fullscreen());
        // Windowed from now on: reminders go on, no second drop.
        assert_eq!(w.update(130.5, false), LostAction::None);
        assert_eq!(w.update(140.0, false), LostAction::Remind);
        assert_eq!(w.update(1000.0, false), LostAction::Remind);
    }

    #[test]
    fn a_user_switch_to_a_window_gives_up_without_a_drop() {
        let mut w = LostWatch::new(LostPolicy::default(), true);
        assert_eq!(w.update(100.0, false), LostAction::Remind);
        w.set_fullscreen(false); // F11 to a window
        assert_eq!(w.update(200.0, false), LostAction::GiveUp, "no drop for a window");
        w.set_fullscreen(true); // F11 back: the source was already given up in this lost period
        assert_eq!(w.update(210.0, false), LostAction::Remind);
        // A new lost period with the output fullscreen drops again.
        assert_eq!(w.update(220.0, true), LostAction::None);
        assert_eq!(w.update(230.0, false), LostAction::Remind);
        assert_eq!(w.update(260.0, false), LostAction::GoWindowed);
    }

    #[test]
    fn the_wait_can_change_during_a_lost_period() {
        let mut w = LostWatch::new(LostPolicy::default(), false);
        assert_eq!(w.update(0.0, false), LostAction::Remind);
        w.set_wait(5.0);
        assert_eq!(w.policy().wait_s, 5.0);
        assert_eq!(w.update(5.0, false), LostAction::GiveUp);
        w.set_wait(-3.0);
        assert_eq!(w.policy().wait_s, 0.0);
    }

    #[test]
    fn wait_zero_waits_forever_and_a_custom_timeout_applies() {
        let mut w = LostWatch::new(
            LostPolicy {
                wait_s: 0.0,
                exit_on_lost: false,
            },
            true,
        );
        let mut t = 0.0;
        while t < 10_000.0 {
            assert_ne!(w.update(t, false), LostAction::GoWindowed);
            t += 1.0;
        }
        assert!(w.is_fullscreen());
        let mut w = LostWatch::new(
            LostPolicy {
                wait_s: 5.0,
                exit_on_lost: false,
            },
            true,
        );
        assert_eq!(w.update(0.0, false), LostAction::Remind);
        assert_eq!(w.update(4.9, false), LostAction::None);
        assert_eq!(w.update(5.0, false), LostAction::GoWindowed);
    }

    #[test]
    fn exit_on_lost_exits_at_the_first_loss() {
        let mut w = LostWatch::new(
            LostPolicy {
                wait_s: 30.0,
                exit_on_lost: true,
            },
            true,
        );
        assert_eq!(w.update(1.0, true), LostAction::None);
        assert_eq!(w.update(2.0, false), LostAction::Exit);
    }

    #[test]
    fn reattaching_restarts_the_clock() {
        let mut w = LostWatch::new(LostPolicy::default(), true);
        assert_eq!(w.update(0.0, false), LostAction::Remind);
        assert_eq!(w.update(20.0, false), LostAction::Remind);
        // Back at 25 s, lost again at 40 s: the 30 s run from 40 s, not from 0 s.
        assert_eq!(w.update(25.0, true), LostAction::None);
        assert_eq!(w.update(40.0, false), LostAction::Remind);
        assert_eq!(w.update(69.9, false), LostAction::Remind);
        assert_eq!(w.update(70.0, false), LostAction::GoWindowed);
    }

    // --- black or stale ---------------------------------------------------------------------

    fn frame(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        px.iter().copied().cycle().take((w * h * 4) as usize).collect()
    }

    #[test]
    fn black_frame_detection() {
        assert!(is_black(&frame(8, 8, [0, 0, 0, 255])));
        assert!(is_black(&frame(8, 8, [2, 1, 0, 255])));
        assert!(!is_black(&frame(8, 8, [3, 0, 0, 255])));
        let mut f = frame(64, 64, [0, 0, 0, 255]);
        let last = f.len() - 4;
        f[last + 1] = 40; // a single lit pixel at the very end
        assert!(!is_black(&f));
        assert!(!is_black(&[]));
        // Transparent black counts as black: alpha is ignored.
        assert!(is_black(&frame(4, 4, [0, 0, 0, 0])));
    }

    #[test]
    fn black_frames_raise_the_message_after_the_delay() {
        let mut h = Health::new(0.0);
        h.note_frame(0.0, Some(true));
        assert_eq!(h.status(1.0), HealthStatus::Ok);
        h.note_frame(2.0, Some(true));
        assert_eq!(h.status(BLACK_AFTER_S - 0.01), HealthStatus::Ok);
        assert_eq!(h.status(BLACK_AFTER_S), HealthStatus::Black);
        assert!(h
            .status(BLACK_AFTER_S)
            .message()
            .unwrap()
            .contains(PROTECTED_OR_NOT_DRAWING));
        // A lit frame clears it at once.
        h.note_frame(4.0, Some(false));
        assert_eq!(h.status(4.0), HealthStatus::Ok);
        // The black timer restarts.
        h.note_frame(5.0, Some(true));
        assert_eq!(h.status(5.0 + BLACK_AFTER_S - 0.5), HealthStatus::Ok);
    }

    #[test]
    fn unsampled_frames_keep_the_black_state() {
        let mut h = Health::new(0.0);
        h.note_frame(0.0, Some(true));
        h.note_frame(1.0, None);
        h.note_frame(2.0, None);
        h.note_frame(3.5, None);
        assert_eq!(h.status(3.5), HealthStatus::Black);
    }

    #[test]
    fn silence_raises_the_stale_message() {
        let mut h = Health::new(0.0);
        // Never a frame: counted from the start of the session.
        assert_eq!(h.status(STALE_AFTER_S - 0.1), HealthStatus::Ok);
        assert_eq!(h.status(STALE_AFTER_S), HealthStatus::Stale);
        h.note_frame(5.0, Some(false));
        assert_eq!(h.status(STALE_AFTER_S), HealthStatus::Ok);
        assert_eq!(h.status(5.0 + STALE_AFTER_S), HealthStatus::Stale);
        assert!(h.status(30.0).message().unwrap().contains(PROTECTED_OR_NOT_DRAWING));
        h.reset(30.0);
        assert_eq!(h.status(31.0), HealthStatus::Ok);
    }

    #[test]
    fn session_loss_without_a_window_close_only_gives_the_stale_message() {
        // TODO(T-80b): a lost capture session whose window stays open (Direct3D 11 device loss, a
        // monitor hot-plug) is not detected as a lost target: the frames just stop and the only
        // reaction is this message. T-80b must restart the capture session. Until then this pins
        // the state machine: silence, then Stale, until a frame arrives again.
        let mut h = Health::new(0.0);
        h.note_frame(1.0, Some(false));
        assert_eq!(h.status(1.0 + STALE_AFTER_S - 0.1), HealthStatus::Ok);
        for t in [11.0, 60.0, 3600.0] {
            assert_eq!(h.status(t), HealthStatus::Stale);
        }
        h.note_frame(3601.0, Some(false));
        assert_eq!(h.status(3601.0), HealthStatus::Ok);
    }

    #[test]
    fn black_scan_is_rate_limited() {
        let mut h = Health::new(0.0);
        assert!(h.wants_sample(0.0));
        h.note_frame(0.0, Some(false));
        assert!(!h.wants_sample(0.05));
        assert!(h.wants_sample(0.1));
        assert_eq!(HealthStatus::Ok.message(), None);
    }

    // --- smoothed source --------------------------------------------------------------------

    /// Low-resolution "pixel art": `w` x `h` native pixels of a small palette in blocks whose widths
    /// and heights are pseudo-random in `runs.0 ..= runs.1` native pixels (a wide enough flat area
    /// survives a filter, and the varying widths avoid a lucky phase between edges and samples).
    fn pixel_art(w: u32, h: u32, runs: (u32, u32)) -> (u32, u32, Vec<[u8; 3]>) {
        let palette: [[u8; 3]; 6] = [
            [20, 20, 40],
            [200, 60, 40],
            [60, 180, 90],
            [240, 230, 120],
            [90, 90, 220],
            [250, 250, 250],
        ];
        let mut seed = 12345u32;
        let mut cuts = |n: u32| -> Vec<u32> {
            // Block index of every native coordinate.
            let (mut idx, mut left, mut cur) = (Vec::new(), 0, 0u32);
            for _ in 0..n {
                if left == 0 {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    left = runs.0 + (seed >> 16) % (runs.1 - runs.0 + 1);
                    cur += 1;
                }
                left -= 1;
                idx.push(cur);
            }
            idx
        };
        let (bx, by) = (cuts(w), cuts(h));
        let mut px = Vec::new();
        for &cy in &by {
            for &cx in &bx {
                let i = (cx * 7 + cy * 13 + (cx * cy) % 5) as usize % palette.len();
                px.push(palette[i]);
            }
        }
        (w, h, px)
    }
    fn to_bgra(px: &[[u8; 3]]) -> Vec<u8> {
        px.iter().flat_map(|p| [p[2], p[1], p[0], 255]).collect()
    }

    #[derive(Clone, Copy, Debug)]
    enum Filter {
        Linear,
        CatmullRom,
        Lanczos3,
    }

    impl Filter {
        fn radius(self) -> i32 {
            match self {
                Filter::Linear => 1,
                Filter::CatmullRom => 2,
                Filter::Lanczos3 => 3,
            }
        }

        fn weight(self, x: f32) -> f32 {
            let a = x.abs();
            match self {
                Filter::Linear => (1.0 - a).max(0.0),
                Filter::CatmullRom => {
                    if a < 1.0 {
                        1.5 * a * a * a - 2.5 * a * a + 1.0
                    } else if a < 2.0 {
                        -0.5 * a * a * a + 2.5 * a * a - 4.0 * a + 2.0
                    } else {
                        0.0
                    }
                }
                Filter::Lanczos3 => {
                    let sinc = |t: f32| {
                        if t.abs() < 1e-6 {
                            1.0
                        } else {
                            (std::f32::consts::PI * t).sin() / (std::f32::consts::PI * t)
                        }
                    };
                    if a < 3.0 {
                        sinc(x) * sinc(x / 3.0)
                    } else {
                        0.0
                    }
                }
            }
        }
    }

    /// Separable resample to `ow` x `oh` with pixel centres aligned (as a GPU filter or an image
    /// library does), in f32, rounded and clamped to 8 bits.
    fn resample(w: u32, h: u32, px: &[[u8; 3]], ow: u32, oh: u32, f: Filter) -> Vec<[u8; 3]> {
        // One axis: taps (source index, weight) of every output coordinate.
        let taps = |n_in: u32, n_out: u32| -> Vec<Vec<(usize, f32)>> {
            (0..n_out)
                .map(|o| {
                    let u = (o as f32 + 0.5) * n_in as f32 / n_out as f32 - 0.5;
                    let i0 = u.floor() as i32;
                    let mut t: Vec<(usize, f32)> = (i0 - f.radius() + 1..=i0 + f.radius())
                        .map(|i| (i.clamp(0, n_in as i32 - 1) as usize, f.weight(u - i as f32)))
                        .collect();
                    let sum: f32 = t.iter().map(|x| x.1).sum();
                    t.iter_mut().for_each(|x| x.1 /= sum);
                    t
                })
                .collect()
        };
        let (tx, ty) = (taps(w, ow), taps(h, oh));
        let mut mid = vec![[0f32; 3]; (ow * h) as usize];
        for y in 0..h as usize {
            for (x, t) in tx.iter().enumerate() {
                for c in 0..3 {
                    mid[y * ow as usize + x][c] = t.iter().map(|&(i, k)| px[y * w as usize + i][c] as f32 * k).sum();
                }
            }
        }
        let mut out = vec![[0u8; 3]; (ow * oh) as usize];
        for (y, t) in ty.iter().enumerate() {
            for x in 0..ow as usize {
                for c in 0..3 {
                    let v: f32 = t.iter().map(|&(i, k)| mid[i * ow as usize + x][c] * k).sum();
                    out[y * ow as usize + x][c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }

    /// Nearest-neighbour resample by a factor `s` (integer or not).
    fn nearest(w: u32, h: u32, px: &[[u8; 3]], s: f32) -> (u32, u32, Vec<[u8; 3]>) {
        let (ow, oh) = ((w as f32 * s).round() as u32, (h as f32 * s).round() as u32);
        let mut out = Vec::new();
        for y in 0..oh {
            for x in 0..ow {
                let sx = (((x as f32 + 0.5) / s) as u32).min(w - 1);
                let sy = (((y as f32 + 0.5) / s) as u32).min(h - 1);
                out.push(px[(sy * w + sx) as usize]);
            }
        }
        (ow, oh, out)
    }

    /// Filtered upscale by `s`: (width, height, BGRA).
    fn filtered(w: u32, h: u32, px: &[[u8; 3]], s: f32, f: Filter) -> (u32, u32, Vec<u8>) {
        let (ow, oh) = ((w as f32 * s).round() as u32, (h as f32 * s).round() as u32);
        (ow, oh, to_bgra(&resample(w, h, px, ow, oh, f)))
    }

    /// "Sharp bilinear": integer nearest prescale up to the next integer, bilinear to the final size.
    fn sharp_bilinear(w: u32, h: u32, px: &[[u8; 3]], s: f32) -> (u32, u32, Vec<u8>) {
        let p = s.ceil();
        let (pw, ph, pre) = nearest(w, h, px, p);
        let (ow, oh) = ((w as f32 * s).round() as u32, (h as f32 * s).round() as u32);
        (ow, oh, to_bgra(&resample(pw, ph, &pre, ow, oh, Filter::Linear)))
    }

    /// Anti-aliased heavy text at 1:1: proportional glyph cells of random width, 3 to 4.5 px stems
    /// and bars at random sub-pixel positions, exact box coverage, dark grey on light grey.
    fn aa_text(w: u32, h: u32) -> Vec<u8> {
        let mut seed = 0x2545_F491u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let mut cov = vec![0f32; (w * h) as usize];
        let overlap = |a0: f32, a1: f32, p: u32| (a1.min(p as f32 + 1.0) - a0.max(p as f32)).max(0.0);
        let rect = |cov: &mut Vec<f32>, x0: f32, x1: f32, y0: f32, y1: f32| {
            for y in (y0.floor() as u32)..(y1.ceil() as u32).min(h) {
                for x in (x0.floor() as u32)..(x1.ceil() as u32).min(w) {
                    let c = overlap(x0, x1, x) * overlap(y0, y1, y);
                    let v = &mut cov[(y * w + x) as usize];
                    *v = v.max(c);
                }
            }
        };
        let mut y = 4.0;
        while y + 18.0 < h as f32 {
            let mut x = 4.0;
            while x + 16.0 < w as f32 {
                let pitch = 9.0 + rnd() * 6.0;
                for _ in 0..1 + (rnd() * 3.0) as u32 {
                    let sx = x + rnd() * (pitch - 5.0);
                    let sw = 3.0 + rnd() * 1.5;
                    rect(&mut cov, sx, sx + sw, y, y + 14.0);
                }
                for _ in 0..(rnd() * 3.0) as u32 {
                    let by = y + rnd() * 10.0;
                    rect(&mut cov, x, x + pitch - 1.0, by, by + 3.0 + rnd());
                }
                x += pitch + 1.0;
            }
            y += 20.0;
        }
        cov.iter()
            .flat_map(|c| {
                let v = (235.0 - c * 205.0).round() as u8;
                [v, v, v, 255]
            })
            .collect()
    }

    fn whole(w: u32, h: u32) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    /// The verdict of a whole-frame analysis, with the numbers for the failure message.
    fn verdict(data: &[u8], w: u32, h: u32) -> (bool, String) {
        let s = edge_stats(data, w, whole(w, h));
        (
            s.looks_like_filtered_upscale(),
            format!("{:?} grid {:.2}", s.counts, s.grid_strength()),
        )
    }

    /// 200 x 150 native pixels in blocks of 3 to 7: flat areas wide enough to survive a Lanczos filter.
    fn art() -> (u32, u32, Vec<[u8; 3]>) {
        pixel_art(200, 150, (3, 7))
    }

    #[test]
    fn crisp_sources_have_no_filtered_edges() {
        let (w, h, px) = art();
        let native = to_bgra(&px);
        let c = edge_counts(&native, w, whole(w, h));
        assert!(c.hard > MIN_EDGES, "{c:?}");
        assert_eq!(c.soft, 0, "{c:?}");
        assert!(!c.looks_smoothed());
    }

    #[test]
    fn nearest_neighbour_at_any_scale_does_not_warn() {
        let (w, h, px) = art();
        for s in [1.0, 2.0, 3.0, 2.5, 3.37] {
            let (bw, bh, up) = nearest(w, h, &px, s);
            let (warn, msg) = verdict(&to_bgra(&up), bw, bh);
            assert!(!warn, "nearest x{s}: {msg}");
        }
    }

    #[test]
    fn bilinear_upscales_warn() {
        let (w, h, px) = art();
        for s in [2.0, 2.5, 3.0, 3.37, 4.0] {
            let (bw, bh, up) = filtered(w, h, &px, s, Filter::Linear);
            let (warn, msg) = verdict(&up, bw, bh);
            assert!(warn, "bilinear x{s}: {msg}");
        }
    }

    #[test]
    fn sharp_bilinear_upscales_warn() {
        let (w, h, px) = art();
        for s in [2.5, 3.37] {
            let (bw, bh, up) = sharp_bilinear(w, h, &px, s);
            let (warn, msg) = verdict(&up, bw, bh);
            assert!(warn, "sharp bilinear x{s}: {msg}");
        }
    }

    #[test]
    fn bicubic_and_lanczos_upscales_warn_despite_ringing() {
        let (w, h, px) = art();
        // Lanczos3 rings over six output pixels at x2: with blocks of 3 to 7 native pixels the flat
        // areas are short and the edge positions noisy, so its grid score is the weakest (about 0.25,
        // against 0.05 for text and 1.0 for the other filters).
        let cases = [
            (Filter::CatmullRom, 2.0),
            (Filter::CatmullRom, 2.5),
            (Filter::CatmullRom, 3.37),
            (Filter::Lanczos3, 2.0),
            (Filter::Lanczos3, 2.5),
            (Filter::Lanczos3, 3.37),
        ];
        for (f, s) in cases {
            let (bw, bh, up) = filtered(w, h, &px, s, f);
            let (warn, msg) = verdict(&up, bw, bh);
            assert!(warn, "{f:?} x{s}: {msg}");
        }
    }

    #[test]
    fn anti_aliased_text_at_one_to_one_does_not_warn() {
        let (w, h) = (800, 600);
        let text = aa_text(w, h);
        let s = edge_stats(&text, w, whole(w, h));
        // The text really has many filtered edges (the old verdict warned) but no grid.
        assert!(s.counts.looks_smoothed(), "{:?}", s.counts);
        assert!(
            !s.looks_like_filtered_upscale(),
            "{:?} grid {:.2}",
            s.counts,
            s.grid_strength()
        );
    }

    #[test]
    fn gradients_flat_and_tiny_frames_do_not_warn() {
        let (w, h) = (600u32, 400u32);
        for steps in [1.0f32, 2.0, 4.0] {
            let mut g = Vec::new();
            for _y in 0..h {
                for x in 0..w {
                    let v = ((x as f32 * steps * 0.4) % 256.0) as u8;
                    g.extend_from_slice(&[v, v / 2, 255 - v, 255]);
                }
            }
            let (warn, msg) = verdict(&g, w, h);
            assert!(!warn, "gradient {steps}: {msg}");
        }
        let f = frame(300, 200, [10, 20, 30, 255]);
        assert_eq!(edge_counts(&f, 300, whole(300, 200)), EdgeCounts::default());
        assert!(!verdict(&f, 300, 200).0);
        let f = frame(20, 20, [10, 20, 30, 255]);
        assert_eq!(edge_counts(&f, 20, whole(20, 20)), EdgeCounts::default());
        assert!(!EdgeCounts {
            hard: 0,
            soft: MIN_EDGES - 1
        }
        .looks_smoothed());
    }

    #[test]
    fn thin_lines_and_noise_are_not_filtered_edges() {
        // Flat background with one-pixel lines of a different colour: not monotone, ignored.
        let (w, h) = (200u32, 120u32);
        let mut f = frame(w, h, [30, 30, 30, 255]);
        for y in 0..h {
            for x in (0..w).step_by(10) {
                let i = ((y * w + x) * 4) as usize;
                f[i..i + 3].copy_from_slice(&[200, 200, 200]);
            }
        }
        let c = edge_counts(&f, w, whole(w, h));
        assert_eq!(c.soft, 0, "{c:?}");
    }

    #[test]
    fn small_overshoot_is_a_filtered_edge_large_one_is_not() {
        let from = [50, 50, 50, 255];
        let to = [250, 250, 250, 255];
        // 25 % of 200 = 50 levels allowed beyond the flat levels.
        assert!(monotone(
            &from,
            &[[40, 40, 40, 255], [140, 140, 140, 255], [255, 255, 255, 255]],
            &to
        ));
        let from = [150, 150, 150, 255];
        assert!(!monotone(&from, &[[100, 100, 100, 255], [200, 200, 200, 255]], &to));
        // Not monotone in the middle.
        assert!(!monotone(&from, &[[200, 200, 200, 255], [100, 100, 100, 255]], &to));
    }

    #[test]
    fn watch_warns_on_a_filtered_upscale_only_when_eligible() {
        let (w, h, px) = art();
        let (bw, bh, smooth) = filtered(w, h, &px, 2.5, Filter::Linear);
        let region = whole(bw, bh);
        let mut watch = SmoothingWatch::default();
        // Not eligible (a divisor was found or given): never warns.
        assert!(!watch.feed(&smooth, bw, region, false));
        assert!(!watch.warning());
        // Eligible: the first analysed frame is enough on this image, and it is raised once.
        assert!(watch.feed(&smooth, bw, region, true));
        assert!(watch.warning());
        assert!(!watch.feed(&smooth, bw, region, true));
        assert!(watch.warning());
        // Becoming ineligible clears the warning.
        watch.feed(&smooth, bw, region, false);
        assert!(!watch.warning());
    }

    #[test]
    fn watch_stays_quiet_on_text_and_stops_after_a_few_checks() {
        let (w, h) = (800, 600);
        let text = aa_text(w, h);
        let region = whole(w, h);
        let mut watch = SmoothingWatch::default();
        for _ in 0..SMOOTH_FRAME_GAP * (SMOOTH_MAX_CHECKS + 3) {
            assert!(!watch.feed(&text, w, region, true));
        }
        assert!(!watch.warning());
        assert_eq!(watch.checks, SMOOTH_MAX_CHECKS);
    }

    #[test]
    fn watch_analyses_one_frame_in_a_few() {
        let (w, h, px) = art();
        let crisp = to_bgra(&px);
        let mut watch = SmoothingWatch::default();
        for _ in 0..SMOOTH_FRAME_GAP {
            watch.feed(&crisp, w, whole(w, h), true);
        }
        assert_eq!(watch.checks, 1);
    }

    // --- size change ------------------------------------------------------------------------

    #[test]
    fn size_change_requests_a_history_clear() {
        let mut s = SizeWatch::default();
        assert!(!s.observe(256, 240), "the first frame is not a change");
        assert!(!s.take_history_clear());
        assert!(!s.observe(256, 240));
        assert!(s.observe(512, 240));
        assert_eq!(s.generation(), 1);
        assert!(s.take_history_clear());
        assert!(!s.take_history_clear(), "consumed once");
        assert!(!s.observe(512, 240));
        assert!(s.observe(256, 240));
        assert_eq!(s.generation(), 2);
    }

    #[test]
    fn a_new_session_can_request_a_history_clear() {
        let mut s = SizeWatch::default();
        s.observe(320, 200);
        s.request_history_clear();
        assert!(s.take_history_clear());
        assert_eq!(s.generation(), 0);
    }

    // ---- minimised or hidden source ----

    /// A minimised source (or one on another virtual desktop) says why no frames come, before the "not drawing"
    /// message the silence would give after 10 s; restored, the notice goes and the health speaks again.
    #[test]
    fn a_minimised_or_cloaked_source_is_named_instead_of_looking_frozen() {
        assert_eq!(SourceVisibility::of(false, false), SourceVisibility::Visible);
        assert_eq!(SourceVisibility::of(true, false), SourceVisibility::Minimised);
        assert_eq!(SourceVisibility::of(false, true), SourceVisibility::Cloaked);
        assert_eq!(SourceVisibility::of(true, true), SourceVisibility::Minimised);
        // Minimised at t = 0: no frames; at 12 s the health alone would say "not drawing".
        let h = Health::new(0.0);
        assert_eq!(h.status(12.0), HealthStatus::Stale);
        let n = source_notice(SourceVisibility::Minimised, h.status(12.0)).unwrap();
        assert!(n.contains("minimised") && n.contains("no frames"), "{n}");
        let n = source_notice(SourceVisibility::Cloaked, HealthStatus::Ok).unwrap();
        assert!(n.contains("another virtual desktop"), "{n}");
        // Restored: frames come again, nothing to say.
        let mut h = h;
        h.note_frame(13.0, Some(false));
        assert_eq!(source_notice(SourceVisibility::Visible, h.status(13.5)), None);
        // Visible but silent for long: the health message as before.
        assert!(source_notice(SourceVisibility::Visible, HealthStatus::Stale)
            .unwrap()
            .contains(PROTECTED_OR_NOT_DRAWING));
    }

    // ---- capture restart on the renderer's adapter ----

    use crate::adapters::Luid;

    /// A fake capture session: its adapter and frame count.
    #[derive(Debug, PartialEq)]
    struct FakeSession(Luid, u64);

    impl CaptureSession for FakeSession {
        fn adapter_luid(&self) -> Luid {
            self.0
        }
        fn frames_captured(&self) -> u64 {
            self.1
        }
    }

    fn luid(n: u32) -> Luid {
        Luid { low: n, high: 0 }
    }

    /// A fake factory: the adapters on which a start succeeds.
    fn factory(ok: &[u32]) -> impl FnMut(Option<Luid>) -> Result<FakeSession, String> + '_ {
        move |l: Option<Luid>| {
            let l = l.expect("a start always names its adapter");
            if ok.contains(&l.low) {
                Ok(FakeSession(l, 0))
            } else {
                Err(format!("no device on {}", l.low))
            }
        }
    }

    #[test]
    fn the_capture_follows_the_renderer_adapter_or_falls_back() {
        let mut base = 0;
        // Same adapter, no wanted adapter, no session: nothing happens.
        let mut cap = Some(FakeSession(luid(1), 10));
        let mut start = factory(&[]);
        assert_eq!(
            follow_renderer_adapter(&mut cap, Some(luid(1)), &mut base, &mut start),
            Restart::Kept
        );
        assert_eq!(
            follow_renderer_adapter(&mut cap, None, &mut base, &mut start),
            Restart::Kept
        );
        let mut none: Option<FakeSession> = None;
        assert_eq!(
            follow_renderer_adapter(&mut none, Some(luid(2)), &mut base, &mut start),
            Restart::Kept
        );
        assert_eq!((cap.as_ref().unwrap().0, base), (luid(1), 0));
        // Another adapter: closed (frames counted), started there.
        let mut start = factory(&[2]);
        assert_eq!(
            follow_renderer_adapter(&mut cap, Some(luid(2)), &mut base, &mut start),
            Restart::Restarted
        );
        assert_eq!((cap.as_ref().unwrap(), base), (&FakeSession(luid(2), 0), 10));
        // The start on the wanted adapter fails: back on the first adapter, with the reason.
        let mut cap = Some(FakeSession(luid(1), 5));
        let mut start = factory(&[1]);
        match follow_renderer_adapter(&mut cap, Some(luid(3)), &mut base, &mut start) {
            Restart::Fallback(why) => assert!(why.contains("no device on 3") && why.contains("CPU copy"), "{why}"),
            r => panic!("{r:?}"),
        }
        assert_eq!(cap, Some(FakeSession(luid(1), 0)), "a new session on the first adapter");
        assert_eq!(base, 15);
        // Both fail: no session, never a stale one.
        let mut start = factory(&[]);
        match follow_renderer_adapter(&mut cap, Some(luid(3)), &mut base, &mut start) {
            Restart::Lost(why) => assert!(
                why.contains("no device on 3") && why.contains("no device on 1"),
                "{why}"
            ),
            r => panic!("{r:?}"),
        }
        assert!(cap.is_none());
    }

    /// The restart and the zero-copy path: the restarted session's frames come from a new pool (a generation
    /// never seen, so the renderer imports it) and number from 1 again (so the frame the renderer held is not
    /// taken for a new one); after a fallback to the first adapter the zero-copy path is refused (cross adapter)
    /// and the CPU copy runs.
    #[test]
    fn a_restart_while_zero_copy_runs_never_leaves_a_stale_pool() {
        use crate::zerocopy::{already_held, decide, needs_import, next_id, CapturePath, HandleKind, Refusal, Support};
        // Session 1 (first guess) published frame 5 from its pool, the renderer imported and holds it.
        let (s1, pool1) = (next_id(), next_id());
        let imported = Some((pool1, 0xA000));
        let held = Some((s1, 5));
        // The restart: session 2, its own pool, frames from 1.
        let (s2, pool2) = (next_id(), next_id());
        assert!(needs_import(imported, (pool2, 0xB000)), "the new pool is imported");
        for seq in 1..=5 {
            assert!(
                !already_held(held, (s2, seq), true),
                "frame {seq} of the new session is uploaded"
            );
        }
        // Fallback onto the first adapter while the renderer is on another one: zero-copy refused, CPU copy.
        let support = Support {
            extension: true,
            handle: Some(HandleKind::Nt),
            same_adapter: Some(false),
        };
        assert_eq!(decide(CapturePath::ZeroCopy, support), Err(Refusal::CrossAdapter));
    }

    // ---- monitor and DPI of the output ----

    #[test]
    fn the_monitor_watch_reports_moves_and_dpi_changes_once() {
        use crate::modes::IRect;
        let (m0, m1) = (IRect::new(0, 0, 2560, 1440), IRect::new(2560, 0, 4480, 1080));
        let mut w = MonitorWatch::default();
        assert_eq!(
            w.observe(m0, 96),
            MonitorChange::default(),
            "the first sample is no change"
        );
        assert_eq!(w.observe(m0, 96), MonitorChange::default());
        assert_eq!(
            w.observe(m1, 144),
            MonitorChange {
                moved: true,
                dpi: Some(144)
            }
        );
        assert_eq!(w.observe(m1, 144), MonitorChange::default(), "reported once");
        // The user changes the scaling of the same monitor.
        assert_eq!(
            w.observe(m1, 120),
            MonitorChange {
                moved: false,
                dpi: Some(120)
            }
        );
        // An unknown DPI (0) is not a change.
        assert_eq!(w.observe(m1, 0), MonitorChange::default());
        assert_eq!(w.observe(m1, 120), MonitorChange::default());
    }
}
