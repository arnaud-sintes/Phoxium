//! Source pre-filters (TODO Low, "source pre-filters (sinc, lanczos)"; parameter `source_prefilter`): how a native
//! pixel is read from the captured frame when the capture is bigger than the native raster (a window scaled by a
//! whole divisor `div` > 1, `scaling::View`).
//!
//! Native pixel `b` covers the block of `div` captured texels starting at `start = origin + b x div`; the block's
//! centre is `c = start + div / 2` in texel units (texel `s` has its centre at `s + 0.5`, so for an odd `div` the
//! centre is the middle of a texel and for an even one the edge between two). The default `point` reads the one
//! texel at `start + div / 2` (integer division), as before. The others weigh the texels
//! `s` around the block's centre by a kernel of the distance in native pixels, `x = (s + 0.5 - c) / div`,
//! separably (`w(x) w(y)`), and divide by the sum of the weights:
//! - `box`: 1 for |x| < 0.5: the mean of the block;
//! - `triangle`: `1 - |x|` up to 1: the bilinear (tent) filter;
//! - `lanczos2`, `lanczos3`: `sinc(x) sinc(x / a)` up to |x| < a (a = 2, 3);
//! - `sinc`: `sinc(x)` cut at |x| < 4, with its ringing.
//!
//! At `div` = 1 the block centre is a texel centre and every kernel is 1 there and 0 at the other integers, so
//! each filter is the identity (the shader skips them). The window is limited to 16 texels on each side of the
//! block's centre; the weights beyond are dropped (a large divisor with a wide kernel is a truncated one). Texels
//! outside the picture area (the crop rectangle of the view: `origin` to `origin + native x div`) repeat its edge
//! texel: the bars or the window chrome around a cropped picture never leak into its edge pixels. The filter acts on the captured code values (gamma-encoded), and the result
//! is clamped to [0, 1] (the negative lobes of the sinc family can overshoot).

/// A pre-filter kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Point,
    Box,
    Triangle,
    Lanczos2,
    Lanczos3,
    Sinc,
}

/// Largest window half width, texels.
pub const MAX_TEXELS: i64 = 16;

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let t = std::f64::consts::PI * x;
        t.sin() / t
    }
}

/// Half width of the kernel in native pixels (0 for `Point`).
#[must_use]
pub fn radius(kind: Kind) -> f64 {
    match kind {
        Kind::Point => 0.0,
        Kind::Box => 0.5,
        Kind::Triangle => 1.0,
        Kind::Lanczos2 => 2.0,
        Kind::Lanczos3 => 3.0,
        Kind::Sinc => 4.0,
    }
}

/// The kernel at distance `x` native pixels.
#[must_use]
pub fn weight(kind: Kind, x: f64) -> f64 {
    let a = x.abs();
    match kind {
        Kind::Point => f64::from(u8::from(a == 0.0)),
        Kind::Box => f64::from(u8::from(a < 0.5)),
        Kind::Triangle => (1.0 - a).max(0.0),
        Kind::Lanczos2 => {
            if a < 2.0 {
                sinc(a) * sinc(a / 2.0)
            } else {
                0.0
            }
        }
        Kind::Lanczos3 => {
            if a < 3.0 {
                sinc(a) * sinc(a / 3.0)
            } else {
                0.0
            }
        }
        Kind::Sinc => {
            if a < 4.0 {
                sinc(a)
            } else {
                0.0
            }
        }
    }
}

/// The texels `lo..=hi` of one axis that the kernel of `kind` covers for the block starting at `start` (the first
/// texel of the block) with `div` texels: the texel `floor(c)` of the block's centre `c = start + div / 2`, plus
/// and minus `ceil(r x div)` (a superset of the texels with a weight), limited to [`MAX_TEXELS`] on each side.
#[must_use]
pub fn window(kind: Kind, start: i64, div: i64) -> (i64, i64) {
    let centre = start as f64 + div as f64 * 0.5;
    let reach = ((radius(kind) * div as f64).ceil() as i64).clamp(0, MAX_TEXELS);
    let c = centre.floor() as i64;
    (c - reach, c + reach)
}

/// One axis: the weights of the texels `lo..=hi` of [`window`] for the block starting at `start`.
#[must_use]
pub fn axis_weights(kind: Kind, start: i64, div: i64) -> Vec<(i64, f64)> {
    let centre = start as f64 + div as f64 * 0.5;
    let (lo, hi) = window(kind, start, div);
    (lo..=hi)
        .map(|s| (s, weight(kind, ((s as f64) + 0.5 - centre) / div as f64)))
        .collect()
}

/// The filtered code values of the native pixel whose block starts at `start` = `origin + b x div` (texels), from
/// `fetch(x, y)` (the captured frame's code values in [0, 1], called with coordinates clamped into `area`), for a
/// picture area `area` = (first texel, last texel), inclusive (the crop rectangle of the view, inside the frame).
/// `Point` is the single texel at `start + div / 2`, clamped.
#[must_use]
pub fn filter(
    kind: Kind,
    start: (i64, i64),
    div: (i64, i64),
    area: ((i64, i64), (i64, i64)),
    fetch: &dyn Fn(i64, i64) -> [f64; 3],
) -> [f64; 3] {
    let at = |x: i64, y: i64| fetch(x.clamp(area.0 .0, area.1 .0), y.clamp(area.0 .1, area.1 .1));
    if kind == Kind::Point || div == (1, 1) {
        return at(start.0 + div.0 / 2, start.1 + div.1 / 2);
    }
    let wx = axis_weights(kind, start.0, div.0);
    let wy = axis_weights(kind, start.1, div.1);
    let (mut acc, mut sum) = ([0.0; 3], 0.0);
    for (y, vy) in &wy {
        for (x, vx) in &wx {
            let w = vx * vy;
            let v = at(*x, *y);
            for c in 0..3 {
                acc[c] += w * v[c];
            }
            sum += w;
        }
    }
    if sum.abs() < 1e-12 {
        return at(start.0 + div.0 / 2, start.1 + div.1 / 2);
    }
    acc.map(|a| (a / sum).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    /// Every kernel is 1 at 0 and 0 at the other integers (so a 1:1 capture is untouched); the box is 1 inside
    /// half a pixel, the triangle is the tent; the lanczos kernels are 0 at their radius.
    #[test]
    fn kernels_are_one_at_zero_and_zero_at_the_integers() {
        for kind in [Kind::Box, Kind::Triangle, Kind::Lanczos2, Kind::Lanczos3, Kind::Sinc] {
            assert!(close(weight(kind, 0.0), 1.0, 1e-15), "{kind:?}");
            for n in 1..=4 {
                assert!(weight(kind, f64::from(n)).abs() < 1e-12, "{kind:?} at {n}");
            }
        }
        assert_eq!(weight(Kind::Box, 0.49), 1.0);
        assert_eq!(weight(Kind::Box, 0.5), 0.0);
        assert!(close(weight(Kind::Triangle, 0.25), 0.75, 1e-15));
        // Lanczos2 at 0.5: sinc(0.5) sinc(0.25) = (2 / pi) x (sin(pi / 4) / (pi / 4)) = 0.63662 x 0.90032 = 0.57316.
        assert!(
            close(weight(Kind::Lanczos2, 0.5), 0.573_16, 1e-4),
            "{}",
            weight(Kind::Lanczos2, 0.5)
        );
        // Lanczos3 at 1.5: sinc(1.5) sinc(0.5) = (-2 / (3 pi)) x (2 / pi) = -0.21221 x 0.63662 = -0.13509.
        assert!(
            close(weight(Kind::Lanczos3, 1.5), -0.135_09, 1e-4),
            "{}",
            weight(Kind::Lanczos3, 1.5)
        );
        // The sinc is cut at 4.
        assert_eq!(weight(Kind::Sinc, 4.0), 0.0);
        assert!(weight(Kind::Sinc, 3.5).abs() > 0.0);
    }

    /// A divisor of 1 is the identity for every kernel: the point sample at the block's centre texel.
    #[test]
    fn a_one_to_one_capture_is_untouched() {
        let fetch = |x: i64, y: i64| [((x * 7 + y * 3) % 11) as f64 / 11.0, 0.5, 0.25];
        for kind in [
            Kind::Point,
            Kind::Box,
            Kind::Triangle,
            Kind::Lanczos2,
            Kind::Lanczos3,
            Kind::Sinc,
        ] {
            assert_eq!(
                filter(kind, (5, 6), (1, 1), ((0, 0), (31, 31)), &fetch),
                fetch(5, 6),
                "{kind:?}"
            );
        }
    }

    /// The box is the mean of the block: a 3 x 3 block of the values 1..9 / 9 averages 5 / 9, the same for every
    /// channel here; the point sample takes the centre texel (the 5).
    #[test]
    fn the_box_averages_the_block_and_the_point_takes_its_centre() {
        let fetch = |x: i64, y: i64| {
            let v = ((y - 3) * 3 + (x - 3) + 1) as f64 / 9.0;
            [v, v, v]
        };
        let b = filter(Kind::Box, (3, 3), (3, 3), ((0, 0), (15, 15)), &fetch);
        assert!(b.iter().all(|c| close(*c, 5.0 / 9.0, 1e-12)), "{b:?}");
        let p = filter(Kind::Point, (3, 3), (3, 3), ((0, 0), (15, 15)), &fetch);
        assert_eq!(p, fetch(4, 4));
    }

    /// A flat region stays flat under every kernel (the weights are normalised), and a lanczos of a step
    /// overshoots beyond the step's levels before the clamp: the clamp keeps it in [0, 1].
    #[test]
    fn flat_stays_flat_and_the_result_is_clamped() {
        let flat = |_: i64, _: i64| [0.4, 0.6, 0.2];
        for kind in [Kind::Box, Kind::Triangle, Kind::Lanczos2, Kind::Lanczos3, Kind::Sinc] {
            let v = filter(kind, (8, 8), (4, 4), ((0, 0), (63, 63)), &flat);
            for (a, b) in v.iter().zip([0.4, 0.6, 0.2]) {
                assert!(close(*a, b, 1e-12), "{kind:?}");
            }
        }
        // A black / white step at texel 12 with 4-texel blocks: the block starting at 12 (centre texel 14) of a
        // sinc-family kernel sees black on its left: values stay within [0, 1].
        let step = |x: i64, _: i64| if x < 12 { [0.0; 3] } else { [1.0; 3] };
        for kind in [Kind::Lanczos2, Kind::Lanczos3, Kind::Sinc] {
            for start in (4..28).step_by(4) {
                let v = filter(kind, (start, 8), (4, 4), ((0, 0), (63, 63)), &step);
                assert!(v.iter().all(|c| (0.0..=1.0).contains(c)), "{kind:?} {start} {v:?}");
            }
        }
    }

    /// The kernel never reads outside the picture area: a black 16 x 8 picture area inside white bars keeps its edge
    /// blocks black under every kernel (the bars would leak in if the taps were clamped to the frame instead).
    #[test]
    fn the_picture_area_edge_repeats_and_the_bars_do_not_leak() {
        let fetch = |x: i64, y: i64| {
            if (8..=23).contains(&x) && (4..=11).contains(&y) {
                [0.0; 3]
            } else {
                [1.0; 3]
            }
        };
        let area = ((8, 4), (23, 11));
        for kind in [Kind::Box, Kind::Triangle, Kind::Lanczos2, Kind::Lanczos3, Kind::Sinc] {
            // The first block (texels 8..12, 4..8) and the last (20..24, 8..12) of the area.
            for start in [(8, 4), (20, 8)] {
                let v = filter(kind, start, (4, 4), area, &fetch);
                assert_eq!(v, [0.0; 3], "{kind:?} at {start:?}");
            }
        }
    }

    /// The window is limited to 16 texels each side: a lanczos3 at a divisor of 8 would want 24.
    #[test]
    fn the_window_is_limited() {
        assert_eq!(window(Kind::Box, 0, 3), (-1, 3));
        let (lo, hi) = window(Kind::Lanczos3, 100, 8);
        assert_eq!((hi - lo) / 2, MAX_TEXELS);
        let (lo, hi) = window(Kind::Triangle, 100, 4);
        assert_eq!((lo, hi), (102 - 4, 102 + 4));
    }
}
