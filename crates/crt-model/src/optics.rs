//! Faceplate optics: halation geometry and glass transmission.

/// Halation ring radius over faceplate thickness: 2 tan(asin(1 / n)), the
/// total-internal-reflection angle traversed twice. [D]
pub fn halation_radius_over_thickness(n: f64) -> f64 {
    2.0 * (1.0 / n).asin().tan()
}

/// Beer-Lambert transmission through `thickness_ratio` times the reference
/// thickness, given the transmission `t_ref` at the reference thickness. [K]
pub fn glass_transmission(t_ref: f64, thickness_ratio: f64) -> f64 {
    t_ref.powf(thickness_ratio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    #[test]
    fn halation() {
        assert!(close(halation_radius_over_thickness(1.52), 1.7471, 0.0001));
    }

    /// T^2.65 for T = 0.46 and 0.86.
    #[test]
    fn transmission_power() {
        assert!(close(glass_transmission(0.46, 2.65), 0.128, 0.0005));
        assert!(close(glass_transmission(0.86, 2.65), 0.671, 0.0005));
        assert!(close(glass_transmission(0.5, 1.0), 0.5, 1e-15));
    }
}
