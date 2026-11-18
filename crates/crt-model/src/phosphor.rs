//! Phosphor persistence: multi-term decay law and per-frame energy.
//!
//! Impulse response per colour (CRT.md "Decay and persistence"):
//! I(t) = sum_i A_i exp(-t / tau_i) + B (1 + t / t0)^-n, normalised to I(0) = 1.
//!
//! Invalid input (non-positive tau or t0, negative n, negative or reversed
//! time intervals, non-finite values) panics: see the `# Panics` sections.

/// One exponential term A exp(-t / tau).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExpTerm {
    /// Amplitude (fraction of I(0)).
    pub a: f64,
    /// Time constant in milliseconds, > 0.
    pub tau_ms: f64,
}

/// Power-law tail B (1 + t / t0)^-n.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PowerTail {
    /// Amplitude at t = 0 (fraction of I(0)).
    pub b: f64,
    /// Time offset in milliseconds, > 0.
    pub t0_ms: f64,
    /// Exponent, >= 0. n <= 1 gives infinite total energy (finite intervals only).
    pub n: f64,
}

/// Phosphor impulse response: a sum of exponentials plus an optional power-law tail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecayFit<'a> {
    /// Exponential terms.
    pub exp_terms: &'a [ExpTerm],
    /// Power-law tail, `None` for a purely exponential phosphor.
    pub tail: Option<PowerTail>,
}

const fn us(a: f64, tau_us: f64) -> ExpTerm {
    ExpTerm {
        a,
        tau_ms: tau_us / 1000.0,
    }
}

/// Red Y2O2S:Eu, Kuhn (2002) P22 fit normalised to I(0) = 1, no tail. [S][D]
pub const KUHN_RED: DecayFit<'static> = DecayFit {
    exp_terms: &[
        us(0.0741, 442.0),
        us(0.0324, 99.5),
        us(0.0370, 19.9),
        us(0.0417, 6.37),
        us(0.2778, 0.227),
        us(0.5370, 0.0227),
    ],
    tail: None,
};

/// Green ZnS:Cu,Al, Kuhn (2002) P22 fit normalised to I(0) = 1. [S][D]
pub const KUHN_GREEN: DecayFit<'static> = DecayFit {
    exp_terms: &[us(0.1042, 1.061), us(0.2815, 0.227), us(0.2534, 0.0318)],
    tail: Some(PowerTail {
        b: 0.3609,
        t0_ms: 0.0055,
        n: 1.1,
    }),
};

/// Blue ZnS:Ag, Kuhn (2002) P22 fit normalised to I(0) = 1. [S][D]
pub const KUHN_BLUE: DecayFit<'static> = DecayFit {
    exp_terms: &[us(0.0323, 1.592), us(0.4309, 0.145), us(0.4740, 0.0398)],
    tail: Some(PowerTail {
        b: 0.0627,
        t0_ms: 0.005,
        n: 1.11,
    }),
};

impl DecayFit<'_> {
    fn check(&self) {
        for e in self.exp_terms {
            assert!(
                e.a.is_finite() && e.tau_ms.is_finite() && e.tau_ms > 0.0,
                "exponential term needs finite A and tau > 0: {e:?}"
            );
        }
        if let Some(p) = self.tail {
            assert!(
                p.b.is_finite() && p.t0_ms.is_finite() && p.t0_ms > 0.0 && p.n.is_finite() && p.n >= 0.0,
                "power tail needs finite B, t0 > 0 and n >= 0: {p:?}"
            );
        }
    }

    /// Intensity at `t_ms` >= 0, relative to I(0).
    ///
    /// # Panics
    /// If `t_ms` is negative or not finite, or a term is invalid.
    pub fn intensity(&self, t_ms: f64) -> f64 {
        self.check();
        assert!(t_ms.is_finite() && t_ms >= 0.0, "t = {t_ms} ms");
        let exp: f64 = self.exp_terms.iter().map(|e| e.a * (-t_ms / e.tau_ms).exp()).sum();
        exp + self.tail.map_or(0.0, |p| p.b * (1.0 + t_ms / p.t0_ms).powf(-p.n))
    }

    /// Closed-form integral of [`Self::intensity`] over [t1_ms, t2_ms] (ms units).
    /// n = 1 uses the logarithmic primitive.
    ///
    /// # Panics
    /// Unless 0 <= `t1_ms` <= `t2_ms` < inf, or if a term is invalid.
    pub fn energy(&self, t1_ms: f64, t2_ms: f64) -> f64 {
        self.check();
        assert!(
            t1_ms.is_finite() && t2_ms.is_finite() && 0.0 <= t1_ms && t1_ms <= t2_ms,
            "interval [{t1_ms}, {t2_ms}] ms"
        );
        let exp: f64 = self
            .exp_terms
            .iter()
            .map(|e| e.a * e.tau_ms * ((-t1_ms / e.tau_ms).exp() - (-t2_ms / e.tau_ms).exp()))
            .sum();
        let tail = self.tail.map_or(0.0, |p| {
            let (u1, u2) = (1.0 + t1_ms / p.t0_ms, 1.0 + t2_ms / p.t0_ms);
            if (p.n - 1.0).abs() < 1e-12 {
                p.b * p.t0_ms * (u2.ln() - u1.ln())
            } else {
                p.b * p.t0_ms / (1.0 - p.n) * (u2.powf(1.0 - p.n) - u1.powf(1.0 - p.n))
            }
        });
        exp + tail
    }

    /// Per-frame energy ratios E_k / E_0 (k = 1, 2, 3) for one excitation at
    /// t = 0, frame k covering [k T, (k + 1) T]. [D]
    ///
    /// # Panics
    /// If `frame_ms` is not finite and positive, or a term is invalid.
    pub fn frame_energy_ratios(&self, frame_ms: f64) -> [f64; 3] {
        assert!(frame_ms.is_finite() && frame_ms > 0.0, "frame = {frame_ms} ms");
        let e = |k: f64| self.energy(k * frame_ms, (k + 1.0) * frame_ms);
        let e0 = e(0.0);
        [e(1.0) / e0, e(2.0) / e0, e(3.0) / e0]
    }

    /// Fraction of the light emitted within [0, `horizon_ms`] that arrives
    /// after the first frame: 1 - E[0, T] / E[0, horizon]. CRT.md normalises
    /// over a 1 s horizon (the n close to 1 tail has a slowly converging or
    /// infinite total). [D]
    ///
    /// # Panics
    /// Unless 0 < `frame_ms` <= `horizon_ms` < inf, or if a term is invalid.
    pub fn fraction_after_first_frame(&self, frame_ms: f64, horizon_ms: f64) -> f64 {
        assert!(
            frame_ms > 0.0 && frame_ms <= horizon_ms,
            "frame {frame_ms}, horizon {horizon_ms}"
        );
        1.0 - self.energy(0.0, frame_ms) / self.energy(0.0, horizon_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;
    use crate::units::frame_period_ms;

    /// Midpoint rule on geometrically growing segments (the fastest red term
    /// is 22.7 ns), independent of the closed form.
    fn numeric_energy(fit: &DecayFit, t1: f64, t2: f64) -> f64 {
        let mut edges = vec![t1];
        let mut h = 1e-7;
        while t1 + h < t2 {
            edges.push(t1 + h);
            h *= 10.0;
        }
        edges.push(t2);
        edges
            .windows(2)
            .map(|w| {
                let steps = 20_000;
                let dt = (w[1] - w[0]) / f64::from(steps);
                (0..steps)
                    .map(|i| fit.intensity(w[0] + (f64::from(i) + 0.5) * dt) * dt)
                    .sum::<f64>()
            })
            .sum()
    }

    fn rel(a: f64, b: f64) -> f64 {
        ((a - b) / b).abs()
    }

    /// Kuhn constants sum to I(0) = 1 (table rounding: 4 decimals per term).
    #[test]
    fn kuhn_fits_normalised() {
        for fit in [KUHN_RED, KUHN_GREEN, KUHN_BLUE] {
            assert!(close(fit.intensity(0.0), 1.0, 2e-4), "{fit:?}");
        }
    }

    /// Closed form against the numerical integral, every channel and frame.
    #[test]
    fn energy_closed_form_matches_numeric() {
        let t = frame_period_ms(60.0);
        for fit in [KUHN_RED, KUHN_GREEN, KUHN_BLUE] {
            for k in 0..4 {
                let (a, b) = (f64::from(k) * t, f64::from(k + 1) * t);
                let (closed, num) = (fit.energy(a, b), numeric_energy(&fit, a, b));
                if closed > 1e-12 {
                    assert!(rel(closed, num) < 1e-6, "frame {k}: {closed} vs {num}");
                } else {
                    assert!(num < 1e-12, "frame {k}: {num}");
                }
            }
        }
        // n = 1 logarithmic branch against the numerical integral.
        let log_fit = DecayFit {
            exp_terms: &[],
            tail: Some(PowerTail {
                b: 1.0,
                t0_ms: 0.005,
                n: 1.0,
            }),
        };
        let (closed, num) = (log_fit.energy(0.0, 16.0), numeric_energy(&log_fit, 0.0, 16.0));
        assert!(rel(closed, num) < 1e-6, "{closed} vs {num}");
        assert!(close(closed, 0.005 * (1.0 + 16.0 / 0.005_f64).ln(), 1e-12));
    }

    /// CRT.md table, 60 Hz: green 5.36 / 2.97 / 2.03 %, blue 4.72 / 2.60 /
    /// 1.78 %, red below 1e-6. Table values are rounded to 0.01 %, so each
    /// ratio must round to them (|x - table| <= 0.005 %); the full-precision
    /// closed-form values are pinned at 1e-6 relative as a regression record.
    #[test]
    fn frame_energy_ratios_60hz() {
        let t = frame_period_ms(60.0);
        let g = KUHN_GREEN.frame_energy_ratios(t);
        let b = KUHN_BLUE.frame_energy_ratios(t);
        let r = KUHN_RED.frame_energy_ratios(t);
        println!("green {g:?}, blue {b:?}, red {r:?}");
        for (got, table) in g.iter().zip([5.36, 2.97, 2.03]).chain(b.iter().zip([4.72, 2.60, 1.78])) {
            assert!(
                close(got * 100.0, table, 0.005 + 1e-9),
                "{} % vs {table} %",
                got * 100.0
            );
        }
        assert!(r[0] < 1e-6 && r[1] < 1e-12 && r[2] < 1e-12, "red {r:?}");
        for (got, pinned) in g.iter().chain(b.iter()).zip(PINNED_60HZ) {
            assert!(rel(*got, pinned) < 1e-6, "{got} vs pinned {pinned}");
        }
    }

    /// Closed-form E_k/E_0 at 60 Hz, green then blue, k = 1..3 (the closed
    /// form is cross-checked by `energy_closed_form_matches_numeric`; rounded
    /// to 0.01 % they are the CRT.md table). Against the rounded table the
    /// relative gap reaches 2.5e-3 (1.7756 vs 1.78 %), hence this pin.
    const PINNED_60HZ: [f64; 6] = [
        0.053_571_178_452_503_7,
        0.029_661_645_938_471_6,
        0.020_328_600_213_716_1,
        0.047_215_673_474_741_0,
        0.025_998_517_400_259_0,
        0.017_756_238_841_610_5,
    ];

    /// CRT.md 50 and 120 Hz rows: green / blue E1/E0 5.19 / 4.57 % and 6.09 / 5.37 %.
    #[test]
    fn frame_energy_ratios_50_120hz() {
        for (hz, g1, b1) in [(50.0, 5.19, 4.57), (120.0, 6.09, 5.37)] {
            let t = frame_period_ms(hz);
            let (g, b) = (
                KUHN_GREEN.frame_energy_ratios(t)[0],
                KUHN_BLUE.frame_energy_ratios(t)[0],
            );
            assert!(close(g * 100.0, g1, 0.005 + 1e-9), "{hz} Hz green {}", g * 100.0);
            assert!(close(b * 100.0, b1, 0.005 + 1e-9), "{hz} Hz blue {}", b * 100.0);
        }
    }

    /// CRT.md: about 21 % (green) and 19 % (blue) of the light emitted within
    /// 1 s arrives after the first 60 Hz frame; red has 91 % of its energy
    /// within 1 ms.
    #[test]
    fn one_second_normalisation() {
        let t = frame_period_ms(60.0);
        let g = KUHN_GREEN.fraction_after_first_frame(t, 1000.0);
        let b = KUHN_BLUE.fraction_after_first_frame(t, 1000.0);
        println!("after first frame: green {g}, blue {b}");
        assert!(close(g, 0.21, 0.005), "green {g}");
        assert!(close(b, 0.19, 0.005), "blue {b}");
        let red_1ms = KUHN_RED.energy(0.0, 1.0) / KUHN_RED.energy(0.0, t);
        assert!(close(red_1ms, 0.91, 0.005), "red within 1 ms {red_1ms}");
    }

    /// Regression record of the superseded CRT.md green fit (A = 1, tau =
    /// 0.05 ms, B = 0.046, t0 = 1 ms, n = 0.72): it gives E1/E0 = 29.7 % at
    /// 60 Hz (CRT.md "Superseded (T-63)"), the figure T-14 reported. Do not use.
    #[test]
    fn superseded_green_fit_gives_29_7_percent() {
        let old = DecayFit {
            exp_terms: &[ExpTerm { a: 1.0, tau_ms: 0.05 }],
            tail: Some(PowerTail {
                b: 0.046,
                t0_ms: 1.0,
                n: 0.72,
            }),
        };
        let g = old.frame_energy_ratios(frame_period_ms(60.0));
        assert!(close(g[0] * 100.0, 29.7, 0.05), "old green E1/E0 = {} %", g[0] * 100.0);
    }

    #[test]
    #[should_panic(expected = "tau > 0")]
    fn zero_tau_panics() {
        let fit = DecayFit {
            exp_terms: &[ExpTerm { a: 1.0, tau_ms: 0.0 }],
            tail: None,
        };
        fit.energy(0.0, 1.0);
    }

    #[test]
    #[should_panic(expected = "t0 > 0")]
    fn zero_t0_panics() {
        let fit = DecayFit {
            exp_terms: &[],
            tail: Some(PowerTail {
                b: 1.0,
                t0_ms: 0.0,
                n: 1.1,
            }),
        };
        fit.intensity(1.0);
    }

    #[test]
    #[should_panic(expected = "interval")]
    fn reversed_interval_panics() {
        KUHN_GREEN.energy(2.0, 1.0);
    }

    #[test]
    #[should_panic(expected = "frame")]
    fn zero_frame_panics() {
        KUHN_GREEN.frame_energy_ratios(0.0);
    }
}
