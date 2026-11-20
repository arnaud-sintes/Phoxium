//! Ageing oracle (T-42, QUESTIONS PHY-14): tube-wide phosphor efficiency loss per colour, cathode wear
//! (emission loss and cutoff drift) and spot growth. Written from `CRT.md` "Saturation and ageing" before the
//! shaders; the burn-in map ([`crate::burnin`]) uses the same wear law with a local charge.
//!
//! # Phosphor efficiency against deposited charge
//!
//! CRT.md gives one sourced figure: a datasheet allows a 15 % luminance loss after 20,000 h at 350 cd/m2 [S],
//! and says blue usually degrades fastest [S][K]. The wear law is the coulombic ageing law of cathodoluminescent
//! phosphors (Pfahnl's law, standard knowledge [K]): the efficiency after a deposited charge N per area is
//! eta = 1 / (1 + C N). It decelerates (half the efficiency is lost at N = 1 / C, a quarter more only at
//! 3 / C), which is the saturating shape a burn-in curve needs; an exponential exp(-C N) would agree at small
//! N and fall to 0 at large N.
//!
//! Charge is counted in **reference hours** (h_ref): hours of a full-white field at the datasheet's 350 cd/m2.
//! A channel driven at light x (1 = the channel's share of full white) on a tube whose full white is L_w
//! cd/m2 deposits x L_w / 350 h_ref per hour (the light of a fresh phosphor is proportional to its beam
//! current, CRT.md "Saturation and ageing": phosphor output is close to linear in current). Per channel c the
//! constant is C_c = r_c k0, with relative rates r = (r_R, r_G, r_B) [E] (blue fastest) and k0 calibrated
//! so that a white field worn for 20,000 h_ref keeps 85 % of its luminance:
//!
//!   sum_c w_c / (1 + r_c k0 20000) = 0.85,
//!
//! w_c the luminance shares of the tube's primaries in its white (`gun::white_weights`, they sum to 1) [D].
//! With equal rates k0 = (1 / 0.85 - 1) / 20000 = 8.8235e-6 per h_ref (hand check in the tests).
//!
//! Tube-wide ageing (this module) is a uniform charge Q_age = hours x mean level x L_w / 350 h_ref, the
//! burn-in map adds a local charge per channel; the efficiency is 1 / (1 + C_c (Q_age + Q_map,c)).
//!
//! # Cathode wear and spot growth
//!
//! CRT.md has no measured cathode wear curve, so these are direct parameters [E], not a function of hours
//! (QUESTIONS PHY-14: "Parameters, not a simulation of hours"): a worn oxide cathode emits less at the same
//! drive (a gain 1 - loss on the beam current per gun), its cutoff drifts (the drive voltage is offset by d
//! before the gun law, so blacks crush when d > 0) and the beam spot grows (the whole spot growth law
//! FWHM^2 = d0^2 + k I^(2n) is scaled by a factor f, so the FWHM at every current is f times larger).

use crate::beam::SpotGrowth;
use crate::gun::Gun;

/// Luminance loss of the datasheet reference point (CRT.md "Saturation and ageing"). [S]
pub const REF_LOSS: f64 = 0.15;
/// Hours of the datasheet reference point. [S]
pub const REF_HOURS: f64 = 20_000.0;
/// Luminance of the datasheet reference point, cd/m2. [S]
pub const REF_LUMINANCE_CD_M2: f64 = 350.0;
/// Default relative wear rates (red, green, blue): blue fastest (CRT.md), red oxysulfide the most robust. [E]
pub const DEFAULT_RATES: [f64; 3] = [0.5, 1.0, 2.0];

/// Pfahnl efficiency 1 / (1 + c q) of a phosphor with constant `c` (per h_ref) after the charge `q` (h_ref).
///
/// # Panics
/// Unless `c` and `q` are finite and not negative.
pub fn efficiency(c: f64, q: f64) -> f64 {
    assert!(
        c.is_finite() && c >= 0.0 && q.is_finite() && q >= 0.0,
        "efficiency: c {c}, q {q}"
    );
    1.0 / (1.0 + c * q)
}

/// The per-channel wear law: efficiency 1 / (1 + c[ch] q).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WearLaw {
    /// Pfahnl constant per channel (red, green, blue), per reference hour.
    pub c: [f64; 3],
}

impl WearLaw {
    /// The law whose white (luminance shares `weights`, summing to 1) loses `ref_loss` of its luminance after
    /// `ref_q` reference hours, with the relative per-channel `rates`: c = rates x k0, k0 solved by bisection
    /// (the white efficiency decreases strictly with k0).
    ///
    /// # Panics
    /// Unless `ref_loss` is in [0, 1), `ref_q` > 0, the rates are finite and not negative with one positive,
    /// and the weights are positive.
    pub fn calibrated(rates: [f64; 3], weights: [f64; 3], ref_loss: f64, ref_q: f64) -> Self {
        assert!(
            (0.0..1.0).contains(&ref_loss) && ref_q > 0.0,
            "calibration: loss {ref_loss}, q {ref_q}"
        );
        assert!(rates.iter().all(|r| r.is_finite() && *r >= 0.0) && rates.iter().any(|r| *r > 0.0));
        assert!(weights.iter().all(|w| w.is_finite() && *w > 0.0), "weights {weights:?}");
        let sum: f64 = weights.iter().sum();
        let target = 1.0 - ref_loss;
        let white = |k0: f64| -> f64 {
            (0..3)
                .map(|c| weights[c] / sum * efficiency(rates[c] * k0, ref_q))
                .sum()
        };
        let (mut lo, mut hi) = (0.0_f64, 1.0 / ref_q);
        while white(hi) > target {
            hi *= 2.0;
        }
        for _ in 0..200 {
            let mid = 0.5 * (lo + hi);
            if white(mid) > target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let k0 = 0.5 * (lo + hi);
        Self {
            c: rates.map(|r| r * k0),
        }
    }

    /// Efficiency per channel after the charges `q` (h_ref).
    pub fn efficiency(&self, q: [f64; 3]) -> [f64; 3] {
        [0, 1, 2].map(|ch| efficiency(self.c[ch], q[ch]))
    }

    /// Luminance efficiency of a white worn uniformly by `q` h_ref, with the white's luminance shares `weights`.
    pub fn white_efficiency(&self, weights: [f64; 3], q: f64) -> f64 {
        let sum: f64 = weights.iter().sum();
        (0..3).map(|ch| weights[ch] / sum * efficiency(self.c[ch], q)).sum()
    }

    /// The charge (h_ref) after which channel `ch` has lost `loss` of its efficiency: 1 / (1 + c q) = 1 - loss.
    ///
    /// # Panics
    /// Unless `loss` is in [0, 1) and the channel's constant is positive.
    pub fn charge_for_loss(&self, ch: usize, loss: f64) -> f64 {
        assert!((0.0..1.0).contains(&loss) && self.c[ch] > 0.0);
        (1.0 / (1.0 - loss) - 1.0) / self.c[ch]
    }
}

/// Reference hours deposited by `hours` of use at the mean picture level `level` (1 = full white) on a tube
/// whose full white is `white_cd_m2`: hours x level x white / [`REF_LUMINANCE_CD_M2`].
///
/// # Panics
/// Unless every input is finite and not negative.
pub fn reference_hours(hours: f64, level: f64, white_cd_m2: f64) -> f64 {
    assert!(
        [hours, level, white_cd_m2].iter().all(|v| v.is_finite() && *v >= 0.0),
        "reference_hours: {hours} h, level {level}, white {white_cd_m2}"
    );
    hours * level * white_cd_m2 / REF_LUMINANCE_CD_M2
}

/// Cathode wear per gun [E]: emission loss (gain 1 - loss on the beam current) and cutoff drift (offset of
/// the drive voltage before the gun law).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cathode {
    pub loss: [f64; 3],
    pub cutoff_drift: [f64; 3],
}

impl Cathode {
    /// Beam current of `channel` for the drive voltage `v` through a worn cathode:
    /// (1 - loss) x gun current at (v - drift).
    pub fn beam_current(&self, gun: &Gun, channel: usize, v: f64) -> f64 {
        (1.0 - self.loss[channel]) * gun.beam_current(channel, v - self.cutoff_drift[channel])
    }
}

/// The spot growth law of a worn gun: both widths scaled by `factor`, so the FWHM at every current is
/// `factor` times larger (d0^2 and k scale by factor^2, the exponent is unchanged).
///
/// # Panics
/// Unless `factor` is finite and positive.
pub fn grown_spot(spot: SpotGrowth, factor: f64) -> SpotGrowth {
    assert!(factor.is_finite() && factor > 0.0, "spot factor {factor}");
    SpotGrowth {
        fwhm_mid: spot.fwhm_mid * factor,
        fwhm_peak: spot.fwhm_peak * factor,
        n: spot.n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    const EQUAL: [f64; 3] = [1.0 / 3.0; 3];
    /// Rec. 709 luminance shares (sRGB primaries, D65 white). [K]
    const REC709: [f64; 3] = [0.2126, 0.7152, 0.0722];

    /// Equal rates: every channel is the white, so k0 = (1 / 0.85 - 1) / 20000 by hand.
    #[test]
    fn equal_rates_calibrate_by_hand() {
        let law = WearLaw::calibrated([1.0; 3], EQUAL, REF_LOSS, REF_HOURS);
        let k0 = (1.0 / 0.85 - 1.0) / 20_000.0;
        assert!(close(k0, 8.823_529_411_764_706e-6, 1e-18));
        for c in law.c {
            assert!(close(c, k0, 1e-16), "{c} against {k0}");
        }
    }

    /// The calibration point: 20,000 h at 350 cd/m2 full white keeps 85 % of the luminance (+/- 0.01), for
    /// the default rates and the Rec. 709 shares; blue wears faster than red by the rate ratio.
    #[test]
    fn calibration_point_and_colour_order() {
        let law = WearLaw::calibrated(DEFAULT_RATES, REC709, REF_LOSS, REF_HOURS);
        let q = reference_hours(REF_HOURS, 1.0, REF_LUMINANCE_CD_M2);
        assert_eq!(q, REF_HOURS);
        assert!(close(law.white_efficiency(REC709, q), 0.85, 1e-12));
        let e = law.efficiency([q; 3]);
        assert!(e[2] < e[1] && e[1] < e[0], "blue fastest, red slowest: {e:?}");
        assert!(close(law.c[2] / law.c[0], 4.0, 1e-12));
        // White drifts warm: red and green keep more than blue.
        assert!(e[2] < 0.85 && e[0] > 0.85);
    }

    /// The curve decelerates (saturating, Pfahnl): equal charge steps lose less and less, never below 0.
    #[test]
    fn efficiency_is_monotonic_and_saturating() {
        let c = 1e-4;
        let mut last = 1.0;
        let mut last_drop = f64::INFINITY;
        for k in 1..=50 {
            let e = efficiency(c, f64::from(k) * 2000.0);
            let drop = last - e;
            assert!(e < last && e > 0.0);
            assert!(drop < last_drop, "decelerating at step {k}");
            (last, last_drop) = (e, drop);
        }
        assert_eq!(efficiency(c, 0.0), 1.0);
        assert_eq!(efficiency(0.0, 1e9), 1.0);
        assert!(close(efficiency(c, 1.0 / c), 0.5, 1e-15));
    }

    /// The inverse: the charge that costs blue 15 % gives blue exactly 0.85.
    #[test]
    fn charge_for_a_given_loss() {
        let law = WearLaw::calibrated(DEFAULT_RATES, REC709, REF_LOSS, REF_HOURS);
        let q = law.charge_for_loss(2, 0.15);
        assert!(close(law.efficiency([q; 3])[2], 0.85, 1e-14));
    }

    /// Reference hours scale with the hours, the mean level and the white luminance.
    #[test]
    fn reference_hours_scale() {
        assert!(close(
            reference_hours(1000.0, 0.25, 100.0),
            1000.0 * 0.25 * 100.0 / 350.0,
            1e-12
        ));
        assert_eq!(reference_hours(0.0, 0.5, 100.0), 0.0);
    }

    /// Cathode: no wear is the gun itself; loss scales the current, drift crushes the blacks.
    #[test]
    fn cathode_wear() {
        let gun = Gun::default();
        let none = Cathode::default();
        for v in [0.0, 0.25, 0.5, 1.0] {
            assert_eq!(none.beam_current(&gun, 1, v), gun.beam_current(1, v));
        }
        let worn = Cathode {
            loss: [0.0, 0.2, 0.0],
            cutoff_drift: [0.05, 0.0, 0.0],
        };
        assert!(close(worn.beam_current(&gun, 1, 1.0), 0.8, 1e-15));
        assert!(close(worn.beam_current(&gun, 0, 0.5), 0.45_f64.powf(2.4), 1e-15));
        assert_eq!(worn.beam_current(&gun, 0, 0.04), 0.0, "below the drifted cutoff: black");
    }

    /// Spot growth: the FWHM at every current is the factor times the new spot's.
    #[test]
    fn grown_spot_scales_every_width() {
        let s = SpotGrowth {
            fwhm_mid: 0.45,
            fwhm_peak: 0.8,
            n: 0.3,
        };
        let g = grown_spot(s, 1.2);
        for i in [0.05, 0.19, 0.5, 1.0] {
            assert!(close(g.fwhm(i), 1.2 * s.fwhm(i), 1e-12), "current {i}");
        }
        assert_eq!(grown_spot(s, 1.0), s);
    }
}
