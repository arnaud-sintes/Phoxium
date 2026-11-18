//! Source integer scaling: recovers the native raster of a source that was upscaled by a simple
//! integer factor with nearest neighbour (for example a 256×240 game shown at 3×).
//!
//! The divisor is either given by the user or detected. Detection looks at the run lengths of
//! identical pixels along rows (horizontal factor) and of identical rows (vertical factor): a factor
//! k is kept while nearly all runs are multiples of k, and the largest surviving k wins. A divisor of
//! 1 means the source is native (x1). Smoothed or non-integer upscales are not handled: they simply
//! detect as 1.

use std::str::FromStr;

/// A rectangle inside the captured frame, in captured pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// Clamps the rectangle to a `w` by `h` frame.
    pub fn clamp(self, w: u32, h: u32) -> Rect {
        let x = self.x.min(w);
        let y = self.y.min(h);
        Rect { x, y, w: self.w.min(w - x), h: self.h.min(h - y) }
    }
}

impl FromStr for Rect {
    type Err = String;
    /// "x,y,w,h"
    fn from_str(s: &str) -> Result<Self, String> {
        let v: Vec<u32> = s.split(',').map(|p| p.trim().parse().map_err(|_| format!("bad number in \"{s}\""))).collect::<Result<_, _>>()?;
        match v[..] {
            [x, y, w, h] if w > 0 && h > 0 => Ok(Rect { x, y, w, h }),
            _ => Err("expected x,y,w,h with a non-zero size".into()),
        }
    }
}

/// Parses "N" or "NXxNY" into a horizontal and vertical divisor.
pub fn parse_divisor(s: &str) -> Result<(u32, u32), String> {
    let parse = |p: &str| p.parse::<u32>().ok().filter(|&n| n >= 1).ok_or_else(|| format!("bad divisor \"{s}\""));
    match s.split_once('x') {
        Some((a, b)) => Ok((parse(a)?, parse(b)?)),
        None => parse(s).map(|n| (n, n)),
    }
}

/// Frames analysed before the detected divisor is locked (unless x1 is certain earlier).
const DETECT_FRAMES: u32 = 30;
/// Largest upscale factor considered.
const MAX_K: usize = 32;
/// Pixels ignored on every side: Windows 11 rounds the corners of a window, which would otherwise
/// leave odd-length runs in the first and last rows.
const MARGIN: u32 = 16;
/// A factor is accepted if at most this fraction of the analysed pixels is not in a run that is a
/// multiple of it.
const TOLERANCE: f64 = 0.01;

pub struct Detector {
    cand_x: [bool; MAX_K + 1],
    cand_y: [bool; MAX_K + 1],
    frames: u32,
    locked: bool,
}

impl Default for Detector {
    fn default() -> Self {
        Detector { cand_x: [true; MAX_K + 1], cand_y: [true; MAX_K + 1], frames: 0, locked: false }
    }
}

/// Marks the factors that do not explain the run lengths `runs` (first and last run dropped,
/// they are cut by the analysed area).
fn eliminate(cand: &mut [bool; MAX_K + 1], runs: &[u32]) {
    if runs.len() < 3 {
        return;
    }
    let runs = &runs[1..runs.len() - 1];
    let total: u64 = runs.iter().map(|&r| r as u64).sum();
    for k in 2..=MAX_K {
        let bad: u64 = runs.iter().filter(|&&r| r as usize % k != 0).map(|&r| r as u64).sum();
        if bad as f64 > TOLERANCE * total as f64 {
            cand[k] = false;
        }
    }
}

fn best(cand: &[bool; MAX_K + 1]) -> u32 {
    (2..=MAX_K).rev().find(|&k| cand[k]).unwrap_or(1) as u32
}

impl Detector {
    pub fn reset(&mut self) {
        *self = Detector::default();
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Divisor to use: (1, 1) until the detection is locked.
    pub fn divisor(&self) -> (u32, u32) {
        if self.locked { (best(&self.cand_x), best(&self.cand_y)) } else { (1, 1) }
    }

    /// Analyses `region` of a BGRA8 frame that is `frame_w` pixels wide.
    pub fn feed(&mut self, data: &[u8], frame_w: u32, region: Rect) {
        let m = MARGIN.min(region.w / 8).min(region.h / 8);
        if self.locked || region.w <= 2 * m + 2 || region.h <= 2 * m + 2 {
            return;
        }
        let (x0, x1) = (region.x + m, region.x + region.w - m);
        let (y0, y1) = (region.y + m, region.y + region.h - m);
        let row = |y: u32| -> &[u8] { &data[((y * frame_w + x0) * 4) as usize..((y * frame_w + x1) * 4) as usize] };

        let mut runs = Vec::new();
        let mut vertical = Vec::new();
        let mut vertical_run = 0u32;
        for y in y0..y1 {
            let current = row(y);
            // Horizontal runs of identical pixels, evaluated per row.
            runs.clear();
            let mut run = 0u32;
            let mut previous: Option<&[u8]> = None;
            for px in current.chunks_exact(4) {
                if previous == Some(px) {
                    run += 1;
                } else {
                    if run > 0 {
                        runs.push(run);
                    }
                    run = 1;
                    previous = Some(px);
                }
            }
            runs.push(run);
            eliminate(&mut self.cand_x, &runs);

            // Vertical runs of identical rows.
            if y > y0 && row(y - 1) == current {
                vertical_run += 1;
            } else {
                if vertical_run > 0 {
                    vertical.push(vertical_run);
                }
                vertical_run = 1;
            }
        }
        vertical.push(vertical_run);
        eliminate(&mut self.cand_y, &vertical);

        self.frames += 1;
        let any = (2..=MAX_K).any(|k| self.cand_x[k] || self.cand_y[k]);
        self.locked = !any || self.frames >= DETECT_FRAMES;
    }
}

/// The part of the captured frame that is the source, and how it maps to native pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct View {
    pub origin: (u32, u32),
    pub div: (u32, u32),
    pub native: (u32, u32),
}

impl View {
    pub fn full(w: u32, h: u32) -> Self {
        View { origin: (0, 0), div: (1, 1), native: (w, h) }
    }

    pub fn new(region: Rect, div: (u32, u32)) -> Self {
        let native = (region.w / div.0, region.h / div.1);
        // A divisor that leaves almost nothing (for example on a flat frame) is not credible.
        let (div, native) = if native.0 < 8 || native.1 < 8 { ((1, 1), (region.w, region.h)) } else { (div, native) };
        View { origin: (region.x, region.y), div, native }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of `w`×`h` native pixels with varied colours, upscaled by (kx, ky).
    fn frame(w: u32, h: u32, kx: u32, ky: u32) -> (Vec<u8>, u32, u32) {
        let (fw, fh) = (w * kx, h * ky);
        let mut data = Vec::new();
        for y in 0..fh {
            for x in 0..fw {
                let (nx, ny) = (x / kx, y / ky);
                let v = ((nx * 7 + ny * 13) % 251) as u8;
                data.extend_from_slice(&[v, v ^ 0x55, v.wrapping_mul(3), 255]);
            }
        }
        (data, fw, fh)
    }

    fn detect(w: u32, h: u32, kx: u32, ky: u32) -> (u32, u32) {
        let (data, fw, fh) = frame(w, h, kx, ky);
        let mut d = Detector::default();
        for _ in 0..DETECT_FRAMES {
            d.feed(&data, fw, Rect { x: 0, y: 0, w: fw, h: fh });
        }
        d.divisor()
    }

    #[test]
    fn detects_integer_upscales() {
        assert_eq!(detect(64, 48, 3, 3), (3, 3));
        assert_eq!(detect(64, 48, 4, 4), (4, 4));
        assert_eq!(detect(64, 48, 2, 3), (2, 3));
    }

    #[test]
    fn native_source_locks_to_one_immediately() {
        let (data, fw, fh) = frame(64, 48, 1, 1);
        let mut d = Detector::default();
        d.feed(&data, fw, Rect { x: 0, y: 0, w: fw, h: fh });
        assert!(d.is_locked());
        assert_eq!(d.divisor(), (1, 1));
    }

    #[test]
    fn tolerates_rounded_window_corners() {
        let (mut data, fw, fh) = frame(64, 48, 3, 3);
        // Black out the bottom corners like a rounded window: odd-length runs on the last rows.
        for y in fh - 9..fh {
            let cut = (fh - y) as usize * 3;
            for x in 0..cut.min(fw as usize) {
                let i = (y as usize * fw as usize + x) * 4;
                data[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
                let j = (y as usize * fw as usize + (fw as usize - 1 - x)) * 4;
                data[j..j + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
        let mut d = Detector::default();
        for _ in 0..DETECT_FRAMES {
            d.feed(&data, fw, Rect { x: 0, y: 0, w: fw, h: fh });
        }
        assert_eq!(d.divisor(), (3, 3));
    }

    #[test]
    fn parses_options() {
        assert_eq!(parse_divisor("3"), Ok((3, 3)));
        assert_eq!(parse_divisor("2x4"), Ok((2, 4)));
        assert!(parse_divisor("0").is_err());
        assert_eq!("1,2,30,40".parse::<Rect>(), Ok(Rect { x: 1, y: 2, w: 30, h: 40 }));
    }
}
