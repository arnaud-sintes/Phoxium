//! Motion clarity: the beam scan spread over several output refreshes (QUESTIONS PHY-12, "Motion clarity"; CRT.md
//! "Display rate").
//!
//! A real tube draws a frame in one sweep of the beam: at any instant only the few lines under the beam are bright,
//! and the phosphor of the others is decaying. A sample-and-hold panel shows the whole frame for the whole frame
//! period, so the eye, tracking a moving object, smears it. On a panel that refreshes `n` times per source frame
//! (240 or 300 Hz for a 60 Hz source, `n` = 4 or 5) the sweep can be reproduced: refresh `k` of the sweep shows the
//! lines the beam crossed during it, the others only their afterglow ([`Mode::Rolling`]), or the whole frame for
//! some of the refreshes and black for the rest, black frame insertion ([`Mode::Strobe`]).
//!
//! Energy: a line is lit for a fraction `duty` of the sweep, so to give the same mean light as a still picture it
//! must be `1 / duty` times brighter while it is lit ([`Plan::gain`] with a boost of 1). A display cannot go above
//! its peak white, so the boost is a choice: 1 keeps the mean light of everything the panel can show (bright
//! highlights clip and lose light), 0 keeps the picture's own level while it is lit (nothing clips, the mean is
//! `duty` times lower). Saturation of the phosphor (`phosphor_saturation`) sees the boosted drive, as the real
//! efficiency droop sees the peak current.
//!
//! This module is the oracle of `shaders/scan.comp` and `src/gpu/stages/scan.rs`: which refreshes light which lines,
//! and how bright. Pure integer and f64 arithmetic.

/// The most refreshes per source frame the model plans for.
pub const MAX_SUBFRAMES: u32 = 16;

/// How the sweep is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The whole frame at every refresh: no scan (the picture is exactly the frame-integrated one).
    Off,
    /// Black frame insertion: the whole frame for the first `lit` refreshes of the sweep, black for the others.
    Strobe,
    /// The beam: refresh `k` of `n` shows the band of lines the beam crosses in it, the next bands follow.
    Rolling,
}

/// What a sweep is, once the output refresh and the parameters are known.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    pub mode: Mode,
    /// Output refreshes per source frame (2 to [`MAX_SUBFRAMES`]).
    pub subframes: u32,
    /// Strobe: the refreshes of the sweep that show the frame (1 to `subframes - 1`); 1 for the rolling scan.
    pub lit: u32,
    /// How much of the energy lost to the dark time is given back as peak (0 to 1).
    pub boost: f64,
}

/// The number of refreshes per source frame: `manual` when it is above 0, else the refresh rate over the scan rate,
/// rounded, within 1 to [`MAX_SUBFRAMES`]. A refresh rate that is not a positive finite number counts as 60 Hz.
pub fn subframes(refresh_hz: f64, scan_source_rate_hz: f64, manual: u32) -> u32 {
    if manual > 0 {
        return manual.min(MAX_SUBFRAMES);
    }
    let refresh = if refresh_hz.is_finite() && refresh_hz > 0.0 {
        refresh_hz
    } else {
        60.0
    };
    let rate = if scan_source_rate_hz.is_finite() && scan_source_rate_hz > 0.0 {
        scan_source_rate_hz
    } else {
        60.0
    };
    ((refresh / rate).round() as u32).clamp(1, MAX_SUBFRAMES)
}

/// The plan of a sweep, `None` when there is none to show: the mode is off, or fewer than 2 refreshes per source
/// frame (nothing to spread the sweep over). `lit` is clamped to 1 to `subframes - 1` and `boost` to 0 to 1; the
/// rolling scan always lights each line for one refresh.
pub fn plan(mode: Mode, refresh_hz: f64, scan_source_rate_hz: f64, manual: u32, lit: u32, boost: f64) -> Option<Plan> {
    if mode == Mode::Off {
        return None;
    }
    let subframes = subframes(refresh_hz, scan_source_rate_hz, manual);
    if subframes < 2 {
        return None;
    }
    let lit = match mode {
        Mode::Strobe => lit.clamp(1, subframes - 1),
        _ => 1,
    };
    let boost = if boost.is_nan() { 1.0 } else { boost.clamp(0.0, 1.0) };
    Some(Plan {
        mode,
        subframes,
        lit,
        boost,
    })
}

/// How far the refresh rate is from `subframes` whole scan periods, as a fraction of one refresh per sweep: 0 when
/// the refresh is exactly `subframes` times the scan rate. The caller warns above [`RATIO_TOLERANCE`]: the sweep
/// then drifts against the source, and the bands beat as flicker.
pub fn ratio_error(refresh_hz: f64, scan_source_rate_hz: f64, subframes: u32) -> f64 {
    if !(refresh_hz.is_finite() && refresh_hz > 0.0 && scan_source_rate_hz.is_finite() && scan_source_rate_hz > 0.0)
        || subframes == 0
    {
        return 1.0;
    }
    (refresh_hz / scan_source_rate_hz - f64::from(subframes)).abs() / f64::from(subframes)
}

/// The relative mismatch of the refresh rate beyond which the sweep is reported as not locked (a 59.94 Hz source
/// on a 60 Hz multiple, 0.1 %, passes; 144 Hz for a 60 Hz sweep of 2, 20 %, does not).
pub const RATIO_TOLERANCE: f64 = 0.02;

impl Plan {
    /// The fraction of the sweep a line is lit: `lit / subframes` (the strobe), `1 / subframes` (the rolling scan).
    pub fn duty(&self) -> f64 {
        f64::from(self.lit) / f64::from(self.subframes)
    }

    /// The factor on the light of a lit line: `1 + boost (1 / duty - 1)`, so 1 with no boost and `1 / duty` with the
    /// full boost, which gives a still picture the same mean light over the sweep.
    pub fn gain(&self) -> f64 {
        1.0 + self.boost * (1.0 / self.duty() - 1.0)
    }

    /// The first line of the band shown at refresh `phase` of the sweep: the bands cut the `lines` raster lines into
    /// `subframes` runs of `lines / subframes` lines, the boundary after each rounded down, the last band ending
    /// at the last line. For the strobe the whole frame is one band.
    pub fn band_start(&self, phase: u32, lines: u32) -> u32 {
        match self.mode {
            Mode::Rolling => {
                let k = phase.min(self.subframes);
                (u64::from(lines) * u64::from(k) / u64::from(self.subframes)) as u32
            }
            _ => 0,
        }
    }

    /// One past the last line of the band shown at refresh `phase` ([`Plan::band_start`] of the next one; `lines`
    /// for the last).
    pub fn band_end(&self, phase: u32, lines: u32) -> u32 {
        match self.mode {
            Mode::Rolling => self.band_start(phase + 1, lines),
            _ => lines,
        }
    }

    /// Whether the frame is lit at refresh `phase`: the strobe's first `lit` refreshes; the rolling scan lights
    /// a band at every refresh.
    pub fn is_lit(&self, phase: u32) -> bool {
        match self.mode {
            Mode::Strobe => phase % self.subframes < self.lit,
            _ => true,
        }
    }

    /// The factor on the light of line `line` of `lines` at refresh `phase` of the sweep: [`Plan::gain`] while the
    /// line is lit, 0 otherwise.
    pub fn line_factor(&self, phase: u32, line: u32, lines: u32) -> f64 {
        let phase = phase % self.subframes;
        if self.is_lit(phase) && (self.band_start(phase, lines)..self.band_end(phase, lines)).contains(&line) {
            self.gain()
        } else {
            0.0
        }
    }

    /// The rows of the tube image that carry the lit band at refresh `phase`, `(first, one past the last)`: the image
    /// is `tube_height` rows, the raster of `lines` lines is centred in it with `margin` rows on each side (the spot's
    /// skirt past the first and last line, which belongs to the first and the last band), and each boundary
    /// between bands lies on a line boundary, rounded to a row. Empty (`first == last`) while the strobe is dark.
    pub fn rows(&self, phase: u32, lines: u32, margin: u32, tube_height: u32) -> (u32, u32) {
        let phase = phase % self.subframes;
        if !self.is_lit(phase) || lines == 0 {
            return (0, 0);
        }
        if self.mode != Mode::Rolling {
            return (0, tube_height);
        }
        let raster = f64::from(tube_height.saturating_sub(2 * margin));
        // The row of the boundary before band `k`: the top of the image for the first, the bottom after the last,
        // else the line boundary rounded to a row (so the bands tile the image even with more bands than lines).
        let edge = |k: u32| {
            if k == 0 {
                0
            } else if k >= self.subframes {
                tube_height
            } else {
                let line = self.band_start(k, lines);
                (margin + (f64::from(line) * raster / f64::from(lines)).round() as u32).min(tube_height)
            }
        };
        (edge(phase), edge(phase + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_number_of_refreshes_follows_the_rates() {
        assert_eq!(subframes(300.0, 60.0, 0), 5);
        assert_eq!(subframes(240.0, 60.0, 0), 4);
        assert_eq!(subframes(120.0, 60.0, 0), 2);
        assert_eq!(subframes(60.0, 60.0, 0), 1);
        assert_eq!(subframes(144.0, 60.0, 0), 2); // 2.4 rounds down: not locked, [`ratio_error`] says so
        assert_eq!(subframes(300.0, 50.0, 0), 6);
        assert_eq!(subframes(300.0, 60.0, 3), 3, "a manual count wins");
        assert_eq!(subframes(300.0, 60.0, 99), MAX_SUBFRAMES);
        assert_eq!(subframes(f64::NAN, 60.0, 0), 1, "an unknown refresh counts as 60 Hz");
        assert_eq!(subframes(0.0, 0.0, 0), 1);
        assert_eq!(subframes(10_000.0, 60.0, 0), MAX_SUBFRAMES);
    }

    #[test]
    fn a_sweep_exists_only_with_a_mode_and_two_refreshes() {
        assert!(plan(Mode::Off, 300.0, 60.0, 0, 1, 1.0).is_none());
        assert!(plan(Mode::Rolling, 60.0, 60.0, 0, 1, 1.0).is_none());
        let p = plan(Mode::Rolling, 300.0, 60.0, 0, 7, f64::NAN).unwrap();
        assert_eq!(
            (p.subframes, p.lit, p.boost),
            (5, 1, 1.0),
            "rolling lights each line for one refresh"
        );
        let s = plan(Mode::Strobe, 240.0, 60.0, 0, 9, 7.0).unwrap();
        assert_eq!(
            (s.subframes, s.lit, s.boost),
            (4, 3, 1.0),
            "the lit count stays below the sweep"
        );
        assert_eq!(plan(Mode::Strobe, 240.0, 60.0, 0, 0, -1.0).unwrap().lit, 1);
        assert_eq!(plan(Mode::Strobe, 240.0, 60.0, 0, 1, -1.0).unwrap().boost, 0.0);
    }

    #[test]
    fn the_ratio_error_flags_a_refresh_that_is_not_a_multiple() {
        assert!(ratio_error(300.0, 60.0, 5) < 1e-12);
        assert!(ratio_error(299.7, 59.94, 5) < RATIO_TOLERANCE);
        assert!(ratio_error(144.0, 60.0, 2) > RATIO_TOLERANCE);
        assert!(ratio_error(165.0, 60.0, 3) > RATIO_TOLERANCE);
        assert_eq!(ratio_error(f64::NAN, 60.0, 5), 1.0);
    }

    /// The bands of a rolling scan partition the lines whatever the count, and each line is lit exactly once per sweep.
    #[test]
    fn the_bands_partition_the_raster() {
        for n in 2..=MAX_SUBFRAMES {
            for lines in [1u32, 2, 7, 16, 17, 100, 224, 240, 243, 480, 1080] {
                let p = plan(Mode::Rolling, 60.0 * f64::from(n), 60.0, 0, 1, 1.0).unwrap();
                assert_eq!(p.subframes, n);
                let mut next = 0;
                for k in 0..n {
                    assert_eq!(p.band_start(k, lines), next, "n {n} lines {lines} band {k}");
                    assert!(p.band_end(k, lines) >= p.band_start(k, lines));
                    next = p.band_end(k, lines);
                }
                assert_eq!(next, lines, "the last band ends at the last line");
                for line in 0..lines {
                    let lit: Vec<u32> = (0..n).filter(|&k| p.line_factor(k, line, lines) > 0.0).collect();
                    assert_eq!(lit.len(), 1, "n {n} lines {lines} line {line}: {lit:?}");
                }
                // Equal bands up to one line.
                let sizes: Vec<u32> = (0..n).map(|k| p.band_end(k, lines) - p.band_start(k, lines)).collect();
                assert!(
                    sizes.iter().max().unwrap() - sizes.iter().min().unwrap() <= 1,
                    "{sizes:?}"
                );
            }
        }
    }

    /// With the full boost the mean over the sweep of what a line shows is what it would show in a still picture, for
    /// both modes, every count; with no boost it is the duty.
    #[test]
    fn the_boost_keeps_the_mean_light() {
        for mode in [Mode::Rolling, Mode::Strobe] {
            for n in 2..=8u32 {
                for lit in 1..n {
                    let full = plan(mode, 60.0 * f64::from(n), 60.0, 0, lit, 1.0).unwrap();
                    let none = plan(mode, 60.0 * f64::from(n), 60.0, 0, lit, 0.0).unwrap();
                    for line in [0u32, 5, 119] {
                        let mean = |p: &Plan| (0..n).map(|k| p.line_factor(k, line, 120)).sum::<f64>() / f64::from(n);
                        assert!((mean(&full) - 1.0).abs() < 1e-12, "{mode:?} n {n} lit {lit}");
                        assert!(
                            (mean(&none) - none.duty()).abs() < 1e-12,
                            "{mode:?} n {n} lit {lit}: {}",
                            mean(&none)
                        );
                    }
                }
            }
        }
        let half = plan(Mode::Rolling, 300.0, 60.0, 0, 1, 0.5).unwrap();
        assert!((half.gain() - 3.0).abs() < 1e-12, "1 + 0.5 (5 - 1)");
    }

    /// The strobe lights the whole frame in its first `lit` refreshes and nothing after.
    #[test]
    fn the_strobe_is_the_whole_frame_then_black() {
        let p = plan(Mode::Strobe, 240.0, 60.0, 0, 1, 1.0).unwrap();
        assert!(
            p.is_lit(0) && !p.is_lit(1) && !p.is_lit(2) && !p.is_lit(3) && p.is_lit(4),
            "the phase wraps"
        );
        assert_eq!(p.rows(0, 240, 6, 300), (0, 300));
        assert_eq!(p.rows(1, 240, 6, 300), (0, 0));
        assert_eq!(p.line_factor(2, 10, 240), 0.0);
        assert!((p.line_factor(0, 10, 240) - 4.0).abs() < 1e-12);
    }

    /// The rows of a band: the first band and the last include the skirt margin, the boundaries are on line
    /// boundaries rounded to rows, the bands tile the image.
    #[test]
    fn the_rows_tile_the_tube_image() {
        for (lines, margin, height) in [(240u32, 6u32, 1000u32), (243, 0, 972), (1080, 4, 1088), (7, 3, 50)] {
            for n in 2..=8u32 {
                let p = plan(Mode::Rolling, 60.0 * f64::from(n), 60.0, 0, 1, 1.0).unwrap();
                let mut next = 0;
                for k in 0..n {
                    let (a, b) = p.rows(k, lines, margin, height);
                    assert_eq!(a, next, "lines {lines} n {n} band {k}");
                    assert!(b >= a);
                    next = b;
                }
                assert_eq!(next, height);
            }
        }
        // Exact boundaries: 240 lines in 1000 - 2 x 6 = 988 rows, 4 bands.
        let p = plan(Mode::Rolling, 240.0, 60.0, 0, 1, 1.0).unwrap();
        assert_eq!(
            p.rows(0, 240, 6, 1000),
            (0, 6 + (60.0f64 * 988.0 / 240.0).round() as u32)
        );
        assert_eq!(p.rows(3, 240, 6, 1000).1, 1000);
    }
}
