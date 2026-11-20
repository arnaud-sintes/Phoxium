//! Source selection (owner requirement 2026-11-20, ARCHITECTURE A23): the product is one window and needs no
//! command line; the window to capture is chosen inside the output window. This module holds the decisions,
//! free of OS calls so they are unit tested on synthetic windows:
//!
//! - [`candidates`]: which top-level windows the picker lists ([`exclusion`]: our own process, invisible,
//!   cloaked, tool and owned windows, zero size, untitled, the shell); minimised windows are kept but flagged
//!   (they deliver no frames until restored).
//! - [`sort_rows`]: the windows recently in the foreground first ([`Recent`]), then alphabetical.
//! - [`matches`], [`visible`], [`step_selection`]: the search box and the keyboard navigation.
//! - [`find_last`], [`start_choice`]: the last choice and `[global.source] ask_at_start`.
//! - [`Phase`], [`Event`], [`next`]: the source state machine NoSource -> Picking -> Attached -> Lost ->
//!   NoSource.
//! - [`ThumbPlan`], [`thumbnail`]: when to take a one-shot preview capture (one at a time, never of our own
//!   windows) and its downscale.
//!
//! The enumeration itself (`sources::enumerate`) and the icons (`sources::exe_icon`) are the thin Win32 glue.

use crate::settings::LastSource;

/// Window classes of the shell (taskbars, desktop, flyouts) and of our own output: never listed.
pub const SHELL_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    "NotifyIconOverflowWindow",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "ultimatecrt_output",
];

/// Executables of the shell's own windows (start menu, search, input panel, lock screen): never listed.
pub const SHELL_EXES: &[&str] = &[
    "ShellExperienceHost.exe",
    "StartMenuExperienceHost.exe",
    "SearchHost.exe",
    "SearchApp.exe",
    "TextInputHost.exe",
    "LockApp.exe",
];

/// Smallest window side (px) taken when the last choice is reattached at start (splash screens, tool tips).
pub const MIN_ATTACH_SIZE: i32 = 32;

/// The facts of one top-level window, as `sources::enumerate` reads them (`EnumWindows` order: top of the
/// z-order first).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawWindow {
    /// Raw `HWND` value.
    pub hwnd: isize,
    pub title: String,
    pub class: String,
    /// Executable file name, `None` when the process cannot be queried (protected, anti-cheat).
    pub exe: Option<String>,
    /// Full executable path (for its icon), `None` as above.
    pub exe_path: Option<String>,
    /// The window belongs to this process (the output window, any other window of ours).
    pub own_process: bool,
    /// `IsWindowVisible`.
    pub visible: bool,
    /// `DWMWA_CLOAKED`: hidden by the compositor (another virtual desktop, a suspended UWP frame).
    pub cloaked: bool,
    /// `WS_EX_TOOLWINDOW`.
    pub tool_window: bool,
    /// `WS_EX_APPWINDOW`: shown in the taskbar even when owned.
    pub app_window: bool,
    /// Has an owner window (a dialog or a palette of another window).
    pub owned: bool,
    /// `IsIconic`.
    pub minimized: bool,
    /// Outer size in px.
    pub width: i32,
    pub height: i32,
    /// Index of the monitor it is on (the order of `output::monitor_rects`).
    pub monitor: Option<usize>,
}

/// Why a window is not listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exclusion {
    OwnProcess,
    Invisible,
    Cloaked,
    ToolWindow,
    Owned,
    ZeroSize,
    Untitled,
    Shell,
}

/// Why `w` is not a candidate, `None` when it is one. Minimised windows are candidates (shown greyed).
pub fn exclusion(w: &RawWindow) -> Option<Exclusion> {
    let shell = SHELL_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(&w.class))
        || w.exe
            .as_deref()
            .is_some_and(|e| SHELL_EXES.iter().any(|s| s.eq_ignore_ascii_case(e)));
    Some(if w.own_process {
        Exclusion::OwnProcess
    } else if !w.visible {
        Exclusion::Invisible
    } else if w.cloaked {
        Exclusion::Cloaked
    } else if w.tool_window && !w.app_window {
        Exclusion::ToolWindow
    } else if w.owned && !w.app_window {
        Exclusion::Owned
    } else if !w.minimized && (w.width <= 0 || w.height <= 0) {
        Exclusion::ZeroSize
    } else if w.title.trim().is_empty() {
        Exclusion::Untitled
    } else if shell {
        Exclusion::Shell
    } else {
        return None;
    })
}

/// One window the picker lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub hwnd: isize,
    pub title: String,
    pub exe: Option<String>,
    pub exe_path: Option<String>,
    pub class: String,
    pub width: i32,
    pub height: i32,
    pub monitor: Option<usize>,
    /// Minimised: listed greyed, with the note that it delivers no frames until restored.
    pub minimized: bool,
}

impl Row {
    /// The program name shown: the executable without `.exe`, else "unknown program".
    pub fn program(&self) -> String {
        match self.exe.as_deref() {
            Some(e) => {
                let stem = e.strip_suffix(".exe").or_else(|| e.strip_suffix(".EXE")).unwrap_or(e);
                stem.to_string()
            }
            None => "unknown program".to_string(),
        }
    }

    /// The second line of a row: `pixel-demo.exe  ·  1280 × 720  ·  monitor 2`, or the minimised note.
    pub fn details(&self) -> String {
        let exe = self.exe.as_deref().unwrap_or("unknown program");
        if self.minimized {
            return format!("{exe}  \u{00B7}  minimised: no frames until restored");
        }
        let mut s = format!("{exe}  \u{00B7}  {} \u{00D7} {}", self.width, self.height);
        if let Some(m) = self.monitor {
            s.push_str(&format!("  \u{00B7}  monitor {}", m + 1));
        }
        s
    }

    /// The last-choice record of this window.
    pub fn last_source(&self) -> LastSource {
        LastSource {
            exe: self.exe.clone().unwrap_or_default(),
            class: self.class.clone(),
            title: self.title.clone(),
        }
    }
}

/// The windows the picker lists, in enumeration order.
pub fn candidates(raw: &[RawWindow]) -> Vec<Row> {
    raw.iter()
        .filter(|w| exclusion(w).is_none())
        .map(|w| Row {
            hwnd: w.hwnd,
            title: w.title.clone(),
            exe: w.exe.clone(),
            exe_path: w.exe_path.clone(),
            class: w.class.clone(),
            width: w.width,
            height: w.height,
            monitor: w.monitor,
            minimized: w.minimized,
        })
        .collect()
}

/// The windows seen in the foreground, most recent first (sampled by the app; seeded with the top of the
/// z-order at start, which Windows keeps in activation order).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recent {
    list: Vec<isize>,
}

/// How many foreground windows [`Recent`] remembers.
pub const RECENT_MAX: usize = 32;

impl Recent {
    /// `hwnd` came to the foreground (0 is ignored).
    pub fn note(&mut self, hwnd: isize) {
        if hwnd == 0 || self.list.first() == Some(&hwnd) {
            return;
        }
        self.list.retain(|&h| h != hwnd);
        self.list.insert(0, hwnd);
        self.list.truncate(RECENT_MAX);
    }

    /// Seeds the order with windows known to be recent (the top of the z-order), after what was noted.
    pub fn seed(&mut self, hwnds: impl IntoIterator<Item = isize>) {
        for h in hwnds {
            if h != 0 && !self.list.contains(&h) && self.list.len() < RECENT_MAX {
                self.list.push(h);
            }
        }
    }

    pub fn as_slice(&self) -> &[isize] {
        &self.list
    }
}

/// Sorts the rows: the windows of `recent` first (most recent first), then the others alphabetically by title
/// (case insensitive), the handle breaking ties so the order is stable.
pub fn sort_rows(rows: &mut [Row], recent: &[isize]) {
    rows.sort_by_cached_key(|r| {
        (
            recent.iter().position(|&h| h == r.hwnd).unwrap_or(usize::MAX),
            r.title.to_lowercase(),
            r.hwnd,
        )
    });
}

/// Whether `row` matches the search `query`: every word of it appears in the title, the executable or the
/// class (case insensitive). An empty query matches everything.
pub fn matches(row: &Row, query: &str) -> bool {
    let hay = format!("{} {} {}", row.title, row.exe.as_deref().unwrap_or(""), row.class).to_lowercase();
    query.split_whitespace().all(|w| hay.contains(&w.to_lowercase()))
}

/// The handles of the rows the search shows, in list order.
pub fn visible(rows: &[Row], query: &str) -> Vec<isize> {
    rows.iter().filter(|r| matches(r, query)).map(|r| r.hwnd).collect()
}

/// The selection after a list change: kept when still shown, else the first row shown (`None` for an
/// empty list).
pub fn keep_selection(visible: &[isize], current: Option<isize>) -> Option<isize> {
    current
        .filter(|c| visible.contains(c))
        .or_else(|| visible.first().copied())
}

/// The selection after Up (`delta` -1), Down (+1) or a page (`delta` ±N): clamped to the ends, no wrap. With
/// nothing selected, Down selects the first row and Up the last.
pub fn step_selection(visible: &[isize], current: Option<isize>, delta: i32) -> Option<isize> {
    if visible.is_empty() {
        return None;
    }
    let last = visible.len() as i64 - 1;
    let i = match current.and_then(|c| visible.iter().position(|&h| h == c)) {
        Some(i) => (i as i64 + delta as i64).clamp(0, last),
        None if delta < 0 => last,
        None => 0,
    };
    Some(visible[i as usize])
}

/// Whether `row` is the program and class of `last` (case insensitive). Without a recorded executable (a
/// protected process) the title must also be identical, as for the reattach key (`lifecycle::TargetKey`).
pub fn is_last(last: &LastSource, row: &Row) -> bool {
    if !last.class.eq_ignore_ascii_case(&row.class) {
        return false;
    }
    match (last.exe.is_empty(), row.exe.as_deref()) {
        (false, Some(e)) => last.exe.eq_ignore_ascii_case(e),
        (true, None) => last.title == row.title,
        _ => false,
    }
}

/// The running window of the last choice: same program and class ([`is_last`]), not minimised, at least
/// [`MIN_ATTACH_SIZE`]; the same title first, then the largest.
pub fn find_last<'a>(last: &LastSource, rows: &'a [Row]) -> Option<&'a Row> {
    rows.iter()
        .filter(|r| is_last(last, r) && !r.minimized && r.width >= MIN_ATTACH_SIZE && r.height >= MIN_ATTACH_SIZE)
        .max_by_key(|r| (r.title == last.title, r.width as i64 * r.height as i64))
}

/// What happens at start without `--window`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartChoice {
    /// Show the window picker.
    Picker,
    /// Attach to this window at once.
    Attach(isize),
}

/// `ask_at_start` on: always the picker. Off: the window of the last choice when one is running
/// ([`find_last`]), else the picker.
pub fn start_choice(ask_at_start: bool, last: Option<&LastSource>, rows: &[Row]) -> StartChoice {
    if ask_at_start {
        return StartChoice::Picker;
    }
    last.and_then(|l| find_last(l, rows))
        .map_or(StartChoice::Picker, |r| StartChoice::Attach(r.hwnd))
}

// ---------------------------------------------------------------------------------------------------------
// The source state machine.

/// Where the source is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing captured and the picker not shown yet (start, after a detach or a lost source given up): the
    /// app shows the picker as soon as no other UI is open.
    NoSource,
    /// The picker is shown; `attached`: a source is still captured behind it (Change source), so Cancel goes
    /// back to it.
    Picking { attached: bool },
    /// A window is captured.
    Attached,
    /// The captured window closed; waiting (reattach to the same program and class) for `wait_s`.
    Lost,
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The picker is opened (the app at NoSource, or Change source in the menu).
    ShowPicker,
    /// A capture session started on a chosen window (or the window of `--window` / the last choice at start).
    Attached,
    /// The chosen window could not be captured (the picker stays, with a message).
    AttachFailed,
    /// Cancel or Esc in the picker.
    Cancel,
    /// Detach in the menu: the capture stops.
    Detach,
    /// The captured window closed or its session was lost.
    SourceLost,
    /// A new window of the same program and class was attached during the wait.
    Reattached,
    /// The wait is over without a reattach.
    GiveUp,
}

/// The next phase. Events that do not apply leave the phase unchanged.
pub fn next(phase: Phase, event: Event) -> Phase {
    use Phase::*;
    match (phase, event) {
        (_, Event::Attached) => Attached,
        (NoSource, Event::ShowPicker) | (Lost, Event::ShowPicker) => Picking { attached: false },
        (Attached, Event::ShowPicker) => Picking { attached: true },
        (Picking { attached: true }, Event::Cancel) => Attached,
        (Picking { .. }, Event::Detach) | (Picking { .. }, Event::SourceLost) => Picking { attached: false },
        (Attached, Event::Detach) | (Lost, Event::Detach) => NoSource,
        (Attached, Event::SourceLost) => Lost,
        (Lost, Event::Reattached) => Attached,
        (Lost, Event::GiveUp) => NoSource,
        (p, _) => p,
    }
}

/// Whether a capture session runs in `phase` (the picture behind the picker in Change source).
pub fn captures(phase: Phase) -> bool {
    matches!(phase, Phase::Attached | Phase::Picking { attached: true })
}

/// Whether the reattach and the lost-source wait run in `phase`.
pub fn follows_target(phase: Phase) -> bool {
    matches!(phase, Phase::Attached | Phase::Lost | Phase::Picking { attached: true })
}

// ---------------------------------------------------------------------------------------------------------
// Preview thumbnails.

/// Seconds a row must stay hovered or selected before its preview is captured (scrolling through the list
/// starts no capture).
pub const THUMB_DWELL_S: f64 = 0.2;
/// Seconds after which a preview of the row still shown is taken again (live-ish).
pub const THUMB_REFRESH_S: f64 = 4.0;
/// Seconds a preview capture may take before it is abandoned (a window that does not draw).
pub const THUMB_TIMEOUT_S: f64 = 1.5;

/// When to start a one-shot preview capture: at most one at a time, never of `own` windows nor of minimised
/// ones (no frames), after a short dwell, again after [`THUMB_REFRESH_S`] or on a refresh request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThumbPlan {
    /// The capture in flight: window and start time.
    pending: Option<(isize, f64)>,
    /// The row wanted and since when.
    want: Option<(isize, f64)>,
    /// The last finished capture: window, time, success.
    last: Option<(isize, f64, bool)>,
    refresh: bool,
}

impl ThumbPlan {
    /// Asks for a new preview of the wanted row now (the refresh button).
    pub fn refresh(&mut self) {
        self.refresh = true;
    }

    /// The window of the capture in flight.
    pub fn pending(&self) -> Option<isize> {
        self.pending.map(|(h, _)| h)
    }

    /// Called every loop while the picker is open with the row whose preview is wanted (hovered, else
    /// selected) and whether it is minimised. Returns the window to start a capture on, if any; the caller
    /// then reports with [`ThumbPlan::finished`].
    pub fn update(&mut self, now: f64, wanted: Option<(isize, bool)>, own: impl Fn(isize) -> bool) -> Option<isize> {
        let wanted = wanted
            .filter(|&(h, minimized)| h != 0 && !minimized && !own(h))
            .map(|(h, _)| h);
        match (wanted, self.want) {
            (Some(h), Some((w, _))) if h == w => {}
            (Some(h), _) => self.want = Some((h, now)),
            (None, _) => self.want = None,
        }
        if self.pending.is_some() {
            return None;
        }
        let (h, since) = self.want?;
        if now - since < THUMB_DWELL_S {
            return None;
        }
        let due = match self.last {
            Some((l, t, _)) if l == h => self.refresh || now - t >= THUMB_REFRESH_S,
            _ => true,
        };
        if !due {
            return None;
        }
        self.refresh = false;
        self.pending = Some((h, now));
        Some(h)
    }

    /// Whether the capture in flight ran past [`THUMB_TIMEOUT_S`] (the caller drops it and reports a failure).
    pub fn timed_out(&self, now: f64) -> bool {
        self.pending.is_some_and(|(_, t)| now - t >= THUMB_TIMEOUT_S)
    }

    /// The capture of `hwnd` ended (`ok`: a frame arrived).
    pub fn finished(&mut self, hwnd: isize, now: f64, ok: bool) {
        if self.pending.is_some_and(|(h, _)| h == hwnd) {
            self.pending = None;
        }
        self.last = Some((hwnd, now, ok));
    }

    /// Forgets everything (the picker closed): the next open starts fresh.
    pub fn reset(&mut self) {
        *self = ThumbPlan::default();
    }
}

/// Downscales a BGRA frame (`w` x `h`) to fit `max_w` x `max_h` (never upscaled), averaging each output pixel's
/// box, and converts it to RGBA with an opaque alpha (an ImGui texture).
pub fn thumbnail(bgra: &[u8], w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
    if w == 0 || h == 0 || bgra.len() < (w * h * 4) as usize {
        return (0, 0, Vec::new());
    }
    let s = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
    let (ow, oh) = (
        ((w as f64 * s).round() as u32).max(1),
        ((h as f64 * s).round() as u32).max(1),
    );
    let mut out = Vec::with_capacity((ow * oh * 4) as usize);
    for oy in 0..oh {
        let (y0, y1) = (oy * h / oh, ((oy + 1) * h / oh).max(oy * h / oh + 1));
        for ox in 0..ow {
            let (x0, x1) = (ox * w / ow, ((ox + 1) * w / ow).max(ox * w / ow + 1));
            let mut sum = [0u64; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    sum[0] += bgra[i + 2] as u64;
                    sum[1] += bgra[i + 1] as u64;
                    sum[2] += bgra[i] as u64;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            out.extend_from_slice(&[
                ((sum[0] + n / 2) / n) as u8,
                ((sum[1] + n / 2) / n) as u8,
                ((sum[2] + n / 2) / n) as u8,
                255,
            ]);
        }
    }
    (ow, oh, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(hwnd: isize, title: &str, exe: &str, class: &str) -> RawWindow {
        RawWindow {
            hwnd,
            title: title.into(),
            class: class.into(),
            exe: Some(exe.into()),
            exe_path: Some(format!(r"C:\Apps\{exe}")),
            visible: true,
            width: 800,
            height: 600,
            monitor: Some(0),
            ..RawWindow::default()
        }
    }

    fn row(hwnd: isize, title: &str, exe: Option<&str>, class: &str) -> Row {
        Row {
            hwnd,
            title: title.into(),
            exe: exe.map(String::from),
            exe_path: None,
            class: class.into(),
            width: 800,
            height: 600,
            monitor: Some(0),
            minimized: false,
        }
    }

    #[test]
    fn filtering_keeps_normal_and_minimised_windows_only() {
        let ok = raw(1, "Game", "game.exe", "SDL_app");
        let cases = [
            (
                RawWindow {
                    own_process: true,
                    ..ok.clone()
                },
                Some(Exclusion::OwnProcess),
            ),
            (
                RawWindow {
                    visible: false,
                    ..ok.clone()
                },
                Some(Exclusion::Invisible),
            ),
            (
                RawWindow {
                    cloaked: true,
                    ..ok.clone()
                },
                Some(Exclusion::Cloaked),
            ),
            (
                RawWindow {
                    tool_window: true,
                    ..ok.clone()
                },
                Some(Exclusion::ToolWindow),
            ),
            (
                RawWindow {
                    tool_window: true,
                    app_window: true,
                    ..ok.clone()
                },
                None,
            ),
            (
                RawWindow {
                    owned: true,
                    ..ok.clone()
                },
                Some(Exclusion::Owned),
            ),
            (RawWindow { width: 0, ..ok.clone() }, Some(Exclusion::ZeroSize)),
            (
                RawWindow {
                    title: "  ".into(),
                    ..ok.clone()
                },
                Some(Exclusion::Untitled),
            ),
            (
                raw(2, "Taskbar", "explorer.exe", "Shell_TrayWnd"),
                Some(Exclusion::Shell),
            ),
            (
                raw(3, "Program Manager", "explorer.exe", "Progman"),
                Some(Exclusion::Shell),
            ),
            (
                raw(4, "Search", "SearchHost.exe", "Windows.UI.Core.CoreWindow"),
                Some(Exclusion::Shell),
            ),
            (
                raw(5, "ultimatecrt", "x.exe", "ultimatecrt_output"),
                Some(Exclusion::Shell),
            ),
            // Minimised windows report a tiny or zero size: listed anyway, flagged.
            (
                RawWindow {
                    minimized: true,
                    width: 0,
                    height: 0,
                    ..ok.clone()
                },
                None,
            ),
            (ok.clone(), None),
        ];
        for (w, want) in &cases {
            assert_eq!(exclusion(w), *want, "{w:?}");
        }
        let all: Vec<RawWindow> = cases.iter().map(|(w, _)| w.clone()).collect();
        let rows = candidates(&all);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|r| r.minimized));
    }

    #[test]
    fn rows_describe_program_size_monitor_and_minimised() {
        let mut r = row(1, "Mina", Some("Mina.exe"), "UnityWndClass");
        r.monitor = Some(1);
        assert_eq!(r.program(), "Mina");
        assert_eq!(r.details(), "Mina.exe  \u{00B7}  800 \u{00D7} 600  \u{00B7}  monitor 2");
        r.minimized = true;
        assert!(r.details().contains("minimised"));
        let u = row(2, "x", None, "C");
        assert_eq!(u.program(), "unknown program");
        assert_eq!(
            u.last_source(),
            LastSource {
                exe: String::new(),
                class: "C".into(),
                title: "x".into()
            }
        );
    }

    #[test]
    fn recent_windows_first_then_alphabetical() {
        let mut rows = vec![
            row(1, "zeta", None, "C"),
            row(2, "Alpha", None, "C"),
            row(3, "beta", None, "C"),
            row(4, "Gamma", None, "C"),
        ];
        let mut recent = Recent::default();
        recent.note(4);
        recent.note(3);
        recent.note(0); // ignored
        recent.note(3); // already first
        assert_eq!(recent.as_slice(), &[3, 4]);
        recent.seed([1, 3]);
        assert_eq!(
            recent.as_slice(),
            &[3, 4, 1],
            "seeded after the noted ones, no duplicates"
        );
        sort_rows(&mut rows, &[3, 4]);
        let order: Vec<isize> = rows.iter().map(|r| r.hwnd).collect();
        assert_eq!(order, [3, 4, 2, 1]);
        for i in 0..40 {
            recent.note(100 + i);
        }
        assert_eq!(recent.as_slice().len(), RECENT_MAX);
        assert_eq!(recent.as_slice()[0], 139);
    }

    #[test]
    fn search_matches_every_word_in_title_exe_or_class() {
        let r = row(1, "Super Metroid (USA)", Some("snes9x-x64.exe"), "Snes9X: WndClass");
        assert!(matches(&r, ""));
        assert!(matches(&r, "metroid"));
        assert!(matches(&r, "SNES9X metroid"));
        assert!(matches(&r, "wndclass"));
        assert!(!matches(&r, "metroid zelda"));
        let rows = vec![r.clone(), row(2, "Notepad", Some("notepad.exe"), "Notepad")];
        assert_eq!(visible(&rows, "note"), [2]);
        assert_eq!(visible(&rows, "  "), [1, 2]);
    }

    #[test]
    fn keyboard_navigation_clamps_and_keeps_the_selection() {
        let v = [10, 20, 30];
        assert_eq!(step_selection(&v, None, 1), Some(10));
        assert_eq!(step_selection(&v, None, -1), Some(30));
        assert_eq!(step_selection(&v, Some(10), 1), Some(20));
        assert_eq!(step_selection(&v, Some(30), 1), Some(30), "no wrap");
        assert_eq!(step_selection(&v, Some(10), -1), Some(10));
        assert_eq!(step_selection(&v, Some(20), 10), Some(30), "a page clamps");
        assert_eq!(
            step_selection(&v, Some(99), 1),
            Some(10),
            "a vanished selection restarts"
        );
        assert_eq!(step_selection(&[], Some(10), 1), None);
        assert_eq!(keep_selection(&v, Some(20)), Some(20));
        assert_eq!(keep_selection(&v, Some(99)), Some(10));
        assert_eq!(keep_selection(&[], Some(20)), None);
    }

    #[test]
    fn the_last_choice_matches_program_and_class_then_title_then_size() {
        let last = LastSource {
            exe: "Mina.exe".into(),
            class: "UnityWndClass".into(),
            title: "Mina".into(),
        };
        let mut big = row(1, "Mina - editor", Some("mina.EXE"), "unitywndclass");
        big.width = 1920;
        big.height = 1080;
        let same_title = row(2, "Mina", Some("Mina.exe"), "UnityWndClass");
        let other_class = row(3, "Mina", Some("Mina.exe"), "Other");
        let mut tiny = row(4, "Mina", Some("Mina.exe"), "UnityWndClass");
        tiny.width = 20;
        let mut minimised = row(5, "Mina", Some("Mina.exe"), "UnityWndClass");
        minimised.minimized = true;
        let rows = vec![big.clone(), same_title.clone(), other_class, tiny, minimised];
        assert_eq!(find_last(&last, &rows).map(|r| r.hwnd), Some(2));
        assert_eq!(find_last(&last, &rows[..1]).map(|r| r.hwnd), Some(1));
        assert_eq!(find_last(&last, &rows[2..]).map(|r| r.hwnd), None);
        // No executable recorded (a protected process): the title must match too.
        let anon = LastSource {
            exe: String::new(),
            class: "C".into(),
            title: "Secret".into(),
        };
        assert!(is_last(&anon, &row(7, "Secret", None, "c")));
        assert!(!is_last(&anon, &row(8, "Other", None, "C")));
        assert!(!is_last(&anon, &row(9, "Secret", Some("x.exe"), "C")));
        assert!(!is_last(&last, &row(9, "Mina", None, "UnityWndClass")));
    }

    #[test]
    fn ask_at_start_rules() {
        let last = LastSource {
            exe: "game.exe".into(),
            class: "SDL_app".into(),
            title: "Game".into(),
        };
        let rows = vec![row(5, "Game", Some("game.exe"), "SDL_app")];
        assert_eq!(
            start_choice(true, Some(&last), &rows),
            StartChoice::Picker,
            "asking wins"
        );
        assert_eq!(start_choice(false, Some(&last), &rows), StartChoice::Attach(5));
        assert_eq!(
            start_choice(false, Some(&last), &[]),
            StartChoice::Picker,
            "not running"
        );
        assert_eq!(start_choice(false, None, &rows), StartChoice::Picker, "no last choice");
    }

    #[test]
    fn state_machine_no_source_picking_attached_lost_no_source() {
        use Phase::*;
        let mut p = NoSource;
        assert!(!captures(p) && !follows_target(p));
        p = next(p, Event::ShowPicker);
        assert_eq!(p, Picking { attached: false });
        assert_eq!(next(p, Event::Cancel), p, "nothing to go back to");
        assert_eq!(next(p, Event::AttachFailed), p, "the picker stays");
        p = next(p, Event::Attached);
        assert_eq!(p, Attached);
        assert!(captures(p) && follows_target(p));
        p = next(p, Event::SourceLost);
        assert_eq!(p, Lost);
        assert!(!captures(p) && follows_target(p));
        assert_eq!(next(p, Event::Reattached), Attached);
        p = next(p, Event::GiveUp);
        assert_eq!(p, NoSource);
        // Change source from an attached source, cancelled, then confirmed.
        let p = next(Attached, Event::ShowPicker);
        assert_eq!(p, Picking { attached: true });
        assert!(captures(p));
        assert_eq!(next(p, Event::Cancel), Attached);
        assert_eq!(next(p, Event::SourceLost), Picking { attached: false });
        assert_eq!(next(p, Event::Detach), Picking { attached: false });
        assert_eq!(next(p, Event::Attached), Attached);
        // Detach and Change source while waiting.
        assert_eq!(next(Attached, Event::Detach), NoSource);
        assert_eq!(next(Lost, Event::Detach), NoSource);
        assert_eq!(next(Lost, Event::ShowPicker), Picking { attached: false });
        // Events that do not apply change nothing.
        assert_eq!(next(NoSource, Event::GiveUp), NoSource);
        assert_eq!(next(Attached, Event::Reattached), Attached);
        assert_eq!(next(Attached, Event::Cancel), Attached);
    }

    #[test]
    fn thumbnails_one_at_a_time_after_a_dwell_never_for_own_or_minimised() {
        let own = |h: isize| h == 99;
        let mut t = ThumbPlan::default();
        assert_eq!(t.update(0.0, Some((1, false)), own), None, "dwell first");
        assert_eq!(t.update(0.1, Some((1, false)), own), None);
        assert_eq!(t.update(0.25, Some((1, false)), own), Some(1));
        assert_eq!(t.pending(), Some(1));
        // Another row while the first capture runs: nothing starts.
        assert_eq!(t.update(0.3, Some((2, false)), own), None);
        assert_eq!(t.update(0.9, Some((2, false)), own), None);
        assert!(!t.timed_out(1.0));
        assert!(t.timed_out(1.75));
        t.finished(1, 1.75, false);
        assert_eq!(
            t.update(1.8, Some((2, false)), own),
            Some(2),
            "the second after the first ended"
        );
        t.finished(2, 2.0, true);
        // The same row: again after the refresh period, or at once on a refresh request.
        assert_eq!(t.update(3.0, Some((2, false)), own), None);
        t.refresh();
        assert_eq!(t.update(3.1, Some((2, false)), own), Some(2));
        t.finished(2, 3.2, true);
        assert_eq!(t.update(7.3, Some((2, false)), own), Some(2));
        t.finished(2, 7.4, true);
        // Own windows and minimised ones: never.
        for _ in 0..3 {
            assert_eq!(t.update(20.0, Some((99, false)), own), None);
            assert_eq!(t.update(30.0, Some((5, true)), own), None);
        }
        assert_eq!(t.update(40.0, None, own), None);
        t.reset();
        assert_eq!(t, ThumbPlan::default());
    }

    #[test]
    fn thumbnails_are_box_averaged_rgba_within_the_bounds() {
        // 4 x 2 BGRA: left half blue-ish (B=200), right half red (R=100).
        let mut bgra = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                bgra.extend_from_slice(if x < 2 { &[200, 0, 0, 255] } else { &[0, 0, 100, 255] });
            }
        }
        let (w, h, px) = thumbnail(&bgra, 4, 2, 2, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, [0, 0, 200, 255, 100, 0, 0, 255]);
        // Never upscaled.
        let (w, h, _) = thumbnail(&bgra, 4, 2, 64, 64);
        assert_eq!((w, h), (4, 2));
        // A large frame fits the box.
        let big = vec![128u8; 1920 * 1080 * 4];
        let (w, h, px) = thumbnail(&big, 1920, 1080, 320, 180);
        assert_eq!((w, h), (320, 180));
        assert_eq!(px.len(), 320 * 180 * 4);
        assert_eq!(thumbnail(&[], 0, 0, 10, 10).0, 0);
    }
}
