//! Pure test-pattern generation (UX-3). Every pattern is rendered as a BGRA frame of the same
//! layout the capture path produces, with sRGB-coded 8-bit values (grey = equal B, G and R).

use std::str::FromStr;

use crate::Res;

pub const DEFAULT_SIZE: (u32, u32) = (320, 240);

/// Near-black PLUGE steps (8-bit sRGB codes), left to right.
pub const PLUGE_LEVELS: [u8; 5] = [0, 3, 6, 10, 16];

#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    /// Whole frame at one level.
    Flat(u8),
    /// Horizontal grey ramp, 0 on the left column to 255 on the right column.
    Ramp,
    /// White 1-px vertical line every N pixels (on columns x % N == 0) on black.
    VLine(u32),
    /// White 1-px horizontal line every N pixels (on rows y % N == 0) on black.
    HLine(u32),
    /// Vertical grating of period N: N/2 white columns then black ones (N >= 2).
    Grating(u32),
    /// Checkerboard of N-px squares, white on the top-left square.
    Checker(u32),
    /// White 1-px lines every N pixels in both directions (convergence grid).
    Crosshatch(u32),
    /// One white pixel at the centre (pixel index w/2, h/2).
    Point,
    /// Centred white window covering `apl` percent of the frame area on black.
    Window(u32),
    /// White vertical bar of the given width moving `speed` px per frame, wrapping.
    Bar { width: u32, speed: u32 },
    /// Five vertical bands of near-black steps ([`PLUGE_LEVELS`]).
    Pluge,
    /// Vertical lines every N pixels overlaid with horizontal lines every N+1 pixels.
    Moire(u32),
}

fn arg(args: &[&str], i: usize, default: u32, what: &str) -> Res<u32> {
    match args.get(i).filter(|s| !s.is_empty()) {
        None => Ok(default),
        Some(s) => s
            .parse()
            .map_err(|_| format!("pattern {what}: bad number \"{s}\"").into()),
    }
}

fn positive(v: u32, what: &str) -> Res<u32> {
    if v == 0 {
        Err(format!("pattern {what} must be at least 1").into())
    } else {
        Ok(v)
    }
}

impl FromStr for Pattern {
    type Err = Box<dyn std::error::Error>;

    /// Parses `name[:arg[:arg]]`, for example `ramp`, `vline:4`, `window:25`, `bar:8:4`.
    fn from_str(text: &str) -> Res<Self> {
        let mut parts = text.split(':');
        let name = parts.next().unwrap_or("").to_ascii_lowercase();
        let args: Vec<&str> = parts.collect();
        Ok(match name.as_str() {
            "flat" => {
                let v = arg(&args, 0, 255, "flat level")?;
                if v > 255 {
                    return Err("pattern flat level must be 0..255".into());
                }
                Pattern::Flat(v as u8)
            }
            "ramp" => Pattern::Ramp,
            "vline" => Pattern::VLine(positive(arg(&args, 0, 8, "vline period")?, "vline period")?),
            "hline" => Pattern::HLine(positive(arg(&args, 0, 8, "hline period")?, "hline period")?),
            "grating" => {
                let n = arg(&args, 0, 2, "grating period")?;
                if n < 2 {
                    return Err("pattern grating period must be at least 2".into());
                }
                Pattern::Grating(n)
            }
            "checker" => Pattern::Checker(positive(arg(&args, 0, 1, "checker size")?, "checker size")?),
            "crosshatch" => Pattern::Crosshatch(positive(arg(&args, 0, 16, "crosshatch period")?, "crosshatch period")?),
            "point" => Pattern::Point,
            "window" => {
                let apl = arg(&args, 0, 25, "window APL")?;
                if apl == 0 || apl > 100 {
                    return Err("pattern window APL must be 1..100".into());
                }
                Pattern::Window(apl)
            }
            "bar" => Pattern::Bar {
                width: positive(arg(&args, 0, 8, "bar width")?, "bar width")?,
                speed: arg(&args, 1, 4, "bar speed")?,
            },
            "pluge" => Pattern::Pluge,
            "moire" => Pattern::Moire(positive(arg(&args, 0, 2, "moire period")?, "moire period")?),
            "" => return Err("empty pattern name".into()),
            other => {
                return Err(format!(
                    "unknown pattern \"{other}\" (flat, ramp, vline, hline, grating, checker, crosshatch, point, window, bar, pluge, moire)"
                )
                .into())
            }
        })
    }
}

/// Parses `WxH` (both at least 1).
pub fn parse_size(text: &str) -> Res<(u32, u32)> {
    let bad = || format!("expected WxH, got \"{text}\"");
    let (w, h) = text.split_once(['x', 'X']).ok_or_else(bad)?;
    let (w, h): (u32, u32) = (w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?);
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err(format!("size {w}x{h} out of range (1..16384)").into());
    }
    Ok((w, h))
}

/// Size of the window of [`Pattern::Window`] for an APL percentage: the frame's aspect ratio,
/// each side rounded, so the area is within rounding of `apl` percent.
pub fn window_size(w: u32, h: u32, apl: u32) -> (u32, u32) {
    if apl >= 100 {
        return (w, h);
    }
    let s = (apl as f64 / 100.0).sqrt();
    (
        ((w as f64 * s).round() as u32).clamp(1, w),
        ((h as f64 * s).round() as u32).clamp(1, h),
    )
}

/// Level of pixel (x, y) of the pattern at animation frame `frame`.
fn level(p: &Pattern, x: u32, y: u32, w: u32, h: u32, frame: u64) -> u8 {
    let on = |b: bool| if b { 255 } else { 0 };
    match *p {
        Pattern::Flat(v) => v,
        Pattern::Ramp => {
            if w <= 1 {
                0
            } else {
                ((x as u64 * 255 + (w as u64 - 1) / 2) / (w as u64 - 1)) as u8
            }
        }
        Pattern::VLine(n) => on(x.is_multiple_of(n)),
        Pattern::HLine(n) => on(y.is_multiple_of(n)),
        Pattern::Grating(n) => on(x % n < n / 2),
        Pattern::Checker(n) => on((x / n + y / n).is_multiple_of(2)),
        Pattern::Crosshatch(n) => on(x.is_multiple_of(n) || y.is_multiple_of(n)),
        Pattern::Point => on(x == w / 2 && y == h / 2),
        Pattern::Window(apl) => {
            let (ww, wh) = window_size(w, h, apl);
            let (x0, y0) = ((w - ww) / 2, (h - wh) / 2);
            on(x >= x0 && x < x0 + ww && y >= y0 && y < y0 + wh)
        }
        Pattern::Bar { width, speed } => {
            let x0 = (frame.wrapping_mul(speed as u64) % w as u64) as u32;
            // Distance to the right of the bar's left edge, wrapping around the frame.
            on((x + w - x0) % w < width)
        }
        Pattern::Pluge => PLUGE_LEVELS[(x as u64 * PLUGE_LEVELS.len() as u64 / w as u64) as usize],
        Pattern::Moire(n) => on(x.is_multiple_of(n) || y.is_multiple_of(n + 1)),
    }
}

/// Renders the pattern as BGRA (alpha 255), `w * h * 4` bytes. `frame` only matters for animated
/// patterns ([`Pattern::Bar`]).
pub fn generate(p: &Pattern, w: u32, h: u32, frame: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            let v = level(p, x, y, w, h, frame);
            data.extend_from_slice(&[v, v, v, 255]);
        }
    }
    data
}

/// True when the pattern changes from frame to frame.
pub fn is_animated(p: &Pattern) -> bool {
    matches!(p, Pattern::Bar { speed, .. } if *speed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(d: &[u8], w: u32, x: u32, y: u32) -> u8 {
        let i = ((y * w + x) * 4) as usize;
        assert_eq!(d[i], d[i + 1]);
        assert_eq!(d[i], d[i + 2]);
        assert_eq!(d[i + 3], 255);
        d[i]
    }

    fn gen(text: &str, w: u32, h: u32, frame: u64) -> Vec<u8> {
        generate(&text.parse().unwrap(), w, h, frame)
    }

    #[test]
    fn flat() {
        let d = gen("flat:77", 5, 3, 0);
        assert_eq!(d.len(), 5 * 3 * 4);
        assert!((0..5).all(|x| (0..3).all(|y| px(&d, 5, x, y) == 77)));
        assert_eq!(px(&gen("flat", 2, 2, 0), 2, 1, 1), 255);
    }

    #[test]
    fn ramp() {
        let d = gen("ramp", 256, 2, 0);
        for x in 0..256 {
            assert_eq!(px(&d, 256, x, 0) as u32, x);
            assert_eq!(px(&d, 256, x, 1) as u32, x);
        }
        let d = gen("ramp", 3, 1, 0);
        assert_eq!([px(&d, 3, 0, 0), px(&d, 3, 1, 0), px(&d, 3, 2, 0)], [0, 128, 255]);
        assert_eq!(px(&gen("ramp", 1, 1, 0), 1, 0, 0), 0);
    }

    #[test]
    fn vline_hline() {
        let d = gen("vline:4", 9, 2, 0);
        let row: Vec<u8> = (0..9).map(|x| px(&d, 9, x, 1)).collect();
        assert_eq!(row, [255, 0, 0, 0, 255, 0, 0, 0, 255]);
        let d = gen("hline:3", 2, 7, 0);
        let col: Vec<u8> = (0..7).map(|y| px(&d, 2, 1, y)).collect();
        assert_eq!(col, [255, 0, 0, 255, 0, 0, 255]);
    }

    #[test]
    fn grating() {
        let d = gen("grating:4", 8, 1, 0);
        let row: Vec<u8> = (0..8).map(|x| px(&d, 8, x, 0)).collect();
        assert_eq!(row, [255, 255, 0, 0, 255, 255, 0, 0]);
        let d = gen("grating", 4, 1, 0);
        let row: Vec<u8> = (0..4).map(|x| px(&d, 4, x, 0)).collect();
        assert_eq!(row, [255, 0, 255, 0]);
    }

    #[test]
    fn checker() {
        let d = gen("checker", 4, 2, 0);
        assert_eq!(
            [px(&d, 4, 0, 0), px(&d, 4, 1, 0), px(&d, 4, 0, 1), px(&d, 4, 1, 1)],
            [255, 0, 0, 255]
        );
        let d = gen("checker:2", 4, 4, 0);
        assert_eq!(
            [
                px(&d, 4, 0, 0),
                px(&d, 4, 1, 1),
                px(&d, 4, 2, 0),
                px(&d, 4, 0, 2),
                px(&d, 4, 3, 3)
            ],
            [255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn crosshatch() {
        let d = gen("crosshatch:4", 8, 8, 0);
        for y in 0..8 {
            for x in 0..8 {
                let want = if x % 4 == 0 || y % 4 == 0 { 255 } else { 0 };
                assert_eq!(px(&d, 8, x, y), want);
            }
        }
    }

    #[test]
    fn point() {
        let d = gen("point", 5, 5, 0);
        let white: Vec<(u32, u32)> = (0..5)
            .flat_map(|y| (0..5).map(move |x| (x, y)))
            .filter(|&(x, y)| px(&d, 5, x, y) == 255)
            .collect();
        assert_eq!(white, [(2, 2)]);
    }

    #[test]
    fn window_apl() {
        for apl in [10u32, 25, 50, 100] {
            for (w, h) in [(320u32, 240u32), (256, 224), (640, 480), (1920, 1080)] {
                let d = gen(&format!("window:{apl}"), w, h, 0);
                let white = (0..h)
                    .flat_map(|y| (0..w).map(move |x| (x, y)))
                    .filter(|&(x, y)| px(&d, w, x, y) == 255)
                    .count();
                let got = white as f64 / (w as f64 * h as f64) * 100.0;
                assert!((got - apl as f64).abs() < 0.5, "{apl}% at {w}x{h} gave {got}");
            }
        }
        // Centred on a small frame: 50 % of 10x10 -> 7x7 at x,y 1..=7.
        let d = gen("window:50", 10, 10, 0);
        assert_eq!(
            [px(&d, 10, 0, 5), px(&d, 10, 1, 5), px(&d, 10, 7, 5), px(&d, 10, 8, 5)],
            [0, 255, 255, 0]
        );
        assert_eq!(
            [px(&d, 10, 5, 0), px(&d, 10, 5, 1), px(&d, 10, 5, 7), px(&d, 10, 5, 8)],
            [0, 255, 255, 0]
        );
    }

    #[test]
    fn bar_moves() {
        let a = gen("bar:2:3", 10, 1, 0);
        let row = |d: &[u8]| (0..10).map(|x| px(d, 10, x, 0)).collect::<Vec<_>>();
        assert_eq!(row(&a), [255, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(row(&gen("bar:2:3", 10, 1, 1)), [0, 0, 0, 255, 255, 0, 0, 0, 0, 0]);
        // Wraps: frame 3 starts at 9.
        assert_eq!(row(&gen("bar:2:3", 10, 1, 3)), [255, 0, 0, 0, 0, 0, 0, 0, 0, 255]);
        assert!(is_animated(&"bar".parse().unwrap()));
        assert!(!is_animated(&Pattern::Ramp));
        // Static patterns ignore the frame number.
        assert_eq!(gen("ramp", 8, 2, 0), gen("ramp", 8, 2, 99));
    }

    #[test]
    fn pluge() {
        let d = gen("pluge", 10, 1, 0);
        let row: Vec<u8> = (0..10).map(|x| px(&d, 10, x, 0)).collect();
        assert_eq!(row, [0, 0, 3, 3, 6, 6, 10, 10, 16, 16]);
    }

    #[test]
    fn moire() {
        let d = gen("moire:2", 6, 7, 0);
        for y in 0..7 {
            for x in 0..6 {
                let want = if x % 2 == 0 || y % 3 == 0 { 255 } else { 0 };
                assert_eq!(px(&d, 6, x, y), want);
            }
        }
    }

    #[test]
    fn parsing() {
        assert_eq!("RAMP".parse::<Pattern>().unwrap(), Pattern::Ramp);
        assert_eq!(
            "bar:8:4".parse::<Pattern>().unwrap(),
            Pattern::Bar { width: 8, speed: 4 }
        );
        assert_eq!("window".parse::<Pattern>().unwrap(), Pattern::Window(25));
        for bad in [
            "",
            "nope",
            "flat:300",
            "vline:0",
            "window:0",
            "window:101",
            "grating:1",
            "checker:x",
        ] {
            assert!(bad.parse::<Pattern>().is_err(), "{bad}");
        }
        assert_eq!(parse_size("640x480").unwrap(), (640, 480));
        for bad in ["640", "0x4", "ax4", "99999x1"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
    }
}
