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
///
/// # Panics
/// Unless 0 <= |`h_mm`| <= `r_mm` < inf (a chord cannot exceed the diameter).
pub fn sagitta_mm(r_mm: f64, h_mm: f64) -> f64 {
    assert!(
        r_mm.is_finite() && h_mm.abs() <= r_mm,
        "sagitta needs |h| <= R: R {r_mm}, h {h_mm}"
    );
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

    /// CRT.md reference-scale table, every column derived from the tube's
    /// diagonal alone (viewable = nominal - 1 in, or the stated viewable
    /// diagonal), 4:3, then compared with the printed (rounded) values: whole
    /// mm for sizes and triads, 0.01 mm for line pitch, 2 significant digits
    /// above 1.5 triads per source pixel and 3 below.
    #[test]
    fn reference_scale_table() {
        // (viewable diagonal in, source w, active lines, triad pitch,
        //  table: w, h, line pitch, triads across, triads per source px)
        let rows = [
            (13.0, 320.0, 240.0, 0.25, 264.0, 198.0, 0.83, 1056.0, 3.3),
            (19.0, 320.0, 240.0, 0.30, 386.0, 290.0, 1.21, 1287.0, 4.0),
            (19.0, 320.0, 240.0, 0.31, 386.0, 290.0, 1.21, 1245.0, 3.9),
            (19.0, 320.0, 240.0, 0.65, 386.0, 290.0, 1.21, 594.0, 1.9),
            (26.0, 320.0, 240.0, 0.80, 528.0, 396.0, 1.65, 660.0, 2.1),
            (14.0, 640.0, 480.0, 0.242, 284.0, 213.0, 0.44, 1176.0, 1.8),
            (14.0, 320.0, 400.0, 0.242, 284.0, 213.0, 0.53, 1176.0, 3.7),
            (16.0, 1024.0, 768.0, 0.25, 325.0, 244.0, 0.32, 1300.0, 1.27),
            (20.0, 1600.0, 1200.0, 0.22, 406.0, 305.0, 0.25, 1847.0, 1.15),
        ];
        for (diag_in, src, lines, triad, w, h, pitch, n, per_px) in rows {
            let (gw, gh) = visible_size_mm(inch_to_mm(diag_in), 4.0 / 3.0);
            assert!(close(gw, w, 0.5) && close(gh, h, 0.5), "{diag_in} in: {gw} x {gh}");
            let p = line_pitch_mm(gh, lines);
            let t = triads_across(gw, triad);
            let s = triads_per_source_px(t.round(), src);
            assert!(close(p, pitch, 0.005), "{diag_in} in: line pitch {p} vs {pitch}");
            // The table divides the width rounded to whole mm: +-0.5 mm / pitch.
            assert!(close(t, n, 0.5 + 0.5 / triad), "{diag_in} in: triads {t} vs {n}");
            let tol = if per_px >= 1.5 { 0.05 } else { 0.005 };
            assert!(close(s, per_px, tol), "{diag_in} in: per source px {s} vs {per_px}");
        }
        // Spot values derived by hand: 14 in viewable is 355.6 mm, 0.8 x =
        // 284.48 mm wide, / 0.242 = 1175.5 triads; 20 in = 508 mm, 406.4 mm,
        // / 0.22 = 1847.3 triads.
        assert!(close(
            triads_across(visible_size_mm(355.6, 4.0 / 3.0).0, 0.242),
            1175.537,
            0.001
        ));
        assert!(close(
            triads_across(visible_size_mm(508.0, 4.0 / 3.0).0, 0.22),
            1847.273,
            0.001
        ));
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
    #[should_panic(expected = "|h| <= R")]
    fn sagitta_chord_beyond_diameter_panics() {
        sagitta_mm(100.0, 101.0);
    }

    #[test]
    fn sagitta_hemisphere() {
        assert!(close(sagitta_mm(100.0, 100.0), 100.0, 1e-12));
        assert!(close(sagitta_mm(100.0, 60.0), 20.0, 1e-12));
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
