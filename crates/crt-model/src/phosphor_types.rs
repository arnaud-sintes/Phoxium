//! Phosphor types of the selector (`phosphor_type`, QUESTIONS PHY-19, CRT.md "Phosphor types").
//!
//! One data row per type: the emission chromaticity (CIE 1931 xy), the decay as a [`DecayFit`] (the same form as
//! the Kuhn P22 fits of [`crate::phosphor`]), and the source line. P22 (the colour tube, the default) is not a row:
//! it keeps the three Kuhn fits and the `tube_*_xy` primaries, unchanged.
//!
//! Every other type is a monochrome tube: one gun, a continuous phosphor coating (no mask), driven by the BT.601
//! luma Y' of the gamma-encoded source ([`mono_drive`], the weights of [`crate::composite::LUMA`]), emitting light
//! of the type's chromaticity ([`mono_light_xyz`]).
//!
//! Approximation common to every row [D]: the registered persistence numbers are step-off responses (time for the
//! light to fall to 10 % or 1 % of its value when a steady excitation is removed, JEDEC definition). They are used
//! here as the impulse response, which is exact only for a pure exponential.

use std::f64::consts::LN_10;

use crate::phosphor::{DecayFit, ExpTerm, PowerTail};

/// One monochrome phosphor type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhosphorType {
    /// Value name of the `phosphor_type` parameter.
    pub id: &'static str,
    /// Emission chromaticity, CIE 1931 (x, y).
    pub xy: (f64, f64),
    /// Impulse response, normalised to I(0) = 1.
    pub fit: DecayFit<'static>,
    /// Registered persistence points (time in ms, remaining fraction) the fit was derived from.
    pub points: &'static [(f64, f64)],
    /// Sources and approximations, as carried into `params/phosphor.toml` and CRT.md.
    pub source: &'static str,
}

/// P4 sulfide (white, direct-view black-and-white TV): blend ZnS:Ag + (Zn,Cd)S:Ag.
/// xy 0.270, 0.300 (RCA TPM-1508A 1961; HP AN-115 1970) [S]. Decay: a pure power law (1 + t / t0)^-n through the
/// two registered points 60 µs to 10 % and 470 µs to 1 % (HP AN-115 Table 3-1; RCA: 60 µs): t0 = t10^2 / (t1 -
/// 2 t10) = 10.29 µs, n = ln 10 / ln(1 + t10 / t0) = 1.198 [D]. Law choice: Dyall 1948 classes P4 as mixed
/// exponential and power law (via Jankowiak, C); a sulfide like Kuhn's P22 green, which has a power-law tail [D].
pub const P4: PhosphorType = PhosphorType {
    id: "p4",
    xy: (0.270, 0.300),
    fit: DecayFit {
        exp_terms: &[],
        tail: Some(PowerTail {
            b: 1.0,
            t0_ms: 3.6 / 350.0,
            n: 1.198_131_959,
        }),
    },
    points: &[(0.060, 0.10), (0.470, 0.01)],
    source: "RCA TPM-1508A (1961) and HP AN-115 (1970) [S]: xy 0.270, 0.300; 60 us to 10 %, 470 us to 1 %. Power law \
             through both points, t0 = 10.29 us, n = 1.198 [D]; step-off times used as the impulse response \
             (approximation)",
};

/// P31 (green, oscilloscopes and many green monitors): ZnS:Cu.
/// xy 0.245, 0.523 (RCA TPM-1508A 1961, the registration-era value) [S]; Phosphor Technology's current grade gives
/// 0.287, 0.521 (0.04 apart in x, unresolved; the research recommends the RCA value). Decay: power law through
/// 38 µs to 10 % and 250 µs to 1 % (RCA ~38 µs; HP AN-115; EIA TEP116-B quoted by Wolf and Deubel 1997):
/// t0 = 8.30 µs, n = 1.339 [D].
pub const P31: PhosphorType = PhosphorType {
    id: "p31",
    xy: (0.245, 0.523),
    fit: DecayFit {
        exp_terms: &[],
        tail: Some(PowerTail {
            b: 1.0,
            t0_ms: 1.444 / 174.0,
            n: 1.339_490_948,
        }),
    },
    points: &[(0.038, 0.10), (0.250, 0.01)],
    source: "xy 0.245, 0.523 RCA TPM-1508A (1961) [S] (Phosphor Technology current grade 0.287, 0.521: the two primary \
             sources disagree by 0.04 in x; the registration-era RCA value is used); 38 us to 10 %, 250 us to 1 % (RCA, \
             HP AN-115, EIA TEP116-B via Wolf and Deubel 1997) [S]. Power law through both points, t0 = 8.30 us, \
             n = 1.339 [D]; step-off times used as the impulse response (approximation)",
};

/// P1 (green, early green screens): Zn2SiO4:Mn (willemite).
/// xy 0.218, 0.712 (RCA TPM-1508A; HP AN-115) [S]. Decay: one exponential, tau = 24 ms / ln 10 = 10.4 ms (HP
/// AN-115: 24 ms to 10 %, 48 ms to 1 %, both on that exponential; RCA: 24.5 ms) [D]. Dyall 1948 classes P1 as
/// exponential (via Jankowiak, C).
pub const P1: PhosphorType = PhosphorType {
    id: "p1",
    xy: (0.218, 0.712),
    fit: DecayFit {
        exp_terms: &[ExpTerm {
            a: 1.0,
            tau_ms: 24.0 / LN_10,
        }],
        tail: None,
    },
    points: &[(24.0, 0.10), (48.0, 0.01)],
    source: "RCA TPM-1508A (1961) and HP AN-115 (1970) [S]: xy 0.218, 0.712; 24 ms to 10 % and 48 ms to 1 % (RCA 24.5 \
             ms). One exponential, tau = 24 ms / ln 10 = 10.4 ms, through both points [D]",
};

/// P39 (green, long persistence; the IBM 5151 monochrome display used it): Zn2SiO4:Mn,As.
/// xy 0.223, 0.698 (HP AN-115, JEDEC registration curve) [S]; Phosphor Technology's current grade 0.195, 0.697.
/// Decay: ONE registered point only, 150 ms to 10 % (HP AN-115) [S]; one exponential through it, tau = 150 ms /
/// ln 10 = 65.1 ms [D]. A one-point approximation: the registration curve is visibly not exponential (concave on
/// a semilog plot) and the 1 % time was not found.
pub const P39: PhosphorType = PhosphorType {
    id: "p39",
    xy: (0.223, 0.698),
    fit: DecayFit {
        exp_terms: &[ExpTerm {
            a: 1.0,
            tau_ms: 150.0 / LN_10,
        }],
        tail: None,
    },
    points: &[(150.0, 0.10)],
    source:
        "HP AN-115 (1970) [S]: xy 0.223, 0.698 (Phosphor Technology current grade 0.195, 0.697); 150 ms to 10 % (the \
             only registered point). One exponential through it, tau = 65.1 ms [D]: a one-point approximation, the \
             registered curve is not exponential and the 1 % time is unknown",
};

/// The Philips yellow data-display blend (no P number; never P3 or P134): 15 % Zn2SiO4:Mn,As + 85 % CdSiO3:Mn,As by
/// weight (US 4,405,880, 1983). xy 0.480, 0.490 measured on example tubes [S]. Decay: the patent gives a lower bound
/// only, at least 34 ms to 10 % of the steady-state brightness (spot technique) [S]; one exponential through the
/// bound, tau = 34 ms / ln 10 = 14.8 ms [D] (the real decay is at least this long).
pub const YELLOW_BLEND: PhosphorType = PhosphorType {
    id: "philips_yellow_data_display_blend",
    xy: (0.480, 0.490),
    fit: DecayFit {
        exp_terms: &[ExpTerm {
            a: 1.0,
            tau_ms: 34.0 / LN_10,
        }],
        tail: None,
    },
    points: &[(34.0, 0.10)],
    source:
        "Gallaro and Hedler (North American Philips), US 4,405,880 (1983) [S]: 15/85 Zn2SiO4:Mn,As + CdSiO3:Mn,As, \
             xy 0.480, 0.490 on example tubes; at least 34 ms to 10 %. One exponential through that lower bound, tau = \
             14.8 ms [D] (the real decay is at least as long); not P3 nor P134 (no data found for those)",
};

/// The monochrome types in the order of the `phosphor_type` values after `p22` (index 1 .. 5).
pub const MONO_TYPES: [PhosphorType; 5] = [P4, P31, P1, P39, YELLOW_BLEND];

/// The monochrome type of `phosphor_type` value index `index` (0 is P22, the colour tube: `None`).
pub fn mono_type(index: u32) -> Option<&'static PhosphorType> {
    (index as usize).checked_sub(1).and_then(|i| MONO_TYPES.get(i))
}

/// The drive of a monochrome tube: the BT.601 luma Y' = 0.299 R' + 0.587 G' + 0.114 B' of the gamma-encoded source
/// ([`crate::composite::LUMA`]), the signal a composite or monochrome-video monitor receives [K]. Applied before the
/// gun law, so the gun gamma acts on Y'.
pub fn mono_drive(rgb: [f64; 3]) -> f64 {
    let w = crate::composite::LUMA;
    w[0] * rgb[0] + w[1] * rgb[1] + w[2] * rgb[2]
}

/// The light XYZ of luminance `l` of chromaticity `xy`: l (x / y, 1, (1 - x - y) / y).
pub fn mono_light_xyz(xy: (f64, f64), l: f64) -> [f64; 3] {
    let (x, y) = xy;
    [l * x / y, l, l * (1.0 - x - y) / y]
}

/// The step-off response of a fit after a long steady excitation: S(t) = integral_t^inf I / integral_0^inf I,
/// computed over [0, `horizon_ms`] (the frame feedback normalises over [`crate::phosphor::TRAIL_HORIZON_MS`]).
pub fn step_off(fit: &DecayFit<'_>, t_ms: f64, horizon_ms: f64) -> f64 {
    fit.energy(t_ms, horizon_ms) / fit.energy(0.0, horizon_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phosphor::TRAIL_HORIZON_MS;

    /// The fits are impulse responses through the registered points: I(t) = fraction (the approximation stated in
    /// the module doc), within 0.1 % relative.
    #[test]
    fn every_fit_passes_through_its_registered_points() {
        for t in MONO_TYPES {
            for &(ms, frac) in t.points {
                let i = t.fit.intensity(ms);
                assert!(
                    (i / frac - 1.0).abs() < 1e-3,
                    "{}: I({ms} ms) = {i}, registered {frac}",
                    t.id
                );
            }
            assert!((t.fit.intensity(0.0) - 1.0).abs() < 1e-12, "{}: I(0) = 1", t.id);
        }
    }

    /// The registered times to 10 % and 1 %, as the model gives them (the impulse response), within 0.5 %.
    #[test]
    fn times_to_10_and_1_percent_match_the_table() {
        let time_to = |fit: &DecayFit<'_>, frac: f64| {
            let (mut lo, mut hi) = (0.0, 5000.0);
            for _ in 0..200 {
                let mid = 0.5 * (lo + hi);
                if fit.intensity(mid) > frac {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        let table: [(&PhosphorType, f64, Option<f64>); 5] = [
            (&P4, 0.060, Some(0.470)),
            (&P31, 0.038, Some(0.250)),
            (&P1, 24.0, Some(48.0)),
            (&P39, 150.0, None),
            (&YELLOW_BLEND, 34.0, None),
        ];
        for (t, t10, t1) in table {
            let got = time_to(&t.fit, 0.1);
            assert!((got / t10 - 1.0).abs() < 5e-3, "{}: t10 {got} vs {t10}", t.id);
            if let Some(t1) = t1 {
                let got = time_to(&t.fit, 0.01);
                assert!((got / t1 - 1.0).abs() < 5e-3, "{}: t1 {got} vs {t1}", t.id);
            }
        }
    }

    /// Frame-to-frame ratios at 60 Hz the research derives for the exponential types: P1 0.20, P39 0.77.
    #[test]
    fn exponential_frame_ratios_at_60_hz() {
        let t = 1000.0 / 60.0;
        for (ty, want) in [(&P1, 0.20), (&P39, 0.77)] {
            let r = ty.fit.frame_energy_ratios(t);
            assert!((r[1] / r[0] - want).abs() < 0.01, "{}: {r:?}", ty.id);
        }
    }

    /// Horizon guard: every shipped fit keeps at most 5 % of its light (within 10 s) beyond the 1 s window the
    /// frame feedback is fitted over (the exponential types none; the power-law sulfides P4 about 4 % and P31 about
    /// 1 %, an extrapolation beyond their last registered point, as for Kuhn's P22 tail). This is the test that
    /// keeps P7 (3 s to 1 %) and P38 (1 s to 10 %) out until the horizon is extended.
    #[test]
    fn energy_beyond_the_trail_horizon_is_small() {
        for t in MONO_TYPES {
            let late = t.fit.energy(TRAIL_HORIZON_MS, 10_000.0) / t.fit.energy(0.0, 10_000.0);
            assert!(late < 0.05, "{}: {late} of the light after 1 s", t.id);
        }
    }

    /// The power laws have n > 1 (finite energy, Kuhn's condition, CRT.md).
    #[test]
    fn power_laws_have_finite_energy() {
        for t in MONO_TYPES {
            if let Some(p) = t.fit.tail {
                assert!(p.n > 1.0, "{}", t.id);
            }
        }
    }

    #[test]
    fn luma_and_light() {
        assert_eq!(mono_drive([1.0, 1.0, 1.0]), 0.299 + 0.587 + 0.114);
        assert!((mono_drive([1.0, 1.0, 1.0]) - 1.0).abs() < 1e-12);
        assert_eq!(mono_drive([1.0, 0.0, 0.0]), 0.299);
        assert_eq!(mono_drive([0.0, 1.0, 0.0]), 0.587);
        assert_eq!(mono_drive([0.0, 0.0, 1.0]), 0.114);
        for t in MONO_TYPES {
            let v = mono_light_xyz(t.xy, 2.0);
            let s = v[0] + v[1] + v[2];
            assert!((v[0] / s - t.xy.0).abs() < 1e-12 && (v[1] / s - t.xy.1).abs() < 1e-12 && v[1] == 2.0);
        }
        assert!(mono_type(0).is_none());
        assert_eq!(mono_type(1).unwrap().id, "p4");
        assert_eq!(mono_type(5).unwrap().id, "philips_yellow_data_display_blend");
        assert!(mono_type(6).is_none());
    }

    /// Loose sanity of the P31 fit against Wolf and Deubel (1997): a repetitively excited P31 bar left 1.4 % of its
    /// modulation 50 ms after it switched off. The model's step-off response at 50 ms (about 3.4 %) is within a
    /// factor 3 of it: order of magnitude only, the two quantities differ.
    #[test]
    fn p31_step_off_is_of_the_measured_order_at_50_ms() {
        let s = step_off(&P31.fit, 50.0, TRAIL_HORIZON_MS);
        assert!(s > 0.014 / 3.0 && s < 0.014 * 3.0, "{s}");
    }
}
