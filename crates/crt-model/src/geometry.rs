//! Tube geometry: visible area, raster line pitch, mask triads, curvature, moire.

/// Visible width and height from a diagonal and an aspect ratio (w / h).
pub fn visible_size_mm(diagonal_mm: f64, aspect: f64) -> (f64, f64) {
    let h = diagonal_mm / (1.0 + aspect * aspect).sqrt();
    (h * aspect, h)
}

/// Line pitch P = visible height / number of active lines (raster fills the
/// visible area). CRT.md "Scanline structure". [D]
pub fn line_pitch_mm(visible_height_mm: f64, active_lines: f64) -> f64 {
    visible_height_mm / active_lines
}

/// Triads across a visible width.
pub fn triads_across(visible_width_mm: f64, triad_pitch_mm: f64) -> f64 {
    visible_width_mm / triad_pitch_mm
}

/// Triads per source pixel: triads across over the source width in pixels.
pub fn triads_per_source_px(triads: f64, source_width_px: f64) -> f64 {
    triads / source_width_px
}

/// Host pixels per triad.
pub fn px_per_triad(host_px: f64, triads: f64) -> f64 {
    host_px / triads
}

/// Delta-gun triad distances for a dot pitch `p_mm`: same-colour repeat along
/// a row P sqrt(3), half of it, P / 2 and P / sqrt(3). [D]
pub fn delta_distances_mm(p_mm: f64) -> [f64; 4] {
    let s3 = 3.0_f64.sqrt();
    [p_mm * s3, p_mm * s3 / 2.0, p_mm / 2.0, p_mm / s3]
}

/// Sagitta of a spherical cap of radius `r_mm` over a half chord `h_mm`:
/// s = R - sqrt(R^2 - h^2). [D]
pub fn sagitta_mm(r_mm: f64, h_mm: f64) -> f64 {
    r_mm - (r_mm * r_mm - h_mm * h_mm).sqrt()
}

/// Moire beat period of two periods p1 and p2: p1 p2 / |p1 - p2|. [K]
pub fn moire_beat(p1: f64, p2: f64) -> f64 {
    p1 * p2 / (p1 - p2).abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;
    use crate::units::inch_to_mm;

    #[test]
    fn visible_size_from_diagonal() {
        let (w, h) = visible_size_mm(500.0, 4.0 / 3.0);
        assert!(close(w, 400.0, 1e-9) && close(h, 300.0, 1e-9));
    }

    /// CRT.md reference scales: viewable diagonal = nominal - 1 in, 4:3.
    /// Rows whose stated size follows that rule.
    #[test]
    fn reference_scale_visible_sizes() {
        for (nominal_in, w, h) in [(14.0, 264.0, 198.0), (20.0, 386.0, 290.0), (17.0, 325.0, 244.0)] {
            let (gw, gh) = visible_size_mm(inch_to_mm(nominal_in - 1.0), 4.0 / 3.0);
            assert!(close(gw, w, 0.6) && close(gh, h, 0.6), "{nominal_in} in: {gw} x {gh}");
        }
        // "26 in viewable": the 26 in is already the viewable diagonal.
        let (gw, gh) = visible_size_mm(inch_to_mm(26.0), 4.0 / 3.0);
        assert!(close(gw, 528.0, 0.6) && close(gh, 396.0, 0.6));
    }

    /// The 15 in and 21 in rows do not follow the nominal - 1 in rule stated
    /// above the table (they match about 13.8 in and 19.8 in viewable).
    /// Recorded, not hidden: see the T-14 report.
    #[test]
    fn reference_scale_rows_off_the_stated_rule() {
        let (w15, _) = visible_size_mm(inch_to_mm(14.0), 4.0 / 3.0);
        let (w21, _) = visible_size_mm(inch_to_mm(20.0), 4.0 / 3.0);
        assert!(close(w15, 284.5, 0.1), "15 in rule width {w15}, table 280");
        assert!(close(w21, 406.4, 0.1), "21 in rule width {w21}, table 402");
    }

    /// CRT.md reference-scale table: line pitch, triads across, triads per source pixel.
    #[test]
    fn reference_scale_table() {
        // (visible w, visible h, source w, active lines, line pitch, triad pitch, triads, per source px)
        let rows = [
            (264.0, 198.0, 320.0, 240.0, 0.83, 0.25, 1056.0, 3.3),
            (386.0, 290.0, 320.0, 240.0, 1.21, 0.30, 1287.0, 4.0),
            (386.0, 290.0, 320.0, 240.0, 1.21, 0.31, 1245.0, 3.9),
            (386.0, 290.0, 320.0, 240.0, 1.21, 0.65, 594.0, 1.9),
            // CRT.md says 2.0; 660 / 320 = 2.06, which rounds to 2.1 (minor rounding slip).
            (528.0, 396.0, 320.0, 240.0, 1.65, 0.80, 660.0, 2.06),
            (280.0, 210.0, 640.0, 480.0, 0.44, 0.242, 1157.0, 1.8),
            (280.0, 210.0, 320.0, 400.0, 0.53, 0.242, 1157.0, 3.6),
            (325.0, 244.0, 1024.0, 768.0, 0.32, 0.25, 1300.0, 1.27),
            (402.0, 302.0, 1600.0, 1200.0, 0.25, 0.22, 1827.0, 1.14),
        ];
        for (w, h, src, lines, pitch, triad, n, per_px) in rows {
            let p = line_pitch_mm(h, lines);
            let t = triads_across(w, triad);
            let s = triads_per_source_px(t, src);
            assert!(close(p, pitch, 0.006), "line pitch {p} vs {pitch}");
            assert!(close(t, n, 0.6), "triads {t} vs {n}");
            // Table rounds to 2 significant digits above 1.5, 3 below.
            let tol = if per_px >= 1.5 { 0.05 } else { 0.005 };
            assert!(close(s, per_px, tol), "per source px {s} vs {per_px}");
        }
        // 256-wide source: x 1.25. 480 lines: half the 240-line pitch.
        assert!(close(
            triads_per_source_px(1245.0, 256.0) / triads_per_source_px(1245.0, 320.0),
            1.25,
            1e-12
        ));
        assert!(close(
            line_pitch_mm(290.0, 480.0),
            line_pitch_mm(290.0, 240.0) / 2.0,
            1e-12
        ));
    }

    #[test]
    fn pvm_20m4u_on_1920_px() {
        let triads = triads_across(386.0, 0.31);
        assert!(close(triads, 1245.0, 0.5));
        assert!(close(px_per_triad(1920.0, triads), 1.54, 0.005));
    }

    #[test]
    fn sagitta_19in_1r() {
        let d = inch_to_mm(19.0);
        let s = sagitta_mm(1.767 * d, d / 2.0);
        assert!(close(s, 34.85, 0.05), "sagitta {s}");
        // Table ratios: sagitta / diagonal 7.2 %, 4.8 %, 3.6 %, 0.9 %.
        for (r_over_d, ratio) in [(1.767, 0.072), (2.65, 0.048), (3.53, 0.036), (14.1, 0.009)] {
            let got = sagitta_mm(r_over_d * d, d / 2.0) / d;
            assert!(close(got, ratio, 0.0006), "R/D {r_over_d}: {got}");
        }
    }

    #[test]
    fn delta_pitch_0_28() {
        let got = delta_distances_mm(0.28);
        for (g, e) in got.iter().zip([0.485, 0.242, 0.140, 0.162]) {
            assert!(close(*g, e, 0.0006), "{g} vs {e}");
        }
    }

    #[test]
    fn moire() {
        assert!(close(moire_beat(3.0, 2.0), 6.0, 1e-12));
        assert!(moire_beat(1.0, 1.0).is_infinite());
    }
}
