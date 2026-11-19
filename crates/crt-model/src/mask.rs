//! Shadow-mask and aperture-grille maths (T-38): emitter geometry in tube
//! millimetres, exact area integrals over a host-pixel footprint, accuracy
//! tiers and readouts (PHY-2, PHY-8).
//!
//! Conventions:
//! - Coordinates are tube millimetres, x to the right and y downwards, origin at
//!   the top-left corner of the visible area. Colour index `c`: 0 = R, 1 = G, 2 = B.
//! - The emitter function `e_c(x, y)` in [0, 1] is the luminous shape of the
//!   phosphor elements of colour `c` (black-matrix opening, CRT.md "Black matrix
//!   and phosphor screen"). It is what multiplies the beam image in the product
//!   form L_c = M_c B_c (CRT.md "Beam and mask interaction").
//! - Every edge is a linear ramp of full width `edge_mm` centred on the nominal
//!   (half-value) edge. With that choice the area of an element is exactly its
//!   nominal area for stripes and bridges, and pi (R^2 + s^2 / 12) for a dot.
//!   The ramp also makes every integral continuous in the rectangle bounds.
//! - `edge_mm` is the full 0-to-1 ramp width as a fraction of the type's quoted
//!   pitch (p for grille and slot, P for delta), centred on the half-value edge.
//!   For grille and slot `validate()` caps it at p/3 - stripe (and edge <= stripe),
//!   so the UI soft range should be min(0.10 p, p/3 - stripe).
//! - The host-pixel value of a channel is the area mean of `e_c` over the pixel
//!   footprint divided by the mean coverage of colour `c`, so a flat white field
//!   has mean 1 (PHY-2 option A).
//!
//! Geometry (CRT.md "Colour selection structures"):
//! - Aperture grille: continuous vertical stripes, triad pitch p, stripe centres
//!   at (c + 1/2) p / 3, stripe width 0.20 to 0.27 p [E], damper-wire shadows as
//!   horizontal dark bands 10 to 30 um wide [S].
//! - Slot mask: triad columns of pitch p with the same three stripes, cut by
//!   bridges of height b V every slot period V; adjacent columns are staggered by
//!   V / 2 [S]. The pattern repeats over (2 p, V) and over (p, V / 2).
//! - Delta: dots on the hexagonal lattice of spacing a = P / sqrt(3). Row j is at
//!   y = j P / 2, dot i of row j at x = (i + 1.5 j) a with colour i mod 3, so each
//!   row is shifted by 1.5 a (CRT.md "Delta (dot) triad geometry"). The pattern
//!   repeats over (P sqrt(3), P) and over (1.5 a, P / 2).

use crate::geometry::{moire_beat, px_per_triad};

/// Barten visibility threshold of a moire modulation, 1.4 %. [S] CRT.md "Moire".
pub const BARTEN_THRESHOLD: f64 = 0.014;

/// Product form L = M x B is valid when the spot FWHM is at least this many
/// triad pitches. [D] CRT.md "Moire" ("a spot of at least 1.5 pitches"), PHY-8.
pub const PRODUCT_FORM_MIN_FWHM_OVER_PITCH: f64 = 1.5;

/// Tier A from this many host pixels per triad (resolved). PHY-2.
pub const TIER_A_MIN_PX_PER_TRIAD: f64 = 6.0;

/// Tier B from this many host pixels per triad (area integrated); tier C below. PHY-2.
pub const TIER_B_MIN_PX_PER_TRIAD: f64 = 3.0;

/// Option D default: presets whose px per triad reaches this on the current host
/// are offered as resolvable alternatives. PLAN T-38 (presets at 2.9 qualify).
pub const OPTION_D_THRESHOLD_PX_PER_TRIAD: f64 = 2.8;

/// Axis-aligned rectangle in tube millimetres (a host-pixel footprint).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn area(&self) -> f64 {
        (self.x1 - self.x0) * (self.y1 - self.y0)
    }
}

/// Aperture grille (Trinitron, Diamondtron).
#[derive(Clone, Debug, PartialEq)]
pub struct Grille {
    /// Triad (stripe) pitch p. [S] per model, CRT.md "Pitch values by family".
    pub pitch_mm: f64,
    /// Stripe width per colour, 0.20 to 0.27 p. [E]
    pub stripe_mm: f64,
    /// Full width of the soft edge ramp, about 5 to 10 % of p. [E]
    pub edge_mm: f64,
    /// Damper-wire shadow centres, from the top of the visible area.
    pub damper_y_mm: Vec<f64>,
    /// Damper-wire shadow width, 0.010 to 0.030 mm. [S]
    pub damper_mm: f64,
    /// Full width of the damper shadow edge ramp (penumbra). [E]
    pub damper_edge_mm: f64,
}

/// Slot mask (in-line guns, staggered slots).
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// Horizontal triad pitch p. [S] per model.
    pub pitch_mm: f64,
    /// Stripe width per colour. [E]
    pub stripe_mm: f64,
    /// Full width of the soft edge ramp, stripes and bridges. [E]
    pub edge_mm: f64,
    /// Vertical slot period V, typically 2 to 3 times p / 3. [E]
    pub slot_period_mm: f64,
    /// Bridge height over V, about 0.10 to 0.20. [E]
    pub bridge_fraction: f64,
}

/// Delta (dot) shadow mask.
#[derive(Clone, Debug, PartialEq)]
pub struct Delta {
    /// Diagonal dot pitch P (nearest same-colour dots, 30 degrees above horizontal). [S]
    pub pitch_mm: f64,
    /// Dot diameter, about 0.4 to 0.5 P. [E]
    pub dot_mm: f64,
    /// Full width of the soft edge ramp, about 5 to 10 % of P. [E]
    pub edge_mm: f64,
}

/// The three mask types (PHY-4: the only structural choice).
#[derive(Clone, Debug, PartialEq)]
pub enum Mask {
    Grille(Grille),
    Slot(Slot),
    Delta(Delta),
}

/// Accuracy tier from host pixels per triad (PHY-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// At least 6 px per triad: element shapes resolved.
    A,
    /// 3 to 6 px per triad: area integrated.
    B,
    /// Under 3 px per triad: prefiltered average only.
    C,
}

/// Tier for a number of host pixels per triad.
pub fn tier(px_per_triad: f64) -> Tier {
    if px_per_triad >= TIER_A_MIN_PX_PER_TRIAD {
        Tier::A
    } else if px_per_triad >= TIER_B_MIN_PX_PER_TRIAD {
        Tier::B
    } else {
        Tier::C
    }
}

/// Smallest vertical mask period, in host px, that is drawn by its area
/// integral (T-39c, D-15). [D] The vertical structure (slot rows of period V / 2
/// once averaged over x, delta rows of period P / 2) is the same for the three
/// colours: there is no colour sub-element to give its own pixel, which is what
/// sets the horizontal limit at 3 px (one per primary, CRT.md "Host-display
/// resolution budget"). What remains is the sampling theorem: a period under
/// 2 px puts its fundamental above the host Nyquist frequency of 0.5 cycle per
/// px, where it aliases to a lower frequency, a ripple that is not on the tube.
/// Measured on the integrated host grid (row means, tests below and the GPU
/// tests): delta at 3.0 px per triad (P / 2 = 1.73 px) 27 % and slot defaults
/// (V / 2 = 1.25 px) 5.4 to 6.1 % depending on the row phase, both above
/// the Barten threshold of 1.4 %, so they must be prefiltered. At 2 px and
/// above the dominant row component is drawn at its own frequency (no alias).
pub const VERTICAL_MIN_PERIOD_PX: f64 = 2.0;

/// Default width of the blend band, in px per triad, centred on the horizontal
/// limit (2.8 to 3.2). `mask_tier_blend`, D-15. The vertical band is the same
/// relative width around its limit: blend x 2 / 3 px (1.867 to 2.133 px).
pub const DEFAULT_TIER_BLEND: f64 = 0.4;

/// `mask_tier`: how the tier of each axis is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TierMode {
    /// Per axis from the host px per period, with the blend band ([`axis_tiers`]).
    Auto,
    /// Area integral over the host pixel on both axes, at any density.
    Integrated,
    /// Both periodic axes averaged over one whole period (the old tier C).
    Prefiltered,
}

/// The tier rule: mode and blend band width (px per triad, 0 = hard switch).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TierRule {
    pub mode: TierMode,
    pub blend: f64,
}

impl Default for TierRule {
    fn default() -> Self {
        Self {
            mode: TierMode::Auto,
            blend: DEFAULT_TIER_BLEND,
        }
    }
}

/// How one axis of the mask is brought to host pixels (T-39c).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AxisTier {
    /// The host pixel itself: exact area integral over its footprint.
    Integrated,
    /// Blend band: the footprint is a box of `box_px` host px (1 < box < one
    /// period) centred on the pixel. Its mean over all positions is still 1.
    Blended { box_px: f64 },
    /// The footprint is a box of one whole period: the axis shows its mean only.
    Prefiltered,
}

impl AxisTier {
    /// Width in host px of the footprint along this axis, for an axis whose
    /// prefilter period is `period_px`.
    pub fn box_px(&self, period_px: f64) -> f64 {
        match *self {
            AxisTier::Integrated => 1.0,
            AxisTier::Blended { box_px } => box_px,
            AxisTier::Prefiltered => period_px,
        }
    }
}

/// Tier of both axes. The grille has no vertical period: its vertical is always
/// integrated (the damper wires are not periodic and stay in every tier).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisTiers {
    pub horizontal: AxisTier,
    pub vertical: AxisTier,
}

/// Tier of one axis whose deciding period is `period_px` host px, against
/// `limit_px` with a blend band `band_px` wide centred on the limit; `box_period_px`
/// is the prefilter box of the axis (one whole period). Under the band:
/// prefiltered; over it: integrated; inside it the box width falls linearly
/// from `box_period_px` at the lower edge to 1 px at the upper edge, so the
/// footprint, and every pixel value, is continuous in the pitch. `band_px <= 0`
/// is the hard switch at the limit (integrated from the limit up).
pub fn axis_tier(period_px: f64, limit_px: f64, band_px: f64, box_period_px: f64) -> AxisTier {
    if band_px <= 0.0 || band_px.is_nan() {
        return if period_px >= limit_px {
            AxisTier::Integrated
        } else {
            AxisTier::Prefiltered
        };
    }
    let (lo, hi) = (limit_px - 0.5 * band_px, limit_px + 0.5 * band_px);
    if period_px >= hi {
        AxisTier::Integrated
    } else if period_px <= lo {
        AxisTier::Prefiltered
    } else {
        let u = (period_px - lo) / (hi - lo);
        AxisTier::Blended {
            box_px: (1.0 - u) * box_period_px + u,
        }
    }
}

/// Per-axis tier decision (T-39c, D-15). `triad_px`: horizontal triad pitch in
/// host px (the box of a horizontal prefilter is one triad); `vertical`: for the
/// slot and delta masks, (deciding period, prefilter period) in host px, that
/// is (V / 2, V) and (P / 2, P), none for the grille. Horizontal limit
/// [`TIER_B_MIN_PX_PER_TRIAD`] with band `rule.blend`; vertical limit
/// [`VERTICAL_MIN_PERIOD_PX`] with band `rule.blend` x 2 / 3 (same relative width).
///
/// Why the vertical box is the whole period V (P) while V / 2 (P / 2) decides:
/// a separable box must hold a whole period of every column, otherwise the
/// stagger component (1 / 2p, 1 / V) of the slot (delta: the hexagonal
/// fundamentals (1 / 2t, 1 / P)) survives at 2 / pi of its amplitude and the
/// two-axis prefilter is no longer flat. The boxes one triad x V (one triad x P)
/// are those of the former tier C, whose mean is exactly 1 at every position.
pub fn axis_tiers(triad_px: f64, vertical: Option<(f64, f64)>, rule: TierRule) -> AxisTiers {
    match rule.mode {
        TierMode::Integrated => AxisTiers {
            horizontal: AxisTier::Integrated,
            vertical: AxisTier::Integrated,
        },
        TierMode::Prefiltered => AxisTiers {
            horizontal: AxisTier::Prefiltered,
            vertical: vertical.map_or(AxisTier::Integrated, |_| AxisTier::Prefiltered),
        },
        TierMode::Auto => AxisTiers {
            horizontal: axis_tier(triad_px, TIER_B_MIN_PX_PER_TRIAD, rule.blend, triad_px),
            vertical: vertical.map_or(AxisTier::Integrated, |(decide, full)| {
                axis_tier(
                    decide,
                    VERTICAL_MIN_PERIOD_PX,
                    rule.blend * VERTICAL_MIN_PERIOD_PX / TIER_B_MIN_PX_PER_TRIAD,
                    full,
                )
            }),
        },
    }
}

/// Trapezoid pulse centred on 0 with nominal (half-value) width `w` and edge
/// ramps of full width `s`: 1 for |r| <= (w - s) / 2, 0 for |r| >= (w + s) / 2.
fn pulse(r: f64, w: f64, s: f64) -> f64 {
    if s <= 0.0 {
        if r.abs() <= 0.5 * w {
            1.0
        } else {
            0.0
        }
    } else {
        ((0.5 * (w + s) - r.abs()) / s).clamp(0.0, 1.0)
    }
}

/// Integral of `pulse` from minus infinity to `r` (piecewise quadratic, needs s <= w).
/// Total area is w.
fn pulse_cumulative(r: f64, w: f64, s: f64) -> f64 {
    let e = 0.5 * w;
    let h = 0.5 * s;
    if r <= -e - h {
        0.0
    } else if r < -e + h {
        (r + e + h).powi(2) / (2.0 * s)
    } else if r <= e - h {
        r + e
    } else if r < e + h {
        w - (e + h - r).powi(2) / (2.0 * s)
    } else {
        w
    }
}

/// Cumulative integral (up to a constant) of a pulse train of period `period`
/// with pulses centred on k * period. Needs w + s <= period, so that only pulse k
/// is non-zero on [k T - T / 2, k T + T / 2).
fn train_cumulative(u: f64, period: f64, w: f64, s: f64) -> f64 {
    let k = (u / period + 0.5).floor();
    k * w + pulse_cumulative(u - k * period, w, s)
}

/// Integral over [a, b] of a pulse train centred on `centre` + k * `period`.
fn train_integral(a: f64, b: f64, centre: f64, period: f64, w: f64, s: f64) -> f64 {
    train_cumulative(b - centre, period, w, s) - train_cumulative(a - centre, period, w, s)
}

/// Stripe centre of colour `c` inside a triad that starts at 0: (c + 1/2) p / 3.
fn stripe_centre(c: usize, pitch: f64) -> f64 {
    (c as f64 + 0.5) * pitch / 3.0
}

/// Signed area of the disc of radius r at the origin inside [0, X] x [0, Y]
/// (odd in X and in Y), so a rectangle is an inclusion-exclusion of four terms.
fn disc_quadrant(x: f64, y: f64, r: f64) -> f64 {
    if x == 0.0 || y == 0.0 {
        return 0.0;
    }
    let sign = x.signum() * y.signum();
    let xa = x.abs().min(r);
    let ya = y.abs().min(r);
    if xa * xa + ya * ya <= r * r {
        return sign * xa * ya;
    }
    // Columns 0 < t < xc have height ya, columns xc < t < xa have height
    // sqrt(r^2 - t^2), whose antiderivative is (t sqrt(r^2 - t^2) + r^2 asin(t / r)) / 2.
    let f = |t: f64| 0.5 * (t * (r * r - t * t).max(0.0).sqrt() + r * r * (t / r).clamp(-1.0, 1.0).asin());
    let xc = (r * r - ya * ya).max(0.0).sqrt();
    sign * (ya * xc + f(xa) - f(xc))
}

/// Exact area of a hard disc (centre cx, cy, radius r) inside a rectangle.
fn disc_rect_area(cx: f64, cy: f64, r: f64, q: &Rect) -> f64 {
    let (x0, x1, y0, y1) = (q.x0 - cx, q.x1 - cx, q.y0 - cy, q.y1 - cy);
    disc_quadrant(x1, y1, r) - disc_quadrant(x0, y1, r) - disc_quadrant(x1, y0, r) + disc_quadrant(x0, y0, r)
}

/// Gauss-Legendre 8-point nodes and weights on [-1, 1]. [K]
const GL8: [(f64, f64); 8] = [
    (-0.960_289_856_497_536_3, 0.101_228_536_290_376_3),
    (-0.796_666_477_413_626_7, 0.222_381_034_453_374_5),
    (-0.525_532_409_916_329, 0.313_706_645_877_887_3),
    (-0.183_434_642_495_649_8, 0.362_683_783_378_362),
    (0.183_434_642_495_649_8, 0.362_683_783_378_362),
    (0.525_532_409_916_329, 0.313_706_645_877_887_3),
    (0.796_666_477_413_626_7, 0.222_381_034_453_374_5),
    (0.960_289_856_497_536_3, 0.101_228_536_290_376_3),
];

/// Integral over a rectangle of a soft dot: 1 inside radius R - s/2, 0 outside
/// R + s/2, linear in between.
///
/// The linear ramp is the mean of hard discs of radius rho uniform on
/// [R - s/2, R + s/2], so the integral is (1 / s) * integral of A(rho) d rho with
/// A the exact disc-rectangle area. A(rho) is smooth except where the circle
/// becomes tangent to an edge line or crosses a corner; the interval is split at
/// those radii and each piece uses a fixed 8-point Gauss-Legendre rule.
fn soft_dot_rect(cx: f64, cy: f64, radius: f64, s: f64, q: &Rect) -> f64 {
    let rmax = radius + 0.5 * s;
    if q.x0 <= cx - rmax && q.x1 >= cx + rmax && q.y0 <= cy - rmax && q.y1 >= cy + rmax {
        return std::f64::consts::PI * (radius * radius + s * s / 12.0);
    }
    if s <= 0.0 {
        return disc_rect_area(cx, cy, radius, q);
    }
    let lo = radius - 0.5 * s;
    let (dx0, dx1, dy0, dy1) = (q.x0 - cx, q.x1 - cx, q.y0 - cy, q.y1 - cy);
    let mut cuts = vec![lo, rmax];
    for d in [dx0, dx1, dy0, dy1] {
        cuts.push(d.abs());
    }
    for dx in [dx0, dx1] {
        for dy in [dy0, dy1] {
            cuts.push(dx.hypot(dy));
        }
    }
    cuts.retain(|&r| r >= lo && r <= rmax);
    cuts.sort_by(f64::total_cmp);
    let mut sum = 0.0;
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b <= a {
            continue;
        }
        let (mid, half) = (0.5 * (a + b), 0.5 * (b - a));
        for (t, wt) in GL8 {
            sum += wt * half * disc_rect_area(cx, cy, mid + half * t, q);
        }
    }
    sum / s
}

impl Grille {
    fn damper_factor(&self, y: f64) -> f64 {
        1.0 - self
            .damper_y_mm
            .iter()
            .map(|&yw| pulse(y - yw, self.damper_mm, self.damper_edge_mm))
            .sum::<f64>()
    }

    /// Integral of the damper factor over [y0, y1].
    fn damper_integral(&self, y0: f64, y1: f64) -> f64 {
        let shadow: f64 = self
            .damper_y_mm
            .iter()
            .map(|&yw| {
                pulse_cumulative(y1 - yw, self.damper_mm, self.damper_edge_mm)
                    - pulse_cumulative(y0 - yw, self.damper_mm, self.damper_edge_mm)
            })
            .sum();
        (y1 - y0) - shadow
    }
}

impl Slot {
    /// Bridge phase of triad column k: 0 for even columns, V / 2 for odd ones.
    fn phase(&self, k: f64) -> f64 {
        if (k.rem_euclid(2.0)) < 0.5 {
            0.0
        } else {
            0.5 * self.slot_period_mm
        }
    }

    fn bridge_mm(&self) -> f64 {
        self.bridge_fraction * self.slot_period_mm
    }
}

impl Delta {
    /// Nearest-neighbour spacing a = P / sqrt(3).
    fn spacing(&self) -> f64 {
        self.pitch_mm / 3.0_f64.sqrt()
    }

    /// Centre and colour of dot i of row j.
    pub fn dot(&self, i: i64, j: i64) -> (f64, f64, usize) {
        let a = self.spacing();
        let x = (i as f64 + 1.5 * j as f64) * a;
        let y = j as f64 * 0.5 * self.pitch_mm;
        (x, y, i.rem_euclid(3) as usize)
    }

    /// Calls `f` for every dot of colour c whose support can reach [x0, x1] x [y0, y1].
    fn for_dots(&self, c: usize, x0: f64, y0: f64, x1: f64, y1: f64, mut f: impl FnMut(f64, f64)) {
        let a = self.spacing();
        let row = 0.5 * self.pitch_mm;
        let rmax = 0.5 * (self.dot_mm + self.edge_mm);
        let j0 = ((y0 - rmax) / row).floor() as i64;
        let j1 = ((y1 + rmax) / row).ceil() as i64;
        for j in j0..=j1 {
            let i0 = ((x0 - rmax) / a - 1.5 * j as f64).floor() as i64;
            let i1 = ((x1 + rmax) / a - 1.5 * j as f64).ceil() as i64;
            for i in i0..=i1 {
                let (x, y, col) = self.dot(i, j);
                if col == c {
                    f(x, y);
                }
            }
        }
    }
}

impl Mask {
    /// Checks the geometry the closed forms rely on: no overlap between elements
    /// of different colours, ramps no wider than the element.
    pub fn validate(&self) -> Result<(), String> {
        let pos = |v: f64, n: &str| {
            if v.is_finite() && v > 0.0 {
                Ok(())
            } else {
                Err(format!("{n} must be positive"))
            }
        };
        let nonneg = |v: f64, n: &str| {
            if v.is_finite() && v >= 0.0 {
                Ok(())
            } else {
                Err(format!("{n} must be zero or positive"))
            }
        };
        match self {
            Mask::Grille(g) => {
                pos(g.pitch_mm, "pitch")?;
                pos(g.stripe_mm, "stripe width")?;
                nonneg(g.edge_mm, "edge")?;
                nonneg(g.damper_mm, "damper width")?;
                nonneg(g.damper_edge_mm, "damper edge")?;
                if g.edge_mm > g.stripe_mm || g.stripe_mm + g.edge_mm > g.pitch_mm / 3.0 {
                    return Err("stripe plus edge must fit in p / 3 and edge <= stripe".into());
                }
                if g.damper_edge_mm > g.damper_mm {
                    return Err("damper edge must not exceed damper width".into());
                }
            }
            Mask::Slot(m) => {
                pos(m.pitch_mm, "pitch")?;
                pos(m.stripe_mm, "stripe width")?;
                nonneg(m.edge_mm, "edge")?;
                pos(m.slot_period_mm, "slot period")?;
                if !(m.bridge_fraction > 0.0 && m.bridge_fraction < 1.0) {
                    return Err("bridge fraction must be in (0, 1)".into());
                }
                if m.edge_mm > m.stripe_mm || m.stripe_mm + m.edge_mm > m.pitch_mm / 3.0 {
                    return Err("stripe plus edge must fit in p / 3 and edge <= stripe".into());
                }
                if m.edge_mm > m.bridge_mm() || m.bridge_mm() + m.edge_mm > m.slot_period_mm {
                    return Err("bridge plus edge must fit in the slot period and edge <= bridge".into());
                }
            }
            Mask::Delta(d) => {
                pos(d.pitch_mm, "pitch")?;
                pos(d.dot_mm, "dot diameter")?;
                nonneg(d.edge_mm, "edge")?;
                if d.edge_mm > d.dot_mm || d.dot_mm + d.edge_mm > d.spacing() {
                    return Err("dot plus edge must fit in P / sqrt(3) and edge <= dot".into());
                }
            }
        }
        Ok(())
    }

    /// Emitter value e_c(x, y) in [0, 1] at one point (pointwise definition).
    pub fn emitter(&self, c: usize, x: f64, y: f64) -> f64 {
        match self {
            Mask::Grille(g) => {
                let u = x - g.pitch_mm * (x / g.pitch_mm).floor();
                pulse(u - stripe_centre(c, g.pitch_mm), g.stripe_mm, g.edge_mm) * g.damper_factor(y)
            }
            Mask::Slot(m) => {
                let k = (x / m.pitch_mm).floor();
                let u = x - k * m.pitch_mm;
                let stripe = pulse(u - stripe_centre(c, m.pitch_mm), m.stripe_mm, m.edge_mm);
                let v = y - m.phase(k);
                let r = v - m.slot_period_mm * (v / m.slot_period_mm).round();
                stripe * (1.0 - pulse(r, m.bridge_mm(), m.edge_mm))
            }
            Mask::Delta(d) => {
                let radius = 0.5 * d.dot_mm;
                let mut sum = 0.0;
                d.for_dots(c, x, y, x, y, |cx, cy| {
                    let r = (x - cx).hypot(y - cy);
                    sum += if d.edge_mm > 0.0 {
                        ((radius + 0.5 * d.edge_mm - r) / d.edge_mm).clamp(0.0, 1.0)
                    } else if r <= radius {
                        1.0
                    } else {
                        0.0
                    };
                });
                sum
            }
        }
    }

    /// Mean coverage f_c of colour c over the periodic pattern (damper wires,
    /// which are not periodic, excluded).
    /// Grille W / p; slot (W / p)(1 - b); delta 2 pi (R^2 + s^2 / 12) / (sqrt(3) P^2)
    /// (two dots of each colour per P sqrt(3) x P cell). [D]
    pub fn coverage(&self) -> f64 {
        match self {
            Mask::Grille(g) => g.stripe_mm / g.pitch_mm,
            Mask::Slot(m) => m.stripe_mm / m.pitch_mm * (1.0 - m.bridge_fraction),
            Mask::Delta(d) => {
                let r = 0.5 * d.dot_mm;
                2.0 * std::f64::consts::PI * (r * r + d.edge_mm * d.edge_mm / 12.0)
                    / (3.0_f64.sqrt() * d.pitch_mm * d.pitch_mm)
            }
        }
    }

    /// Exact integral of e_c over a rectangle (mm^2). Closed form for the grille
    /// and the slot mask (separable per triad column), radial Gauss-Legendre over
    /// exact disc areas for the delta dots.
    pub fn integral(&self, c: usize, q: &Rect) -> f64 {
        match self {
            Mask::Grille(g) => {
                train_integral(
                    q.x0,
                    q.x1,
                    stripe_centre(c, g.pitch_mm),
                    g.pitch_mm,
                    g.stripe_mm,
                    g.edge_mm,
                ) * g.damper_integral(q.y0, q.y1)
            }
            Mask::Slot(m) => {
                let p = m.pitch_mm;
                let centre = stripe_centre(c, p);
                let k0 = (q.x0 / p).floor();
                let k1 = (q.x1 / p).floor();
                let mut sum = 0.0;
                let mut k = k0;
                while k <= k1 {
                    let left = k * p;
                    let xa = q.x0.max(left);
                    let xb = q.x1.min(left + p);
                    if xb > xa {
                        let xs = pulse_cumulative(xb - left - centre, m.stripe_mm, m.edge_mm)
                            - pulse_cumulative(xa - left - centre, m.stripe_mm, m.edge_mm);
                        let bridges =
                            train_integral(q.y0, q.y1, m.phase(k), m.slot_period_mm, m.bridge_mm(), m.edge_mm);
                        sum += xs * ((q.y1 - q.y0) - bridges);
                    }
                    k += 1.0;
                }
                sum
            }
            Mask::Delta(d) => {
                let mut sum = 0.0;
                d.for_dots(c, q.x0, q.y0, q.x1, q.y1, |cx, cy| {
                    sum += soft_dot_rect(cx, cy, 0.5 * d.dot_mm, d.edge_mm, q);
                });
                sum
            }
        }
    }

    /// Normalised area mean of colour c over a host-pixel footprint (tiers A and
    /// B): integral / (area * coverage). Flat white has mean 1.
    pub fn area_mean(&self, c: usize, q: &Rect) -> f64 {
        self.integral(c, q) / (q.area() * self.coverage())
    }

    /// Tier C value: the host-pixel footprint convolved with a box prefilter of
    /// exactly one mask period along each periodic axis (width >= 1 triad).
    ///
    /// A box of one full period integrates every period completely, so the
    /// prefiltered mask is constant along that axis and equal to the period mean:
    /// grille, x box p, value 1 times the damper factor averaged over the pixel
    /// height (the wires are not periodic and stay); slot, box p x V, for any
    /// position the p window holds a fraction alpha of a stripe of one column
    /// phase and 1 - alpha of the other, each integrating a full V period to the
    /// same value, so 1; delta, box (1.5 a) x P, the same argument with the rows
    /// j and j + 1 (shifted by 1.5 a) in the role of the two column phases, so 1.
    /// The flat-field ripple is therefore 0, under the 1.4 % Barten threshold.
    ///
    /// Tier C caveat: in tier C the mask vanishes entirely (no visible structure
    /// remains), so a hard switch jumps at 3.0 px per triad (the unfiltered ripple
    /// is 100 % from 2.9 to 3.05 px per triad); the blend band of T-39c
    /// ([`axis_tiers`]) narrows the box continuously instead. Renders at less
    /// than 3 px per triad must therefore use `area_mean` over a box-filtered
    /// footprint (see `tier_c_area_mean_box_ripple`), not the unfiltered pitch grid.
    /// This function returns the constant 1 (damper wires aside) by construction, so
    /// it is not evidence of the ripple; `prefilter_box_integrates_to_the_mean` is.
    pub fn prefiltered(&self, _c: usize, q: &Rect) -> f64 {
        match self {
            Mask::Grille(g) => g.damper_integral(q.y0, q.y1) / (q.y1 - q.y0),
            Mask::Slot(_) | Mask::Delta(_) => 1.0,
        }
    }

    /// Per-axis tiers of this mask seen at `host_px_per_mm` (T-39c, [`axis_tiers`]).
    pub fn axis_tiers(&self, host_px_per_mm: f64, rule: TierRule) -> AxisTiers {
        let vertical = self
            .vertical_period_mm()
            .zip(self.vertical_box_mm())
            .map(|(v, full)| (host_px_per_mm * v, host_px_per_mm * full));
        axis_tiers(host_px_per_mm * self.triad_pitch_mm(), vertical, rule)
    }

    /// Footprint of a host pixel `q` under `tiers` (T-39c): each axis is the
    /// pixel itself (integrated), a box of `box_px` pixels or of one whole period
    /// (prefiltered: one triad horizontally, V or P vertically) centred on the
    /// pixel centre. `px_mm` is the size of one host pixel in mask units.
    pub fn footprint(&self, q: &Rect, tiers: AxisTiers, px_mm: f64) -> Rect {
        let width = |t: AxisTier, pixel: f64, period: f64| match t {
            AxisTier::Integrated => pixel,
            AxisTier::Blended { box_px } => box_px * px_mm,
            AxisTier::Prefiltered => period,
        };
        let w = width(tiers.horizontal, q.x1 - q.x0, self.triad_pitch_mm());
        let h = width(
            tiers.vertical,
            q.y1 - q.y0,
            self.vertical_box_mm().unwrap_or(q.y1 - q.y0),
        );
        let (cx, cy) = (0.5 * (q.x0 + q.x1), 0.5 * (q.y0 + q.y1));
        Rect::new(cx - 0.5 * w, cy - 0.5 * h, cx + 0.5 * w, cy + 0.5 * h)
    }

    /// Host-pixel value of channel c under the per-axis tiers `tiers` (T-39c):
    /// the area mean over [`Mask::footprint`]. Both periodic axes prefiltered is
    /// [`Mask::prefiltered`] (the closed form, exactly 1 aside from damper wires),
    /// and so is a grille prefiltered horizontally (its vertical is never periodic).
    /// A box over a whole number of periods keeps every value at the mean; a box of
    /// a fraction rho of a period (blend band) leaves a ripple, bounded for hard
    /// edges and coverage f per period by (1 - rho) / (2 f + rho - 1) when
    /// rho >= max(f, 1 - f) (the window holds between rho - (1 - f) and f of one
    /// element): it falls to 0 linearly as rho reaches 1, so the band is continuous.
    pub fn host_value(&self, c: usize, q: &Rect, tiers: AxisTiers, px_mm: f64) -> f64 {
        let prefiltered = tiers.horizontal == AxisTier::Prefiltered
            && (tiers.vertical == AxisTier::Prefiltered || matches!(self, Mask::Grille(_)));
        if prefiltered {
            self.prefiltered(c, q)
        } else {
            self.area_mean(c, &self.footprint(q, tiers, px_mm))
        }
    }

    /// Largest host-pixel value [`Mask::host_value`] of a flat white field under `tiers`, over the three
    /// channels and every pixel phase, for host pixels of `px_mm` mask units (T-84 headroom readout, audit B1).
    /// The value of a channel is its area mean over coverage, so its mean over the phase is 1 and this peak
    /// over that mean is how far the mask lifts the brightest pixel above the field mean: about 1 / fill for
    /// a resolved stripe, 1 when prefiltered. The phases searched are the pixel centred on each element of
    /// each colour (where a box-filtered symmetric element peaks) and a 24 x 24 grid over one period of the
    /// pattern (a guard for overlapping boxes); damper wires only lower the value and are not modelled here. [D]
    pub fn peak_value(&self, tiers: AxisTiers, px_mm: f64) -> f64 {
        let (period_x, period_y) = match self {
            Mask::Grille(g) => (g.pitch_mm, px_mm),
            Mask::Slot(m) => (2.0 * m.pitch_mm, m.slot_period_mm),
            Mask::Delta(d) => (d.pitch_mm * 3.0_f64.sqrt(), d.pitch_mm),
        };
        let at = |c: usize, x: f64, y: f64| {
            let q = Rect::new(x - 0.5 * px_mm, y - 0.5 * px_mm, x + 0.5 * px_mm, y + 0.5 * px_mm);
            self.host_value(c, &q, tiers, px_mm)
        };
        let mut peak: f64 = 0.0;
        for c in 0..3 {
            let (cx, cy) = match self {
                Mask::Grille(g) => (stripe_centre(c, g.pitch_mm), 0.0),
                // Column 0 has its bridges centred on y = 0, so its slots are centred on V / 2.
                Mask::Slot(m) => (stripe_centre(c, m.pitch_mm), 0.5 * m.slot_period_mm),
                Mask::Delta(d) => {
                    let (x, y, _) = d.dot(c as i64, 0);
                    (x, y)
                }
            };
            peak = peak.max(at(c, cx, cy));
            const N: usize = 24;
            // The grille's value does not depend on y (no dampers here): one row is enough.
            let rows = if matches!(self, Mask::Grille(_)) { 1 } else { N };
            for i in 0..N {
                for j in 0..rows {
                    let x = period_x * i as f64 / N as f64;
                    let y = period_y * j as f64 / N as f64;
                    peak = peak.max(at(c, x, y));
                }
            }
        }
        peak
    }

    /// Vertical prefilter box: the whole vertical period of one column, V for the
    /// slot mask and P for delta (rows j and j + 2 hold the same colour at the
    /// same x); none for the grille. See [`axis_tiers`] for why not V / 2 or P / 2.
    pub fn vertical_box_mm(&self) -> Option<f64> {
        match self {
            Mask::Grille(_) => None,
            Mask::Slot(m) => Some(m.slot_period_mm),
            Mask::Delta(d) => Some(d.pitch_mm),
        }
    }

    /// Horizontal triad pitch (the y-averaged horizontal period): p for the grille
    /// and the slot mask, P sqrt(3) / 2 for delta (vendor "horizontal dot pitch"). [S]
    pub fn triad_pitch_mm(&self) -> f64 {
        match self {
            Mask::Grille(g) => g.pitch_mm,
            Mask::Slot(m) => m.pitch_mm,
            Mask::Delta(d) => d.pitch_mm * 3.0_f64.sqrt() / 2.0,
        }
    }

    /// Vertical period of the x-averaged mask, the one scanlines beat against.
    /// Slot: V / 2, because the half-slot stagger of adjacent columns cancels the
    /// odd harmonics of the vertical mask frequency 1 / V once averaged over x.
    /// Per type:
    /// none for the grille, V / 2 for the slot mask (the column stagger halves
    /// V once averaged over two columns), P / 2 for delta (row spacing). [D]
    pub fn vertical_period_mm(&self) -> Option<f64> {
        match self {
            Mask::Grille(_) => None,
            Mask::Slot(m) => Some(0.5 * m.slot_period_mm),
            Mask::Delta(d) => Some(0.5 * d.pitch_mm),
        }
    }
}

/// Damper-wire positions from the top of the visible area: one wire at 2/3 of the
/// height (15 in and below), two at 1/3 and 2/3 [S]; three at 1/4, 1/2 and 3/4 [E]
/// (CRT.md gives the count above 21 in, not the positions).
pub fn damper_positions_mm(count: usize, visible_height_mm: f64) -> Vec<f64> {
    let h = visible_height_mm;
    match count {
        0 => vec![],
        1 => vec![2.0 * h / 3.0],
        2 => vec![h / 3.0, 2.0 * h / 3.0],
        n => (1..=n).map(|k| k as f64 * h / (n as f64 + 1.0)).collect(),
    }
}

/// Beat period, in host pixels, between harmonic n of the triad frequency and
/// the host pixel grid: the alias of n / p sampled every p / ppt is
/// |n / ppt - round(n / ppt)| cycles per pixel. For n = 1 and ppt in (2/3, 2) it
/// equals the CRT.md beat p h / |p - h| expressed in pixels. [D]
pub fn alias_period_px(px_per_triad: f64, harmonic: u32) -> f64 {
    let f = harmonic as f64 / px_per_triad;
    1.0 / (f - f.round()).abs()
}

/// Beat period in mm of harmonic `n` of a periodic pattern of pitch `p_line`
/// against a second pattern of pitch `p_mask`: 1 / |n / p_line - 1 / p_mask|.
/// For n = 1 it equals `moire_beat` (p1 p2 / |p1 - p2|), which is only a
/// low-frequency beat when the pitches are within a factor of 2. [D]
pub fn harmonic_beat_mm(p_line: f64, p_mask: f64, n: u32) -> f64 {
    1.0 / (n as f64 / p_line - 1.0 / p_mask).abs()
}

/// Dominant scan-moire beat: harmonic `harmonic` of the line frequency is the
/// one nearest the vertical mask frequency, giving the real low-frequency beat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanHarmonic {
    pub harmonic: u32,
    pub beat_mm: f64,
}

/// UI readouts of the mask against the host and the beam (PHY-2, PHY-8, UX-1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Readouts {
    pub px_per_triad: f64,
    pub triads_per_source_px: f64,
    pub fwhm_over_pitch: f64,
    /// FWHM >= 1.5 triad pitches. The pitch used is `Mask::triad_pitch_mm`; for delta
    /// that is the horizontal triad pitch 0.866 P (not P): the spot-MTF rule applies
    /// at the lattice's fundamental horizontal frequency 1 / (0.866 P).
    pub product_form_valid: bool,
    /// Horizontal tier class A, B or C from px per triad (hard thresholds, UI label).
    pub tier: Tier,
    /// The per-axis decision the shader draws (T-39c): the same function and the
    /// same f64 inputs as the frame constants (`FrameConstants::with_mask_tiers`).
    pub axis_tiers: AxisTiers,
    /// Deciding vertical period in host px (slot V / 2, delta P / 2), none for a grille.
    pub vertical_period_px: Option<f64>,
    /// Host-grid moire: host pixel pitch against triad pitch, p1 p2 / |p1 - p2|, in
    /// mm. This is the moire of the display's pixel grid, not CRT.md's video moire
    /// (see `source_moire_mm`). Only a low-frequency beat within a factor of 2.
    pub host_moire_mm: f64,
    /// Video moire as in CRT.md: source pixel pitch (in tube mm) against triad
    /// pitch, p1 p2 / |p1 - p2|. Valid as the low-frequency beat only when the two
    /// pitches are within a factor of 2 (n = 1); otherwise use `harmonic_beat_mm`.
    pub source_moire_mm: f64,
    /// Host alias periods in pixels of harmonics 2 and 3 of the triad frequency,
    /// reported only for 2 to 3 px per triad, where they dominate (at 2.9 px per
    /// triad: 3.2 px and 29 px). `alias_period_px` is the general form.
    pub host_harmonic_alias_px: Option<[f64; 2]>,
    /// Scan moire: line pitch against the vertical mask period, in mm (none for a
    /// grille). n = 1 formula, see `scan_harmonic` for the real beat.
    pub scan_moire_mm: Option<f64>,
    /// Real scan beat from the line harmonic nearest the mask frequency (slot
    /// 396 mm / 240 lines: 5th harmonic, 33 mm; delta 0.44 mm lines: 3rd, 3.08 mm).
    pub scan_harmonic: Option<ScanHarmonic>,
}

/// Readouts for a mask shown at `host_px_per_mm`, a source pixel `source_px_mm`
/// wide on the tube, a raster line pitch and a spot FWHM, under the tier `rule`.
pub fn readouts(
    mask: &Mask,
    host_px_per_mm: f64,
    source_px_mm: f64,
    line_pitch_mm: f64,
    spot_fwhm_mm: f64,
    rule: TierRule,
) -> Readouts {
    let pitch = mask.triad_pitch_mm();
    let ppt = host_px_per_mm * pitch;
    let fwhm_over_pitch = spot_fwhm_mm / pitch;
    let vertical = mask.vertical_period_mm();
    Readouts {
        px_per_triad: ppt,
        triads_per_source_px: source_px_mm / pitch,
        fwhm_over_pitch,
        product_form_valid: fwhm_over_pitch >= PRODUCT_FORM_MIN_FWHM_OVER_PITCH,
        tier: tier(ppt),
        axis_tiers: mask.axis_tiers(host_px_per_mm, rule),
        vertical_period_px: vertical.map(|v| host_px_per_mm * v),
        host_moire_mm: moire_beat(1.0 / host_px_per_mm, pitch),
        source_moire_mm: moire_beat(source_px_mm, pitch),
        host_harmonic_alias_px: (2.0..3.0)
            .contains(&ppt)
            .then(|| [alias_period_px(ppt, 2), alias_period_px(ppt, 3)]),
        scan_moire_mm: vertical.map(|v| moire_beat(line_pitch_mm, v)),
        scan_harmonic: vertical.map(|v| {
            let n = ((line_pitch_mm / v).round() as u32).max(1);
            ScanHarmonic {
                harmonic: n,
                beat_mm: harmonic_beat_mm(line_pitch_mm, v, n),
            }
        }),
    }
}
/// Option D: ids of the presets whose px per triad reaches `threshold` when the
/// tube image is `host_image_width_px` wide. Presets are (id, triads across).
pub fn option_d(presets: &[(u32, f64)], host_image_width_px: f64, threshold: f64) -> Vec<u32> {
    presets
        .iter()
        .filter(|(_, triads)| px_per_triad(host_image_width_px, *triads) >= threshold)
        .map(|(id, _)| *id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    // Test tubes. Values inside the CRT.md ranges: stripe 0.25 p (0.20 to 0.27 p),
    // edge 5 to 10 % of pitch, slot period 2.5 p / 3 (2 to 3 times p / 3), bridge
    // 15 % (10 to 20 %), dot 0.45 P (0.4 to 0.5 P), damper 20 um (10 to 30 um).
    fn grille() -> Mask {
        let p = 0.31;
        Mask::Grille(Grille {
            pitch_mm: p,
            stripe_mm: 0.25 * p,
            edge_mm: 0.07 * p,
            damper_y_mm: vec![1.0, 2.5],
            damper_mm: 0.02,
            damper_edge_mm: 0.01,
        })
    }

    fn slot() -> Mask {
        let p = 0.8;
        Mask::Slot(Slot {
            pitch_mm: p,
            stripe_mm: 0.25 * p,
            edge_mm: 0.06 * p,
            slot_period_mm: 2.5 * p / 3.0,
            bridge_fraction: 0.15,
        })
    }

    fn delta() -> Mask {
        let p = 0.28;
        Mask::Delta(Delta {
            pitch_mm: p,
            dot_mm: 0.45 * p,
            edge_mm: 0.05 * p,
        })
    }

    fn masks() -> [Mask; 3] {
        [grille(), slot(), delta()]
    }

    /// T-84 headroom: the mask peak over the field mean. A host pixel far smaller than an element sits inside
    /// its plateau: 1 / coverage (grille 1 / 0.25 = 4, slot 1 / (0.25 x 0.85) = 4.7059, delta 1 / f). A pixel
    /// that holds a whole grille stripe and its ramps (p = 0.78, stripe 0.195 + edge 0.0546 = 0.2496 mm, pixel
    /// 1 / 3.7895 = 0.26389 mm) collects the whole stripe: (0.195 / 0.26389) / 0.25 = p / pixel = 2.9558 px per
    /// triad. Prefiltered (both axes): 1.
    #[test]
    fn peak_value_over_the_field_mean() {
        let integrated = AxisTiers {
            horizontal: AxisTier::Integrated,
            vertical: AxisTier::Integrated,
        };
        let pre = AxisTiers {
            horizontal: AxisTier::Prefiltered,
            vertical: AxisTier::Prefiltered,
        };
        let plain = |p: f64| {
            Mask::Grille(Grille {
                pitch_mm: p,
                stripe_mm: 0.25 * p,
                edge_mm: 0.07 * p,
                damper_y_mm: Vec::new(),
                damper_mm: 0.0,
                damper_edge_mm: 0.0,
            })
        };
        assert!(close(plain(0.31).peak_value(integrated, 0.005), 4.0, 1e-9));
        assert!(close(slot().peak_value(integrated, 0.005), 1.0 / (0.25 * 0.85), 1e-9));
        let d = delta();
        assert!(close(d.peak_value(integrated, 0.002), 1.0 / d.coverage(), 1e-6));
        let px = 1.0 / 3.789_473_7;
        assert!(close(plain(0.78).peak_value(integrated, px), 0.78 / px, 1e-6));
        for m in masks() {
            assert!(close(m.peak_value(pre, 0.2), 1.0, 1e-9), "{m:?}");
        }
        // Blend band: between the prefiltered 1 and the integrated peak.
        let blended = AxisTiers {
            horizontal: AxisTier::Blended { box_px: 2.0 },
            vertical: AxisTier::Integrated,
        };
        let b = plain(0.78).peak_value(blended, px);
        assert!(b > 1.0 && b < 0.78 / px, "{b}");
    }

    /// Seeded LCG (Knuth MMIX constants), uniform in [0, 1).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// Midpoint-rule integral of the pointwise emitter on n x n samples.
    fn sampled(mask: &Mask, c: usize, q: &Rect, n: usize) -> f64 {
        let dx = (q.x1 - q.x0) / n as f64;
        let dy = (q.y1 - q.y0) / n as f64;
        let mut sum = 0.0;
        for j in 0..n {
            let y = q.y0 + (j as f64 + 0.5) * dy;
            for i in 0..n {
                sum += mask.emitter(c, q.x0 + (i as f64 + 0.5) * dx, y);
            }
        }
        sum * dx * dy
    }

    #[test]
    fn test_masks_are_valid() {
        for m in masks() {
            m.validate().unwrap();
        }
        let mut bad = grille();
        if let Mask::Grille(g) = &mut bad {
            g.stripe_mm = 0.33 * g.pitch_mm;
        }
        assert!(bad.validate().is_err());
    }

    /// CRT.md delta table for P = 0.28 mm, measured on the dot lattice itself:
    /// same-colour repeat along a row P sqrt(3) = 0.485, same-colour horizontal
    /// spacing between adjacent rows P sqrt(3) / 2 = 0.242, row spacing P / 2 =
    /// 0.140, neighbour spacing in a row P / sqrt(3) = 0.162 mm. Also: nearest
    /// same colour in the next row is at distance P, 30 degrees above horizontal.
    #[test]
    fn delta_lattice_p_0_28() {
        let Mask::Delta(d) = delta() else { unreachable!() };
        let (x0, y0, c0) = d.dot(0, 0);
        let (x3, _, c3) = d.dot(3, 0);
        assert_eq!(c0, c3);
        assert!(close(x3 - x0, 0.485, 0.0006));
        let (x1, _, c1) = d.dot(1, 0);
        assert_ne!(c0, c1);
        assert!(close(x1 - x0, 0.162, 0.0006));
        // Same colour in row 1: i = 0 (x = 1.5 a) is the nearest one.
        let (xr, yr, cr) = d.dot(0, 1);
        assert_eq!(cr, c0);
        assert!(close(xr - x0, 0.242, 0.0006));
        assert!(close(yr - y0, 0.140, 1e-12));
        assert!(close((xr - x0).hypot(yr - y0), 0.28, 1e-12));
        assert!(close((yr - y0).atan2(xr - x0).to_degrees(), 30.0, 1e-9));
        assert!(close(delta().triad_pitch_mm(), 0.242, 0.0006));
    }

    /// Coverage by hand:
    /// grille W / p = 0.25; slot 0.25 x (1 - 0.15) = 0.2125;
    /// delta, s = 0: CRT.md "pi h^2 / (4 A_t)" with A_t = P^2 sqrt(3) / 2 (three
    /// dots of area a^2 sqrt(3) / 2 each, a = P / sqrt(3)) and h = 0.45 P:
    /// pi 0.2025 / (2 sqrt 3) = 0.183647; with the ramp s = 0.05 P the dot area is
    /// pi (R^2 + s^2 / 12) (mean of discs of radius R - s/2 .. R + s/2), so
    /// 2 pi (0.050625 + 0.000208333) / sqrt 3 = 0.184403 (computed separately in
    /// PowerShell). All inside the CRT.md transmission ranges (15 to 27 %).
    #[test]
    fn coverage_hand_values() {
        assert!(close(grille().coverage(), 0.25, 1e-12));
        assert!(close(slot().coverage(), 0.2125, 1e-12));
        assert!(close(delta().coverage(), 0.184_403, 1e-6));
        let hard = Mask::Delta(Delta {
            pitch_mm: 0.28,
            dot_mm: 0.45 * 0.28,
            edge_mm: 0.0,
        });
        assert!(close(hard.coverage(), 0.183_647, 1e-6));
    }

    /// Closed-form integrals against a 4096 x 4096 midpoint sum of the pointwise
    /// emitter, random rectangles of 0.3 to 3 triads, all colours. The relative
    /// error is taken against max(reference, coverage x area), because a small
    /// rectangle can fall between two elements of one colour.
    fn check_integrals(mask: Mask, seed: u64, rects: usize, extra: &[Rect]) {
        let mut rng = Lcg(seed);
        let t = mask.triad_pitch_mm();
        let mut list: Vec<Rect> = extra.to_vec();
        for _ in 0..rects {
            let x0 = (rng.next() * 40.0 - 20.0) * t;
            let y0 = (rng.next() * 40.0 - 20.0) * t;
            let w = (0.3 + 2.7 * rng.next()) * t;
            let h = (0.3 + 2.7 * rng.next()) * t;
            list.push(Rect::new(x0, y0, x0 + w, y0 + h));
        }
        // One thread per (rectangle, colour): the 4096^2 sums are slow in debug builds.
        let results: Vec<(Rect, usize, f64, f64)> = std::thread::scope(|s| {
            let handles: Vec<_> = list
                .iter()
                .flat_map(|q| (0..3).map(move |c| (*q, c)))
                .map(|(q, c)| {
                    let mask = &mask;
                    s.spawn(move || (q, c, mask.integral(c, &q), sampled(mask, c, &q, 4096)))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut worst: f64 = 0.0;
        for (q, c, exact, numeric) in results {
            let scale = numeric.abs().max(mask.coverage() * q.area());
            let rel = (exact - numeric).abs() / scale;
            worst = worst.max(rel);
            assert!(rel <= 1e-4, "{q:?} c{c}: exact {exact} numeric {numeric} rel {rel}");
        }
        println!("worst relative error {worst:.3e}");
    }

    #[test]
    fn grille_integrals_match_sampling() {
        // One rectangle across the damper wire at y = 1.0 mm.
        check_integrals(grille(), 1, 3, &[Rect::new(0.11, 0.93, 0.52, 1.07)]);
    }

    #[test]
    fn slot_integrals_match_sampling() {
        check_integrals(slot(), 2, 4, &[]);
    }

    #[test]
    fn delta_integrals_match_sampling() {
        check_integrals(delta(), 3, 4, &[]);
    }

    /// Flat white over 10 x 10 triad periods at random offsets: every channel's
    /// area mean is 1 within 0.1 %. Windows are 10 horizontal triad pitches by
    /// 10 vertical repeats (grille: p, slot: V, delta: P), away from damper wires.
    #[test]
    fn flat_white_mean_is_one() {
        let mut rng = Lcg(7);
        for m in masks() {
            let t = m.triad_pitch_mm();
            let v = match &m {
                Mask::Grille(g) => g.pitch_mm,
                Mask::Slot(s) => s.slot_period_mm,
                Mask::Delta(d) => d.pitch_mm,
            };
            for _ in 0..5 {
                let x0 = rng.next() * 50.0;
                let y0 = 3.0 + rng.next() * 50.0;
                let q = Rect::new(x0, y0, x0 + 10.0 * t, y0 + 10.0 * v);
                let white: f64 = (0..3).map(|c| m.area_mean(c, &q)).sum::<f64>() / 3.0;
                for c in 0..3 {
                    assert!(close(m.area_mean(c, &q), 1.0, 0.001), "{m:?} c{c}");
                }
                assert!(close(white, 1.0, 0.001));
            }
        }
    }

    /// Michelson ripple (max - min) / (max + min) of one channel over a grid of
    /// square host pixels of size triad pitch / ppt.
    fn ripple(m: &Mask, c: usize, ppt: f64, filtered: bool, x0: f64, y0: f64) -> f64 {
        let h = m.triad_pitch_mm() / ppt;
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for j in 0..24 {
            for i in 0..48 {
                let q = Rect::new(
                    x0 + i as f64 * h,
                    y0 + j as f64 * h,
                    x0 + (i + 1) as f64 * h,
                    y0 + (j + 1) as f64 * h,
                );
                let v = if filtered {
                    m.host_value(c, &q, m.axis_tiers(1.0 / h, TierRule::default()), h)
                } else {
                    m.area_mean(c, &q)
                };
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        (hi - lo) / (hi + lo)
    }

    /// Prefilter box of one period (grille p x 0.137, slot p x V, delta 1.5 a x P).
    fn period_box(m: &Mask) -> (f64, f64) {
        match m {
            Mask::Grille(g) => (g.pitch_mm, 0.137),
            Mask::Slot(s) => (s.pitch_mm, s.slot_period_mm),
            Mask::Delta(d) => (1.5 * d.pitch_mm / 3.0_f64.sqrt(), d.pitch_mm),
        }
    }

    /// Tier C: flat-field ripple of a genuinely area-averaged mask. Each host pixel
    /// (triad pitch / ppt, ppt in [1.0, 3.0) by 0.05) takes `area_mean` over a one
    /// period box centred on it, not `prefiltered()` (which is the constant 1).
    /// Threshold 1e-6 (exact integration of whole periods; the delta dots use
    /// quadrature, about 2e-8), far under the 1.4 % Barten threshold, so a
    /// regression in the box-filter premise or the integrals is caught. The same
    /// grid with the plain pixel footprint (unfiltered) must exceed Barten at some
    /// density, so the test is not vacuous. Grille away from the damper wires.
    #[test]
    fn tier_c_area_mean_box_ripple() {
        for m in masks() {
            let (lx, ly) = period_box(&m);
            let mut worst_unfiltered: f64 = 0.0;
            for k in 0..40 {
                let ppt = 1.0 + 0.05 * k as f64;
                assert_eq!(tier(ppt), Tier::C);
                let h = m.triad_pitch_mm() / ppt;
                for c in 0..3 {
                    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
                    for j in 0..24 {
                        for i in 0..48 {
                            let cx = 3.1 + (i as f64 + 0.5) * h;
                            let cy = 3.3 + (j as f64 + 0.5) * h;
                            let q = Rect::new(cx - 0.5 * lx, cy - 0.5 * ly, cx + 0.5 * lx, cy + 0.5 * ly);
                            let v = m.area_mean(c, &q);
                            lo = lo.min(v);
                            hi = hi.max(v);
                        }
                    }
                    let r = (hi - lo) / (hi + lo);
                    assert!(r < 1e-6, "{m:?} ppt {ppt} c{c}: box-filtered ripple {r}");
                    if k % 8 == 3 {
                        worst_unfiltered = worst_unfiltered.max(ripple(&m, c, ppt, false, 3.1, 3.3));
                    }
                }
            }
            assert!(worst_unfiltered > BARTEN_THRESHOLD, "{m:?}: {worst_unfiltered}");
        }
    }
    /// The tier C derivation: the prefilter box of one period (grille p x any,
    /// slot p x V, delta 1.5 a x P) integrates to the period mean at every
    /// position, checked with the closed-form integrals (themselves checked
    /// against sampling above), so the prefiltered mask is constant. Tolerance
    /// 1e-6: exact for the grille and slot, the delta dots use quadrature (about
    /// 2e-8 error).
    #[test]
    fn prefilter_box_integrates_to_the_mean() {
        let mut rng = Lcg(11);
        for m in masks() {
            let (lx, ly) = match &m {
                Mask::Grille(g) => (g.pitch_mm, 0.137),
                Mask::Slot(s) => (s.pitch_mm, s.slot_period_mm),
                Mask::Delta(d) => (1.5 * d.pitch_mm / 3.0_f64.sqrt(), d.pitch_mm),
            };
            for _ in 0..50 {
                let x = rng.next() * 20.0 - 10.0;
                let y = 3.0 + rng.next() * 20.0;
                for c in 0..3 {
                    let v = m.area_mean(c, &Rect::new(x, y, x + lx, y + ly));
                    assert!(close(v, 1.0, 1e-6), "{m:?} c{c} at ({x}, {y}): {v}");
                }
            }
        }
    }

    /// A 20 um damper wire inside a 0.2 mm host row removes 0.02 / 0.2 = 10 % of
    /// that row (shadow area = nominal width, the ramp is symmetric), in tier C
    /// and in the area mean over whole periods in x.
    #[test]
    fn damper_wire_attenuation() {
        let m = grille();
        let q = Rect::new(0.0, 0.9, 0.31 * 4.0, 1.1);
        assert!(close(m.prefiltered(0, &q), 0.9, 1e-12));
        for c in 0..3 {
            assert!(close(m.area_mean(c, &q), 0.9, 1e-12));
        }
        let away = Rect::new(0.0, 1.2, 0.31 * 4.0, 1.4);
        assert!(close(m.prefiltered(0, &away), 1.0, 1e-12));
        assert_eq!(damper_positions_mm(1, 210.0), vec![140.0]);
        assert_eq!(damper_positions_mm(2, 300.0), vec![100.0, 200.0]);
    }

    #[test]
    fn tiers() {
        assert_eq!(tier(6.0), Tier::A);
        assert_eq!(tier(5.99), Tier::B);
        assert_eq!(tier(3.0), Tier::B);
        assert_eq!(tier(2.99), Tier::C);
    }

    /// PVM-20M4U class tube (CRT.md reference scales): 386 mm wide, p = 0.31 mm,
    /// 1920 host px, 320 source px, 240 lines on 290 mm.
    /// Hand: 386 / 0.31 = 1245.2 triads, 1920 / 1245.2 = 1.542 px per triad (tier C),
    /// 1245.2 / 320 = 3.891 triads per source px. FWHM 0.6 mm: 0.6 / 0.31 = 1.935
    /// (product form valid), 0.4 mm: 1.290 (not valid).
    /// Host-grid moire: host pitch 386 / 1920 = 0.201042 mm against 0.31 mm,
    /// 0.201042 x 0.31 / 0.108958 = 0.57199 mm. No scan moire on a grille.
    #[test]
    fn readouts_pvm_20m4u() {
        let m = Mask::Grille(Grille {
            pitch_mm: 0.31,
            stripe_mm: 0.25 * 0.31,
            edge_mm: 0.02,
            damper_y_mm: vec![],
            damper_mm: 0.0,
            damper_edge_mm: 0.0,
        });
        let r = readouts(
            &m,
            1920.0 / 386.0,
            386.0 / 320.0,
            290.0 / 240.0,
            0.6,
            TierRule::default(),
        );
        assert!(close(r.px_per_triad, 1.542, 0.0005));
        assert!(close(r.triads_per_source_px, 3.891, 0.0005));
        assert!(close(r.fwhm_over_pitch, 1.935, 0.0005));
        assert!(r.product_form_valid);
        assert_eq!(r.tier, Tier::C);
        assert!(close(r.host_moire_mm, 0.57199, 0.00001));
        assert_eq!(r.scan_moire_mm, None);
        assert_eq!(r.scan_harmonic, None);
        assert_eq!(r.host_harmonic_alias_px, None); // 1.542 px per triad, outside 2 to 3
        let r = readouts(
            &m,
            1920.0 / 386.0,
            386.0 / 320.0,
            290.0 / 240.0,
            0.4,
            TierRule::default(),
        );
        assert!(close(r.fwhm_over_pitch, 1.290, 0.0005));
        assert!(!r.product_form_valid);
    }

    /// Source moire (CRT.md video moire) by hand, grille p = 0.31 mm, source pixel
    /// 0.5 mm on the tube (ratio 1.61, within a factor of 2 so n = 1 is the real
    /// beat): 0.5 x 0.31 / |0.5 - 0.31| = 0.155 / 0.19 = 0.815789 mm. With a host
    /// grid of 2 px/mm (0.5 mm pixels, equal to the source pitch by choice)
    /// host_moire_mm gives the same number; with 4 px/mm (0.25 mm pixels)
    /// 0.25 x 0.31 / 0.06 = 1.291667 mm, while source_moire_mm is unchanged.
    #[test]
    fn source_moire_hand_value() {
        let m = Mask::Grille(Grille {
            pitch_mm: 0.31,
            stripe_mm: 0.25 * 0.31,
            edge_mm: 0.02,
            damper_y_mm: vec![],
            damper_mm: 0.0,
            damper_edge_mm: 0.0,
        });
        let r = readouts(&m, 2.0, 0.5, 1.0, 0.6, TierRule::default());
        assert!(close(r.source_moire_mm, 0.815_789, 0.000_001));
        assert!(close(r.host_moire_mm, 0.815_789, 0.000_001));
        let r = readouts(&m, 4.0, 0.5, 1.0, 0.6, TierRule::default());
        assert!(close(r.source_moire_mm, 0.815_789, 0.000_001));
        assert!(close(r.host_moire_mm, 1.291_667, 0.000_001));
    }

    /// Harmonic aliases by hand at 2.9 px per triad (p = 2.9 mm, 1 px/mm):
    /// n = 2: 2 / 2.9 = 0.689655 cycles per px, alias 1 - 0.689655 = 0.310345,
    /// period 1 / 0.310345 = 3.2222 px (2.9 / 0.9); n = 3: 3 / 2.9 = 1.034483,
    /// alias 0.034483, period 29.0 px (2.9 / 0.1). Not reported outside [2, 3).
    #[test]
    fn host_harmonic_aliases_hand_values() {
        let m = Mask::Grille(Grille {
            pitch_mm: 2.9,
            stripe_mm: 0.7,
            edge_mm: 0.1,
            damper_y_mm: vec![],
            damper_mm: 0.0,
            damper_edge_mm: 0.0,
        });
        let r = readouts(&m, 1.0, 1.0, 1.0, 5.0, TierRule::default());
        let [a2, a3] = r.host_harmonic_alias_px.unwrap();
        assert!(close(a2, 2.9 / 0.9, 1e-9));
        assert!(close(a3, 29.0, 1e-9));
        assert!(readouts(&m, 3.0 / 2.9, 1.0, 1.0, 5.0, TierRule::default())
            .host_harmonic_alias_px
            .is_none());
        assert!(readouts(&m, 1.9 / 2.9, 1.0, 1.0, 5.0, TierRule::default())
            .host_harmonic_alias_px
            .is_none());
    }

    /// Real scan beats by hand. Slot 26 in (line 1.65 mm, V / 2 = 1/3 mm): mask
    /// frequency 3 per mm, line harmonic n = round(1.65 / 0.3333) = round(4.95) = 5,
    /// 5 / 1.65 = 3.030303, difference 0.030303 = 1/33, beat 33.0 mm. Delta
    /// (line 0.44, rows P / 2 = 0.14): 1 / 0.14 = 7.142857, n = round(3.14) = 3,
    /// 3 / 0.44 = 6.818182, difference 25 / 77 = 0.324675, beat 77 / 25 = 3.08 mm.
    /// Grille: none.
    #[test]
    fn scan_harmonic_hand_values() {
        let r = readouts(
            &slot(),
            1920.0 / 528.0,
            528.0 / 320.0,
            396.0 / 240.0,
            2.0,
            TierRule::default(),
        );
        let h = r.scan_harmonic.unwrap();
        assert_eq!(h.harmonic, 5);
        assert!(close(h.beat_mm, 33.0, 1e-9));
        let r = readouts(&delta(), 1920.0 / 280.0, 280.0 / 640.0, 0.44, 0.5, TierRule::default());
        let h = r.scan_harmonic.unwrap();
        assert_eq!(h.harmonic, 3);
        assert!(close(h.beat_mm, 3.08, 1e-9));
        assert!(close(harmonic_beat_mm(1.65, 1.0 / 3.0, 1), 0.41772, 0.00001));
    }
    /// Scan moire by hand: 26 in slot TV, 396 mm / 240 lines = 1.65 mm line pitch,
    /// p = 0.8 mm, V = 2.5 x 0.8 / 3 = 0.6667 mm, x-averaged period V / 2 = 0.3333:
    /// 1.65 x 0.3333 / 1.3167 = 0.41772 mm. Delta P = 0.28 on 0.44 mm lines (15 in
    /// VGA): rows P / 2 = 0.14, 0.44 x 0.14 / 0.30 = 0.205333 mm.
    #[test]
    fn scan_moire_hand_values() {
        let s = slot();
        let r = readouts(
            &s,
            1920.0 / 528.0,
            528.0 / 320.0,
            396.0 / 240.0,
            2.0,
            TierRule::default(),
        );
        assert!(close(r.scan_moire_mm.unwrap(), 0.41772, 0.00001));
        let r = readouts(&delta(), 1920.0 / 280.0, 280.0 / 640.0, 0.44, 0.5, TierRule::default());
        assert!(close(r.scan_moire_mm.unwrap(), 0.205_333, 0.000_001));
    }

    /// Moire formula against the mask integration itself: grille p = 1.1 mm seen
    /// through 1.0 mm host pixels (1.1 px per triad). Beat 1.0 x 1.1 / 0.1 = 11 mm =
    /// 11 px, so the area-mean row repeats after 11 pixels and not before.
    /// The harmonic-alias form gives the same 11 px for n = 1.
    #[test]
    fn moire_period_in_the_integrated_row() {
        let m = Mask::Grille(Grille {
            pitch_mm: 1.1,
            stripe_mm: 0.275,
            edge_mm: 0.05,
            damper_y_mm: vec![],
            damper_mm: 0.0,
            damper_edge_mm: 0.0,
        });
        assert!(close(moire_beat(1.0, 1.1), 11.0, 1e-9));
        assert!(close(alias_period_px(1.1, 1), 11.0, 1e-9));
        for c in 0..3 {
            let v: Vec<f64> = (0..80)
                .map(|i| m.area_mean(c, &Rect::new(0.37 + i as f64, 0.0, 1.37 + i as f64, 1.0)))
                .collect();
            for i in 0..69 {
                assert!(close(v[i + 11], v[i], 1e-9), "c{c} i{i}");
            }
            for q in 1..11 {
                let d = (0..69).map(|i| (v[i + q] - v[i]).abs()).fold(0.0, f64::max);
                assert!(d > 1e-3, "c{c}: also periodic over {q} px");
            }
        }
        // n = 1 alias equals the CRT.md beat in pixels, ppt / |ppt - 1|, on (2/3, 2).
        for ppt in [0.8, 1.3, 1.542, 1.9] {
            assert!(close(alias_period_px(ppt, 1), ppt / (ppt - 1.0).abs(), 1e-9));
        }
        // PVM at 1.542 px per triad: third harmonic beats at 1 / |3 / 1.542 - 2| = 18.357 px.
        assert!(close(alias_period_px(1.542, 3), 18.357, 0.001));
    }

    /// Option D with the PHY-4 extended preset list on a 1920 px wide image.
    /// Triads across from CRT.md / QUESTIONS PHY-4: 1 PVM-20M4U 1245, 2 BVM-20F1U
    /// 1287, 3 KV TV 660, 4 GDM-F520 1827, 5 26 in slot 660, 6 13 in GE 440,
    /// 7 CromaClear 17 in 325 / 0.25 = 1300, 8 arcade 480, 9 15 in VGA 1157,
    /// 10 17 in delta 0.26 mm diagonal (PHY-4) = 0.225 mm horizontal, 325 / 0.225 = 1443.
    /// Hand px per triad: 1.54, 1.49, 2.91, 1.05, 2.91, 4.36, 1.48, 4.00, 1.66, 1.33, so presets 3, 5, 6 and 8 reach 2.8.
    #[test]
    fn option_d_presets() {
        let presets = [
            (1, 1245.0),
            (2, 1287.0),
            (3, 660.0),
            (4, 1827.0),
            (5, 660.0),
            (6, 440.0),
            (7, 1300.0),
            (8, 480.0),
            (9, 1157.0),
            (10, 1443.0),
        ];
        assert_eq!(
            option_d(&presets, 1920.0, OPTION_D_THRESHOLD_PX_PER_TRIAD),
            vec![3, 5, 6, 8]
        );
    }

    // ---- T-39c: per-axis tiers and the blend band. Masks in host px (1 px per mm), default shapes of
    // params/mask.toml: stripe 0.25 p, edge 0.07 p (P for delta), slot bridge 0.15, dot 0.45 P.

    fn grille_px(ppt: f64) -> Mask {
        Mask::Grille(Grille {
            pitch_mm: ppt,
            stripe_mm: 0.25 * ppt,
            edge_mm: 0.07 * ppt,
            damper_y_mm: vec![],
            damper_mm: 0.0,
            damper_edge_mm: 0.0,
        })
    }

    fn slot_px(ppt: f64, period: f64) -> Mask {
        Mask::Slot(Slot {
            pitch_mm: ppt,
            stripe_mm: 0.25 * ppt,
            edge_mm: 0.07 * ppt,
            slot_period_mm: period * ppt,
            bridge_fraction: 0.15,
        })
    }

    /// Delta of `ppt` px per horizontal triad: P = ppt / (sqrt 3 / 2).
    fn delta_px(ppt: f64) -> Mask {
        let p = ppt / (0.75_f64).sqrt();
        Mask::Delta(Delta {
            pitch_mm: p,
            dot_mm: 0.45 * p,
            edge_mm: 0.07 * p,
        })
    }

    /// Per-axis rule by hand (blend 0.4: horizontal band 2.8 to 3.2 px per triad; vertical band 2 -/+
    /// 0.4 / 3 = 1.8667 to 2.1333 px; inside a band, u = position in it and box = (1 - u) period + u px).
    /// Grille: no vertical period, vertical always integrated; 2.79 px per triad prefiltered; 3.0 (u = 0.5)
    /// box 0.5 x 3 + 0.5 = 2.0 px; 3.2 and above integrated. Hard switch (blend 0): 2.999 prefiltered, 3.0
    /// integrated (the old tier B threshold). Delta at 3.0 px per triad: P = 3 / 0.866025 = 3.4641 px,
    /// P / 2 = 1.7321 px, under 1.8667: prefiltered vertically. Delta with P = 4 px (P / 2 = 2.0, u = 0.5):
    /// vertical box 0.5 x 4 + 0.5 = 2.5 px; its triad 3.4641 px is over 3.2: integrated. Slot, default
    /// period 0.8333 p, at 3.0 px per triad: V / 2 = 1.25 px, prefiltered; at 6: V / 2 = 2.5 px,
    /// integrated. Modes: integrated forces both axes, prefiltered both periodic axes (grille vertical stays).
    #[test]
    fn per_axis_tier_rule_hand_values() {
        let auto = TierRule::default();
        let hard = TierRule {
            mode: TierMode::Auto,
            blend: 0.0,
        };
        let (i, p) = (AxisTier::Integrated, AxisTier::Prefiltered);
        let blended = |t: AxisTier| match t {
            AxisTier::Blended { box_px } => box_px,
            other => panic!("not blended: {other:?}"),
        };
        for ppt in [1.0, 2.79, 3.0, 3.2, 8.0] {
            assert_eq!(grille_px(ppt).axis_tiers(1.0, auto).vertical, i, "{ppt}");
        }
        assert_eq!(grille_px(2.79).axis_tiers(1.0, auto).horizontal, p);
        assert_eq!(grille_px(2.8).axis_tiers(1.0, auto).horizontal, p);
        assert!(close(
            blended(grille_px(3.0).axis_tiers(1.0, auto).horizontal),
            2.0,
            1e-12
        ));
        assert_eq!(grille_px(3.2).axis_tiers(1.0, auto).horizontal, i);
        assert_eq!(grille_px(2.999).axis_tiers(1.0, hard).horizontal, p);
        assert_eq!(grille_px(3.0).axis_tiers(1.0, hard).horizontal, i);

        let d = delta_px(3.0);
        assert!(close(d.vertical_period_mm().unwrap(), 1.732_051, 1e-6));
        assert_eq!(d.axis_tiers(1.0, auto).vertical, p);
        assert_eq!(d.axis_tiers(1.0, hard).vertical, p);
        let d4 = Mask::Delta(Delta {
            pitch_mm: 4.0,
            dot_mm: 1.8,
            edge_mm: 0.28,
        });
        let t = d4.axis_tiers(1.0, auto);
        assert_eq!(t.horizontal, i);
        assert!(close(blended(t.vertical), 2.5, 1e-12));

        let s3 = slot_px(3.0, 0.8333);
        assert!(close(s3.vertical_period_mm().unwrap(), 1.24995, 1e-9));
        assert_eq!(s3.axis_tiers(1.0, auto).vertical, p);
        assert_eq!(slot_px(6.0, 0.8333).axis_tiers(1.0, auto).vertical, i);

        let forced = |mode| TierRule { mode, blend: 0.4 };
        for m in [grille_px(1.5), slot_px(1.5, 0.8333), delta_px(1.5)] {
            let t = m.axis_tiers(1.0, forced(TierMode::Integrated));
            assert_eq!((t.horizontal, t.vertical), (i, i));
            let t = m.axis_tiers(1.0, forced(TierMode::Prefiltered));
            let v = if matches!(m, Mask::Grille(_)) { i } else { p };
            assert_eq!((t.horizontal, t.vertical), (p, v));
        }
    }

    /// Michelson ripple over host rows 0..rows of the row means of channel c (each row averaged over
    /// [0.37, 0.37 + xwin], a whole number of horizontal periods), the rows integrated over the host row
    /// or over the vertical footprint of `vertical`.
    fn row_ripple(m: &Mask, c: usize, vertical: AxisTier, xwin: f64, rows: usize) -> f64 {
        let tiers = AxisTiers {
            horizontal: AxisTier::Integrated,
            vertical,
        };
        let v: Vec<f64> = (0..rows)
            .map(|y| {
                let q = Rect::new(0.37, y as f64 + 0.21, 0.37 + xwin, y as f64 + 1.21);
                m.area_mean(c, &m.footprint(&q, tiers, 1.0))
            })
            .collect();
        let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        (hi - lo) / (hi + lo)
    }

    /// The vertical rule removes the vertical host-grid ripple (D-15). At 3.0 px per triad the horizontal
    /// periods are whole numbers of px: slot 2 p = 6 px, delta cell X = 2 x 3 = 6 px, so a row mean over 6 px
    /// is the x-averaged profile, whose period is V / 2 (slot) or P / 2 (delta). Integrated over host rows
    /// that period is under 2 px (slot 1.25, delta 1.732) and aliases: the ripple of the row means is above
    /// the 1.4 % Barten threshold (the reviews measured 5.4 % and 27 %; this test prints its own numbers).
    /// Prefiltered vertically (box V or P, the rule's decision here) each row holds whole periods of every
    /// column: 0 up to the quadrature (1e-6). Grille: no vertical period, its rows are flat without wires.
    #[test]
    fn vertical_rule_removes_the_row_ripple() {
        let auto = TierRule::default();
        for (name, m) in [("slot", slot_px(3.0, 0.8333)), ("delta", delta_px(3.0))] {
            let decided = m.axis_tiers(1.0, auto).vertical;
            assert_eq!(decided, AxisTier::Prefiltered, "{name}");
            for c in 0..3 {
                let before = row_ripple(&m, c, AxisTier::Integrated, 6.0, 48);
                let after = row_ripple(&m, c, decided, 6.0, 48);
                println!("{name} 3.0 px per triad c{c}: row ripple integrated {before:.4}, rule {after:.2e}");
                assert!(before > BARTEN_THRESHOLD, "{name} c{c}: {before}");
                assert!(after < 1e-6, "{name} c{c}: {after}");
            }
        }
        // At the vertical limit (delta P / 2 = 2.0 px, P = 4) the rule blends (u = 0.5); integrated rows
        // there are informative only (period 2 px, at the Nyquist limit, no alias to a slower beat).
        let d = delta_px(4.0 * 0.75_f64.sqrt());
        let r = row_ripple(&d, 1, AxisTier::Integrated, 2.0 * 4.0 * 0.75_f64.sqrt() * 50.0, 48);
        println!("delta P / 2 = 2 px: integrated row ripple {r:.4}");
        let g = grille_px(3.0);
        assert_eq!(g.axis_tiers(1.0, auto).vertical, AxisTier::Integrated);
        assert!(row_ripple(&g, 0, AxisTier::Integrated, 3.0, 24) < 1e-12);
    }

    /// Ripple of channel 0 of the host values of `m` under `rule`, over 160 host pixels at dense
    /// sub-pixel phases (x = 0.0371 k): the continuous-phase Michelson ripple of the rendered flat field.
    fn band_ripple(m: &Mask, rule: TierRule) -> f64 {
        let tiers = m.axis_tiers(1.0, rule);
        let (lo, hi) = (0..160)
            .map(|k| {
                let x = 0.0371 * k as f64;
                m.host_value(0, &Rect::new(x, 0.3, x + 1.0, 1.3), tiers, 1.0)
            })
            .fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v), b.max(v)));
        (hi - lo) / (hi + lo)
    }

    /// Blend band (D-15), all three types, 2.8 to 3.2 px per triad by 0.01. Inside the band the box is
    /// rho = (1 - u) + u / t triads (t px per triad, u the position in the band): for a train of elements
    /// of coverage f per period with hard edges a box of rho periods holds between max(0, rho - (1 - f))
    /// and min(rho, f) of one element, so the ripple is (1 - rho) / (2 f + rho - 1) once rho >= max(f,
    /// 1 - f), 1 below: it rises from 0 at the lower edge linearly in 1 - rho. Hand, grille f = 0.25: at
    /// 2.82 (u = 0.05, rho = 0.9677) 0.069; at 2.90 (rho = 0.8362) 0.488; 1 from rho = 0.75 (2.95)
    /// up. The soft edges only lower these. Asserted: 0 at 2.8 (exactly 1 everywhere), non-decreasing
    /// with the pitch, no step over 0.15 per 0.01 px per triad, saturated at 3.2; with the hard switch
    /// (blend 0) the step at 3.0 is a jump from 0 to above 0.5. The mean of a box average over all
    /// positions is the mask mean, 1, for any width (a convolution keeps the mean).
    #[test]
    fn blend_band_is_continuous_and_monotone() {
        let auto = TierRule::default();
        let hard = TierRule {
            mode: TierMode::Auto,
            blend: 0.0,
        };
        type Make = fn(f64) -> Mask;
        let makers: [(&str, Make); 3] = [
            ("grille", grille_px),
            ("slot", |t| slot_px(t, 0.8333)),
            ("delta", delta_px),
        ];
        for (name, make) in makers {
            let r: Vec<f64> = (0..=40)
                .map(|k| band_ripple(&make(2.8 + 0.01 * k as f64), auto))
                .collect();
            println!(
                "{name} band ripple: {:?}",
                r.iter().map(|v| format!("{v:.3}")).collect::<Vec<_>>()
            );
            assert!(r[0] < 1e-9, "{name}: {}", r[0]);
            let mut worst: f64 = 0.0;
            for k in 0..40 {
                assert!(
                    r[k + 1] >= r[k] - 1e-3,
                    "{name}: ripple falls at {}",
                    2.8 + 0.01 * k as f64
                );
                worst = worst.max(r[k + 1] - r[k]);
            }
            assert!(worst <= 0.15, "{name}: step {worst}");
            assert!(r[40] > 0.5, "{name}: {}", r[40]);
            let below = band_ripple(&make(2.995), hard);
            let above = band_ripple(&make(3.0), hard);
            println!("{name}: hard switch {below:.3} -> {above:.3}; band largest step {worst:.3}");
            assert!(below < 1e-9 && above > 0.5);
        }
    }

    /// The 8-point Gauss-Legendre rule integrates x^14 exactly on [-1, 1] (2 / 15).
    #[test]
    fn gauss_legendre_8() {
        let w: f64 = GL8.iter().map(|(_, w)| w).sum();
        assert!(close(w, 2.0, 1e-14));
        let i: f64 = GL8.iter().map(|(x, w)| w * x.powi(14)).sum();
        assert!(close(i, 2.0 / 15.0, 1e-14));
    }

    /// Hard disc fully inside a rectangle: pi r^2; quarter disc: pi r^2 / 4.
    #[test]
    fn disc_rectangle_area() {
        let pi = std::f64::consts::PI;
        assert!(close(
            disc_rect_area(0.0, 0.0, 1.0, &Rect::new(-2.0, -2.0, 2.0, 2.0)),
            pi,
            1e-12
        ));
        assert!(close(
            disc_rect_area(0.0, 0.0, 1.0, &Rect::new(0.0, 0.0, 2.0, 2.0)),
            pi / 4.0,
            1e-12
        ));
        // Half-plane cut at x = 0.5: segment r^2 acos(d / r) - d sqrt(r^2 - d^2).
        let seg = (0.5_f64).acos() - 0.5 * 0.75_f64.sqrt();
        assert!(close(
            disc_rect_area(0.0, 0.0, 1.0, &Rect::new(0.5, -2.0, 2.0, 2.0)),
            seg,
            1e-12
        ));
    }
}
