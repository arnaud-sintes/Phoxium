//! Analogue impairments (optional module, off by default): the f64 oracle of the `impair` slot
//! (`shaders/impair.comp`). Voltage domain, between the video amplifier and the gun: the input and the output are
//! drive voltages R'G'B' on the amplifier's sample grid (K samples per source pixel, one row per scanline).
//!
//! # Model (`CRT.md` "Signal path", "Scan jitter and swim"; every number is an estimate [E], these are controls,
//! not the model of one set)
//!
//! Time is the frame index over the field rate (`field_rate_hz`, a property of the source, 60 for most), so every
//! effect is a pure function of (line, frame, sample): the same picture twice gives the same impairments, and a
//! test can name them. Line y of frame n is "drawn" at t = n / f_field + y / (lines f_field).
//!
//! 1. **Line displacement** d(y, n), in source pixels, moves the line right (the beam starts the line late); the
//!    signal at sample x is read from x - d (linear interpolation between samples, black beyond the line's ends):
//!    - *jitter*: d += `jitter_px` x u, u uniform in [-1, 1) drawn per line and frame (the line start is not
//!      locked exactly);
//!    - *crawl*: d += `crawl_px` sin(2 pi (`crawl_speed_hz` t0 + y / `crawl_period_lines`)), a slow wave down the
//!      picture that drifts with time (mains or a poor oscillator pulling the horizontal deflection);
//!    - *line tracking* (a tape or a failing sync): a band of `tracking_lines` lines centred on the line
//!      p = frac(`tracking_position` + `tracking_roll_hz` t0) x lines, weighted w(y) = max(0, 1 - |y + 1/2 - p| /
//!      (band / 2)) (a triangle); inside it d += `tracking_px` w (1/2 + u / 2), u drawn per line and frame (the lines
//!      lean right and tear), and the noise below grows by `tracking_noise` w.
//!
//!    (t0 above is the time of line 0 of the frame, n / f_field.)
//! 2. **Ghost** (a reflection in the cable or an echo off a building): the displaced signal plus `ghost_gain` times
//!    itself `ghost_delay_px` pixels later, v += g v(x - delay). Negative gain is an inverted ghost.
//! 3. **Hum** (mains interference on the video): v += `hum_amount` sin(2 pi (f_hum t + ...)) on every channel,
//!    phase = f_hum x (n + y / lines) / f_field, so one picture holds f_hum / f_field bars and they roll at
//!    f_hum / f_field - round(...) cycles a frame (50 Hz mains against 60 Hz fields: a bar slowly rolling up).
//! 4. **Noise**: v += (`noise` + `tracking_noise` w) g, g near-Gaussian ([`crate::noise::gaussian`]) drawn per source
//!    pixel, line and frame, the same on the three channels (a luma noise, as the eye reads video noise), so it
//!    changes every frame.
//! 5. **Snow**: each cell of `snow_length` pixels on a line is hit with probability `snow` per frame; a hit
//!    replaces the signal with a grey level uniform in [0, 1) (receiver noise at full amplitude, in short streaks).
//! 6. The result is not allowed below black: v = max(v, 0) (a beam cannot give negative light; the gun law is
//!    undefined below its cutoff).
//!
//! Alpha is not touched. With every amount at 0 ([`Impair::is_neutral`]) the module is the identity, bit for bit.
//!
//! Not modelled: the sync pulses and their loss, colour-burst loss, the interaction of ghosts with the chroma (the
//! module works on the decoded R'G'B', after `composite`), head-switching noise at a tape's bottom.

use std::f64::consts::PI;

use crate::noise;

/// Noise seeds (`noise::cell`), one per effect; the shader repeats them.
pub const SEED_JITTER: u32 = 0x4a49_5431;
pub const SEED_TRACK: u32 = 0x5452_4b31;
pub const SEED_NOISE: u32 = 0x4e4f_4931;
pub const SEED_SNOW: u32 = 0x534e_4f31;
/// Mixed into a snow cell's hash to draw its grey level.
pub const SNOW_LEVEL_MIX: u32 = 0x5bd1_e995;

/// The impairments' settings, physical units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Impair {
    /// Field (frame) rate of the source, Hz: the time base.
    pub field_rate_hz: f64,
    /// Hum amplitude, in drive voltage units.
    pub hum_amount: f64,
    /// Hum (mains) frequency, Hz.
    pub hum_hz: f64,
    /// Peak line start jitter, source pixels.
    pub jitter_px: f64,
    /// Amplitude of the slow line wave, source pixels.
    pub crawl_px: f64,
    /// Period of the line wave down the picture, lines.
    pub crawl_period_lines: f64,
    /// Drift of the line wave, cycles per second.
    pub crawl_speed_hz: f64,
    /// Ghost gain (negative: inverted).
    pub ghost_gain: f64,
    /// Ghost delay, source pixels.
    pub ghost_delay_px: f64,
    /// Noise standard deviation, drive voltage units.
    pub noise: f64,
    /// Probability that a snow cell is hit, per frame.
    pub snow: f64,
    /// Snow streak length, pixels (at least 1).
    pub snow_length: u32,
    /// Peak displacement inside the tracking band, source pixels.
    pub tracking_px: f64,
    /// Height of the tracking band, lines.
    pub tracking_lines: f64,
    /// Position of the band's centre, as a fraction of the picture height.
    pub tracking_position: f64,
    /// Roll of the band, picture heights per second.
    pub tracking_roll_hz: f64,
    /// Extra noise inside the band, drive voltage units.
    pub tracking_noise: f64,
}

impl Default for Impair {
    /// Every amount at 0 (the module neutral), the other settings at the defaults of `params/impair.toml`.
    fn default() -> Self {
        Self {
            field_rate_hz: 60.0,
            hum_amount: 0.0,
            hum_hz: 50.0,
            jitter_px: 0.0,
            crawl_px: 0.0,
            crawl_period_lines: 90.0,
            crawl_speed_hz: 0.25,
            ghost_gain: 0.0,
            ghost_delay_px: 5.0,
            noise: 0.0,
            snow: 0.0,
            snow_length: 3,
            tracking_px: 0.0,
            tracking_lines: 24.0,
            tracking_position: 0.9,
            tracking_roll_hz: 0.0,
            tracking_noise: 0.2,
        }
    }
}

impl Impair {
    /// Whether every amount is 0: the module changes nothing.
    pub fn is_neutral(&self) -> bool {
        self.hum_amount == 0.0
            && self.jitter_px == 0.0
            && self.crawl_px == 0.0
            && self.ghost_gain == 0.0
            && self.noise == 0.0
            && self.snow == 0.0
            && self.tracking_px == 0.0
    }

    /// Time of line 0 of frame `frame`, seconds.
    pub fn time_s(&self, frame: u32) -> f64 {
        f64::from(frame) / self.field_rate_hz
    }

    /// Hum phase at line 0 of the frame (cycles in [0, 1)) and its advance per line (cycles).
    pub fn hum_phase(&self, frame: u32, lines: u32) -> (f64, f64) {
        (
            (self.hum_hz * self.time_s(frame)).rem_euclid(1.0),
            self.hum_hz / (self.field_rate_hz * f64::from(lines.max(1))),
        )
    }

    /// Phase of the line wave at line 0 of the frame (cycles in [0, 1)) and its advance per line (cycles).
    pub fn crawl_phase(&self, frame: u32) -> (f64, f64) {
        (
            (self.crawl_speed_hz * self.time_s(frame)).rem_euclid(1.0),
            1.0 / self.crawl_period_lines.max(1.0),
        )
    }

    /// Centre of the tracking band, in lines from the top of the picture.
    pub fn tracking_centre_lines(&self, frame: u32, lines: u32) -> f64 {
        (self.tracking_position + self.tracking_roll_hz * self.time_s(frame)).rem_euclid(1.0) * f64::from(lines)
    }

    /// Weight of the tracking band at line `y`: a triangle of height 1 over `tracking_lines` lines; 0 without a
    /// tracking displacement.
    pub fn tracking_weight(&self, y: u32, frame: u32, lines: u32) -> f64 {
        if self.tracking_px == 0.0 {
            return 0.0;
        }
        let half = 0.5 * self.tracking_lines.max(1.0);
        let dist = (f64::from(y) + 0.5 - self.tracking_centre_lines(frame, lines)).abs();
        (1.0 - dist / half).max(0.0)
    }

    /// Displacement of line `y` of frame `frame`, source pixels (positive: to the right).
    pub fn displacement_px(&self, y: u32, frame: u32, lines: u32) -> f64 {
        let mut d = 0.0;
        if self.jitter_px != 0.0 {
            d += self.jitter_px * noise::symmetric(noise::cell(SEED_JITTER, y, frame, 0));
        }
        if self.crawl_px != 0.0 {
            let (phase0, per_line) = self.crawl_phase(frame);
            d += self.crawl_px * (2.0 * PI * (phase0 + f64::from(y) * per_line)).sin();
        }
        let w = self.tracking_weight(y, frame, lines);
        if w > 0.0 {
            let u = noise::symmetric(noise::cell(SEED_TRACK, y, frame, 0));
            d += self.tracking_px * w * (0.5 + 0.5 * u);
        }
        d
    }

    /// The impaired row `y` of a frame of `lines` lines (frame index `frame`): `row` is the amplifier's row,
    /// `k` samples per source pixel.
    pub fn line(&self, y: u32, frame: u32, lines: u32, k: usize, row: &[[f64; 3]]) -> Vec<[f64; 3]> {
        if self.is_neutral() {
            return row.to_vec();
        }
        let kf = k as f64;
        let d = self.displacement_px(y, frame, lines);
        let w = self.tracking_weight(y, frame, lines);
        let hum = if self.hum_amount != 0.0 {
            let (phase0, per_line) = self.hum_phase(frame, lines);
            self.hum_amount * (2.0 * PI * (phase0 + f64::from(y) * per_line)).sin()
        } else {
            0.0
        };
        let sigma = self.noise + self.tracking_noise * w;
        let len = self.snow_length.max(1);
        let sample = |u: f64| -> [f64; 3] {
            let i0 = u.floor();
            let t = u - i0;
            let get = |i: f64| {
                if i >= 0.0 && (i as usize) < row.len() {
                    row[i as usize]
                } else {
                    [0.0; 3]
                }
            };
            let (a, b) = (get(i0), get(i0 + 1.0));
            [0, 1, 2].map(|c| a[c] * (1.0 - t) + b[c] * t)
        };
        (0..row.len())
            .map(|s| {
                let u = s as f64 - d * kf;
                let mut v = sample(u);
                if self.ghost_gain != 0.0 {
                    let g = sample(u - self.ghost_delay_px * kf);
                    for c in 0..3 {
                        v[c] += self.ghost_gain * g[c];
                    }
                }
                let px = (s / k) as u32;
                let mut add = hum;
                if sigma != 0.0 {
                    add += sigma * noise::gaussian(noise::cell(SEED_NOISE, px, y, frame));
                }
                for c in v.iter_mut() {
                    *c += add;
                }
                if self.snow > 0.0 {
                    let h = noise::cell(SEED_SNOW, px / len, y, frame);
                    if noise::unit(h) < self.snow {
                        v = [noise::unit(noise::hash(h ^ SNOW_LEVEL_MIX)); 3];
                    }
                }
                v.map(|c| c.max(0.0))
            })
            .collect()
    }

    /// The impaired frame: `rows[y]` is the amplifier's row y.
    pub fn frame(&self, rows: &[Vec<[f64; 3]>], frame: u32, k: usize) -> Vec<Vec<[f64; 3]>> {
        let lines = rows.len() as u32;
        rows.iter()
            .enumerate()
            .map(|(y, row)| self.line(y as u32, frame, lines, k, row))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: usize = 4;

    fn row(len: usize, f: impl Fn(usize) -> f64) -> Vec<[f64; 3]> {
        (0..len).map(|s| [f(s); 3]).collect()
    }

    /// Every amount at 0: the identity, whatever the other settings.
    #[test]
    fn neutral_is_the_identity() {
        let i = Impair::default();
        assert!(i.is_neutral());
        let r = row(64, |s| (s as f64 * 0.37).sin().abs());
        assert_eq!(i.line(5, 9, 100, K, &r), r);
        let tracking_only_noise = Impair {
            tracking_noise: 0.5,
            ghost_delay_px: 9.0,
            ..i
        };
        assert!(
            tracking_only_noise.is_neutral(),
            "a band without a displacement does not exist"
        );
    }

    /// A ghost of gain 0.5 three pixels later copies an impulse of 1 at sample 40 to sample 52 (3 px x K = 12
    /// samples), halved, and changes nothing else; a negative gain would have been clamped at black.
    #[test]
    fn a_ghost_copies_an_impulse_later_and_weaker() {
        let i = Impair {
            ghost_gain: 0.5,
            ghost_delay_px: 3.0,
            ..Impair::default()
        };
        let r = row(100, |s| if s == 40 { 1.0 } else { 0.0 });
        let out = i.line(0, 0, 10, K, &r);
        for (s, p) in out.iter().enumerate() {
            let want = match s {
                40 => 1.0,
                52 => 0.5,
                _ => 0.0,
            };
            assert!(
                (p[0] - want).abs() < 1e-12 && p[0] == p[1] && p[1] == p[2],
                "sample {s}: {p:?}"
            );
        }
    }

    /// Hum, hand numbers: 60 Hz mains on 60 Hz fields is one bar per picture, standing: line y of 100 has
    /// 0.1 sin(2 pi y / 100), 0.1 at line 25, clamped to 0 at line 75, whatever the frame. 50 Hz on 60 Hz moves the
    /// phase by 5/6 cycle a frame: at line 0 of frame 5 it is frac(4.1667) = 1/6, 0.1 sin(pi / 3) = 0.0866.
    #[test]
    fn hum_is_a_bar_that_rolls_with_the_beat() {
        let steady = Impair {
            hum_amount: 0.1,
            hum_hz: 60.0,
            ..Impair::default()
        };
        let zero = row(8, |_| 0.0);
        for frame in [0, 1, 7] {
            assert!(
                (steady.line(25, frame, 100, K, &zero)[3][0] - 0.1).abs() < 1e-12,
                "frame {frame}"
            );
            assert_eq!(steady.line(75, frame, 100, K, &zero)[3][0], 0.0);
        }
        let beat = Impair {
            hum_amount: 0.1,
            hum_hz: 50.0,
            ..Impair::default()
        };
        let v = beat.line(0, 5, 100, K, &zero)[0][1];
        assert!((v - 0.1 * (PI / 3.0).sin()).abs() < 1e-12, "{v}");
        assert_eq!(
            beat.line(0, 2, 100, K, &zero)[0][0],
            0.0,
            "frame 2 is in the negative half"
        );
    }

    /// Jitter moves each line by its own draw: the centre of a pulse (ones over samples 100 to 299 of 400, far
    /// from the ends) shifts by d K samples, d = jitter x the line's uniform draw, inside +-jitter, with the
    /// pulse's area kept (the linear interpolation preserves both moments' sums); the draw differs per line.
    #[test]
    fn jitter_moves_each_line_by_its_draw() {
        let i = Impair {
            jitter_px: 2.0,
            ..Impair::default()
        };
        let r = row(400, |s| if (100..300).contains(&s) { 1.0 } else { 0.0 });
        let mut ds = Vec::new();
        for y in 0..40 {
            let d = i.displacement_px(y, 3, 40);
            assert_eq!(d, 2.0 * noise::symmetric(noise::cell(SEED_JITTER, y, 3, 0)));
            assert!(d.abs() <= 2.0);
            let out = i.line(y, 3, 40, K, &r);
            let area: f64 = out.iter().map(|p| p[0]).sum();
            let centre: f64 = out.iter().enumerate().map(|(s, p)| s as f64 * p[0]).sum::<f64>() / area;
            assert!((area - 200.0).abs() < 1e-9, "line {y}: area {area}");
            assert!(
                (centre - (199.5 + d * K as f64)).abs() < 1e-9,
                "line {y}: centre {centre}, d {d}"
            );
            ds.push((d * 1e6).round() as i64);
        }
        ds.sort_unstable();
        ds.dedup();
        assert!(ds.len() > 30, "{} distinct displacements", ds.len());
    }

    /// Crawl: sin(2 pi (phase + y / period)) x amplitude: 1 px at line 25 of a 100 line period, frame 0; the drift
    /// at 0.25 cycles a second on 60 Hz frames moves it by 1/240 cycle a frame.
    #[test]
    fn crawl_is_a_slow_wave_down_the_picture() {
        let i = Impair {
            crawl_px: 1.0,
            crawl_period_lines: 100.0,
            ..Impair::default()
        };
        assert!((i.displacement_px(25, 0, 200) - 1.0).abs() < 1e-12);
        assert!(i.displacement_px(0, 0, 200).abs() < 1e-12);
        let d1 = i.displacement_px(0, 60, 200);
        assert!((d1 - (2.0 * PI * 0.25).sin()).abs() < 1e-12, "{d1}");
    }

    /// The tracking band, hand numbers: 20 lines centred on line 50 of 100: weight 0.95 at line 49 (distance 1/2
    /// of 10), 0.05 at line 59, 0 at 60 and beyond and at 39; no displacement amount, no band. The band rolls: at
    /// 0.1 picture heights a second and 60 Hz, frame 60 is 10 lines on.
    #[test]
    fn the_tracking_band_is_a_triangle_that_rolls() {
        let i = Impair {
            tracking_px: 6.0,
            tracking_lines: 20.0,
            tracking_position: 0.5,
            ..Impair::default()
        };
        let w = |y| i.tracking_weight(y, 0, 100);
        assert!((w(49) - 0.95).abs() < 1e-12 && (w(50) - 0.95).abs() < 1e-12);
        assert!((w(59) - 0.05).abs() < 1e-12 && w(60) == 0.0 && w(39) == 0.0 && w(10) == 0.0);
        assert_eq!(Impair { tracking_px: 0.0, ..i }.tracking_weight(49, 0, 100), 0.0);
        let rolling = Impair {
            tracking_roll_hz: 0.1,
            ..i
        };
        assert!((rolling.tracking_centre_lines(60, 100) - 60.0).abs() < 1e-9);
        assert!((rolling.tracking_weight(59, 60, 100) - 0.95).abs() < 1e-9);
        // Inside the band the displacement is between 0 and tracking_px w (1/2 + u / 2).
        let d = i.displacement_px(49, 0, 100);
        assert!((0.0..=6.0 * 0.95).contains(&d), "{d}");
        assert_eq!(i.displacement_px(10, 0, 100), 0.0);
    }

    /// Noise has the asked standard deviation (per source pixel, the same on the three channels, new every frame)
    /// on a grey level high enough for the clamp not to cut it; snow hits the asked share of cells with grey
    /// levels, in streaks of the asked length.
    #[test]
    fn noise_and_snow_have_their_statistics() {
        let n = Impair {
            noise: 0.05,
            ..Impair::default()
        };
        let r = row(2000 * K, |_| 0.5);
        let mut v = Vec::new();
        for y in 0..40 {
            let out = n.line(y, 1, 40, K, &r);
            assert!(out.iter().all(|p| p[0] == p[1] && p[1] == p[2]));
            v.extend(out.iter().step_by(K).map(|p| p[0] - 0.5));
            // The K samples of a pixel share one draw.
            assert!(out[0..K].iter().all(|p| p[0] == out[0][0]));
        }
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        assert!(mean.abs() < 2e-3 && (sd - 0.05).abs() < 1.5e-3, "mean {mean}, sd {sd}");
        let other_frame = n.line(0, 2, 40, K, &r);
        assert_ne!(other_frame, n.line(0, 1, 40, K, &r));

        let s = Impair {
            snow: 0.1,
            snow_length: 4,
            ..Impair::default()
        };
        let zero = row(2000 * K, |_| 0.0);
        let (mut cells, mut hit) = (0usize, 0usize);
        for y in 0..40 {
            let out = s.line(y, 0, 40, K, &zero);
            for c in 0..2000 / 4 {
                cells += 1;
                let first = out[c * 4 * K][0];
                let streak: Vec<f64> = (0..4 * K).map(|j| out[c * 4 * K + j][0]).collect();
                assert!(streak.iter().all(|&x| x == first), "a cell is one grey");
                if first > 0.0 {
                    hit += 1;
                }
            }
        }
        let share = hit as f64 / cells as f64;
        assert!((share - 0.1).abs() < 0.01, "{share}");
    }

    /// Nothing leaves [0, black]: a strong negative ghost and hum clamp at 0.
    #[test]
    fn the_result_is_never_below_black() {
        let i = Impair {
            ghost_gain: -1.0,
            hum_amount: 0.4,
            noise: 0.2,
            ..Impair::default()
        };
        let r = row(200, |s| (s % 7) as f64 / 7.0);
        for y in 0..20 {
            assert!(i.line(y, 4, 20, K, &r).iter().all(|p| p.iter().all(|&c| c >= 0.0)));
        }
    }
}
