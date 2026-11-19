//! Pure layout: where the native image lands inside the window client area, and which rectangles
//! are left for the bars (letterbox, pillarbox). No Win32 here, everything is unit tested.
//!
//! Conventions
//! - A native image is `nw x nh` pixels. A pixel aspect ratio `n:m` (width : height of one pixel)
//!   makes the displayed width `nw * n / m` (8:7 stretches a 256 wide NES image to 292.57 units).
//! - Integer modes scale the height by N and the width by `N * n / m`, floored, so a PAR other
//!   than 1:1 gives pixel columns of two widths (nearest neighbour). `Native` ignores the PAR (it
//!   is the true 1:1 pixel display).
//! - `Fit` picks `N = floor(min(cw * m / (nw * n), ch / nh))` clipped to 1..=10, `Integer(n)` uses
//!   N as given (the destination may be larger than the client, it is then clipped by the window).
//! - `FitFree` fills the client in the limiting direction and keeps the aspect, sizes floored.
//! - `Stretch` fills the whole client, aspect (and PAR) not kept, no bars.
//! - The destination is centred (`(client - size) / 2`, floored, also when negative) or put at the
//!   top-left corner. The bars are the client area minus the destination.

/// Largest integer scale of the automatic and manual modes.
pub const MAX_SCALE: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    #[cfg(test)]
    pub fn area(&self) -> i64 {
        if self.is_empty() {
            0
        } else {
            self.w as i64 * self.h as i64
        }
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0), (b - y).max(0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleMode {
    /// True 1:1 pixel display, PAR ignored.
    Native,
    /// Integer upscale x1..x10, nearest.
    Integer(u32),
    /// Largest integer scale that fits the client.
    Fit,
    /// Non-integer fit to the client, aspect preserved.
    FitFree,
    /// Fills the whole client, aspect not preserved.
    Stretch,
}

impl ScaleMode {
    /// True for the non-integer modes, where a smoothing filter applies.
    pub fn is_free(self) -> bool {
        matches!(self, ScaleMode::FitFree | ScaleMode::Stretch)
    }

    /// The M key order: native, integer (at `last`), fit, fit-free, stretch, native.
    pub fn next(self, last: u32) -> ScaleMode {
        match self {
            ScaleMode::Native => ScaleMode::Integer(last.clamp(1, MAX_SCALE)),
            ScaleMode::Integer(_) => ScaleMode::Fit,
            ScaleMode::Fit => ScaleMode::FitFree,
            ScaleMode::FitFree => ScaleMode::Stretch,
            ScaleMode::Stretch => ScaleMode::Native,
        }
    }

    /// Name in the `--present` vocabulary (`1to1`, `integer`, `autofit`, `fit`, `stretch`).
    pub fn present_name(self) -> &'static str {
        match self {
            ScaleMode::Native => "1to1",
            ScaleMode::Integer(_) => "integer",
            ScaleMode::Fit => "autofit",
            ScaleMode::FitFree => "fit",
            ScaleMode::Stretch => "stretch",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Center,
    TopLeft,
}

/// Pixel aspect ratio, width : height of one pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Par {
    pub n: u32,
    pub m: u32,
}

impl Par {
    pub const SQUARE: Par = Par { n: 1, m: 1 };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Destination rectangle of the native image, in client coordinates (may exceed the client
    /// for a manual integer scale that is too large).
    pub dest: Rect,
    /// Integer scale used, `None` for `FitFree`.
    pub scale: Option<u32>,
    /// Rectangles of the client area outside the destination (top, bottom, left, right order).
    pub bars: Vec<Rect>,
}

/// Computes the layout for a client area of `cw x ch` pixels.
pub fn layout(cw: u32, ch: u32, nw: u32, nh: u32, par: Par, mode: ScaleMode, align: Align) -> Layout {
    let (cw, ch, nw, nh) = (cw as u64, ch as u64, nw as u64, nh as u64);
    let (pn, pm) = (par.n.max(1) as u64, par.m.max(1) as u64);
    if cw == 0 || ch == 0 || nw == 0 || nh == 0 {
        return Layout {
            dest: Rect::new(0, 0, 0, 0),
            scale: None,
            bars: Vec::new(),
        };
    }
    let (dw, dh, scale) = match mode {
        ScaleMode::Native => (nw, nh, Some(1)),
        ScaleMode::Integer(n) => {
            let n = n.clamp(1, MAX_SCALE) as u64;
            (nw * n * pn / pm, nh * n, Some(n as u32))
        }
        ScaleMode::Fit => {
            let by_w = cw * pm / (nw * pn);
            let by_h = ch / nh;
            let n = by_w.min(by_h).clamp(1, MAX_SCALE as u64);
            (nw * n * pn / pm, nh * n, Some(n as u32))
        }
        ScaleMode::FitFree => {
            // Width limited when cw / (nw*pn/pm) <= ch / nh, that is cw * nh * pm <= ch * nw * pn.
            if cw * nh * pm <= ch * nw * pn {
                (cw, (cw * nh * pm / (nw * pn)).max(1), None)
            } else {
                ((ch * nw * pn / (pm * nh)).max(1), ch, None)
            }
        }
        ScaleMode::Stretch => (cw, ch, None),
    };
    let (dw, dh) = (dw as i64, dh as i64);
    let (x, y) = match align {
        Align::Center => ((cw as i64 - dw).div_euclid(2), (ch as i64 - dh).div_euclid(2)),
        Align::TopLeft => (0, 0),
    };
    let dest = Rect::new(x as i32, y as i32, dw as i32, dh as i32);
    let client = Rect::new(0, 0, cw as i32, ch as i32);
    Layout {
        dest,
        scale,
        bars: bars(client, dest),
    }
}

/// The client area minus `dest` (clipped to the client), as up to four rectangles.
pub fn bars(client: Rect, dest: Rect) -> Vec<Rect> {
    let d = dest.intersect(&client);
    if d.is_empty() {
        return vec![client];
    }
    [
        Rect::new(client.x, client.y, client.w, d.y - client.y),
        Rect::new(client.x, d.bottom(), client.w, client.bottom() - d.bottom()),
        Rect::new(client.x, d.y, d.x - client.x, d.h),
        Rect::new(d.right(), d.y, client.right() - d.right(), d.h),
    ]
    .into_iter()
    .filter(|r| !r.is_empty())
    .collect()
}

/// Client size that shows `nw x nh` at the integer scale `n` with the PAR exactly (no bars).
pub fn client_for_scale(nw: u32, nh: u32, par: Par, n: u32) -> (u32, u32) {
    let n = n.clamp(1, MAX_SCALE) as u64;
    let (pn, pm) = (par.n.max(1) as u64, par.m.max(1) as u64);
    ((nw as u64 * n * pn / pm).max(1) as u32, (nh as u64 * n) as u32)
}

/// Maps a native pixel centre to client coordinates for a destination rectangle.
pub fn native_to_client(dest: Rect, nw: u32, nh: u32, x: u32, y: u32) -> (i32, i32) {
    let fx = dest.x as f64 + (x as f64 + 0.5) * dest.w as f64 / nw as f64;
    let fy = dest.y as f64 + (y as f64 + 0.5) * dest.h as f64 / nh as f64;
    (fx.floor() as i32, fy.floor() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQ: Par = Par::SQUARE;

    fn l(cw: u32, ch: u32, nw: u32, nh: u32, par: Par, mode: ScaleMode, a: Align) -> Layout {
        layout(cw, ch, nw, nh, par, mode, a)
    }

    fn covered(lay: &Layout, cw: i32, ch: i32) -> i64 {
        let client = Rect::new(0, 0, cw, ch);
        let d = lay.dest.intersect(&client).area();
        d + lay.bars.iter().map(Rect::area).sum::<i64>()
    }

    #[test]
    fn native_is_one_to_one_centred() {
        // 1920x1080 client, 256x240: x = (1920-256)/2 = 832, y = (1080-240)/2 = 420.
        let r = l(1920, 1080, 256, 240, SQ, ScaleMode::Native, Align::Center);
        assert_eq!(r.dest, Rect::new(832, 420, 256, 240));
        assert_eq!(r.scale, Some(1));
        // Bars: top 1920x420, bottom 1920x420, left 832x240, right 832x240.
        assert_eq!(
            r.bars,
            vec![
                Rect::new(0, 0, 1920, 420),
                Rect::new(0, 660, 1920, 420),
                Rect::new(0, 420, 832, 240),
                Rect::new(1088, 420, 832, 240),
            ]
        );
    }

    #[test]
    fn native_ignores_par() {
        let r = l(
            1000,
            500,
            256,
            240,
            Par { n: 8, m: 7 },
            ScaleMode::Native,
            Align::TopLeft,
        );
        assert_eq!(r.dest, Rect::new(0, 0, 256, 240));
    }

    #[test]
    fn integer_exact_client_has_no_bars() {
        let r = l(768, 720, 256, 240, SQ, ScaleMode::Integer(3), Align::Center);
        assert_eq!(r.dest, Rect::new(0, 0, 768, 720));
        assert!(r.bars.is_empty());
    }

    #[test]
    fn integer_manual_centred_floor() {
        // 1001x800 client, 320x240 x2 = 640x480: x = (1001-640)/2 = 180 (floor of 180.5), y = 160.
        let r = l(1001, 800, 320, 240, SQ, ScaleMode::Integer(2), Align::Center);
        assert_eq!(r.dest, Rect::new(180, 160, 640, 480));
        assert_eq!(covered(&r, 1001, 800), 1001 * 800);
    }

    #[test]
    fn integer_manual_larger_than_client_is_negative_offset() {
        // 640x480 x2 = 1280x960 in a 1000x900 client: x = (1000-1280)/2 = -140, y = -30.
        let r = l(1000, 900, 640, 480, SQ, ScaleMode::Integer(2), Align::Center);
        assert_eq!(r.dest, Rect::new(-140, -30, 1280, 960));
        assert!(r.bars.is_empty());
        // Odd overflow floors: 1001 - 1280 = -279, floor(-139.5) = -140.
        let r = l(1001, 900, 640, 480, SQ, ScaleMode::Integer(2), Align::Center);
        assert_eq!(r.dest.x, -140);
    }

    #[test]
    fn integer_scale_is_clipped_to_1_10() {
        let r = l(100, 100, 16, 16, SQ, ScaleMode::Integer(0), Align::TopLeft);
        assert_eq!(r.scale, Some(1));
        let r = l(100, 100, 16, 16, SQ, ScaleMode::Integer(11), Align::TopLeft);
        assert_eq!(r.scale, Some(10));
        assert_eq!(r.dest, Rect::new(0, 0, 160, 160));
    }

    #[test]
    fn fit_picks_floor_of_min_ratio() {
        // 1920x1080, 256x240: min(7.5, 4.5) -> 4 -> 1024x960, x = 448, y = 60.
        let r = l(1920, 1080, 256, 240, SQ, ScaleMode::Fit, Align::Center);
        assert_eq!(r.scale, Some(4));
        assert_eq!(r.dest, Rect::new(448, 60, 1024, 960));
        // 2560x1440, 320x240: min(8, 6) -> 6 -> 1920x1440, x = 320, y = 0.
        let r = l(2560, 1440, 320, 240, SQ, ScaleMode::Fit, Align::Center);
        assert_eq!(r.scale, Some(6));
        assert_eq!(r.dest, Rect::new(320, 0, 1920, 1440));
        // 1280x720 native in 1920x1080: 1.5 -> 1.
        let r = l(1920, 1080, 1280, 720, SQ, ScaleMode::Fit, Align::Center);
        assert_eq!(r.scale, Some(1));
        assert_eq!(r.dest, Rect::new(320, 180, 1280, 720));
        // 640x480 in 1280x960 -> exactly 2 (ratio 2.0, floor stays 2).
        let r = l(1280, 960, 640, 480, SQ, ScaleMode::Fit, Align::Center);
        assert_eq!(r.scale, Some(2));
        assert!(r.bars.is_empty());
    }

    #[test]
    fn fit_clips_to_ten_and_never_below_one() {
        // 3840x2160 with 16x16: min(240, 135) -> 10.
        let r = l(3840, 2160, 16, 16, SQ, ScaleMode::Fit, Align::Center);
        assert_eq!(r.scale, Some(10));
        assert_eq!(r.dest, Rect::new(1840, 1000, 160, 160));
        // 1920x1080 image in a 800x600 client: ratio < 1, scale 1, dest larger than the client.
        let r = l(800, 600, 1920, 1080, SQ, ScaleMode::Fit, Align::Center);
        assert_eq!(r.scale, Some(1));
        assert_eq!(r.dest, Rect::new(-560, -240, 1920, 1080));
    }

    #[test]
    fn fit_with_par() {
        let par = Par { n: 8, m: 7 };
        // 1000x800 client, 256x240 8:7: by_w = 1000*7/(256*8) = 3.41 -> 3, by_h = 3 -> N = 3.
        // dest w = floor(256*3*8/7) = floor(877.71) = 877, h = 720; x = (1000-877)/2 = 61, y = 40.
        let r = l(1000, 800, 256, 240, par, ScaleMode::Fit, Align::Center);
        assert_eq!(r.scale, Some(3));
        assert_eq!(r.dest, Rect::new(61, 40, 877, 720));
        // Narrow client: 700 wide: by_w = 700*7/2048 = 2.39 -> 2: w = floor(256*2*8/7) = 585, h = 480.
        let r = l(700, 800, 256, 240, par, ScaleMode::Fit, Align::Center);
        assert_eq!(r.scale, Some(2));
        assert_eq!(r.dest, Rect::new(57, 160, 585, 480));
    }

    #[test]
    fn integer_with_par_floors_width() {
        // 2:1 pixels: 100x50 at x3 -> 600x150.
        let r = l(
            600,
            150,
            100,
            50,
            Par { n: 2, m: 1 },
            ScaleMode::Integer(3),
            Align::Center,
        );
        assert_eq!(r.dest, Rect::new(0, 0, 600, 150));
        // 4:3 on 3 px: 3*4/3 = 4 exactly.
        assert_eq!(client_for_scale(3, 3, Par { n: 4, m: 3 }, 1), (4, 3));
    }

    #[test]
    fn fit_free_preserves_aspect_height_limited() {
        // 1920x1080 client, 256x240: height limited: h = 1080, w = floor(1080*256/240) = 1152.
        let r = l(1920, 1080, 256, 240, SQ, ScaleMode::FitFree, Align::Center);
        assert_eq!(r.scale, None);
        assert_eq!(r.dest, Rect::new(384, 0, 1152, 1080));
        assert_eq!(covered(&r, 1920, 1080), 1920 * 1080);
    }

    #[test]
    fn fit_free_width_limited() {
        // 1000x1000 client, 320x240: width limited: w = 1000, h = floor(1000*240/320) = 750, y = 125.
        let r = l(1000, 1000, 320, 240, SQ, ScaleMode::FitFree, Align::Center);
        assert_eq!(r.dest, Rect::new(0, 125, 1000, 750));
    }

    #[test]
    fn fit_free_with_par_and_upscale_of_small_image() {
        // 1000x1000 client, 256x240 at 8:7: displayed ratio 292.57:240, width limited:
        // h = floor(1000*240*7/(256*8)) = floor(820.31) = 820, y = 90.
        let r = l(
            1000,
            1000,
            256,
            240,
            Par { n: 8, m: 7 },
            ScaleMode::FitFree,
            Align::Center,
        );
        assert_eq!(r.dest, Rect::new(0, 90, 1000, 820));
    }

    #[test]
    fn fit_free_exact_ratio_is_full_client() {
        let r = l(1280, 960, 640, 480, SQ, ScaleMode::FitFree, Align::Center);
        assert_eq!(r.dest, Rect::new(0, 0, 1280, 960));
        assert!(r.bars.is_empty());
    }

    #[test]
    fn top_left_alignment() {
        let r = l(1920, 1080, 256, 240, SQ, ScaleMode::Integer(2), Align::TopLeft);
        assert_eq!(r.dest, Rect::new(0, 0, 512, 480));
        // Two bars: below (1920x600) and right (1408x480).
        assert_eq!(r.bars, vec![Rect::new(0, 480, 1920, 600), Rect::new(512, 0, 1408, 480)]);
    }

    #[test]
    fn bars_cover_the_rest_for_a_matrix() {
        let clients = [
            (1920, 1080),
            (1366, 768),
            (800, 600),
            (2560, 1440),
            (333, 777),
            (17, 19),
        ];
        let natives = [(256, 240), (320, 240), (640, 480), (1280, 720), (1920, 1080), (16, 16)];
        let pars = [SQ, Par { n: 8, m: 7 }, Par { n: 4, m: 3 }];
        let modes = [
            ScaleMode::Native,
            ScaleMode::Integer(1),
            ScaleMode::Integer(4),
            ScaleMode::Fit,
            ScaleMode::FitFree,
            ScaleMode::Stretch,
        ];
        for &(cw, ch) in &clients {
            for &(nw, nh) in &natives {
                for &par in &pars {
                    for &mode in &modes {
                        for align in [Align::Center, Align::TopLeft] {
                            let r = layout(cw, ch, nw, nh, par, mode, align);
                            let client = Rect::new(0, 0, cw as i32, ch as i32);
                            // dest plus bars tile the client exactly.
                            assert_eq!(
                                covered(&r, cw as i32, ch as i32),
                                client.area(),
                                "{cw}x{ch} {nw}x{nh} {mode:?}"
                            );
                            // Bars never overlap the destination.
                            for b in &r.bars {
                                assert!(b.intersect(&r.dest).is_empty());
                                assert!(b.intersect(&client) == *b);
                            }
                            if let ScaleMode::Fit | ScaleMode::FitFree | ScaleMode::Stretch = mode {
                                if r.scale.is_none_or(|s| s > 1) {
                                    assert!(r.dest.w <= cw as i32 && r.dest.h <= ch as i32);
                                }
                            }
                            if let ScaleMode::FitFree = mode {
                                // Aspect preserved within one pixel of flooring.
                                let want = nw as f64 * par.n as f64 / par.m as f64 / nh as f64;
                                let got = r.dest.w as f64 / r.dest.h as f64;
                                assert!((want - got).abs() <= (want + 1.0) / r.dest.h.min(r.dest.w) as f64 + 1e-9);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn stretch_fills_the_client_without_bars() {
        for &(cw, ch) in &[(1920u32, 1080u32), (333, 777), (17, 19), (256, 240)] {
            for align in [Align::Center, Align::TopLeft] {
                let r = l(cw, ch, 256, 240, Par { n: 8, m: 7 }, ScaleMode::Stretch, align);
                assert_eq!(r.dest, Rect::new(0, 0, cw as i32, ch as i32));
                assert_eq!(r.scale, None);
                assert!(r.bars.is_empty());
            }
        }
        // Native larger than the client: still exactly the client (a downscale).
        let r = l(800, 600, 1920, 1080, SQ, ScaleMode::Stretch, Align::Center);
        assert_eq!(r.dest, Rect::new(0, 0, 800, 600));
    }

    #[test]
    fn mode_cycle_visits_every_mode_and_returns() {
        let mut m = ScaleMode::Native;
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(m.present_name());
            m = m.next(4);
        }
        assert_eq!(m, ScaleMode::Native);
        assert_eq!(seen, ["1to1", "integer", "autofit", "fit", "stretch"]);
        assert_eq!(ScaleMode::Native.next(4), ScaleMode::Integer(4));
        assert_eq!(ScaleMode::Native.next(0), ScaleMode::Integer(1));
        assert_eq!(ScaleMode::Native.next(99), ScaleMode::Integer(MAX_SCALE));
    }

    #[test]
    fn is_free_marks_the_non_integer_modes() {
        assert!(ScaleMode::FitFree.is_free() && ScaleMode::Stretch.is_free());
        assert!(!ScaleMode::Native.is_free() && !ScaleMode::Fit.is_free() && !ScaleMode::Integer(2).is_free());
    }

    #[test]
    fn integer_every_scale_1_to_10() {
        for n in 1..=MAX_SCALE {
            // Exact client: no bars, origin.
            let (cw, ch) = client_for_scale(256, 240, SQ, n);
            assert_eq!((cw, ch), (256 * n, 240 * n));
            let r = l(cw, ch, 256, 240, SQ, ScaleMode::Integer(n), Align::Center);
            assert_eq!(r.dest, Rect::new(0, 0, cw as i32, ch as i32), "x{n}");
            assert_eq!(r.scale, Some(n));
            assert!(r.bars.is_empty());
            // A 2560x1440 host: centred, floor of half the remainder.
            let r = l(2560, 1440, 320, 200, SQ, ScaleMode::Integer(n), Align::Center);
            let (dw, dh) = (320 * n as i32, 200 * n as i32);
            assert_eq!(
                r.dest,
                Rect::new((2560 - dw).div_euclid(2), (1440 - dh).div_euclid(2), dw, dh)
            );
            assert_eq!(covered(&r, 2560, 1440), 2560 * 1440);
        }
    }

    #[test]
    fn fit_reaches_every_scale_1_to_10() {
        // A client exactly k times the native (plus a margin smaller than one native size) gives k.
        for k in 1..=MAX_SCALE {
            let (cw, ch) = (160 * k + 159, 144 * k + 143);
            let r = l(cw, ch, 160, 144, SQ, ScaleMode::Fit, Align::Center);
            assert_eq!(r.scale, Some(k), "client {cw}x{ch}");
            assert!(r.dest.w <= cw as i32 && r.dest.h <= ch as i32);
        }
    }

    #[test]
    fn odd_sizes_centre_with_floor() {
        // Odd native and odd client: 257x241 x1 in 1000x777: x = floor(743/2) = 371, y = floor(536/2) = 268.
        let r = l(1000, 777, 257, 241, SQ, ScaleMode::Native, Align::Center);
        assert_eq!(r.dest, Rect::new(371, 268, 257, 241));
        // The right bar gets the extra column: 1000 - 371 - 257 = 372.
        assert_eq!(r.bars[3], Rect::new(628, 268, 372, 241));
        assert_eq!(covered(&r, 1000, 777), 1000 * 777);
        // Odd fit: 255x239 in 801x601: min(3.14, 2.51) -> 2: 510x478 at (145, 61).
        let r = l(801, 601, 255, 239, SQ, ScaleMode::Fit, Align::Center);
        assert_eq!(r.dest, Rect::new(145, 61, 510, 478));
        // Fit-free with a 1 pixel remainder: 3x2 in 10x5: height limited, w = floor(5*3/2) = 7, x = 1.
        let r = l(10, 5, 3, 2, SQ, ScaleMode::FitFree, Align::Center);
        assert_eq!(r.dest, Rect::new(1, 0, 7, 5));
    }

    #[test]
    fn native_larger_than_client_clamps_fit_to_one() {
        // 1920x1080 native in a 1280x720 client: fit stays x1 (never below), centred, clipped by the
        // window; fit-free downscales instead.
        let r = l(1280, 720, 1920, 1080, SQ, ScaleMode::Fit, Align::Center);
        assert_eq!(r.scale, Some(1));
        assert_eq!(r.dest, Rect::new(-320, -180, 1920, 1080));
        assert!(r.bars.is_empty());
        let r = l(1280, 720, 1920, 1080, SQ, ScaleMode::FitFree, Align::Center);
        assert_eq!(r.dest, Rect::new(0, 0, 1280, 720));
        // Only one axis too large: 640x480 in 1000x400: by_h = 0 -> 1; vertical overflow, side bars.
        let r = l(1000, 400, 640, 480, SQ, ScaleMode::Fit, Align::Center);
        assert_eq!(r.dest, Rect::new(180, -40, 640, 480));
        assert_eq!(r.bars, vec![Rect::new(0, 0, 180, 400), Rect::new(820, 0, 180, 400)]);
    }

    #[test]
    fn empty_client_gives_empty_layout() {
        let r = l(0, 600, 256, 240, SQ, ScaleMode::Fit, Align::Center);
        assert!(r.dest.is_empty());
        assert!(r.bars.is_empty());
    }

    #[test]
    fn dest_larger_than_client_has_no_bars_when_covering() {
        let b = bars(Rect::new(0, 0, 100, 100), Rect::new(-10, -10, 200, 200));
        assert!(b.is_empty());
        // Fully outside: the whole client is a bar.
        let b = bars(Rect::new(0, 0, 100, 100), Rect::new(200, 200, 10, 10));
        assert_eq!(b, vec![Rect::new(0, 0, 100, 100)]);
    }

    #[test]
    fn client_for_scale_matches_integer_layout() {
        assert_eq!(client_for_scale(256, 240, SQ, 3), (768, 720));
        assert_eq!(client_for_scale(256, 240, Par { n: 8, m: 7 }, 3), (877, 720));
    }

    #[test]
    fn native_to_client_maps_pixel_centres() {
        let dest = Rect::new(100, 50, 768, 720);
        // Native pixel (0,0) centre is at 100 + 0.5*3 = 101.5 -> 101, 50 + 1.5 -> 51.
        assert_eq!(native_to_client(dest, 256, 240, 0, 0), (101, 51));
        // Pixel (255, 239): 100 + 255.5*3 = 866.5 -> 866; 50 + 239.5*3 = 768.5 -> 768.
        assert_eq!(native_to_client(dest, 256, 240, 255, 239), (866, 768));
    }
}
