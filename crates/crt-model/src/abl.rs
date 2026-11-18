//! Automatic beam limiting (ABL) and raster breathing (EHT sag), T-35, QUESTIONS PHY-13.
//!
//! Model (CRT.md "ABL, blooming and EHT regulation"):
//!
//! - Frame APL `a`: mean over the raster of the three beam currents, (I_r + I_g + I_b) / 3, relative
//!   (1 = full-field white at full drive), of the unlimited current (gun output). The anode current is
//!   `a * I_fw` with `I_fw` the anode current of full-field white (mA). [E]
//! - Low-pass in time with real frame times: `x_n = x_(n-1) + alpha_n (a_n - x_(n-1))`,
//!   `alpha_n = 1 - exp(-dt_n / tau)`, `x_0 = 0` (cold start on black). `tau = 0` follows `a` at once,
//!   `dt = 0` keeps `x`. For a step of `a` from 0 to 1 at constant `dt`: `x_n = 1 - exp(-n dt / tau)`,
//!   63.2 % at `t = tau`. [D]
//! - Gain `g = 1 / (1 + k max(0, x I_fw - I_th))` (CRT.md model line), with `k` in 1/mA derived from the
//!   configured full-white ratio `r` (full-field white over a small window below the threshold):
//!   `k = (1/r - 1) / (I_fw - I_th)`, so that `x = 1` gives `g = r` exactly; when `I_fw <= I_th` the
//!   limiter never acts (`g = 1`). The gain of frame `n + 1` uses `x_n` (one frame of latency).
//! - Raster scale `s = 1 + b (x - a_ref)`: positive `b` enlarges the raster with APL (EHT sag; raster
//!   size goes as 1/sqrt(V_anode)). `b = 0` gives exactly 1.
//!
//! Default constants (params/abl.toml), derived from CRT.md:
//! - `I_th = 1.4` mA [S] (RCA, US 5,313,294).
//! - `tau = 0.5 / 3 = 0.1667` s [D]: a first-order filter is within 5 % of its final value at 3 tau, so
//!   the 0.5 s settling of US 4,330,792 gives tau = 0.1667 s.
//! - `I_fw = 2.8` mA [D/E]: with the step 0 -> 1 the anode current `I_fw (1 - exp(-t / tau))` crosses
//!   `I_th` at `t = -tau ln(1 - I_th / I_fw) = tau ln 2 = 0.1155` s, the 0.1 s onset of US 4,330,792.
//! - `r = 0.5` [E] (CRT.md: full white 30 to 60 % of a small window). With `I_fw = 2.8` and
//!   `I_th = 1.4`: `k = (2 - 1) / 1.4 = 0.714286` per mA.

/// ABL and breathing constants (units in the field names).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Abl {
    /// Anode current of full-field white without limiting, mA.
    pub full_white_ma: f64,
    /// Average anode current above which the limiter acts, mA.
    pub threshold_ma: f64,
    /// Full-field white luminance over the luminance of a small window below the threshold.
    pub full_white_ratio: f64,
    /// Time constant of the average-current low-pass, s.
    pub tau_s: f64,
    /// Raster breathing: relative size change per unit of filtered APL (positive enlarges).
    pub breathing: f64,
    /// APL at which the raster has its nominal size.
    pub breathing_ref: f64,
}

impl Default for Abl {
    /// The defaults of `params/abl.toml` (module documentation).
    fn default() -> Self {
        Self {
            full_white_ma: 2.8,
            threshold_ma: 1.4,
            full_white_ratio: 0.5,
            tau_s: 0.5 / 3.0,
            breathing: 0.0,
            breathing_ref: 0.5,
        }
    }
}

impl Abl {
    /// Gain slope k in 1/mA, 0 when the limiter cannot act (`I_fw <= I_th`).
    ///
    /// # Panics
    /// Unless `full_white_ratio` is in (0, 1].
    pub fn k_per_ma(&self) -> f64 {
        assert!(
            self.full_white_ratio > 0.0 && self.full_white_ratio <= 1.0,
            "full-white ratio must be in (0, 1]: {}",
            self.full_white_ratio
        );
        if self.full_white_ma <= self.threshold_ma {
            return 0.0;
        }
        (1.0 / self.effective_ratio() - 1.0) / (self.full_white_ma - self.threshold_ma)
    }

    /// The full-white ratio actually used: `full_white_ratio` clamped up to `I_th / I_fw`. Above the
    /// threshold the limited anode current `x I_fw g(x)` has the derivative
    /// `I_fw (1 - k I_th) / (1 + k (x I_fw - I_th))^2`, so it rises with the APL only while
    /// `k <= 1 / I_th`, i.e. `r >= I_th / I_fw`; a smaller ratio would make a brighter picture draw less
    /// current, which no limiter does. The defaults (0.5 = 1.4 / 2.8) sit on the bound (the limited
    /// current is then constant, I_th, above the threshold). [D]
    pub fn effective_ratio(&self) -> f64 {
        if self.full_white_ma <= 0.0 {
            return self.full_white_ratio;
        }
        self.full_white_ratio.max(self.threshold_ma / self.full_white_ma)
    }

    /// Gain g of the filtered APL `x`.
    pub fn gain(&self, x: f64) -> f64 {
        1.0 / (1.0 + self.k_per_ma() * (x * self.full_white_ma - self.threshold_ma).max(0.0))
    }

    /// One step of the low-pass: the new filtered APL from `x` and the frame APL `a` after `dt_s`.
    pub fn filter(&self, x: f64, a: f64, dt_s: f64) -> f64 {
        let alpha = if self.tau_s <= 0.0 {
            1.0
        } else {
            1.0 - (-dt_s / self.tau_s).exp()
        };
        x + alpha * (a - x)
    }

    /// Raster scale of the filtered APL `x`.
    pub fn raster_scale(&self, x: f64) -> f64 {
        1.0 + self.breathing * (x - self.breathing_ref)
    }

    /// Filtered APL after each frame of `apl` (frame APLs) at constant `dt_s`, from a cold start.
    pub fn response(&self, apl: &[f64], dt_s: f64) -> Vec<f64> {
        let mut x = 0.0;
        apl.iter()
            .map(|&a| {
                x = self.filter(x, a, dt_s);
                x
            })
            .collect()
    }
}

/// APL of the window pattern: lit area over frame area, (ww * wh) / (w * h).
pub fn window_apl(w: u32, h: u32, ww: u32, wh: u32) -> f64 {
    f64::from(ww) * f64::from(wh) / (f64::from(w) * f64::from(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    #[test]
    fn default_slope_and_ratio() {
        let a = Abl::default();
        // k = (1 / 0.5 - 1) / (2.8 - 1.4) = 1 / 1.4.
        assert!(close(a.k_per_ma(), 0.714_285_714, 1e-9));
        assert!(close(a.gain(1.0), 0.5, 1e-12));
        // Below the threshold (x I_fw <= 1.4): exactly 1.
        assert_eq!(a.gain(0.1), 1.0);
        assert_eq!(a.gain(0.5), 1.0);
        // x = 0.75: I = 2.1 mA, g = 1 / (1 + 0.7 / 1.4) = 2 / 3.
        assert!(close(a.gain(0.75), 2.0 / 3.0, 1e-12));
    }

    /// A ratio below I_th / I_fw behaves as the bound; at the bound the limited current is constant
    /// above the threshold, and it never falls with the APL.
    #[test]
    fn ratio_is_clamped_to_a_rising_current() {
        let low = Abl {
            full_white_ratio: 0.3,
            ..Abl::default()
        };
        let bound = Abl::default();
        assert_eq!(low.effective_ratio(), 0.5);
        assert_eq!(low.k_per_ma(), bound.k_per_ma());
        for x in [0.4, 0.6, 0.8, 1.0] {
            assert_eq!(low.gain(x), bound.gain(x));
            assert!(close(
                x * bound.full_white_ma * bound.gain(x),
                bound.threshold_ma.min(x * 2.8),
                1e-12
            ));
        }
        let ok = Abl {
            full_white_ratio: 0.7,
            ..Abl::default()
        };
        assert_eq!(ok.effective_ratio(), 0.7);
        let current = |x: f64| x * ok.full_white_ma * ok.gain(x);
        assert!(current(1.0) > current(0.8) && current(0.8) > current(0.6));
    }

    #[test]
    fn no_limiting_when_full_white_under_threshold() {
        let a = Abl {
            full_white_ma: 1.0,
            ..Abl::default()
        };
        assert_eq!(a.k_per_ma(), 0.0);
        assert_eq!(a.gain(1.0), 1.0);
    }

    /// Step 0 -> 1: 63.2 % at t = tau; onset (anode current crosses I_th) at tau ln 2 = 0.1155 s; 95 %
    /// at 3 tau = 0.5 s (CRT.md: onset about 0.1 s, steady state about 0.5 s).
    #[test]
    fn step_response() {
        let a = Abl {
            tau_s: 0.25,
            ..Abl::default()
        };
        let r = a.response(&[1.0; 15], 1.0 / 60.0);
        assert!(close(r[14], 1.0 - (-1.0f64).exp(), 1e-12));
        let d = Abl::default();
        let onset = -d.tau_s * (1.0 - d.threshold_ma / d.full_white_ma).ln();
        assert!(close(onset, 0.115_524, 1e-6));
        assert!(close(1.0 - (-0.5 / d.tau_s).exp(), 0.950_213, 1e-6));
    }

    #[test]
    fn filter_edges() {
        let a = Abl {
            tau_s: 0.0,
            ..Abl::default()
        };
        assert_eq!(a.filter(0.2, 0.7, 0.016), 0.7);
        let b = Abl::default();
        assert_eq!(b.filter(0.2, 0.7, 0.0), 0.2);
    }

    #[test]
    fn breathing() {
        let a = Abl::default();
        assert_eq!(a.raster_scale(0.9), 1.0);
        let b = Abl {
            breathing: 0.03,
            breathing_ref: 0.25,
            ..Abl::default()
        };
        assert!(close(b.raster_scale(1.0), 1.0225, 1e-12));
        assert!(close(b.raster_scale(0.0), 0.9925, 1e-12));
    }

    #[test]
    fn window_area() {
        assert!(close(window_apl(320, 240, 101, 76), 7676.0 / 76800.0, 1e-15));
    }
}
