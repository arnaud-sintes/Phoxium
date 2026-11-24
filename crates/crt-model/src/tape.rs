//! A VHS tape and the vertical roll of a set that lost its sync (TODO Low, "vsync roll, VHS and tape module";
//! parameters `params/tape.toml`), on the drive voltage R'G'B' in the amplifier's sample domain (`k` samples per
//! source pixel, one row per scanline), optional and off by default.
//!
//! For the output line `y` of frame `f`, `lines` lines in the picture, in this order:
//! 1. **Roll** (`roll_hz` pictures per second, 0 off): the offset is `o = floor(fract(roll_hz f / field_rate) x
//!    lines)` lines and line `y` shows source row `(y - o) mod lines`; source rows under `roll_bar_lines` are the
//!    black bar of the blanking interval, which crosses the picture with the roll.
//! 2. **Head switching**: the last `head_lines` lines tear sideways by `head_shift_px x w x (0.5 + 0.5 u)`, `w` the
//!    ramp 0 to 1 over the band (`(i + 0.5) / head_lines`, `i` the line in the band), `u` a draw in [-1, 1) per
//!    line and frame. Grey noise of deviation `head_noise x w` is added to the same lines.
//! 3. **Luma and chroma bandwidth**: the signal is split into BT.601 luma Y and colour differences U, V (the
//!    scales of `crt_model::tv`); Y is low-passed with a triangular kernel of half width `round(luma_blur_px x k)`
//!    samples, U and V with one of `round(chroma_blur_px x k)`, taken `chroma_shift_px x k` samples later (the
//!    colour lags the picture, as a tape's colour-under signal does). Weights `(R + 1 - |j|) / (R + 1)^2`.
//! 4. **Dropouts**: with probability `dropout` per line and frame a white streak of `dropout_length_px x (0.5 +
//!    0.5 v)` pixels starts at a uniform pixel of the line.
//!
//! The line's ends repeat (the sample index is clamped), and the result is clamped at 0. Every random number is
//! `crt_model::noise` of a seed, the line and the frame, so the shader draws the same ones. Every amount is an
//! estimate: `CRT.md` holds no measured tape data.

use crate::noise::{cell, gaussian, hash, symmetric, unit};
use crate::tv::{KB, KG, KR, U_SCALE, V_SCALE};

/// Seeds of the draws ("HED1", "HDN1", "DRP1").
pub const SEED_HEAD: u32 = 0x4845_4431;
pub const SEED_HEAD_NOISE: u32 = 0x4844_4e31;
pub const SEED_DROPOUT: u32 = 0x4452_5031;
/// Mixes that make the dropout's length and position from its hash (Murmur3 constants).
pub const MIX_LENGTH: u32 = 0x85eb_ca6b;
pub const MIX_POSITION: u32 = 0xc2b2_ae35;
/// Largest half width of a low-pass, samples.
pub const MAX_RADIUS: i64 = 32;

/// Settings of the tape module.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tape {
    /// Frames per second of the source (the time base of the roll).
    pub field_rate_hz: f64,
    /// Half width of the luma low-pass, source pixels.
    pub luma_blur_px: f64,
    /// Half width of the chroma low-pass, source pixels.
    pub chroma_blur_px: f64,
    /// Chroma delay, source pixels (positive: the colour lags to the right).
    pub chroma_shift_px: f64,
    /// Lines of the head-switching band at the bottom (0 off).
    pub head_lines: f64,
    /// Peak sideways tear of the band, source pixels.
    pub head_shift_px: f64,
    /// Deviation of the grey noise at the band's last line.
    pub head_noise: f64,
    /// Probability per line and frame of a dropout.
    pub dropout: f64,
    /// Length of a dropout, source pixels (0.5 to 1 of it).
    pub dropout_length_px: f64,
    /// Vertical roll, pictures per second (0 off).
    pub roll_hz: f64,
    /// Height of the black bar, lines.
    pub roll_bar_lines: f64,
}

impl Default for Tape {
    fn default() -> Self {
        Self {
            field_rate_hz: 60.0,
            luma_blur_px: 0.0,
            chroma_blur_px: 0.0,
            chroma_shift_px: 0.0,
            head_lines: 0.0,
            head_shift_px: 0.0,
            head_noise: 0.0,
            dropout: 0.0,
            dropout_length_px: 24.0,
            roll_hz: 0.0,
            roll_bar_lines: 12.0,
        }
    }
}

fn sample(row: &[[f64; 3]], u: f64) -> [f64; 3] {
    let n = row.len() as i64;
    let i0 = u.floor();
    let t = u - i0;
    let a = (i0 as i64).clamp(0, n - 1) as usize;
    let b = (i0 as i64 + 1).clamp(0, n - 1) as usize;
    [0, 1, 2].map(|c| row[a][c] * (1.0 - t) + row[b][c] * t)
}

fn yuv(c: [f64; 3]) -> [f64; 3] {
    let y = KR * c[0] + KG * c[1] + KB * c[2];
    [y, U_SCALE * (c[2] - y), V_SCALE * (c[0] - y)]
}

fn rgb(y: f64, u: f64, v: f64) -> [f64; 3] {
    let (r, b) = (y + v / V_SCALE, y + u / U_SCALE);
    [r, (y - KR * r - KB * b) / KG, b]
}

impl Tape {
    /// Half width in samples of a low-pass of `px` source pixels at `k` samples per pixel.
    #[must_use]
    pub fn radius(px: f64, k: usize) -> i64 {
        ((px * k as f64).round() as i64).clamp(0, MAX_RADIUS)
    }

    /// Whether the head-switching band does anything.
    #[must_use]
    pub fn has_head(&self) -> bool {
        self.head_lines >= 1.0 && (self.head_shift_px != 0.0 || self.head_noise != 0.0)
    }

    /// Whether the bandwidth step (luma, chroma, shift) runs for `k` samples per pixel.
    #[must_use]
    pub fn has_filter(&self, k: usize) -> bool {
        Self::radius(self.luma_blur_px, k) > 0
            || Self::radius(self.chroma_blur_px, k) > 0
            || self.chroma_shift_px != 0.0
    }

    /// Whether the module does nothing at `k` samples per pixel.
    #[must_use]
    pub fn is_neutral(&self, k: usize) -> bool {
        !self.has_head() && !self.has_filter(k) && self.dropout <= 0.0 && self.roll_hz == 0.0
    }

    /// The roll offset of `frame`, lines in [0, lines).
    #[must_use]
    pub fn roll_offset(&self, frame: u32, lines: u32) -> u32 {
        if self.roll_hz == 0.0 || lines == 0 {
            return 0;
        }
        let phase = (self.roll_hz * f64::from(frame) / self.field_rate_hz.max(1e-9)).rem_euclid(1.0);
        ((phase * f64::from(lines)).floor() as u32).min(lines - 1)
    }

    /// The source row shown by output line `y`, or `None` for the black bar.
    #[must_use]
    pub fn source_row(&self, y: u32, frame: u32, lines: u32) -> Option<u32> {
        if self.roll_hz == 0.0 {
            return Some(y);
        }
        let o = i64::from(self.roll_offset(frame, lines));
        let src = (i64::from(y) - o).rem_euclid(i64::from(lines)) as u32;
        (f64::from(src) >= self.roll_bar_lines).then_some(src)
    }

    /// The head-switching ramp of line `y`: 0 outside the band, `(i + 0.5) / head_lines` in it.
    #[must_use]
    pub fn head_weight(&self, y: u32, lines: u32) -> f64 {
        if !self.has_head() {
            return 0.0;
        }
        let first = f64::from(lines) - self.head_lines;
        let i = f64::from(y) - first;
        if i < 0.0 {
            0.0
        } else {
            (i + 0.5) / self.head_lines
        }
    }

    /// The dropout of line `y` at `frame`: the covered range of source pixel positions, if any.
    #[must_use]
    pub fn dropout_span(&self, y: u32, frame: u32, width_px: f64) -> Option<(f64, f64)> {
        if self.dropout <= 0.0 {
            return None;
        }
        let h = cell(SEED_DROPOUT, y, frame, 0);
        if unit(h) >= self.dropout {
            return None;
        }
        let length = self.dropout_length_px * (0.5 + 0.5 * unit(hash(h ^ MIX_LENGTH)));
        let start = unit(hash(h ^ MIX_POSITION)) * width_px;
        Some((start, start + length))
    }

    /// Output line `y` of `frame` from the source row `row` it shows (amp samples, `k` per source pixel).
    #[must_use]
    pub fn line(&self, y: u32, frame: u32, lines: u32, k: usize, row: &[[f64; 3]]) -> Vec<[f64; 3]> {
        let w = self.head_weight(y, lines);
        let tear = if w > 0.0 && self.head_shift_px != 0.0 {
            self.head_shift_px * w * (0.5 + 0.5 * symmetric(cell(SEED_HEAD, y, frame, 0)))
        } else {
            0.0
        };
        let (ry, rc) = (Self::radius(self.luma_blur_px, k), Self::radius(self.chroma_blur_px, k));
        let filter = self.has_filter(k);
        let kf = k as f64;
        let width_px = row.len() as f64 / kf;
        let span = self.dropout_span(y, frame, width_px);
        let sigma = self.head_noise * w;
        let weight = |r: i64, j: i64| (r + 1 - j.abs()) as f64 / ((r + 1) * (r + 1)) as f64;
        (0..row.len())
            .map(|x| {
                let u = x as f64 - tear * kf;
                let mut v = if filter {
                    let mut ly = 0.0;
                    for j in -ry..=ry {
                        ly += weight(ry, j) * yuv(sample(row, u + j as f64))[0];
                    }
                    let (mut cu, mut cv) = (0.0, 0.0);
                    for j in -rc..=rc {
                        let t = yuv(sample(row, u - self.chroma_shift_px * kf + j as f64));
                        cu += weight(rc, j) * t[1];
                        cv += weight(rc, j) * t[2];
                    }
                    rgb(ly, cu, cv)
                } else {
                    sample(row, u)
                };
                if let Some((a, b)) = span {
                    let xs = (x as f64 + 0.5) / kf;
                    if xs >= a && xs < b {
                        v = [1.0; 3];
                    }
                }
                if sigma != 0.0 {
                    let add = sigma * gaussian(cell(SEED_HEAD_NOISE, (x / k) as u32, y, frame));
                    v = v.map(|c| c + add);
                }
                v.map(|c| c.max(0.0))
            })
            .collect()
    }

    /// The whole image: the roll, then every line.
    #[must_use]
    pub fn frame(&self, rows: &[Vec<[f64; 3]>], frame: u32, k: usize) -> Vec<Vec<[f64; 3]>> {
        let lines = rows.len() as u32;
        (0..lines)
            .map(|y| match self.source_row(y, frame, lines) {
                Some(s) => self.line(y, frame, lines, k, &rows[s as usize]),
                None => vec![[0.0; 3]; rows[y as usize].len()],
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    const K: usize = 4;

    fn grey_impulse(len: usize, at: usize) -> Vec<[f64; 3]> {
        (0..len).map(|x| if x == at { [1.0; 3] } else { [0.0; 3] }).collect()
    }

    #[test]
    fn neutral_is_the_identity() {
        let t = Tape::default();
        assert!(t.is_neutral(K));
        let rows: Vec<Vec<[f64; 3]>> = (0..6)
            .map(|y| (0..20).map(|x| [x as f64 / 20.0, y as f64 / 6.0, 0.5]).collect())
            .collect();
        assert_eq!(t.frame(&rows, 3, K), rows);
        // Each effect alone wakes the module; a blur below half a sample does not.
        assert!(!Tape { luma_blur_px: 0.5, ..t }.is_neutral(K));
        assert!(
            Tape { luma_blur_px: 0.1, ..t }.is_neutral(K),
            "0.1 px x 4 rounds to no sample"
        );
        assert!(!Tape {
            chroma_shift_px: 0.2,
            ..t
        }
        .is_neutral(K));
        assert!(!Tape { dropout: 0.01, ..t }.is_neutral(K));
        assert!(!Tape { roll_hz: 0.1, ..t }.is_neutral(K));
        assert!(!Tape {
            head_lines: 4.0,
            head_noise: 0.1,
            ..t
        }
        .is_neutral(K));
        assert!(
            Tape { head_lines: 4.0, ..t }.is_neutral(K),
            "a band with no tear and no noise does nothing"
        );
    }

    /// The luma low-pass of an impulse is the triangle: half width 2 samples (0.5 px at K = 4) gives weights
    /// 1, 2, 3, 2, 1 over 9 around the impulse; the colour differences of a grey are 0, so every channel is the
    /// same.
    #[test]
    fn the_luma_blur_is_a_triangle() {
        let t = Tape {
            luma_blur_px: 0.5,
            ..Tape::default()
        };
        let out = t.line(0, 0, 10, K, &grey_impulse(32, 16));
        for (x, want) in [
            (14, 1.0 / 9.0),
            (15, 2.0 / 9.0),
            (16, 3.0 / 9.0),
            (17, 2.0 / 9.0),
            (18, 1.0 / 9.0),
            (13, 0.0),
        ] {
            assert!(out[x].iter().all(|c| close(*c, want, 1e-12)), "{x}: {:?}", out[x]);
        }
    }

    /// The colour lags the picture: a grey to (0.8, 0.3, 0.3) step at sample 20 with the chroma delayed 1 px (4
    /// samples) shows the new luma at 20 (Y = 0.2392 + 0.1761 + 0.0342 = 0.4495) but the new colour only from
    /// sample 24; before the colour arrives every channel is the luma.
    #[test]
    fn the_chroma_lags_the_luma() {
        let row: Vec<[f64; 3]> = (0..40)
            .map(|x| if x < 20 { [0.2; 3] } else { [0.8, 0.3, 0.3] })
            .collect();
        let t = Tape {
            chroma_shift_px: 1.0,
            ..Tape::default()
        };
        let out = t.line(0, 0, 10, K, &row);
        let y = KR * 0.8 + KG * 0.3 + KB * 0.3;
        assert!(close(y, 0.4495, 1e-12));
        for (x, v) in out.iter().enumerate().take(24).skip(20) {
            assert!(v.iter().all(|c| close(*c, y, 1e-9)), "{x}: {v:?}");
        }
        for (a, b) in out[24].iter().zip([0.8, 0.3, 0.3]) {
            assert!(close(*a, b, 1e-9), "{:?}", out[24]);
        }
        assert!(out[10].iter().all(|c| close(*c, 0.2, 1e-9)));
    }

    /// Roll: 0.5 pictures per second at 60 fps, frame 30: phase 0.25, offset floor(0.25 x 240) = 60. Line 60 shows
    /// source row 0, which is under the 12-line bar (black); line 72 shows row 12; line 0 shows row 180. Without
    /// the roll every line shows its own row.
    #[test]
    fn the_roll_moves_the_picture_and_its_black_bar() {
        let t = Tape {
            roll_hz: 0.5,
            roll_bar_lines: 12.0,
            ..Tape::default()
        };
        assert_eq!(t.roll_offset(30, 240), 60);
        assert_eq!(t.source_row(60, 30, 240), None);
        assert_eq!(t.source_row(71, 30, 240), None);
        assert_eq!(t.source_row(72, 30, 240), Some(12));
        assert_eq!(t.source_row(0, 30, 240), Some(180));
        assert_eq!(t.roll_offset(0, 240), 0);
        assert_eq!(Tape::default().source_row(5, 30, 240), Some(5));
        // A whole picture later the picture is back: frame 120 is two seconds, one full turn at 0.5 Hz.
        assert_eq!(t.roll_offset(120, 240), 0);
        // The rows of an image: the bar is black, the others come from the source.
        let rows: Vec<Vec<[f64; 3]>> = (0..240).map(|y| vec![[y as f64 / 240.0 + 0.1; 3]; 8]).collect();
        let out = t.frame(&rows, 30, K);
        assert_eq!(out[65], vec![[0.0; 3]; 8]);
        assert!(close(out[72][0][0], 12.0 / 240.0 + 0.1, 1e-12));
    }

    /// The head-switching ramp over 6 lines of 240: line 233 is outside, 234 has (0 + 0.5) / 6, 239 has 5.5 / 6.
    /// The tear is at most `head_shift_px x w`.
    #[test]
    fn the_head_band_ramps_towards_the_bottom() {
        let t = Tape {
            head_lines: 6.0,
            head_shift_px: 8.0,
            head_noise: 0.2,
            ..Tape::default()
        };
        assert_eq!(t.head_weight(233, 240), 0.0);
        assert!(close(t.head_weight(234, 240), 0.5 / 6.0, 1e-12));
        assert!(close(t.head_weight(239, 240), 5.5 / 6.0, 1e-12));
        // Outside the band the line is untouched; inside, a ramp row is displaced and noisy.
        let ramp: Vec<[f64; 3]> = (0..64).map(|x| [x as f64 / 64.0; 3]).collect();
        assert_eq!(t.line(100, 3, 240, K, &ramp), ramp);
        let inside = t.line(239, 3, 240, K, &ramp);
        assert_ne!(inside, ramp);
        let tear = 8.0 * 5.5 / 6.0 * (0.5 + 0.5 * symmetric(cell(SEED_HEAD, 239, 3, 0)));
        assert!((0.0..=8.0).contains(&tear));
        // Sample 40 reads the ramp at 40 - 4 tear (plus the noise, deviation 0.2 x 5.5 / 6, at most 3.47 sigma).
        let noise = 0.2 * 5.5 / 6.0 * gaussian(cell(SEED_HEAD_NOISE, 10, 239, 3));
        let want = ((40.0 - tear * 4.0) / 64.0 + noise).max(0.0);
        assert!(close(inside[40][0], want, 1e-9), "{} {want}", inside[40][0]);
    }

    /// Dropouts: about the chosen share of the lines is hit, each streak is white, 0.5 to 1 of the length long and
    /// inside the line.
    #[test]
    fn dropouts_are_white_streaks_at_the_chosen_rate() {
        let t = Tape {
            dropout: 0.05,
            dropout_length_px: 20.0,
            ..Tape::default()
        };
        let lines = 20_000u32;
        let hit: Vec<(f64, f64)> = (0..lines).filter_map(|y| t.dropout_span(y, 1, 320.0)).collect();
        let share = hit.len() as f64 / f64::from(lines);
        assert!((share - 0.05).abs() < 0.005, "{share}");
        assert!(hit
            .iter()
            .all(|(a, b)| (10.0..=20.0).contains(&(b - a)) && *a >= 0.0 && *a < 320.0));
        let y = (0..lines).find(|y| t.dropout_span(*y, 1, 8.0).is_some()).unwrap();
        let (a, b) = t.dropout_span(y, 1, 8.0).unwrap();
        let row = vec![[0.1; 3]; 32];
        let out = t.line(y, 1, lines, K, &row);
        for (x, v) in out.iter().enumerate() {
            let xs = (x as f64 + 0.5) / 4.0;
            let want = if xs >= a && xs < b { 1.0 } else { 0.1 };
            assert_eq!(v[0], want, "{x}");
        }
        assert!(Tape::default().dropout_span(0, 0, 8.0).is_none());
    }
}
