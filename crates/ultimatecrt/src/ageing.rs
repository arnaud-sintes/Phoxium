//! Ageing and burn-in on the CPU side (T-42, T-43, QUESTIONS PHY-14): the values the shaders need, from the
//! parameter table (`params/ageing.toml`, `params/burnin.toml`) and the oracle (`crt_model::ageing`).
//!
//! - **Spot growth** of a worn gun ([`effective_params`]): the two beam widths `beam_fwhm_mid_mm` and
//!   `beam_fwhm_peak_mm` are multiplied by `ageing_spot_growth` before the frame is recorded
//!   (`gpu::pipeline`), so every user of the spot law (the beam shaders, the skirt margin, the warp plan, the
//!   headroom readout) sees the grown spot; the law's exponent is unchanged, so the FWHM at every current is
//!   the factor times larger (`crt_model::ageing::grown_spot`).
//! - **Cathode wear**: `shaders/gun.comp` reads `ageing_cathode_loss_rgb` and `ageing_cutoff_drift_rgb`
//!   directly (only while `ageing_enable` is on).
//! - **Phosphor efficiency** ([`WearConstants`]): the per-channel Pfahnl constants c (calibrated on the tube
//!   white), the tube-wide charge of the hours of use and the charge per unit light and hour, pushed to
//!   `shaders/wear.comp` by the `wear` slot.

use crt_model::ageing::{reference_hours, WearLaw, REF_LUMINANCE_CD_M2};

use crate::params::Params;

/// Rec. 709 luminance shares, the fallback when the tube white is not reachable from its primaries. [K]
const REC709_SHARES: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// The parameters the chain renders with: `params` with the grown spot of a worn gun while ageing is on
/// (`ageing_spot_growth`, clamped to [0.1, 10]); unchanged otherwise (bit for bit).
pub fn effective_params(params: &Params) -> Params {
    let mut p = *params;
    if params.ageing_enable && params.ageing_spot_growth != 1.0 && params.ageing_spot_growth.is_finite() {
        let f = params.ageing_spot_growth.clamp(0.1, 10.0);
        p.beam_fwhm_mid_mm *= f;
        p.beam_fwhm_peak_mm *= f;
    }
    p
}

/// The wear law of the tube: the per-channel Pfahnl constants, calibrated so that the tube white loses
/// `ageing_ref_loss` after `ageing_ref_hours` of full white at 350 cd/m2 (luminance shares of the tube
/// primaries, `white::luminance_shares`).
pub fn wear_law(params: &Params) -> WearLaw {
    let rates = params.ageing_rates_rgb.map(|r| f64::from(r).clamp(0.0, 100.0));
    let rates = if rates.iter().any(|r| *r > 0.0) {
        rates
    } else {
        [1.0; 3]
    };
    let shares = crate::white::luminance_shares(params).unwrap_or(REC709_SHARES);
    let loss = f64::from(params.ageing_ref_loss).clamp(0.0, 0.99);
    let hours = f64::from(params.ageing_ref_hours).max(1.0);
    WearLaw::calibrated(rates, shares, loss, hours)
}

/// The tube-wide charge (reference hours) of the hours of use: 0 unless `ageing_enable` is on.
pub fn age_charge(params: &Params) -> f64 {
    if !params.ageing_enable {
        return 0.0;
    }
    let clean = |v: f32| if v.is_finite() { f64::from(v).max(0.0) } else { 0.0 };
    reference_hours(
        clean(params.ageing_hours),
        clean(params.ageing_mean_level),
        clean(params.ageing_white_cd_m2),
    )
}

/// Reference hours deposited by one hour of light 1 (full white of the channel): L_w / 350.
pub fn charge_per_light_hour(params: &Params) -> f64 {
    let w = f64::from(params.ageing_white_cd_m2);
    if w.is_finite() {
        w.max(0.0) / REF_LUMINANCE_CD_M2
    } else {
        0.0
    }
}

/// The phosphor efficiency of the `wear` slot is not 1 everywhere: ageing with a charge, or burn-in on.
pub fn wear_active(params: &Params) -> bool {
    params.burnin_enable || age_charge(params) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_change_nothing() {
        let p = Params::default();
        assert_eq!(effective_params(&p), p);
        assert_eq!(age_charge(&p), 0.0);
        assert!(!wear_active(&p));
    }

    #[test]
    fn spot_growth_only_while_ageing_is_on() {
        let mut p = Params {
            ageing_spot_growth: 1.2,
            ..Params::default()
        };
        assert_eq!(effective_params(&p), p, "ageing off");
        p.ageing_enable = true;
        let e = effective_params(&p);
        assert_eq!(e.beam_fwhm_mid_mm, p.beam_fwhm_mid_mm * 1.2);
        assert_eq!(e.beam_fwhm_peak_mm, p.beam_fwhm_peak_mm * 1.2);
    }

    /// The default tube (sRGB primaries, D65) calibrates the white to 0.85 at the reference point, and the
    /// hours of use give hours x level x 100 / 350 reference hours.
    #[test]
    fn law_and_charge_of_the_defaults() {
        let mut p = Params::default();
        let law = wear_law(&p);
        let shares = crate::white::luminance_shares(&p).unwrap();
        assert!((shares.iter().sum::<f64>() - 1.0).abs() < 1e-6);
        // 0.15 as an f32 parameter is 0.15000000596.
        assert!((law.white_efficiency(shares, 20_000.0) - 0.85).abs() < 1e-7);
        p.ageing_enable = true;
        p.ageing_hours = 7000.0;
        assert!((age_charge(&p) - 7000.0 * f64::from(0.3_f32) * 100.0 / 350.0).abs() < 1e-9);
        assert!(wear_active(&p));
        assert!((charge_per_light_hour(&p) - 100.0 / 350.0).abs() < 1e-12);
    }
}
