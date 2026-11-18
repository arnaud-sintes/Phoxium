//! Units and conversions. Names carry their unit: `_mm`, `_ms`, `_ns`, `_mhz`, `_ua`.

/// Millimetres per inch (exact). [K]
pub const MM_PER_INCH: f64 = 25.4;

/// Inches to millimetres.
pub fn inch_to_mm(inch: f64) -> f64 {
    inch * MM_PER_INCH
}

/// Frame period in milliseconds for a refresh rate in hertz.
pub fn frame_period_ms(refresh_hz: f64) -> f64 {
    1000.0 / refresh_hz
}

/// FWHM of a Gaussian over its sigma: 2 sqrt(2 ln 2), about 2.3548. [K]
pub const FWHM_PER_SIGMA: f64 = 2.354_820_045_030_949;

/// Full width at half maximum from a Gaussian sigma (any length unit).
pub fn fwhm_from_sigma(sigma: f64) -> f64 {
    sigma * FWHM_PER_SIGMA
}

/// Gaussian sigma from a full width at half maximum (any length unit).
pub fn sigma_from_fwhm(fwhm: f64) -> f64 {
    fwhm / FWHM_PER_SIGMA
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    #[test]
    fn fwhm_is_2_355_sigma() {
        assert!(close(fwhm_from_sigma(1.0), 2.355, 0.0005));
        assert!(close(FWHM_PER_SIGMA, 2.0 * (2.0 * 2.0_f64.ln()).sqrt(), 1e-15));
        assert!(close(sigma_from_fwhm(fwhm_from_sigma(0.3)), 0.3, 1e-12));
    }

    #[test]
    fn inch_and_frame_period() {
        assert!(close(inch_to_mm(19.0), 482.6, 1e-9));
        assert!(close(frame_period_ms(60.0), 16.666_666_666_666_668, 1e-12));
    }
}
