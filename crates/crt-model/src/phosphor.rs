//! Phosphor persistence: decay law and per-frame energy.

/// Phosphor decay: I(t) = a exp(-t / tau) + b (1 + t / t0)^-n. [D]
pub fn decay(t_ms: f64, a: f64, tau_ms: f64, b: f64, t0_ms: f64, n: f64) -> f64 {
    a * (-t_ms / tau_ms).exp() + b * (1.0 + t_ms / t0_ms).powf(-n)
}

/// Closed-form integral of [`decay`] over [t1_ms, t2_ms] (n != 1).
pub fn decay_energy(t1_ms: f64, t2_ms: f64, a: f64, tau_ms: f64, b: f64, t0_ms: f64, n: f64) -> f64 {
    let exp_part = a * tau_ms * ((-t1_ms / tau_ms).exp() - (-t2_ms / tau_ms).exp());
    let f = |t: f64| b * t0_ms / (1.0 - n) * (1.0 + t / t0_ms).powf(1.0 - n);
    exp_part + f(t2_ms) - f(t1_ms)
}

/// Per-frame energy ratios E_k / E_0 (k = 1, 2, 3) for one excitation at t = 0,
/// frame k covering [k T, (k + 1) T].
pub fn frame_energy_ratios(frame_ms: f64, a: f64, tau_ms: f64, b: f64, t0_ms: f64, n: f64) -> [f64; 3] {
    let e = |k: f64| decay_energy(k * frame_ms, (k + 1.0) * frame_ms, a, tau_ms, b, t0_ms, n);
    let e0 = e(0.0);
    [e(1.0) / e0, e(2.0) / e0, e(3.0) / e0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;
    use crate::units::frame_period_ms;

    #[test]
    fn green_tail() {
        let at = |t| decay(t, 0.0, 1.0, 0.046, 1.0, 0.72);
        assert!(close(at(8.0) * 100.0, 0.95, 0.005));
        assert!(close(at(16.0) * 100.0, 0.60, 0.005));
        // CRT.md also quotes "1 % at 7.3 ms" for the same fit.
        assert!(close(at(7.3) * 100.0, 1.0, 0.03), "{} % at 7.3 ms", at(7.3) * 100.0);
    }

    /// The closed form matches a midpoint-rule integration.
    #[test]
    fn energy_closed_form() {
        let (a, tau, b, t0, n) = (1.0, 0.05, 0.046, 1.0, 0.72);
        let steps = 200_000;
        let h = 16.0 / f64::from(steps);
        let num: f64 = (0..steps)
            .map(|i| decay((f64::from(i) + 0.5) * h, a, tau, b, t0, n) * h)
            .sum();
        assert!(close(decay_energy(0.0, 16.0, a, tau, b, t0, n), num, 1e-6));
    }

    /// Records the per-frame energy ratios of the CRT.md green fit (A = 1,
    /// tau = 0.05 ms, B = 0.046, t0 = 1 ms, n = 0.72) and red fit (A = 1,
    /// tau = 0.5 ms, no tail). Green E1/E0 comes out near 30 %, not in the
    /// 0.5 to 4 % ghost-trail range of PHY-12.
    #[test]
    fn decay_frame_energy_recorded() {
        let t = frame_period_ms(60.0);
        let g = frame_energy_ratios(t, 1.0, 0.05, 0.046, 1.0, 0.72);
        let r = frame_energy_ratios(t, 1.0, 0.5, 0.0, 1.0, 0.72);
        println!("green E_k/E_0 = {g:?}, red E_k/E_0 = {r:?}");
        assert!(g[0] > 0.25 && g[0] < 0.35, "green E1/E0 = {}", g[0]);
        assert!(r[0] < 1e-9);
    }

    /// PHY-12 gate of T-14. Fails with the CRT.md green fit: flagged to the
    /// owner, researched in T-63 (the CRT.md decay constants are not changed
    /// here). Run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "CRT.md green fit vs PHY-12 discrepancy, see T-63"]
    fn decay_green_ghost_trail_in_phy12_range() {
        let g = frame_energy_ratios(frame_period_ms(60.0), 1.0, 0.05, 0.046, 1.0, 0.72);
        assert!((0.005..=0.04).contains(&g[0]), "green E1/E0 = {}", g[0]);
    }
}
