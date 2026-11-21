//! Output window modes (T-57): the pure state machine behind F11. Fullscreen (borderless, covers one monitor) and a
//! framed, resizable window, the remembered window rectangle, and the re-placement after a monitor change. No OS
//! call: `output.rs` reads the window and the monitors and applies the [`Placement`] it gets back.

/// Smallest outer size of the framed window.
pub const MIN_OUTER: (i32, i32) = (160, 120);

/// A rectangle in virtual-screen pixels (right and bottom exclusive).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl IRect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> IRect {
        IRect {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Width, saturated to the `i32` range (a hand-edited rectangle may span more than `i32::MAX`).
    pub fn width(&self) -> i32 {
        self.right.saturating_sub(self.left)
    }

    /// Height, saturated like [`IRect::width`].
    pub fn height(&self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }

    /// Area, in `i64` (saturated for the full `i32` plane).
    pub fn area(&self) -> i64 {
        let w = i64::from(self.right) - i64::from(self.left);
        let h = i64::from(self.bottom) - i64::from(self.top);
        w.max(0).saturating_mul(h.max(0))
    }

    /// Area of the overlap with `other` (in `i64`, saturated: no overflow for any pair of rectangles).
    pub fn overlap(&self, other: &IRect) -> i64 {
        let w = i64::from(self.right.min(other.right)) - i64::from(self.left.max(other.left));
        let h = i64::from(self.bottom.min(other.bottom)) - i64::from(self.top.max(other.top));
        w.max(0).saturating_mul(h.max(0))
    }

    pub fn to_array(self) -> [i32; 4] {
        [self.left, self.top, self.right, self.bottom]
    }

    pub fn from_array(a: [i32; 4]) -> IRect {
        IRect::new(a[0], a[1], a[2], a[3])
    }

    /// A rectangle read from the settings file, `None` when it cannot be a window rectangle: empty or inverted,
    /// larger than [`MAX_EXTENT`] on a side, or with a coordinate beyond [`MAX_COORD`] (a hand-edited file).
    /// The caller falls back to the default placement.
    pub fn from_saved(a: [i32; 4]) -> Option<IRect> {
        let r = IRect::from_array(a);
        let coords_ok = a.iter().all(|v| v.unsigned_abs() <= MAX_COORD as u32);
        let size_ok = |lo: i32, hi: i32| hi > lo && i64::from(hi) - i64::from(lo) <= i64::from(MAX_EXTENT);
        (coords_ok && size_ok(r.left, r.right) && size_ok(r.top, r.bottom)).then_some(r)
    }
}

/// Largest side of a remembered window rectangle (the largest Windows desktop is 32767 px wide).
pub const MAX_EXTENT: i32 = 32_767;
/// Largest absolute coordinate of a remembered window rectangle (the virtual screen lies well within it).
pub const MAX_COORD: i32 = 100_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowMode {
    Fullscreen,
    Windowed,
}

/// Where the window goes after a mode change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    /// Borderless, topmost, covering `cover`.
    Fullscreen { cover: IRect },
    /// Framed and resizable, with this outer rectangle.
    Windowed { rect: IRect },
}

/// The fraction of `r` that lies on some monitor (0 to 1). Monitors do not overlap.
pub fn visible_fraction(r: &IRect, monitors: &[IRect]) -> f64 {
    let area = r.area();
    if area == 0 {
        return 0.0;
    }
    let visible: i64 = monitors.iter().map(|m| r.overlap(m)).sum();
    visible as f64 / area as f64
}

/// A remembered rectangle is used only when it has a usable size and at least half of it is on a monitor.
const MIN_VISIBLE: f64 = 0.5;

/// True when `r` can be used as the rectangle of the framed window: not tiny, and at least half on a monitor.
pub fn usable_rect(r: &IRect, monitors: &[IRect]) -> bool {
    r.width() >= MIN_OUTER.0 && r.height() >= MIN_OUTER.1 && visible_fraction(r, monitors) >= MIN_VISIBLE
}

/// `size` (outer), clamped to 9/10 of `monitor` and to [`MIN_OUTER`], centred on `monitor`.
pub fn center_in(size: (i32, i32), monitor: &IRect) -> IRect {
    let w = size.0.min(monitor.width() * 9 / 10).max(MIN_OUTER.0);
    let h = size.1.min(monitor.height() * 9 / 10).max(MIN_OUTER.1);
    let left = monitor.left + (monitor.width() - w) / 2;
    let top = monitor.top + (monitor.height() - h) / 2;
    IRect::new(left, top, left + w, top + h)
}

/// The rectangle of the windowed output: the remembered one when it is still usable, else `default_outer` centred
/// on `fallback` (the monitor the output was on).
pub fn restore_rect(saved: Option<IRect>, monitors: &[IRect], fallback: &IRect, default_outer: (i32, i32)) -> IRect {
    match saved {
        Some(r) if usable_rect(&r, monitors) => r,
        _ => center_in(default_outer, fallback),
    }
}

/// The monitor that holds most of `r`, the first one when none does.
fn best_monitor(r: &IRect, monitors: &[IRect]) -> Option<IRect> {
    monitors
        .iter()
        .max_by_key(|m| (r.overlap(m), std::cmp::Reverse(m.left.abs() + m.top.abs())))
        .copied()
        .or_else(|| monitors.first().copied())
}

/// The mode of the output and the rectangle to come back to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeState {
    pub mode: WindowMode,
    /// The outer rectangle of the windowed output the last time it was seen (or loaded from the settings).
    pub windowed: Option<IRect>,
}

impl ModeState {
    pub fn new(mode: WindowMode, saved: Option<IRect>) -> ModeState {
        ModeState { mode, windowed: saved }
    }

    /// Records the current outer rectangle while windowed, when it is usable (a minimised window has
    /// coordinates far off screen and keeps the previous value).
    pub fn remember(&mut self, outer: IRect, monitors: &[IRect]) {
        if self.mode == WindowMode::Windowed && usable_rect(&outer, monitors) {
            self.windowed = Some(outer);
        }
    }

    /// F11. From windowed: remembers `outer` and goes fullscreen over `monitor_of_window`, so a window moved to
    /// another monitor goes fullscreen there. From fullscreen: back to the remembered rectangle (the same
    /// monitor and size), or `default_outer` centred on `monitor_of_window` when none is usable.
    pub fn toggle(
        &mut self,
        outer: IRect,
        monitor_of_window: IRect,
        monitors: &[IRect],
        default_outer: (i32, i32),
    ) -> Placement {
        match self.mode {
            WindowMode::Windowed => {
                self.remember(outer, monitors);
                self.mode = WindowMode::Fullscreen;
                Placement::Fullscreen {
                    cover: monitor_of_window,
                }
            }
            WindowMode::Fullscreen => {
                let rect = restore_rect(self.windowed, monitors, &monitor_of_window, default_outer);
                self.windowed = Some(rect);
                self.mode = WindowMode::Windowed;
                Placement::Windowed { rect }
            }
        }
    }

    /// After a monitor change (`WM_DISPLAYCHANGE`): a fullscreen output is re-placed over the monitor that now
    /// holds most of it (the same monitor at another resolution, else the first one); a windowed one is moved
    /// onto a monitor only when less than half of it is still visible. `None` when nothing has to move.
    pub fn display_change(&self, outer: IRect, monitors: &[IRect]) -> Option<Placement> {
        let best = best_monitor(&outer, monitors)?;
        match self.mode {
            WindowMode::Fullscreen => (best != outer).then_some(Placement::Fullscreen { cover: best }),
            WindowMode::Windowed => {
                if visible_fraction(&outer, monitors) >= MIN_VISIBLE {
                    None
                } else {
                    Some(Placement::Windowed {
                        rect: center_in((outer.width(), outer.height()), &best),
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M0: IRect = IRect {
        left: 0,
        top: 0,
        right: 2560,
        bottom: 1440,
    };
    const M1: IRect = IRect {
        left: 2560,
        top: 0,
        right: 5120,
        bottom: 1440,
    };
    const DEFAULT: (i32, i32) = (1296, 759);

    fn r(l: i32, t: i32, rr: i32, b: i32) -> IRect {
        IRect::new(l, t, rr, b)
    }

    #[test]
    fn fullscreen_to_windowed_to_fullscreen_remembers_the_rectangle() {
        let mons = [M0, M1];
        let mut s = ModeState::new(WindowMode::Fullscreen, None);
        // First F11 from fullscreen, nothing remembered: the default size centred on the monitor it covered.
        let p = s.toggle(M1, M1, &mons, DEFAULT);
        let Placement::Windowed { rect } = p else {
            panic!("{p:?}")
        };
        assert_eq!((rect.width(), rect.height()), DEFAULT);
        assert_eq!(
            rect.left + rect.width() / 2,
            M1.left + M1.width() / 2,
            "centred on monitor 1"
        );
        assert_eq!(s.mode, WindowMode::Windowed);
        // The user moves and resizes it, then F11 to fullscreen: the covered monitor is the one it is on.
        let moved = r(100, 80, 1400, 880);
        let p = s.toggle(moved, M0, &mons, DEFAULT);
        assert_eq!(p, Placement::Fullscreen { cover: M0 });
        assert_eq!(s.mode, WindowMode::Fullscreen);
        assert_eq!(s.windowed, Some(moved));
        // F11 again: exactly the rectangle it had.
        assert_eq!(s.toggle(M0, M0, &mons, DEFAULT), Placement::Windowed { rect: moved });
    }

    #[test]
    fn the_windowed_output_comes_back_on_its_own_monitor_not_the_fullscreen_one() {
        let mons = [M0, M1];
        let mut s = ModeState::new(WindowMode::Windowed, None);
        let on_m1 = r(3000, 200, 4200, 900);
        assert_eq!(s.toggle(on_m1, M1, &mons, DEFAULT), Placement::Fullscreen { cover: M1 });
        // Whatever monitor is reported for the fullscreen window, the remembered rectangle wins.
        assert_eq!(s.toggle(M1, M1, &mons, DEFAULT), Placement::Windowed { rect: on_m1 });
    }

    #[test]
    fn a_remembered_rectangle_on_a_missing_monitor_is_not_used() {
        let mons = [M0]; // monitor 1 was unplugged
        let mut s = ModeState::new(WindowMode::Fullscreen, Some(r(3000, 200, 4200, 900)));
        let Placement::Windowed { rect } = s.toggle(M0, M0, &mons, DEFAULT) else {
            panic!()
        };
        assert_eq!((rect.width(), rect.height()), DEFAULT);
        assert!(visible_fraction(&rect, &mons) >= 0.99);
    }

    #[test]
    fn a_saved_rectangle_that_is_tiny_or_mostly_off_screen_is_ignored() {
        let mons = [M0];
        // Minimised windows report coordinates like -32000.
        assert_eq!(
            restore_rect(Some(r(-32000, -32000, -31840, -31880)), &mons, &M0, DEFAULT).width(),
            DEFAULT.0
        );
        // Too small.
        assert_eq!(
            restore_rect(Some(r(10, 10, 50, 40)), &mons, &M0, DEFAULT).width(),
            DEFAULT.0
        );
        // Mostly off screen (a quarter visible).
        assert_eq!(
            restore_rect(Some(r(2300, 100, 3320, 900)), &mons, &M0, DEFAULT).width(),
            DEFAULT.0
        );
        // Partly off screen but more than half visible is kept.
        let keep = r(1800, 100, 2900, 900);
        assert_eq!(restore_rect(Some(keep), &mons, &M0, DEFAULT), keep);
    }

    #[test]
    fn a_window_spanning_two_monitors_counts_as_visible() {
        let mons = [M0, M1];
        let span = r(2000, 100, 3200, 900);
        assert!((visible_fraction(&span, &mons) - 1.0).abs() < 1e-9);
        assert_eq!(restore_rect(Some(span), &mons, &M0, DEFAULT), span);
    }

    #[test]
    fn the_default_size_is_clamped_to_the_monitor() {
        let small = r(0, 0, 800, 600);
        let c = center_in((1296, 759), &small);
        assert_eq!((c.width(), c.height()), (720, 540));
        assert_eq!((c.left, c.top), (40, 30));
        // Never below the minimum.
        let tiny = r(0, 0, 100, 100);
        let c = center_in((1296, 759), &tiny);
        assert_eq!((c.width(), c.height()), MIN_OUTER);
    }

    #[test]
    fn remember_follows_the_window_only_while_windowed_and_only_usable_rectangles() {
        let mons = [M0];
        let mut s = ModeState::new(WindowMode::Windowed, None);
        let a = r(50, 50, 1350, 800);
        s.remember(a, &mons);
        assert_eq!(s.windowed, Some(a));
        s.remember(r(-32000, -32000, -31000, -31200), &mons); // minimised
        assert_eq!(s.windowed, Some(a));
        s.mode = WindowMode::Fullscreen;
        s.remember(M0, &mons); // the fullscreen rectangle is not a window rectangle
        assert_eq!(s.windowed, Some(a));
    }

    /// R-42: hand-edited rectangles never overflow (no panic in a debug build) and are not usable.
    #[test]
    fn extreme_rectangles_do_not_overflow() {
        let mons = [M0, M1];
        let huge = IRect::from_array([i32::MIN, 0, i32::MAX, 100]);
        assert_eq!(huge.width(), i32::MAX, "saturated");
        assert_eq!(huge.area(), (i64::from(i32::MAX) - i64::from(i32::MIN)) * 100);
        assert!(!usable_rect(&huge, &mons));
        let all = IRect::from_array([i32::MIN, i32::MIN, i32::MAX, i32::MAX]);
        assert!(visible_fraction(&all, &mons) < 1e-6);
        assert!(!usable_rect(&all, &mons));
        let inverted = IRect::from_array([i32::MAX, i32::MAX, i32::MIN, i32::MIN]);
        assert_eq!(inverted.width(), i32::MIN, "saturated, negative");
        assert_eq!((inverted.area(), inverted.overlap(&M0)), (0, 0));
        assert!(!usable_rect(&inverted, &mons));
        assert_eq!(restore_rect(Some(huge), &mons, &M0, DEFAULT).width(), DEFAULT.0);
        let moved = ModeState::new(WindowMode::Windowed, None).display_change(all, &mons);
        assert!(matches!(moved, Some(Placement::Windowed { rect }) if usable_rect(&rect, &mons)));
    }

    /// R-42: absurd remembered rectangles are rejected at load (the default placement is used instead).
    #[test]
    fn saved_rectangles_are_sanitised() {
        assert_eq!(IRect::from_saved([100, 50, 1380, 770]), Some(r(100, 50, 1380, 770)));
        assert_eq!(
            IRect::from_saved([-2000, -100, -500, 900]),
            Some(r(-2000, -100, -500, 900))
        );
        for bad in [
            [i32::MIN, 0, i32::MAX, 100],
            [0, 0, 0, 100],
            [10, 10, 5, 100],
            [0, 0, 40_000, 100],
            [200_000, 0, 201_000, 100],
            [0, i32::MIN, 100, i32::MIN + 50],
        ] {
            assert_eq!(IRect::from_saved(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn display_change_keeps_a_fullscreen_output_on_its_monitor_at_the_new_resolution() {
        let s = ModeState::new(WindowMode::Fullscreen, None);
        // Same layout: nothing to do.
        assert_eq!(s.display_change(M1, &[M0, M1]), None);
        // Monitor 1 switched to 1920x1080: cover the monitor that holds most of the old rectangle.
        let m1_new = r(2560, 0, 4480, 1080);
        assert_eq!(
            s.display_change(M1, &[M0, m1_new]),
            Some(Placement::Fullscreen { cover: m1_new })
        );
        // Monitor 1 unplugged: the output falls back to the remaining one.
        assert_eq!(s.display_change(M1, &[M0]), Some(Placement::Fullscreen { cover: M0 }));
        // No monitor information: no move.
        assert_eq!(s.display_change(M1, &[]), None);
    }

    #[test]
    fn display_change_moves_a_stranded_window_but_leaves_a_visible_one() {
        let s = ModeState::new(WindowMode::Windowed, None);
        let on_m1 = r(3000, 200, 4200, 900);
        assert_eq!(s.display_change(on_m1, &[M0, M1]), None);
        // Monitor 1 unplugged: the window is centred on monitor 0, same size.
        let Some(Placement::Windowed { rect }) = s.display_change(on_m1, &[M0]) else {
            panic!()
        };
        assert_eq!((rect.width(), rect.height()), (1200, 700));
        assert!(visible_fraction(&rect, &[M0]) >= 0.99);
    }

    #[test]
    fn rect_arrays_round_trip() {
        let a = r(-5, 6, 700, 800);
        assert_eq!(IRect::from_array(a.to_array()), a);
    }
}
