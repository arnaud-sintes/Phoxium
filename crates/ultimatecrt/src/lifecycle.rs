//! Target lifecycle (T-56, CAP-5, CAP-7, SRC-2, SRC-8): the decisions around the captured window,
//! kept free of OS calls so they are unit tested on synthetic data.
//!
//! - [`Reattacher`]: when the target closes (or its capture session is lost) and a window with the
//!   same executable and class appears, say which window to capture next. The key is the one of the
//!   source profiles (T-51): executable file name and window class, case insensitive.
//! - [`Health`] and [`is_black`]: a protected window, an excluded window or one that does not draw
//!   captures as black frames or as no frames at all (CAP-5): a message instead of a silent black.
//! - [`SmoothingWatch`]: warns when no integer scale was found and the edges look smoothed (SRC-2).
//! - [`SizeWatch`]: flags a change of the source size (SRC-8). Resolution-dependent resources are
//!   rebuilt by the renderer on its own; the history clear is a flag for the persistence stage (T-41).
//!
//! Times are seconds on any monotonic clock, passed in, so the logic is deterministic.

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
    /// queried (elevated process) both sides must lack it and the title must be identical, otherwise
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
pub fn choose_reattach<'a>(
    target: &TargetKey,
    last_hwnd: isize,
    windows: &'a [WindowEntry],
) -> Option<&'a WindowEntry> {
    windows
        .iter()
        .filter(|w| target.matches(w) && w.width >= MIN_TARGET_SIZE && w.height >= MIN_TARGET_SIZE)
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
}

impl Reattacher {
    /// `hwnd` is the window currently captured, `target` its key.
    pub fn new(target: TargetKey, hwnd: isize) -> Self {
        Reattacher {
            target,
            hwnd,
            phase: Phase::Attached,
            failures: 0,
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
    /// returns the current window list; both are only called when needed.
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
                match choose_reattach(&self.target, self.hwnd, &windows) {
                    Some(w) => Step::TryAttach(w.hwnd),
                    None => Step::Idle,
                }
            }
        }
    }

    /// Reports the outcome of a [`Step::TryAttach`]. On failure the next attempt waits
    /// [`REATTACH_RETRY_S`].
    pub fn attach_result(&mut self, now: f64, hwnd: isize, ok: bool) {
        if ok {
            self.hwnd = hwnd;
            self.phase = Phase::Attached;
            self.failures = 0;
        } else {
            self.failures += 1;
            self.phase = Phase::Lost {
                next_poll: now + REATTACH_RETRY_S,
            };
        }
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

/// Pixels ignored on every side (rounded window corners), as in the scale detector.
const MARGIN: u32 = 16;
/// One line in this many is analysed, in both directions.
const LINE_STRIDE: u32 = 4;
/// Longest run of in-between pixels taken for a filtered edge (an upscale of up to x9).
const MAX_RAMP: usize = 8;
/// Edges whose flat sides differ by less than this (largest channel) are noise, not edges.
const MIN_EDGE_STEP: u8 = 16;
/// Edges needed before the verdict means anything.
const MIN_EDGES: u64 = 64;
/// Fraction of filtered edges above which the source looks smoothed.
const SMOOTHED_FRACTION: f64 = 0.5;
/// Frames between two analyses, and the analyses taken before giving up.
const SMOOTH_FRAME_GAP: u32 = 15;
const SMOOTH_MAX_CHECKS: u32 = 8;

/// Edges between two flat areas (runs of at least two identical pixels), counted by kind.
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

fn max_diff(a: &[u8; 4], b: &[u8; 4]) -> u8 {
    (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0)
}

/// True when each of B, G, R goes monotonically (up or down) from `from` through `ramp` to `to`.
fn monotone(from: &[u8; 4], ramp: &[[u8; 4]], to: &[u8; 4]) -> bool {
    (0..3).all(|c| {
        let mut up = true;
        let mut down = true;
        let mut prev = from[c];
        for v in ramp.iter().map(|p| p[c]).chain(std::iter::once(to[c])) {
            up &= v >= prev;
            down &= v <= prev;
            prev = v;
        }
        up || down
    })
}

/// Counts the edges of one line of pixels. Pixels are grouped in runs of identical values; a run of
/// two or more pixels is flat. Between two consecutive flat runs there are either no pixels (hard
/// edge) or a few single pixels. If those go monotonically from one side to the other it is a
/// filtered edge. Anything else (texture, a thin line, a gradient) says nothing and is skipped.
fn scan_line(line: &[[u8; 4]], counts: &mut EdgeCounts) {
    let mut prev_flat: Option<[u8; 4]> = None;
    let mut ramp: Vec<[u8; 4]> = Vec::new();
    let mut ramp_overflow = false;
    let mut i = 0;
    while i < line.len() {
        let px = line[i];
        let mut j = i + 1;
        while j < line.len() && line[j] == px {
            j += 1;
        }
        if j - i >= 2 {
            if let Some(from) = prev_flat {
                if max_diff(&from, &px) >= MIN_EDGE_STEP && !ramp_overflow {
                    if ramp.is_empty() {
                        counts.hard += 1;
                    } else if monotone(&from, &ramp, &px) {
                        counts.soft += 1;
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
/// [`LINE_STRIDE`], horizontally and vertically.
pub fn edge_counts(data: &[u8], frame_w: u32, region: Rect) -> EdgeCounts {
    let mut counts = EdgeCounts::default();
    let m = MARGIN.min(region.w / 8).min(region.h / 8);
    if region.w <= 2 * m + 2 || region.h <= 2 * m + 2 {
        return counts;
    }
    let (x0, x1) = (region.x + m, region.x + region.w - m);
    let (y0, y1) = (region.y + m, region.y + region.h - m);
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * frame_w + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    };
    let mut line: Vec<[u8; 4]> = Vec::new();
    for y in (y0..y1).step_by(LINE_STRIDE as usize) {
        line.clear();
        line.extend((x0..x1).map(|x| at(x, y)));
        scan_line(&line, &mut counts);
    }
    for x in (x0..x1).step_by(LINE_STRIDE as usize) {
        line.clear();
        line.extend((y0..y1).map(|y| at(x, y)));
        scan_line(&line, &mut counts);
    }
    counts
}

/// Accumulates [`EdgeCounts`] over a few frames while no integer scale is in use, and raises the
/// SRC-2 warning when the edges look smoothed.
#[derive(Debug, Default)]
pub struct SmoothingWatch {
    frames: u32,
    checks: u32,
    counts: EdgeCounts,
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
    /// user), the detection is locked and found no integer scale; otherwise the watch is reset.
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
        self.counts.add(edge_counts(data, frame_w, region));
        self.warned = self.counts.looks_smoothed();
        self.warned
    }
}

/// The SRC-2 warning text.
pub const SMOOTHED_WARNING: &str = "no integer scale found and the edges look smoothed: the source is probably \
    scaled with filtering, which cannot be undone (use the game at its native size or an integer scale)";

// ---------------------------------------------------------------------------------------------
// Source size change
// ---------------------------------------------------------------------------------------------

/// Notices a change of the source size (a mode switch, a window resize, a relaunch with another
/// size). The renderer rebuilds its resolution-dependent resources by itself when it is given a
/// frame of another size; what remains is the history (afterglow, ABL state), which the
/// persistence stage (T-41) must clear: it polls [`SizeWatch::take_history_clear`].
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
        assert_eq!(choose_reattach(&key, 10, &list).unwrap().hwnd, 20);
        let list = [small.clone(), retitled.clone()];
        assert_eq!(choose_reattach(&key, 10, &list).unwrap().hwnd, 21);
        let list = [small, retitled, big, same_handle];
        assert_eq!(choose_reattach(&key, 10, &list).unwrap().hwnd, 10);
    }

    #[test]
    fn choice_ignores_other_programs_and_tiny_windows() {
        let key = TargetKey::of(&game(10));
        let list = [
            entry(30, "Mina", Some("Mina.exe"), "UnityWndClass", 16, 16),
            entry(31, "Mina", Some("Editor.exe"), "UnityWndClass", 1280, 720),
            entry(32, "Mina", Some("Mina.exe"), "SplashClass", 1280, 720),
        ];
        assert!(choose_reattach(&key, 10, &list).is_none());
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
    fn black_scan_is_rate_limited() {
        let mut h = Health::new(0.0);
        assert!(h.wants_sample(0.0));
        h.note_frame(0.0, Some(false));
        assert!(!h.wants_sample(0.05));
        assert!(h.wants_sample(0.1));
        assert_eq!(HealthStatus::Ok.message(), None);
    }

    // --- smoothed source --------------------------------------------------------------------

    /// Native raster of flat colour blocks (a stand-in for pixel art): `bw` x `bh` blocks of `cell`
    /// pixels, colours from a small palette, deterministic.
    fn native_art(bw: u32, bh: u32, cell: u32) -> (u32, u32, Vec<[u8; 3]>) {
        let palette: [[u8; 3]; 6] = [
            [20, 20, 40],
            [200, 60, 40],
            [60, 180, 90],
            [240, 230, 120],
            [90, 90, 220],
            [250, 250, 250],
        ];
        let (w, h) = (bw * cell, bh * cell);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let (bx, by) = (x / cell, y / cell);
                let i = (bx * 7 + by * 13 + (bx * by) % 5) as usize % palette.len();
                px.push(palette[i]);
            }
        }
        (w, h, px)
    }

    fn to_bgra(px: &[[u8; 3]]) -> Vec<u8> {
        px.iter().flat_map(|p| [p[2], p[1], p[0], 255]).collect()
    }

    fn nearest(w: u32, h: u32, px: &[[u8; 3]], k: u32) -> (u32, u32, Vec<u8>) {
        let mut out = Vec::new();
        for y in 0..h * k {
            for x in 0..w * k {
                let p = px[((y / k) * w + x / k) as usize];
                out.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        (w * k, h * k, out)
    }

    /// Bilinear upscale by `k`, pixel centres aligned as a GPU texture filter does.
    fn bilinear(w: u32, h: u32, px: &[[u8; 3]], k: u32) -> (u32, u32, Vec<u8>) {
        let mut out = Vec::new();
        let at = |x: i32, y: i32| px[(y.clamp(0, h as i32 - 1) as u32 * w + x.clamp(0, w as i32 - 1) as u32) as usize];
        for y in 0..h * k {
            for x in 0..w * k {
                let fx = (x as f32 + 0.5) / k as f32 - 0.5;
                let fy = (y as f32 + 0.5) / k as f32 - 0.5;
                let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                let mut c = [0u8; 3];
                for (ch, v) in c.iter_mut().enumerate() {
                    let p = |dx, dy| at(x0 + dx, y0 + dy)[ch] as f32;
                    let top = p(0, 0) * (1.0 - tx) + p(1, 0) * tx;
                    let bottom = p(0, 1) * (1.0 - tx) + p(1, 1) * tx;
                    *v = (top * (1.0 - ty) + bottom * ty).round() as u8;
                }
                out.extend_from_slice(&[c[2], c[1], c[0], 255]);
            }
        }
        (w * k, h * k, out)
    }

    fn whole(w: u32, h: u32) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    #[test]
    fn crisp_sources_have_no_filtered_edges() {
        let (w, h, px) = native_art(40, 30, 8);
        let native = to_bgra(&px);
        let c = edge_counts(&native, w, whole(w, h));
        assert!(c.hard > MIN_EDGES, "{c:?}");
        assert_eq!(c.soft, 0, "{c:?}");
        assert!(!c.looks_smoothed());

        let (bw, bh, up) = nearest(w, h, &px, 3);
        let c = edge_counts(&up, bw, whole(bw, bh));
        assert!(!c.looks_smoothed(), "{c:?}");
        assert_eq!(c.soft, 0, "{c:?}");
    }

    #[test]
    fn bilinear_upscales_look_smoothed() {
        let (w, h, px) = native_art(40, 30, 6);
        for k in [3, 4, 6] {
            let (bw, bh, up) = bilinear(w, h, &px, k);
            let c = edge_counts(&up, bw, whole(bw, bh));
            assert!(c.looks_smoothed(), "x{k}: {c:?}");
            assert!(c.soft > c.hard * 3, "x{k}: {c:?}");
        }
    }

    #[test]
    fn flat_or_tiny_frames_do_not_warn() {
        let f = frame(300, 200, [10, 20, 30, 255]);
        assert_eq!(edge_counts(&f, 300, whole(300, 200)), EdgeCounts::default());
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
    fn watch_warns_on_a_smoothed_source_only_when_eligible() {
        let (w, h, px) = native_art(40, 30, 6);
        let (bw, bh, smooth) = bilinear(w, h, &px, 4);
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
    fn watch_stays_quiet_on_a_crisp_source_and_stops_after_a_few_checks() {
        let (w, h, px) = native_art(40, 30, 8);
        let crisp = to_bgra(&px);
        let region = whole(w, h);
        let mut watch = SmoothingWatch::default();
        for _ in 0..SMOOTH_FRAME_GAP * (SMOOTH_MAX_CHECKS + 3) {
            assert!(!watch.feed(&crisp, w, region, true));
        }
        assert!(!watch.warning());
        assert_eq!(watch.checks, SMOOTH_MAX_CHECKS);
    }

    #[test]
    fn watch_analyses_one_frame_in_a_few() {
        let (w, h, px) = native_art(40, 30, 8);
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
}
