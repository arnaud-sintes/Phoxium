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

    /// The same impulse response with the power-law amplitude B scaled by `scale` (the "tail
    /// strength" of T-41; CRT.md T-63: about 0.07 to 1 from the Ni-killer residual ratios, an
    /// assumption [E]). The exponential terms and I(0) normalisation of the fast part are unchanged.
    ///
    /// # Panics
    /// If `scale` is negative or not finite.
    pub fn with_tail_scale(&self, scale: f64) -> Self {
        assert!(scale.is_finite() && scale >= 0.0, "tail scale {scale}");
        Self {
            exp_terms: self.exp_terms,
            tail: self.tail.map(|p| PowerTail { b: p.b * scale, ..p }),
        }
    }

    /// Per-frame trail ratios r_k = E_k / E_0 for k = 1 .. K - 1, K = round(`horizon_ms` /
    /// `frame_ms`) frames in the truncation horizon (index 0 holds k = 1). [D]
    ///
    /// # Panics
    /// Unless 0 < `frame_ms` <= `horizon_ms` < inf, or if a term is invalid.
    pub fn trail_ratios(&self, frame_ms: f64, horizon_ms: f64) -> Vec<f64> {
        assert!(
            frame_ms.is_finite() && frame_ms > 0.0 && horizon_ms.is_finite() && frame_ms <= horizon_ms,
            "frame {frame_ms}, horizon {horizon_ms}"
        );
        let frames = (horizon_ms / frame_ms).round().max(1.0) as u32;
        let e0 = self.energy(0.0, frame_ms);
        (1..frames)
            .map(|k| {
                let k = f64::from(k);
                self.energy(k * frame_ms, (k + 1.0) * frame_ms) / e0
            })
            .collect()
    }

    /// Frame-feedback recurrence (60 Hz persistence, T-41) of this impulse response for a frame
    /// period `frame_ms`, with the trail truncated at `horizon_ms` (CRT.md: 1 s). [D]
    ///
    /// Purely exponential response (no tail, or B = 0): every exponential term is exact, term i
    /// gives rho_i = exp(-T / tau_i) and c_i = A_i tau_i (1 - rho_i) rho_i / E_0 (its E_k / E_0 is
    /// c_i rho_i^(k - 1)); terms with c_i below 1e-15 are dropped (below FP32 resolution), the
    /// [`FEEDBACK_TERMS`] largest are kept.
    ///
    /// With a power-law tail: the trail r_k = E_k / E_0 (all terms, k = 1 .. K - 1) is fitted by
    /// [`FEEDBACK_TERMS`] geometric sequences, exact at k = 1, 2, 3 (a Vandermonde system, the fourth
    /// amplitude free, see [`fit_amplitudes`]), the rates chosen by a grid search then a coordinate
    /// refinement that minimises the mean squared relative error over k = 4 .. K - 1 (plus
    /// `SUM_WEIGHT` times the squared relative error of the total against the 1 s truncated sum, 0:
    /// see its comment); negative amplitudes are rejected (the trail stays positive and decreasing).
    /// Beyond the window the fitted terms keep decaying (soft truncation instead of a hard cut at 1 s).
    ///
    /// Normalisation: w0 = 1 / (1 + sum_j c_j / (1 - rho_j)), the exact steady-state gain 1 of the
    /// realised recurrence.
    ///
    /// # Panics
    /// Unless 0 < `frame_ms` <= `horizon_ms` < inf, or if a term is invalid.
    pub fn frame_feedback(&self, frame_ms: f64, horizon_ms: f64) -> FrameFeedback {
        let r = self.trail_ratios(frame_ms, horizon_ms);
        let has_tail = self.tail.is_some_and(|p| p.b > 0.0);
        let mut fb = FrameFeedback::IDENTITY;
        if !has_tail {
            let e0 = self.energy(0.0, frame_ms);
            let mut terms: Vec<(f64, f64)> = self
                .exp_terms
                .iter()
                .map(|e| {
                    let rho = (-frame_ms / e.tau_ms).exp();
                    (rho, e.a * e.tau_ms * (1.0 - rho) * rho / e0)
                })
                .filter(|&(_, c)| c > 1e-15)
                .collect();
            terms.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (j, &(rho, c)) in terms.iter().take(FEEDBACK_TERMS).enumerate() {
                fb.rho[j] = rho;
                fb.c[j] = c;
            }
        } else if r.len() >= 3 && r[0] > 0.0 {
            let (rho, c) = fit_trail(&r);
            fb.rho = rho;
            fb.c = c;
        }
        fb.w0 = 1.0 / (1.0 + fb.trail_sum());
        fb
    }
}

/// Number of geometric terms of the frame-feedback recurrence (on the GPU: 4 terms x 3 channels in
/// three RGBA32F history images).
pub const FEEDBACK_TERMS: usize = 4;

/// Truncation horizon of the trail in ms: the static-image normalisation uses the integral over
/// the first second (CRT.md "Consequences for T-41"; n close to 1 makes the infinite total
/// diverge or converge very slowly). [D]
pub const TRAIL_HORIZON_MS: f64 = 1000.0;

/// Frame-feedback recurrence of one channel (T-41). With x[m] the frame-integrated excitation
/// of frame m (the no-persistence light) and S_j the history of term j:
///
/// y[m] = w0 (x[m] + sum_j S_j[m]),  S_j[m + 1] = rho_j S_j[m] + c_j x[m],  S_j = 0 at reset.
///
/// Unrolled, y[m] = w0 (x[m] + sum_{k >= 1} t_k x[m - k]) with t_k = sum_j c_j rho_j^(k - 1)
/// ([`FrameFeedback::trail`]), the fitted E_k / E_0; a constant x gives y = w0 (1 + sum_k t_k) x = x.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameFeedback {
    /// Weight of the current frame (and of the whole recurrence).
    pub w0: f64,
    /// Per-frame decay ratio of each term, in [0, 1).
    pub rho: [f64; FEEDBACK_TERMS],
    /// Amplitude of each term relative to E_0 (its contribution to t_1).
    pub c: [f64; FEEDBACK_TERMS],
}

impl FrameFeedback {
    /// No persistence: y = x.
    pub const IDENTITY: Self = Self {
        w0: 1.0,
        rho: [0.0; FEEDBACK_TERMS],
        c: [0.0; FEEDBACK_TERMS],
    };

    /// Realised trail t_k = y_k / y_0 after a single excitation, k >= 1.
    pub fn trail(&self, k: u32) -> f64 {
        assert!(k >= 1, "trail index starts at 1");
        self.rho
            .iter()
            .zip(self.c)
            .map(|(&rho, c)| c * rho.powi(k as i32 - 1))
            .sum()
    }

    /// Total realised trail sum_{k >= 1} t_k = sum_j c_j / (1 - rho_j).
    pub fn trail_sum(&self) -> f64 {
        self.rho.iter().zip(self.c).map(|(&rho, c)| c / (1.0 - rho)).sum()
    }
}

/// Amplitudes c of three geometric terms with distinct rates `rho`, exact at the first three
/// samples: sum_j c_j rho_j^(k - 1) = r_k, k = 1, 2, 3. Lagrange form of the Vandermonde solve:
/// c_j = (r_3 - (rho_a + rho_b) r_2 + rho_a rho_b r_1) / ((rho_j - rho_a)(rho_j - rho_b)), with a, b
/// the two other terms.
fn vandermonde3(rho: [f64; 3], r: [f64; 3]) -> [f64; 3] {
    let mut c = [0.0; 3];
    for j in 0..3 {
        let (a, b) = (rho[(j + 1) % 3], rho[(j + 2) % 3]);
        c[j] = (r[2] - (a + b) * r[1] + a * b * r[0]) / ((rho[j] - a) * (rho[j] - b));
    }
    c
}

/// Best amplitudes of four geometric terms with distinct rates `rho` for the trail `r` (r[0] =
/// t_1), exact at k = 1, 2, 3, and their cost: mean squared relative error over k = 4 .. K - 1
/// plus the squared relative error of the total trail energy against `total`.
///
/// The exact amplitudes form a line c(alpha) = c_p + alpha n: c_p solves the first three terms
/// alone (c_p[3] = 0, [`vandermonde3`]), and n_j = 1 / prod_{m != j} (rho_j - rho_m) spans the null
/// space of the 3 x 4 Vandermonde rows rho^0, rho^1, rho^2 (the third divided difference of a
/// polynomial of degree 2 vanishes). The cost is quadratic in alpha: its minimum, clamped to the
/// interval where every amplitude is >= 0. `None` when the rates coincide or no alpha keeps the
/// amplitudes non-negative.
fn fit_amplitudes(rho: [f64; 4], r: &[f64], total: f64) -> Option<(f64, [f64; 4])> {
    for i in 0..4 {
        for j in i + 1..4 {
            if (rho[i] - rho[j]).abs() < 1e-9 {
                return None;
            }
        }
    }
    let p3 = vandermonde3([rho[0], rho[1], rho[2]], [r[0], r[1], r[2]]);
    let cp = [p3[0], p3[1], p3[2], 0.0];
    let n: [f64; 4] =
        std::array::from_fn(|j| 1.0 / (0..4).filter(|&m| m != j).map(|m| rho[j] - rho[m]).product::<f64>());
    // Quadratic cost a2 alpha^2 + 2 a1 alpha + a0 (a0 not needed for the minimiser).
    let (mut a2, mut a1) = (0.0, 0.0);
    let weight = 1.0 / (r.len().saturating_sub(3).max(1) as f64);
    let mut pow = rho.map(|x| x * x * x);
    for &target in &r[3..] {
        let tp: f64 = (0..4).map(|j| cp[j] * pow[j]).sum();
        let tn: f64 = (0..4).map(|j| n[j] * pow[j]).sum();
        let w = weight / (target * target);
        a2 += w * tn * tn;
        a1 += w * (tp - target) * tn;
        for j in 0..4 {
            pow[j] *= rho[j];
        }
    }
    let sp: f64 = (0..4).map(|j| cp[j] / (1.0 - rho[j])).sum();
    let sn: f64 = (0..4).map(|j| n[j] / (1.0 - rho[j])).sum();
    a2 += SUM_WEIGHT * sn * sn / (total * total);
    a1 += SUM_WEIGHT * (sp - total) * sn / (total * total);
    let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
    for j in 0..4 {
        if n[j] > 0.0 {
            lo = lo.max(-cp[j] / n[j]);
        } else if n[j] < 0.0 {
            hi = hi.min(-cp[j] / n[j]);
        } else if cp[j] < 0.0 {
            return None;
        }
    }
    if lo.is_nan() || hi.is_nan() || lo > hi {
        return None;
    }
    let alpha = if a2 > 0.0 {
        (-a1 / a2).clamp(lo, hi)
    } else {
        lo.max(0.0).min(hi)
    };
    let c: [f64; 4] = std::array::from_fn(|j| (cp[j] + alpha * n[j]).max(0.0));
    if c.iter().any(|x| !x.is_finite()) {
        return None;
    }
    Some((trail_cost(rho, c, r, total), c))
}

/// Mean squared relative error of the trail over k = 4 .. K - 1 plus the squared relative error
/// of the total (see [`fit_amplitudes`]).
fn trail_cost(rho: [f64; 4], c: [f64; 4], r: &[f64], total: f64) -> f64 {
    let mut pow = rho.map(|x| x * x * x);
    let mut err = 0.0;
    for &target in &r[3..] {
        let fit: f64 = (0..4).map(|j| c[j] * pow[j]).sum();
        err += ((fit - target) / target).powi(2);
        for j in 0..4 {
            pow[j] *= rho[j];
        }
    }
    let sum: f64 = (0..4).map(|j| c[j] / (1.0 - rho[j])).sum();
    err / (r.len().saturating_sub(3).max(1) as f64) + SUM_WEIGHT * ((sum - total) / total).powi(2)
}

/// Weight of the total-energy error in the fit cost: 0, the fit follows the trail inside the
/// horizon only. A sum of exponentials cannot stop at 1 s: forcing its total to the truncated sum
/// costs about 16 % relative error on the last frames of the window, while the unconstrained fit is
/// within about 1 % over the whole window and rolls off smoothly after it (its total exceeds the
/// truncated sum by 13 to 17 %, see the tests). The steady state stays exactly 1 either way.
const SUM_WEIGHT: f64 = 0.0;

/// Four-term fit of the trail `r` (see [`DecayFit::frame_feedback`]): grid search of the time
/// constants kappa (frames, rho = exp(-1 / kappa), log-spaced from 0.1 to 2 K), then coordinate
/// refinement in log space with a shrinking step.
fn fit_trail(r: &[f64]) -> ([f64; 4], [f64; 4]) {
    let total: f64 = r.iter().sum();
    let k_max = 2.0 * (r.len() + 1) as f64;
    const N: usize = 24;
    let grid: Vec<f64> = (0..N)
        .map(|i| 0.1 * (k_max / 0.1).powf(i as f64 / (N - 1) as f64))
        .collect();
    let eval = |kappa: [f64; 4]| fit_amplitudes(kappa.map(|k| (-1.0 / k).exp()), r, total);
    let mut best: Option<(f64, [f64; 4])> = None;
    for a in 0..N {
        for b in a + 1..N {
            for c in b + 1..N {
                for d in c + 1..N {
                    let kappa = [grid[a], grid[b], grid[c], grid[d]];
                    if let Some((cost, _)) = eval(kappa) {
                        if best.is_none_or(|(b, _)| cost < b) {
                            best = Some((cost, kappa));
                        }
                    }
                }
            }
        }
    }
    let Some((mut cost, mut kappa)) = best else {
        // No non-negative four-term solution: one geometric term through t_1 and t_2.
        let rho = (r[1] / r[0]).clamp(0.0, 0.999);
        return ([rho, 0.0, 0.0, 0.0], [r[0], 0.0, 0.0, 0.0]);
    };
    let mut step = (k_max / 0.1).powf(1.0 / (N - 1) as f64);
    for _ in 0..200 {
        let mut improved = false;
        for j in 0..4 {
            for f in [step, 1.0 / step] {
                let mut k = kappa;
                k[j] *= f;
                if let Some((c, _)) = eval(k) {
                    if c < cost {
                        (cost, kappa, improved) = (c, k, true);
                    }
                }
            }
        }
        if !improved {
            step = step.sqrt();
            if step < 1.0 + 1e-6 {
                break;
            }
        }
    }
    let (_, c) = eval(kappa).expect("the refined fit is valid");
    (kappa.map(|k| (-1.0 / k).exp()), c)
}

// ---------------------------------------------------------------------------------------------------
// Saturation (T-40)
// ---------------------------------------------------------------------------------------------------

/// Default saturation strengths s of red, green and blue (per unit of white light), used when the
/// saturation is switched on: sulfides (blue ZnS:Ag, green ZnS:Cu,Al) saturate more than the oxysulfide
/// red (CRT.md "Saturation and ageing"). The values are estimates sized for "a few percent" of warm shift
/// of a white highlight (white 1 gives 0.990, 0.971, 0.943, blue / red 0.953). [E]
pub const SATURATION_DEFAULT: [f64; 3] = [0.01, 0.03, 0.06];

/// Light output of a phosphor of saturation strength `s` for the light `light` it would emit without
/// saturation (linear light, 1 = the reference white drive of that channel):
///
/// L = f(I) = I / (1 + s I)
///
/// the efficiency droop at high current density written as a single-parameter rational curve (an [E]
/// choice: CRT.md gives the effect qualitatively, no measured curve). Properties: f(0) = 0, f'(0) = 1
/// (the small-signal gain is 1, so dark and mid tones are unchanged to first order), f' = 1 / (1 + s I)^2
/// is positive (strictly monotonic), f is below 1 / s and tends to 1 / s for large I (a finite maximum
/// light). The relative loss is 1 - f / I = s I / (1 + s I): 1 % at s I = 0.0101. s = 0 is the exact identity. [D]
///
/// The model applies it per phosphor element to the light of the instantaneous beam current; the
/// renderer applies it to the area-averaged light per tube pixel (see `shaders/phosphor.comp`).
///
/// # Panics
/// If `s` is negative or not finite, or `light` is negative or not finite.
pub fn saturate(light: f64, s: f64) -> f64 {
    assert!(s.is_finite() && s >= 0.0, "saturation strength {s}");
    assert!(light.is_finite() && light >= 0.0, "light {light}");
    light / (1.0 + s * light)
}

/// Derivative f'(I) = 1 / (1 + s I)^2 of [`saturate`]. [D]
///
/// # Panics
/// As [`saturate`].
pub fn saturate_gain(light: f64, s: f64) -> f64 {
    assert!(s.is_finite() && s >= 0.0, "saturation strength {s}");
    assert!(light.is_finite() && light >= 0.0, "light {light}");
    1.0 / ((1.0 + s * light) * (1.0 + s * light))
}

/// Saturation of the three channels of a light triple. [D]
///
/// # Panics
/// As [`saturate`].
pub fn saturate_rgb(light: [f64; 3], s: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|c| saturate(light[c], s[c]))
}

#[cfg(test)]
mod saturation_tests {
    use super::*;
    use crate::close;

    /// Values derived by hand. s = 0.25: f(1) = 1 / 1.25 = 0.8, f(2) = 2 / 1.5 = 4/3, f(4) = 4 / 2 = 2,
    /// f(0.5) = 0.5 / 1.125 = 4/9. s = 0.5: f(1) = 2/3, f(2) = 1, f(4) = 4/3, f(0.5) = 0.5 / 1.25 = 0.4.
    /// s = 1: f(1) = 1/2, f(2) = 2/3, f(4) = 4/5, f(0.5) = 0.5 / 1.5 = 1/3.
    #[test]
    fn hand_derived_values() {
        let table = [
            // (s, [(I, f)])
            (0.25, [(0.5, 4.0 / 9.0), (1.0, 0.8), (2.0, 4.0 / 3.0), (4.0, 2.0)]),
            (0.5, [(0.5, 0.4), (1.0, 2.0 / 3.0), (2.0, 1.0), (4.0, 4.0 / 3.0)]),
            (1.0, [(0.5, 1.0 / 3.0), (1.0, 0.5), (2.0, 2.0 / 3.0), (4.0, 0.8)]),
        ];
        for (s, rows) in table {
            for (i, f) in rows {
                assert!(close(saturate(i, s), f, 1e-15), "s {s} I {i}");
            }
        }
        // The default white: 1 / 1.01, 1 / 1.03, 1 / 1.06.
        let w = saturate_rgb([1.0; 3], SATURATION_DEFAULT);
        assert!(close(w[0], 0.990_099_009_9, 1e-9));
        assert!(close(w[1], 0.970_873_786_4, 1e-9));
        assert!(close(w[2], 0.943_396_226_4, 1e-9));
        // Blue relative to red: 1.01 / 1.06 = 0.952830..., a 4.717 % warm shift.
        assert!(close(w[2] / w[0], 1.01 / 1.06, 1e-12));
    }

    /// f(0) = 0, f'(0) = 1 (analytic and by a central difference of f), s = 0 is the identity, f is
    /// strictly increasing and bounded by 1 / s.
    #[test]
    fn small_signal_gain_monotonic_bounded() {
        for s in [0.0, 0.01, 0.06, 0.5, 1.0] {
            assert_eq!(saturate(0.0, s), 0.0);
            assert_eq!(saturate_gain(0.0, s), 1.0);
            let h = 1e-6;
            let numeric = (saturate(h, s) - saturate(0.0, s)) / h;
            assert!(close(numeric, 1.0, 2.0 * s * h + 1e-9), "s {s}: {numeric}");
            let mut prev = 0.0;
            for k in 1..=2000 {
                let i = f64::from(k) * 0.05;
                let f = saturate(i, s);
                assert!(f > prev, "s {s} I {i}");
                if s > 0.0 {
                    assert!(f < 1.0 / s, "bounded by 1 / s");
                }
                // The analytic gain agrees with a central difference.
                let d = (saturate(i + 1e-5, s) - saturate(i - 1e-5, s)) / 2e-5;
                assert!(close(d, saturate_gain(i, s), 1e-8), "s {s} I {i}");
                prev = f;
            }
            assert_eq!(saturate(3.7, 0.0), 3.7);
        }
    }

    /// Blue loses more than green, green more than red, at the same light above zero.
    #[test]
    fn default_strengths_order_the_channels() {
        let [r, g, b] = SATURATION_DEFAULT;
        assert!(r < g && g < b);
        for i in [0.5, 1.0, 2.0, 10.0] {
            assert!(saturate(i, b) < saturate(i, g) && saturate(i, g) < saturate(i, r));
        }
    }

    #[test]
    #[should_panic(expected = "saturation strength")]
    fn negative_strength_panics() {
        let _ = saturate(1.0, -0.1);
    }

    #[test]
    #[should_panic(expected = "light")]
    fn negative_light_panics() {
        let _ = saturate(-1.0, 0.1);
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

    /// T-41 frame feedback of the Kuhn green and blue fits (tail strength 1 and 0.07) at 50, 60 and
    /// 120 Hz: the realised trail equals the closed-form E_k / E_0 at k = 1, 2, 3 (1e-9 relative),
    /// stays within 3 % of it from k = 4 to the end of the 1 s window (measured 1.0 to 1.2 % at 50 and
    /// 60 Hz, 2.5 % at 120 Hz), and the
    /// steady state is exactly 1. Recorded deviation (design choice, see `SUM_WEIGHT`): the fitted
    /// tail continues after 1 s, so its total exceeds the 1 s truncated sum by 11 to 17 % and w0 is
    /// 1.5 to 3.5 % below the truncated-integral value 1 - (fraction after the first frame)
    /// (green 60 Hz: 0.7628 against 0.7881); bounds 20 % and 5 %.
    ///
    /// Tail strength s scales only B: E_k for k >= 1 is s times the tail part (the exponential terms
    /// are 0 after one frame at these rates), E_0 = E0_exp + s E0_tail, so the trail shape, hence the
    /// fitted rates, do not depend on s and the amplitudes scale with r_1.
    #[test]
    fn frame_feedback_kuhn_tails() {
        for hz in [50.0, 60.0, 120.0] {
            let t = frame_period_ms(hz);
            for (name, fit) in [("green", KUHN_GREEN), ("blue", KUHN_BLUE)] {
                for s in [1.0, 0.07] {
                    let f = fit.with_tail_scale(s);
                    let fb = f.frame_feedback(t, TRAIL_HORIZON_MS);
                    let r = f.trail_ratios(t, TRAIL_HORIZON_MS);
                    let oracle = f.frame_energy_ratios(t);
                    for k in 1..=3u32 {
                        let o = oracle[k as usize - 1];
                        assert!(
                            rel(fb.trail(k), o) < 1e-9,
                            "{name} {hz} Hz s {s} k {k}: {} vs {o}",
                            fb.trail(k)
                        );
                    }
                    let max_err = (4..r.len() as u32 + 1)
                        .map(|k| rel(fb.trail(k), r[k as usize - 1]))
                        .fold(0.0, f64::max);
                    let total: f64 = r.iter().sum();
                    let sum_err = rel(fb.trail_sum(), total);
                    let w0_oracle = 1.0 - f.fraction_after_first_frame(t, t * (r.len() + 1) as f64);
                    println!(
                        "{name} {hz} Hz s {s}: w0 {:.5} (oracle {w0_oracle:.5}) rho {:?} c {:?} max rel err k>=4 {max_err:.4} sum err {sum_err:.5}",
                        fb.w0, fb.rho, fb.c
                    );
                    assert!(close(fb.w0 * (1.0 + fb.trail_sum()), 1.0, 1e-12));
                    assert!(max_err < 0.03, "{name} {hz} Hz s {s}: fit error {max_err}");
                    assert!(sum_err < 0.20, "{name} {hz} Hz s {s}: total error {sum_err}");
                    assert!(
                        rel(fb.w0, w0_oracle) < 0.05,
                        "{name} {hz} Hz: w0 {} vs {w0_oracle}",
                        fb.w0
                    );
                }
            }
        }
    }

    /// Red has no tail: its exponential terms are exact (t_1 = E_1 / E_0 below 1e-6 at 60 Hz).
    #[test]
    fn frame_feedback_red_is_exact() {
        for hz in [50.0, 60.0, 120.0] {
            let t = frame_period_ms(hz);
            let fb = KUHN_RED.frame_feedback(t, TRAIL_HORIZON_MS);
            let o = KUHN_RED.frame_energy_ratios(t);
            println!("red {hz} Hz: {fb:?}, oracle {o:?}");
            // E_1 / E_0 is about 4e-17 at 60 Hz: below FP32 resolution, the terms are dropped (1e-15).
            assert!((fb.trail(1) - o[0]).abs() < 1e-15, "{} vs {}", fb.trail(1), o[0]);
            assert!(fb.trail(2) <= 1e-15 && fb.w0 > 1.0 - 1e-8);
        }
        assert!(
            KUHN_RED
                .frame_feedback(frame_period_ms(60.0), TRAIL_HORIZON_MS)
                .trail(1)
                < 1e-6
        );
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
