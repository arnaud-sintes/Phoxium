//! Faceplate optics (CRT.md rule 7, "Glass, halation, glare and reflections"; QUESTIONS PHY-10): the f64
//! oracle of the `optics` stage, written from the physics notes before the shader.
//!
//! # Model, in the order light meets it
//!
//! Input: the linear light L_c (per channel, white 1) that the phosphor screen emits into each host pixel
//! of the tube image. Lengths are tube millimetres, converted with the host px per mm.
//!
//! 1. **Phosphor-layer diffusion** (CRT.md "Other stray-light mechanisms": near-field glow on the scale of
//!    0.05 to 0.5 mm [E]). A fraction `diffusion_weight` of the light is spread by a unit-area Gaussian of
//!    sigma `diffusion_sigma_mm`; the rest stays. Energy conserving:
//!    D = (1 - w_d) L + w_d (G * L).
//! 2. **Halation** (CRT.md rule 7, "Halation (internal reflection ring)"). The phosphor emits into the
//!    glass (index n) as a Lambertian source: the power per polar angle theta is proportional to
//!    2 sin(theta) cos(theta) (unit integral over the hemisphere). Light reaching the front glass-air
//!    surface is reflected with the unpolarised Fresnel reflectance R(theta) of the glass side, which rises
//!    from ((n - 1) / (n + 1))^2 = 4.3 % at normal incidence to 1 at the critical angle
//!    theta_c = asin(1 / n) (total internal reflection beyond it). The reflected light comes back to the
//!    phosphor plane at r = 2 t tan(theta) (t = glass thickness), after a path 2 t / cos(theta) inside
//!    the glass, so it is attenuated by T^(2 / cos(theta)) (Beer-Lambert, T = transmission of the panel
//!    through t at normal incidence; CRT.md: "attenuated by about T^2.65", which is 2 / cos(theta_c)).
//!    The phosphor scatters it back toward the viewer (a Lambertian secondary source). Radial energy:
//!    f(theta) = 2 sin(theta) cos(theta) R(theta) T^(2 / cos(theta)), mapped to r by
//!    d(theta) / dr = cos^2(theta) / (2 t); per unit area k(r) = f(theta) cos^2(theta) / (2 t) / (2 pi r).
//!    The result is a ring with a sharp inner edge at r_c = 2 t tan(theta_c) = 1.747 t (n = 1.52), a weak
//!    Fresnel haze inside it, and a tail outside that the glass absorbs. The secondary scattering
//!    efficiency (phosphor albedo, black-matrix fraction, aluminium backing) and higher bounces are not
//!    known separately (CRT.md: amplitude 0.5 to 5 %, unresolved), so the AMPLITUDE is a parameter: the
//!    halo holds the fraction `halation_weight` of the emitted energy and the direct image keeps the rest.
//!    Only the shape comes from the geometry (first return only; the second ring, at twice the radius and
//!    about ten times weaker, is not modelled). The kernel is truncated at the radius that holds
//!    1 - [`HALO_TAIL`] of its energy and renormalised.
//! 3. **Veiling glare** (CRT.md "Other stray-light mechanisms" 2: light leaking from the back of the
//!    phosphor into the tube and returning, nearly uniform). A fraction `glare_weight` of every pixel's
//!    light is spread uniformly over the tube image: each pixel receives `glare_weight` times the mean
//!    picture level (APL). Energy conserving within the image.
//!    G = (1 - w_h - w_g) D + w_h (K * D) + w_g mean(D).
//!    **Bloom** (the soft glow around bright areas; owner request 2026-11-20, QUESTIONS PHY-10) is a fourth
//!    redistribution of the same partition, after the halo and the glare (the [`Bloom`] doc has the
//!    derivation): a fraction `w_b tint_c q` of each pixel's light leaves it and is spread by a smooth,
//!    radially symmetric kernel B (a mixture of three Gaussians of 1/e radii r, 3 r, 9 r):
//!    G = (1 - w_h - w_g - w_b tint_c q) D + w_h (K * D) + w_g mean(D) + w_b tint_c (B * (q D)),
//!    with q the soft-knee feed fraction ([`bloom_feed`]) of the local brightness. Energy conserving like the
//!    others (q and tint_c in [0, 1], w_h + w_g + w_b <= 1).
//! 4. **Vignetting** (CRT.md "Uniformity": corners 70 to 90 % of the centre). Dominant optical cause:
//!    thicker (tinted) glass toward the edge, T_edge = T_centre^(t_edge / t_centre). With a thickness that
//!    grows with the square of the distance from the centre, the relative transmission is
//!    v^(rho^2), rho = distance from the image centre over the half diagonal of the raster rectangle, v
//!    = `vignette.corner` (the corner over centre ratio). That law is the physical case (glass thickness;
//!    the shadow-mask transmission falls a little toward the edges too, CRT.md "Uniformity", with the same
//!    smooth shape). The shape controls of [`Vignette`] generalise it into an artistic grade (start radius,
//!    falloff power, soft onset, round or aspect-following, offset centre, floor); at their defaults the
//!    gain is v^(rho^2), bit for bit the formula above.
//! 5. **Tint**: per-channel relative transmission `tint` of the panel glass. The tube white
//!    (`params/colour.toml`) is measured through the glass at the centre, so (1, 1, 1) is the neutral
//!    value; a tint shifts the white.
//! 6. **Ambient reflection** (CRT.md "Surface coatings and reflections"): the screen reflects room light
//!    diffusely with reflectance T^2 r_s (the light crosses the glass twice; r_s = reflectance of the
//!    unlit phosphor screen and matrix behind the glass, CRT.md: raised blacks T^2 x 15 to 40 % of the
//!    ambient). Under an illuminance E (lux) a Lambertian reflector shows E rho / pi cd/m^2; in units of
//!    the tube white luminance L_w (cd/m^2), per channel A_c = E (T tint_c)^2 r_s / (pi L_w). Added
//!    uniformly (the specular reflection needs a room model and is not included).
//!
//! out_c = V tint_c G_c + A_c, V the vignette gain ([`Vignette::gain`], v^(rho^2) at its defaults).
//!
//! Boundary: no light exists outside the tube image (zero padding); light the diffusion or the halo
//! carries past the image edge is lost, as in the `screen` stage.
//!
//! **Light bounds (review R-49, [`light_weights`]):** every step only moves light, so its weights must form a
//! partition of it. Each weight is clamped to [0, 1] (NaN reads as 0), and when w_h + w_g + w_b exceeds 1 the
//! three are divided by their sum: the direct weight 1 - w_h - w_g - w_b tint_c q is then never negative
//! (tint_c and q are in [0, 1]) and the redistributed fraction never exceeds 1. The diffusion is a separate
//! step (D, then G), already a partition with w_d in [0, 1]. With non-negative input light, every term of G
//! is non-negative and G holds the light of D (up to the edge loss), so no weight setting can create negative
//! light or gain energy. The shader applies the same rule (its `pyramid` and `gaussians` halo methods
//! approximate this model's non-negative kernel with signed amplitudes; their small undershoot is a method
//! error, measured by the stage tests, not clamped).

use std::f64::consts::{PI, SQRT_2};

/// The redistribution weights (w_d, w_h, w_g, w_b) the model uses for the parameters `diffusion`, `halation`,
/// `glare` and `bloom` (module doc, "Light bounds"): each clamped to [0, 1], NaN read as 0, and w_h, w_g, w_b
/// divided by their sum when it exceeds 1. [D]
pub fn light_weights(diffusion: f64, halation: f64, glare: f64, bloom: f64) -> [f64; 4] {
    let unit = |w: f64| if w > 0.0 { w.min(1.0) } else { 0.0 };
    let (wd, wh, wg, wb) = (unit(diffusion), unit(halation), unit(glare), unit(bloom));
    let sum = wh + wg + wb;
    if sum > 1.0 {
        [wd, wh / sum, wg / sum, wb / sum]
    } else {
        [wd, wh, wg, wb]
    }
}

/// A value clamped to [0, 1], NaN read as 0.
fn unit(x: f64) -> f64 {
    if x > 0.0 {
        x.min(1.0)
    } else {
        0.0
    }
}

/// Radii of the three bloom lobes over the dominant one (r, 3 r, 9 r): a geometric ladder of ratio 3 spans
/// two decades of distance with three Gaussians, enough to imitate a power-law tail (the [`Bloom`] doc). [E]
pub const BLOOM_LOBE_RATIO: [f64; 3] = [1.0, 3.0, 9.0];

/// The bloom: a smooth halo around bright areas, the light of the faceplate's and the eye's wide-angle
/// scatter (module doc, step 3).
///
/// # Derivation
///
/// - **Where the glow comes from.** Light that leaves its pixel at a wide angle and lands far from it: (1)
///   multiple scattering in the phosphor layer and the faceplate (CRT.md "Other stray-light mechanisms":
///   beyond the near-field glow of the diffusion step, the scatter has a long, weak tail [E]); (2) the
///   observer's own eye, whose point spread function has a power-law tail (the CIE glare spread function,
///   veiling luminance about 10 E / theta^2 [K]) that makes bright text and sprites on a bright CRT look
///   glowing. Neither has a sourced amplitude for a given tube, so the amount `weight` is a parameter, like
///   the halation's.
/// - **Shape.** A smooth, radially symmetric, unit-area kernel. A single Gaussian of 1/e radius r
///   (exp(-d^2 / r^2), sigma r / sqrt 2) is the narrow case; a heavier tail is a mixture of three Gaussians of
///   radii r, 3 r and 9 r ([`BLOOM_LOBE_RATIO`]) whose weights the `falloff` f in [0, 1] sets
///   ([`bloom_lobe_weights`]: 1 - f / 2, 0.3 f, 0.2 f; they sum to 1, so the energy does not depend on f).
///   At f = 1 the profile falls roughly as an exponential over the two decades the lobes span, the
///   practical stand-in for the power law (the far tail of the eye's glare is cut at 9 r).
/// - **Threshold (artistic).** Physical scatter has no threshold: every bit of light spreads. A camera or a
///   film grade, and the impression of glow, come from highlights only, so `threshold` T (in units of the
///   tube white) lets only the light above T feed the bloom: per pixel, the fraction q = e(m) / m of its
///   light, e the soft-knee excess of the local brightness m (quadratic knee of half width T / 2, C1,
///   [`bloom_feed`]). m is the brightest channel of the light averaged at about half the dominant lobe's
///   sigma (the mask's triads average out): the block means of pyramid level k_q ([`bloom_knee_level`]) read
///   bilinearly ([`pyramid_bilinear`], the GPU's own read, so the knee is the same on both sides; a Gaussian
///   average would differ from it by up to a few percent of white along sharp edges near the threshold). q is
///   in [0, 1], so the light that leaves a
///   pixel never exceeds its own: the direct image stays non-negative. T = 0 feeds all the light (q = 1).
/// - **Tint.** `tint_c` in [0, 1] scales the fraction per channel (the bloom of a channel at 0 is off): the
///   light a channel puts in the bloom leaves that channel's direct image, so every channel conserves its
///   energy whatever the tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bloom {
    /// Fraction of the (fed) light redistributed into the bloom, [0, 1] (w_h + w_g + w_b <= 1).
    pub weight: f64,
    /// 1/e radius of the dominant lobe, mm.
    pub radius_mm: f64,
    /// 0: one Gaussian lobe; 1: the heaviest tail.
    pub falloff: f64,
    /// Brightness (tube white = 1) above which the light feeds the bloom; 0: all the light.
    pub threshold: f64,
    /// Per-channel scale of the weight, [0, 1].
    pub tint: [f64; 3],
}

impl Default for Bloom {
    /// The parameter defaults: off (weight 0), radius 8 mm, falloff 0.5, threshold 0, neutral tint.
    fn default() -> Self {
        Bloom {
            weight: 0.0,
            radius_mm: 8.0,
            falloff: 0.5,
            threshold: 0.0,
            tint: [1.0; 3],
        }
    }
}

/// Weights of the three bloom lobes for the falloff `f` (clamped to [0, 1]): (1 - f / 2, 0.3 f, 0.2 f), sum 1.
/// [E]
pub fn bloom_lobe_weights(falloff: f64) -> [f64; 3] {
    let f = unit(falloff);
    [1.0 - 0.5 * f, 0.3 * f, 0.2 * f]
}

/// Sigmas (px) of the three bloom lobes for a dominant 1/e radius `radius_mm` at `px_per_mm`: r k / sqrt 2.
pub fn bloom_sigmas_px(radius_mm: f64, px_per_mm: f64) -> [f64; 3] {
    BLOOM_LOBE_RATIO.map(|k| radius_mm * k * px_per_mm / SQRT_2)
}

/// The soft-knee feed fraction q in [0, 1] of a pixel whose local brightness is `m`, for the threshold `t`:
/// e(m) / m with e = 0 below t - k, (m - t + k)^2 / (4 k) within k of t, m - t above t + k, k = t / 2 (e is
/// C1 and e <= m, so q <= 1). t <= 0 (or NaN) gives 1: all the light feeds the bloom; m <= 0 gives 0. [D]
pub fn bloom_feed(m: f64, t: f64) -> f64 {
    if t.is_nan() || t <= 0.0 {
        return 1.0;
    }
    if m.is_nan() || m <= 0.0 {
        return 0.0;
    }
    let k = 0.5 * t;
    let e = if m <= t - k {
        0.0
    } else if m >= t + k {
        m - t
    } else {
        (m - t + k).powi(2) / (4.0 * k)
    };
    (e / m).clamp(0.0, 1.0)
}

/// Effective Gaussian sigma (px) of pyramid level k (2^k x 2^k box means) read bilinearly at full resolution:
/// the box chain adds (4^k - 1) / 12 px^2, the bilinear tent 4^k / 6. [D]
pub fn pyramid_sigma(k: usize) -> f64 {
    let s = 4f64.powi(k as i32);
    ((s - 1.0) / 12.0 + s / 6.0).sqrt()
}

/// The pyramid level whose bilinear read gives the bloom's knee brightness for a dominant lobe of sigma
/// `sigma1_px`: the coarsest level k >= 1 with [`pyramid_sigma`] (k) <= sigma1 / 2 (level 1 when none). [D]
pub fn bloom_knee_level(sigma1_px: f64) -> usize {
    let mut k = 1;
    while k < 30 && pyramid_sigma(k + 1) <= 0.5 * sigma1_px {
        k += 1;
    }
    k
}

/// The light averaged as the bloom's knee reads it: the means of the 2^k x 2^k blocks of `img` (blocks from
/// the origin, zero outside the image: pyramid level k), interpolated bilinearly at every pixel (the texel of
/// block c sits at c 2^k + (2^k - 1) / 2; beyond the outer texel centres the edge texels are held). An
/// average of about [`pyramid_sigma`] (k) px, the exact form of the GPU's read (`OPT_BILINEAR`). [D]
pub fn pyramid_bilinear(img: &TubeImage, k: usize) -> TubeImage {
    let (w, h) = (img.width, img.height);
    let s = 1usize << k;
    let (lw, lh) = (w.div_ceil(s).max(1), h.div_ceil(s).max(1));
    let mut level = vec![[0.0; 3]; lw * lh];
    for y in 0..h {
        for x in 0..w {
            let t = &mut level[(y / s) * lw + x / s];
            for (tc, v) in t.iter_mut().zip(img.rgb[y * w + x]) {
                *tc += v;
            }
        }
    }
    let area = (s * s) as f64;
    for t in &mut level {
        t.iter_mut().for_each(|v| *v /= area);
    }
    let mut out = TubeImage::new(w, h);
    let sf = s as f64;
    let at = |i: f64, n: usize| {
        let f = i.floor();
        let clampi = |v: f64| v.clamp(0.0, n as f64 - 1.0) as usize;
        (i - f, clampi(f), clampi(f + 1.0))
    };
    for y in 0..h {
        let (fy, y0, y1) = at((y as f64 - 0.5 * (sf - 1.0)) / sf, lh);
        for x in 0..w {
            let (fx, x0, x1) = at((x as f64 - 0.5 * (sf - 1.0)) / sf, lw);
            let (t00, t10) = (level[y0 * lw + x0], level[y0 * lw + x1]);
            let (t01, t11) = (level[y1 * lw + x0], level[y1 * lw + x1]);
            out.rgb[y * w + x] = std::array::from_fn(|c| {
                let a = t00[c] * (1.0 - fx) + t10[c] * fx;
                let b = t01[c] * (1.0 - fx) + t11[c] * fx;
                a * (1.0 - fy) + b * fy
            });
        }
    }
    out
}

/// Mass of a unit-area 1D Gaussian of `sigma_px` over the pixel [d - 1/2, d + 1/2]. [K]
pub fn gaussian_pixel_mass(d: i64, sigma_px: f64) -> f64 {
    assert!(
        sigma_px.is_finite() && sigma_px > 0.0,
        "gaussian_pixel_mass: {sigma_px}"
    );
    let s = sigma_px * SQRT_2;
    let a = (d as f64 - 0.5) / s;
    let b = (d as f64 + 0.5) / s;
    0.5 * (crate::beam::erf(b) - crate::beam::erf(a))
}

/// The pixel kernel of a mixture of radially symmetric Gaussians (`weights`, `sigmas_px`): radius R and the
/// (2R + 1)^2 masses (layout of [`halo_kernel_px`]), each lobe the product of its two 1D pixel masses. Cut at
/// 7 sigma of the widest lobe or `max_radius` px, whichever is smaller, and NOT renormalised: what lies
/// beyond `max_radius` (the image size) cannot reach a pixel of the image.
pub fn gaussian_mixture_px(weights: &[f64], sigmas_px: &[f64], max_radius: usize) -> (usize, Vec<f64>) {
    let widest = sigmas_px
        .iter()
        .zip(weights)
        .filter(|(_, &w)| w > 0.0)
        .map(|(&s, _)| s)
        .fold(0.0, f64::max);
    let rad = ((7.0 * widest).ceil() as usize + 1).min(max_radius.max(1));
    let n = 2 * rad + 1;
    let mut k = vec![0.0; n * n];
    for (&w, &s) in weights.iter().zip(sigmas_px) {
        if w <= 0.0 {
            continue;
        }
        let m: Vec<f64> = (0..n).map(|i| gaussian_pixel_mass(i as i64 - rad as i64, s)).collect();
        for dy in 0..n {
            for dx in 0..n {
                k[dy * n + dx] += w * m[dy] * m[dx];
            }
        }
    }
    (rad, k)
}

/// The vignette: the physical glass law v^(rho^2) and its artistic shape controls (module doc, step 4).
///
/// The normalised radius rho (1 at the raster corner) is measured from the centre moved by `centre` (fractions
/// of the raster half width and half height), with rho^2 = rho_round^2 = d^2 / (half diagonal)^2 at
/// `roundness` 1 (a circle, the physical case) and rho_aspect^2 = ((dx / half width)^2 + (dy / half
/// height)^2) / 2 at 0 (an ellipse that follows the picture's aspect), blended linearly in between. Then
/// x = (rho - start) / (1 - start) (0 where the darkening starts, 1 at the corner), a soft onset of width
/// `softness` (s(x) = 0 for x <= 0, x^2 / (2 w) up to w, x - w / 2 beyond, C1, normalised to s(1) = 1),
/// the profile p = s^`exponent` (p = rho^2 at the defaults: start 0, softness 0, exponent 2), and the gain
/// floor + (1 - floor) v^p: 1 at the centre, v (1 - floor) + floor at the corner. v is clamped to
/// [1e-4, 1] (0 reads as 1e-4, black to the eye, so the falloff stays smooth). Disabled: gain 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vignette {
    pub enable: bool,
    /// Gain at the raster corner relative to the centre (before the floor), [0, 1].
    pub corner: f64,
    /// Normalised radius where the darkening starts, [0, 0.95].
    pub start: f64,
    /// Power of the profile, [0.25, 16].
    pub exponent: f64,
    /// Width of the soft onset (normalised radius), [0, 1].
    pub softness: f64,
    /// 1: circle (the physical case); 0: ellipse of the picture's aspect.
    pub roundness: f64,
    /// Offset of the centre, fractions of the raster half width and half height.
    pub centre: [f64; 2],
    /// Lowest gain (the light left at the darkest point), [0, 1].
    pub floor: f64,
}

impl Default for Vignette {
    /// The parameter defaults: the physical law v^(rho^2) with v = 0.85.
    fn default() -> Self {
        Vignette {
            enable: true,
            corner: 0.85,
            start: 0.0,
            exponent: 2.0,
            softness: 0.0,
            roundness: 1.0,
            centre: [0.0; 2],
            floor: 0.0,
        }
    }
}

impl Vignette {
    /// The physical law alone with the corner ratio `corner` (every shape control at its default).
    pub fn physical(corner: f64) -> Self {
        Vignette {
            corner,
            ..Vignette::default()
        }
    }

    /// Whether the gain is 1 everywhere (disabled, corner 1 or floor 1).
    pub fn is_neutral(&self) -> bool {
        !self.enable || self.corner >= 1.0 || self.floor >= 1.0
    }

    /// The profile p (0 at the centre, 1 at the corner) at the offsets (`dx`, `dy`) px from the image centre,
    /// for a raster of half sizes (`half_w`, `half_h`) px.
    pub fn profile(&self, dx: f64, dy: f64, half_w: f64, half_h: f64) -> f64 {
        let dx = dx - self.centre[0] * half_w;
        let dy = dy - self.centre[1] * half_h;
        let hd2 = (4.0 * half_w * half_w + 4.0 * half_h * half_h) / 4.0;
        let round = (dx * dx + dy * dy) / hd2;
        let roundness = unit(self.roundness);
        let r2 = if roundness >= 1.0 {
            round
        } else {
            let aspect = 0.5 * ((dx / half_w).powi(2) + (dy / half_h).powi(2));
            (1.0 - roundness) * aspect + roundness * round
        };
        let n = self.exponent.clamp(0.25, 16.0);
        let start = self.start.clamp(0.0, 0.95);
        let w = unit(self.softness);
        if start <= 0.0 && w <= 0.0 {
            return if n == 2.0 { r2 } else { r2.powf(0.5 * n) };
        }
        let ramp = |x: f64| {
            if x <= 0.0 {
                0.0
            } else if x < w {
                x * x / (2.0 * w)
            } else {
                x - 0.5 * w
            }
        };
        let x = (r2.sqrt() - start) / (1.0 - start);
        (ramp(x) / ramp(1.0)).powf(n)
    }

    /// The gain at the offsets (`dx`, `dy`) px from the image centre (struct doc).
    pub fn gain(&self, dx: f64, dy: f64, half_w: f64, half_h: f64) -> f64 {
        if !self.enable {
            return 1.0;
        }
        let v = vignette(self.corner.clamp(1e-4, 1.0), self.profile(dx, dy, half_w, half_h));
        let f = unit(self.floor);
        if f > 0.0 {
            f + (1.0 - f) * v
        } else {
            v
        }
    }
}

/// Fraction of the halo energy beyond the truncation radius of the halo kernel (then renormalised). [E]
pub const HALO_TAIL: f64 = 0.01;

/// Halation ring radius over faceplate thickness: 2 tan(asin(1 / n)), the
/// total-internal-reflection angle traversed twice. [D]
///
/// # Panics
/// Unless `n` > 1 and finite (no total internal reflection otherwise).
pub fn halation_radius_over_thickness(n: f64) -> f64 {
    assert!(n.is_finite() && n > 1.0, "halation needs a refractive index n > 1: {n}");
    2.0 * (1.0 / n).asin().tan()
}

/// Beer-Lambert transmission through `thickness_ratio` times the reference
/// thickness, given the transmission `t_ref` at the reference thickness. [K]
pub fn glass_transmission(t_ref: f64, thickness_ratio: f64) -> f64 {
    t_ref.powf(thickness_ratio)
}

/// Unpolarised Fresnel reflectance of a glass-air surface seen from the glass (index `n`) at the
/// incidence angle `theta` (radians); 1 at and beyond the critical angle (total internal reflection). [K]
///
/// # Panics
/// Unless `n` > 1 and finite.
pub fn internal_reflectance(n: f64, theta: f64) -> f64 {
    assert!(n.is_finite() && n > 1.0, "internal_reflectance: n = {n}");
    let s = n * theta.sin();
    if s >= 1.0 {
        return 1.0;
    }
    let ci = theta.cos();
    let ct = (1.0 - s * s).sqrt();
    let rs = (n * ci - ct) / (n * ci + ct);
    let rp = (ci - n * ct) / (ci + n * ct);
    0.5 * (rs * rs + rp * rp)
}

/// The faceplate glass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glass {
    /// Thickness at the centre, mm.
    pub thickness_mm: f64,
    /// Refractive index.
    pub index: f64,
    /// Transmission through `thickness_mm` at normal incidence (0, 1].
    pub transmission: f64,
}

/// The normalised halation kernel of a [`Glass`] (module doc, step 2).
#[derive(Clone, Copy, Debug)]
pub struct Halo {
    pub glass: Glass,
    /// Integral of f(theta) over [0, pi / 2] (untruncated).
    total: f64,
    /// Integral of f(theta) up to the truncation radius.
    within: f64,
    /// Truncation radius, mm.
    pub support_mm: f64,
}

/// Composite Simpson rule of `f` over [a, b] with `n` (even) intervals.
fn simpson(f: impl Fn(f64) -> f64, a: f64, b: f64, n: usize) -> f64 {
    let h = (b - a) / n as f64;
    let mut s = f(a) + f(b);
    for i in 1..n {
        let w = if i % 2 == 1 { 4.0 } else { 2.0 };
        s += w * f(a + i as f64 * h);
    }
    s * h / 3.0
}

impl Halo {
    /// # Panics
    /// Unless the thickness is positive, n > 1 and 0 < T <= 1 (all finite).
    pub fn new(glass: Glass) -> Halo {
        let Glass {
            thickness_mm: t,
            index: n,
            transmission: tr,
        } = glass;
        assert!(t.is_finite() && t > 0.0, "Halo: thickness {t}");
        assert!(n.is_finite() && n > 1.0, "Halo: index {n}");
        assert!(tr.is_finite() && tr > 0.0 && tr <= 1.0, "Halo: transmission {tr}");
        let mut h = Halo {
            glass,
            total: 0.0,
            within: 0.0,
            support_mm: f64::INFINITY,
        };
        h.total = h.energy_to_angle(0.5 * PI);
        // Truncation radius: bisection on the angle for 1 - E(theta) = HALO_TAIL.
        let (mut lo, mut hi) = (0.0, 0.5 * PI);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if 1.0 - h.energy_to_angle(mid) / h.total > HALO_TAIL {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        h.support_mm = 2.0 * t * hi.tan();
        h.within = h.energy_to_angle(hi);
        h
    }

    /// Unnormalised angular energy f(theta) = 2 sin cos R(theta) T^(2 / cos).
    pub fn angular(&self, theta: f64) -> f64 {
        let c = theta.cos();
        if c <= 0.0 {
            return 0.0;
        }
        let g = self.glass;
        2.0 * theta.sin() * c * internal_reflectance(g.index, theta) * g.transmission.powf(2.0 / c)
    }

    /// Integral of f from 0 to `theta` (split at the critical angle, where R has an infinite slope).
    fn energy_to_angle(&self, theta: f64) -> f64 {
        let tc = (1.0 / self.glass.index).asin();
        let f = |x: f64| self.angular(x);
        if theta <= tc {
            simpson(f, 0.0, theta, 4096)
        } else {
            simpson(f, 0.0, tc, 4096) + simpson(f, tc, theta, 4096)
        }
    }

    /// Radius of the inner edge of the ring (total internal reflection), mm.
    pub fn ring_radius_mm(&self) -> f64 {
        self.glass.thickness_mm * halation_radius_over_thickness(self.glass.index)
    }

    /// Fraction of the untruncated halo energy within radius `r_mm`.
    pub fn energy_within(&self, r_mm: f64) -> f64 {
        let theta = (r_mm / (2.0 * self.glass.thickness_mm)).atan();
        self.energy_to_angle(theta) / self.total
    }

    /// The kernel per mm^2 at radius `r_mm`: unit integral over the plane, 0 beyond the truncation radius.
    pub fn density(&self, r_mm: f64) -> f64 {
        let r = r_mm.abs();
        if r > self.support_mm {
            return 0.0;
        }
        let t = self.glass.thickness_mm;
        let g = self.glass;
        if r < 1e-9 * t {
            // Limit r -> 0: f ~ 2 theta R(0) T^2, r = 2 t theta.
            let r0 = internal_reflectance(g.index, 0.0);
            return r0 * g.transmission.powi(2) / (4.0 * PI * t * t) / self.within;
        }
        let theta = (r / (2.0 * t)).atan();
        let c = theta.cos();
        self.angular(theta) * c * c / (2.0 * t) / (2.0 * PI * r) / self.within
    }
}

/// Kernel mass of each host pixel offset of the halo of `halo` at `px_per_mm`: radius R (pixels) and the
/// (2R + 1)^2 row-major masses (offset (dx, dy) at index (dy + R)(2R + 1) + dx + R), the density
/// integrated over each pixel by an s x s midpoint rule (s = 4; 12 near the ring edge), normalised to sum 1.
pub fn halo_kernel_px(halo: &Halo, px_per_mm: f64) -> (usize, Vec<f64>) {
    assert!(px_per_mm.is_finite() && px_per_mm > 0.0, "halo_kernel_px: {px_per_mm}");
    let r_px = halo.support_mm * px_per_mm;
    let rad = r_px.ceil() as usize + 1;
    let n = 2 * rad + 1;
    let edge_px = halo.ring_radius_mm() * px_per_mm;
    let mut k = vec![0.0; n * n];
    let mut sum = 0.0;
    for dy in 0..n {
        for dx in 0..n {
            let (x, y) = (dx as f64 - rad as f64, dy as f64 - rad as f64);
            let rc = (x * x + y * y).sqrt();
            if rc > r_px + 1.0 {
                continue;
            }
            let s = if (rc - edge_px).abs() < 1.5 || rc < 1.5 { 12 } else { 4 };
            let mut m = 0.0;
            for j in 0..s {
                for i in 0..s {
                    let u = x - 0.5 + (i as f64 + 0.5) / s as f64;
                    let v = y - 0.5 + (j as f64 + 0.5) / s as f64;
                    m += halo.density((u * u + v * v).sqrt() / px_per_mm);
                }
            }
            let m = m / (s * s) as f64 / (px_per_mm * px_per_mm);
            k[dy * n + dx] = m;
            sum += m;
        }
    }
    for v in &mut k {
        *v /= sum;
    }
    (rad, k)
}

/// Mass a uniform source pixel puts into a host pixel `d` pixels away under a unit-area Gaussian of sigma
/// `sigma_px`, per axis: the integral of the tent (1 - |u|) over u in [-1, 1] times g(d + u), by Simpson's
/// rule. [D]
pub fn diffusion_weight_px(d: i64, sigma_px: f64) -> f64 {
    assert!(
        sigma_px.is_finite() && sigma_px > 0.0,
        "diffusion_weight_px: {sigma_px}"
    );
    let g = |x: f64| (-0.5 * (x / sigma_px).powi(2)).exp() / (sigma_px * (2.0 * PI).sqrt());
    let f = |u: f64| (1.0 - u.abs()) * g(d as f64 + u);
    simpson(f, -1.0, 0.0, 2000) + simpson(f, 0.0, 1.0, 2000)
}

/// Relative transmission of the vignette at `rho2` (squared distance over squared half diagonal). [D]
pub fn vignette(corner_ratio: f64, rho2: f64) -> f64 {
    corner_ratio.powf(rho2)
}

/// Ambient light diffusely reflected by the screen, in units of the tube white:
/// E (T tint)^2 r_s / (pi L_w). [D]
pub fn ambient_level(lux: f64, transmission: f64, tint: f64, screen_reflectance: f64, white_nits: f64) -> f64 {
    assert!(white_nits > 0.0, "ambient_level: white {white_nits} cd/m^2");
    lux * (transmission * tint).powi(2) * screen_reflectance / (PI * white_nits)
}

/// All parameters of the optical model (module doc).
#[derive(Clone, Copy, Debug)]
pub struct Optics {
    pub glass: Glass,
    pub halation_weight: f64,
    pub diffusion_weight: f64,
    pub diffusion_sigma_mm: f64,
    pub glare_weight: f64,
    pub bloom: Bloom,
    pub vignette: Vignette,
    pub tint: [f64; 3],
    pub ambient_lux: f64,
    pub screen_reflectance: f64,
    pub white_nits: f64,
}

/// A linear-light image of the tube domain (rgb per host pixel, row major).
#[derive(Clone, Debug, PartialEq)]
pub struct TubeImage {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<[f64; 3]>,
}

impl TubeImage {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            rgb: vec![[0.0; 3]; width * height],
        }
    }
}

/// In-place radix-2 complex FFT (length a power of two); `inverse` without the 1 / N scale.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let ang = sign * 2.0 * PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (xr, xi) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
                let t = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = t;
            }
        }
        len <<= 1;
    }
}

/// 2D FFT of a (w x h, row major, powers of two) complex array.
fn fft2(re: &mut [f64], im: &mut [f64], w: usize, h: usize, inverse: bool) {
    for y in 0..h {
        fft(&mut re[y * w..(y + 1) * w], &mut im[y * w..(y + 1) * w], inverse);
    }
    let (mut cr, mut ci) = (vec![0.0; h], vec![0.0; h]);
    for x in 0..w {
        for y in 0..h {
            cr[y] = re[y * w + x];
            ci[y] = im[y * w + x];
        }
        fft(&mut cr, &mut ci, inverse);
        for y in 0..h {
            re[y * w + x] = cr[y];
            im[y * w + x] = ci[y];
        }
    }
}

/// Linear (zero-padded) convolution of the three channels of `img` with the centred kernel
/// (`rad`, `k` as [`halo_kernel_px`]), by FFT.
pub fn convolve(img: &TubeImage, rad: usize, k: &[f64]) -> TubeImage {
    let (w, h) = (img.width, img.height);
    let pw = (w + rad + 1).next_power_of_two();
    let ph = (h + rad + 1).next_power_of_two();
    let n = 2 * rad + 1;
    let (mut kr, mut ki) = (vec![0.0; pw * ph], vec![0.0; pw * ph]);
    for dy in 0..n {
        for dx in 0..n {
            let x = (dx as i64 - rad as i64).rem_euclid(pw as i64) as usize;
            let y = (dy as i64 - rad as i64).rem_euclid(ph as i64) as usize;
            kr[y * pw + x] += k[dy * n + dx];
        }
    }
    fft2(&mut kr, &mut ki, pw, ph, false);
    let mut out = TubeImage::new(w, h);
    // Two real channels per complex transform: (a + i b) * k = a * k + i b * k for a real kernel.
    for pair in [[0usize, 1], [2, 2]] {
        let (mut re, mut im) = (vec![0.0; pw * ph], vec![0.0; pw * ph]);
        for y in 0..h {
            for x in 0..w {
                re[y * pw + x] = img.rgb[y * w + x][pair[0]];
                if pair[1] != pair[0] {
                    im[y * pw + x] = img.rgb[y * w + x][pair[1]];
                }
            }
        }
        fft2(&mut re, &mut im, pw, ph, false);
        for i in 0..pw * ph {
            let (a, b) = (re[i], im[i]);
            re[i] = a * kr[i] - b * ki[i];
            im[i] = a * ki[i] + b * kr[i];
        }
        fft2(&mut re, &mut im, pw, ph, true);
        let scale = 1.0 / (pw * ph) as f64;
        for y in 0..h {
            for x in 0..w {
                out.rgb[y * w + x][pair[0]] = re[y * pw + x] * scale;
                if pair[1] != pair[0] {
                    out.rgb[y * w + x][pair[1]] = im[y * pw + x] * scale;
                }
            }
        }
    }
    out
}

/// Separable diffusion: each axis convolved with [`diffusion_weight_px`] out to 6 sigma + 2 pixels.
fn diffuse(img: &TubeImage, sigma_px: f64) -> TubeImage {
    let r = (6.0 * sigma_px).ceil() as i64 + 2;
    let wts: Vec<f64> = (-r..=r).map(|d| diffusion_weight_px(d, sigma_px)).collect();
    let (w, h) = (img.width as i64, img.height as i64);
    let pass = |src: &TubeImage, horizontal: bool| {
        let mut out = TubeImage::new(src.width, src.height);
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0; 3];
                for d in -r..=r {
                    let (sx, sy) = if horizontal { (x - d, y) } else { (x, y - d) };
                    if sx < 0 || sy < 0 || sx >= w || sy >= h {
                        continue;
                    }
                    let p = src.rgb[(sy * w + sx) as usize];
                    let wt = wts[(d + r) as usize];
                    for c in 0..3 {
                        acc[c] += wt * p[c];
                    }
                }
                out.rgb[(y * w + x) as usize] = acc;
            }
        }
        out
    };
    pass(&pass(img, true), false)
}

impl Optics {
    /// The halo of these parameters.
    pub fn halo(&self) -> Halo {
        Halo::new(self.glass)
    }

    /// The output of the model (module doc) for the tube image `img` at `px_per_mm`, with the raster
    /// rectangle inset by `margin` (x, y) pixels per side (the vignette's corner).
    ///
    /// # Panics
    /// Unless `px_per_mm` > 0 (the model is in millimetres).
    pub fn apply(&self, img: &TubeImage, px_per_mm: f64, margin: (usize, usize)) -> TubeImage {
        assert!(
            px_per_mm > 0.0,
            "Optics::apply needs a placed tube: {px_per_mm} px per mm"
        );
        let (w, h) = (img.width, img.height);
        let [wd, wh, wg, wb] = light_weights(
            self.diffusion_weight,
            self.halation_weight,
            self.glare_weight,
            self.bloom.weight,
        );
        let sigma_px = self.diffusion_sigma_mm * px_per_mm;
        let d = if wd > 0.0 && sigma_px > 0.0 {
            let g = diffuse(img, sigma_px);
            let mut out = img.clone();
            for (o, gv) in out.rgb.iter_mut().zip(&g.rgb) {
                for c in 0..3 {
                    o[c] = (1.0 - wd) * o[c] + wd * gv[c];
                }
            }
            out
        } else {
            img.clone()
        };
        let halo = if wh > 0.0 {
            let (rad, k) = halo_kernel_px(&self.halo(), px_per_mm);
            Some(convolve(&d, rad, &k))
        } else {
            None
        };
        let mut mean = [0.0; 3];
        for p in &d.rgb {
            for c in 0..3 {
                mean[c] += p[c];
            }
        }
        for m in &mut mean {
            *m /= (w * h) as f64;
        }
        // Bloom: the feed q D, its kernel, the per-channel weights.
        let wbc = self.bloom.tint.map(|t| wb * unit(t));
        let bloom = if wbc.iter().any(|&x| x > 0.0) {
            let sig = bloom_sigmas_px(self.bloom.radius_mm, px_per_mm);
            assert!(
                sig[0].is_finite() && sig[0] > 0.0,
                "Optics::apply: bloom radius {} mm",
                self.bloom.radius_mm
            );
            let q: Vec<f64> = if self.bloom.threshold > 0.0 {
                let avg = pyramid_bilinear(&d, bloom_knee_level(sig[0]));
                avg.rgb
                    .iter()
                    .map(|p| bloom_feed(p[0].max(p[1]).max(p[2]), self.bloom.threshold))
                    .collect()
            } else {
                vec![1.0; w * h]
            };
            let mut feed = d.clone();
            for (p, qv) in feed.rgb.iter_mut().zip(&q) {
                p.iter_mut().for_each(|c| *c *= qv);
            }
            let (r, k) = gaussian_mixture_px(&bloom_lobe_weights(self.bloom.falloff), &sig, w.max(h));
            Some((q, convolve(&feed, r, &k)))
        } else {
            None
        };
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let rw = w as f64 - 2.0 * margin.0 as f64;
        let rh = h as f64 - 2.0 * margin.1 as f64;
        let amb: Vec<f64> = (0..3)
            .map(|c| {
                ambient_level(
                    self.ambient_lux,
                    self.glass.transmission,
                    self.tint[c],
                    self.screen_reflectance,
                    self.white_nits,
                )
            })
            .collect();
        let mut out = TubeImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
                let v = self.vignette.gain(dx, dy, rw / 2.0, rh / 2.0);
                for c in 0..3 {
                    let hv = halo.as_ref().map_or(0.0, |hi| hi.rgb[i][c]);
                    let (q, bv) = bloom.as_ref().map_or((0.0, 0.0), |(q, b)| (q[i], b.rgb[i][c]));
                    let g = (1.0 - wh - wg - wbc[c] * q) * d.rgb[i][c] + wh * hv + wg * mean[c] + wbc[c] * bv;
                    out.rgb[i][c] = v * self.tint[c] * g + amb[c];
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    /// Review R-49: the weights form a partition of the light whatever the parameters (each in [0, 1], the
    /// direct weight 1 - w_h - w_g >= 0, NaN as 0), and are unchanged inside the valid range.
    #[test]
    fn light_weights_are_a_partition() {
        assert_eq!(light_weights(0.2, 0.03, 0.01, 0.0), [0.2, 0.03, 0.01, 0.0]);
        assert_eq!(light_weights(0.2, 0.6, 0.4, 0.0), [0.2, 0.6, 0.4, 0.0]);
        assert_eq!(light_weights(0.2, 0.3, 0.2, 0.5), [0.2, 0.3, 0.2, 0.5]);
        let [_, wh, wg, _] = light_weights(0.0, 0.8, 0.8, 0.0);
        assert!(close(wh, 0.5, 1e-15) && close(wg, 0.5, 1e-15));
        let [_, wh, wg, _] = light_weights(0.0, 3.0, 1.0, 0.0);
        assert!(
            close(wh, 0.5, 1e-15) && close(wg, 0.5, 1e-15),
            "each clamped to 1 first"
        );
        let [_, wh, wg, wb] = light_weights(0.0, 0.5, 0.25, 0.75);
        assert!(close(wh, 1.0 / 3.0, 1e-15) && close(wg, 1.0 / 6.0, 1e-15) && close(wb, 0.5, 1e-15));
        assert_eq!(light_weights(-1.0, -0.5, f64::NAN, -2.0), [0.0; 4]);
        assert_eq!(light_weights(2.0, f64::INFINITY, 0.0, 0.0), [1.0, 1.0, 0.0, 0.0]);
        let grid = [-1.0, 0.0, 0.3, 0.8, 1.0, 5.0, f64::NAN];
        for wd in [-1.0, 0.0, 0.5, 1.0, 7.0, f64::NAN] {
            for wh in grid {
                for wg in grid {
                    for wb in grid {
                        let [a, b, c, e] = light_weights(wd, wh, wg, wb);
                        for x in [a, b, c, e] {
                            assert!((0.0..=1.0).contains(&x), "{wd} {wh} {wg} {wb}: {x}");
                        }
                        let direct = 1.0 - b - c - e;
                        assert!(direct >= -1e-15, "{wd} {wh} {wg} {wb}: direct {direct}");
                    }
                }
            }
        }
    }

    /// Review R-49: halation 0.8 with glare 0.8 (and a diffusion weight beyond 1) gives no negative light,
    /// and with a neutral glass and no edge loss (a small spot far from the edges) the total light is kept.
    #[test]
    fn extreme_weights_keep_light_non_negative() {
        let o = Optics {
            glass: Glass {
                thickness_mm: 2.0,
                index: 1.52,
                transmission: 0.9,
            },
            halation_weight: 0.8,
            diffusion_weight: 1.5,
            diffusion_sigma_mm: 0.2,
            glare_weight: 0.8,
            bloom: Bloom::default(),
            vignette: Vignette::physical(1.0),
            tint: [1.0; 3],
            ambient_lux: 0.0,
            screen_reflectance: 0.0,
            white_nits: 100.0,
        };
        // An image that holds the whole halo of the 8 x 8 spot (no edge loss).
        let ppm = 4.0;
        let r = (o.halo().support_mm * ppm).ceil() as usize + 4;
        let (w, h) = (2 * r + 8, 2 * r + 8);
        let mut img = TubeImage::new(w, h);
        for y in r..r + 8 {
            for x in r..r + 8 {
                img.rgb[y * w + x] = [1.0, 0.5, 2.0];
            }
        }
        let out = o.apply(&img, ppm, (0, 0));
        for c in 0..3 {
            let (tin, tout): (f64, f64) = (img.rgb.iter().map(|p| p[c]).sum(), out.rgb.iter().map(|p| p[c]).sum());
            assert!(out.rgb.iter().all(|p| p[c] >= 0.0), "channel {c}: negative light");
            assert!(close(tout, tin, 1e-6 * tin), "channel {c}: {tout} vs {tin}");
        }
    }

    #[test]
    fn halation() {
        assert!(close(halation_radius_over_thickness(1.52), 1.7471, 0.0001));
        // n = sqrt(2): critical angle 45 deg, tan = 1, ratio exactly 2.
        assert!(close(halation_radius_over_thickness(2.0_f64.sqrt()), 2.0, 1e-12));
    }

    #[test]
    #[should_panic(expected = "n > 1")]
    fn halation_index_below_one_panics() {
        halation_radius_over_thickness(0.9);
    }

    /// T^2.65 for T = 0.46 and 0.86.
    #[test]
    fn transmission_power() {
        assert!(close(glass_transmission(0.46, 2.65), 0.128, 0.0005));
        assert!(close(glass_transmission(0.86, 2.65), 0.671, 0.0005));
        assert!(close(glass_transmission(0.5, 1.0), 0.5, 1e-15));
    }

    /// Fresnel: 4.26 % at normal incidence for n = 1.52 (((n - 1) / (n + 1))^2), 1 at the critical
    /// angle, monotonic in between.
    #[test]
    fn fresnel_reflectance() {
        let n = 1.52;
        assert!(close(internal_reflectance(n, 0.0), (0.52_f64 / 2.52).powi(2), 1e-15));
        let tc = (1.0 / n).asin();
        assert_eq!(internal_reflectance(n, tc + 1e-9), 1.0);
        assert!(internal_reflectance(n, tc - 1e-6) > 0.99);
        let mut last = 0.0;
        for i in 0..100 {
            let r = internal_reflectance(n, tc * f64::from(i) / 100.0);
            assert!(r >= last - 1e-12, "{i}");
            last = r;
        }
    }

    fn glass() -> Glass {
        Glass {
            thickness_mm: 12.0,
            index: 1.52,
            transmission: 0.85,
        }
    }

    /// The kernel integrates to 1 (radial Simpson over the support), and the critical angle is where the
    /// density jumps: the inner edge of the ring is at 1.747 t (20.97 mm for t = 12 mm). The haze inside
    /// holds a few percent of the halo, the ring peaks just outside the edge.
    #[test]
    fn halo_kernel_shape() {
        let h = Halo::new(glass());
        let rc = h.ring_radius_mm();
        assert!(close(rc, 12.0 * 1.7471, 0.002), "{rc}");
        let total = simpson(|r| 2.0 * PI * r * h.density(r), 0.0, rc, 20000)
            + simpson(|r| 2.0 * PI * r * h.density(r), rc, h.support_mm, 20000);
        assert!(close(total, 1.0, 1e-4), "{total}");
        // Steepest rise: just inside the edge (Fresnel to 1), within 0.05 mm of it.
        let mut best = (0.0, 0.0);
        let step = 0.01;
        let mut r = 1.0;
        while r < 3.0 * rc {
            let slope = h.density(r + step) - h.density(r);
            if slope > best.1 {
                best = (r, slope);
            }
            r += step;
        }
        assert!((best.0 - rc).abs() < 0.05, "edge at {} vs {rc}", best.0);
        let haze = h.energy_within(rc * 0.999);
        assert!(haze > 0.01 && haze < 0.1, "haze fraction {haze}");
        println!(
            "t 12 mm, n 1.52, T 0.85: edge {rc:.3} mm, haze {:.2} %, support {:.1} mm ({:.2} t), peak density \
             {:.3e} / mm^2 at the edge, {:.3e} at 0",
            100.0 * haze,
            h.support_mm,
            h.support_mm / 12.0,
            h.density(rc * 1.0001),
            h.density(0.0)
        );
        // Dark glass absorbs the long paths: a shorter support.
        let dark = Halo::new(Glass {
            transmission: 0.46,
            ..glass()
        });
        assert!(dark.support_mm < h.support_mm);
    }

    /// The pixel kernel: unit sum, symmetric, the edge where the physics puts it (t = 12 mm at
    /// 4.966 px/mm: 104.1 px, PVM-20M4U at 1440 lines).
    #[test]
    fn pixel_kernel() {
        let h = Halo::new(glass());
        let ppm = 4.966;
        let (rad, k) = halo_kernel_px(&h, ppm);
        let n = 2 * rad + 1;
        assert!(close(k.iter().sum::<f64>(), 1.0, 1e-12));
        assert!(close(k[rad * n + rad + 10], k[rad * n + rad - 10], 1e-15));
        assert!(close(k[(rad + 10) * n + rad], k[rad * n + rad + 10], 1e-15));
        // Radial profile along +x: the largest step is between 103 and 105 px.
        let row = &k[rad * n..(rad + 1) * n];
        let (mut at, mut best) = (0, 0.0);
        for x in 1..rad {
            let s = row[rad + x + 1] - row[rad + x];
            if s > best {
                best = s;
                at = x;
            }
        }
        let edge = h.ring_radius_mm() * ppm;
        assert!(
            (at as f64 + 0.5 - edge).abs() <= 1.0,
            "edge step at {at} px vs {edge:.2}"
        );
    }

    /// Diffusion weights: unit sum, symmetric, the variance of a Gaussian plus the two pixel boxes
    /// (sigma^2 + 1/6).
    #[test]
    fn diffusion_weights() {
        for sigma in [0.3_f64, 1.0, 2.5] {
            let r = (8.0 * sigma).ceil() as i64 + 2;
            let w: Vec<f64> = (-r..=r).map(|d| diffusion_weight_px(d, sigma)).collect();
            assert!(close(w.iter().sum::<f64>(), 1.0, 1e-9), "{sigma}");
            // sum d^2 w(d) = integral of g(x) (x^2 + f (1 - f)), f = frac(x): sigma^2 + 1/6 once sigma >= 1.
            let var: f64 = (-r..=r).zip(&w).map(|(d, x)| (d * d) as f64 * x).sum();
            if sigma >= 1.0 {
                assert!(close(var, sigma * sigma + 1.0 / 6.0, 1e-4), "{sigma}: {var}");
            }
            assert!(close(w[0], w[w.len() - 1], 1e-15));
        }
    }

    /// FFT convolution against a direct sum on a small random image.
    #[test]
    fn convolution_matches_direct_sum() {
        let (w, h) = (13, 9);
        let mut img = TubeImage::new(w, h);
        let mut s = 1u64;
        for p in &mut img.rgb {
            for c in p.iter_mut() {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                *c = (s >> 33) as f64 / f64::from(1u32 << 31);
            }
        }
        let rad = 4;
        let n = 2 * rad + 1;
        let k: Vec<f64> = (0..n * n).map(|i| 1.0 + (i % 7) as f64).collect();
        let out = convolve(&img, rad, &k);
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                for c in 0..3 {
                    let mut acc = 0.0;
                    for dy in -(rad as i64)..=rad as i64 {
                        for dx in -(rad as i64)..=rad as i64 {
                            let (sx, sy) = (x - dx, y - dy);
                            if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                                continue;
                            }
                            let kk = k[((dy + rad as i64) * n as i64 + dx + rad as i64) as usize];
                            acc += kk * img.rgb[(sy * w as i64 + sx) as usize][c];
                        }
                    }
                    let got = out.rgb[(y * w as i64 + x) as usize][c];
                    assert!(close(got, acc, 1e-9), "{x} {y} {c}: {got} vs {acc}");
                }
            }
        }
    }

    fn off() -> Optics {
        Optics {
            glass: glass(),
            halation_weight: 0.0,
            diffusion_weight: 0.0,
            diffusion_sigma_mm: 0.1,
            glare_weight: 0.0,
            bloom: Bloom::default(),
            vignette: Vignette::physical(1.0),
            tint: [1.0; 3],
            ambient_lux: 0.0,
            screen_reflectance: 0.25,
            white_nits: 100.0,
        }
    }

    /// Every effect at zero: identity. Halo, diffusion and glare conserve the light of a centred point
    /// (the halo fits inside), the glare of a dark patch is linear in the surround, the vignette corner is v.
    #[test]
    fn model_properties() {
        let (w, h) = (241, 201);
        let mut img = TubeImage::new(w, h);
        img.rgb[100 * w + 120] = [1000.0, 500.0, 250.0];
        let ppm = 1.0;
        assert_eq!(off().apply(&img, ppm, (0, 0)), img);
        let small = Glass {
            thickness_mm: 6.0,
            ..glass()
        };
        let on = Optics {
            glass: small,
            halation_weight: 0.03,
            diffusion_weight: 0.2,
            diffusion_sigma_mm: 1.5,
            glare_weight: 0.01,
            ..off()
        };
        let out = on.apply(&img, ppm, (0, 0));
        assert!(Halo::new(small).support_mm < 99.0);
        for c in 0..3 {
            let a: f64 = img.rgb.iter().map(|p| p[c]).sum();
            let b: f64 = out.rgb.iter().map(|p| p[c]).sum();
            assert!(close(b / a, 1.0, 1e-9), "channel {c}: {}", b / a);
        }
        // Vignette.
        let mut flat = TubeImage::new(w, h);
        flat.rgb.iter_mut().for_each(|p| *p = [1.0; 3]);
        let v = Optics {
            vignette: Vignette::physical(0.8),
            ..off()
        }
        .apply(&flat, ppm, (0, 0));
        assert!(close(v.rgb[100 * w + 120][0], 1.0, 1e-4));
        let corner = v.rgb[0][0];
        let rho2 = (120.0f64.powi(2) + 100.0f64.powi(2)) / (120.5f64.powi(2) + 100.5f64.powi(2));
        assert!(close(corner, 0.8f64.powf(rho2), 1e-12));
        // Glare: a dark patch in a bright surround reads w_g x APL.
        for level in [0.5, 1.0, 2.0] {
            let mut pic = TubeImage::new(w, h);
            pic.rgb.iter_mut().for_each(|p| *p = [level; 3]);
            pic.rgb[100 * w + 120] = [0.0; 3];
            let g = Optics {
                glare_weight: 0.02,
                ..off()
            }
            .apply(&pic, ppm, (0, 0));
            let apl = level * ((w * h - 1) as f64) / (w * h) as f64;
            assert!(close(g.rgb[100 * w + 120][1], 0.02 * apl, 1e-12));
        }
        // Ambient: 100 lux, T 0.85, r_s 0.25, 100 cd/m^2 white: 100 x 0.7225 x 0.25 / (pi 100) = 0.0575.
        let a = Optics {
            ambient_lux: 100.0,
            ..off()
        }
        .apply(&TubeImage::new(w, h), ppm, (0, 0));
        assert!(close(a.rgb[0][2], 0.7225 * 0.25 / PI, 1e-12));
    }

    fn bloom(weight: f64, radius_mm: f64, falloff: f64, threshold: f64) -> Optics {
        Optics {
            bloom: Bloom {
                weight,
                radius_mm,
                falloff,
                threshold,
                tint: [1.0; 3],
            },
            ..off()
        }
    }

    fn sums(img: &TubeImage) -> [f64; 3] {
        let mut s = [0.0; 3];
        for p in &img.rgb {
            for c in 0..3 {
                s[c] += p[c];
            }
        }
        s
    }

    /// Bloom energy: a flat white field keeps its level away from the edges (the redistribution only moves
    /// light), an impulse keeps its total light (the kernel fits the image) with any falloff, threshold and
    /// tint, and no value is negative even at the largest weights (normalised with halation and glare).
    #[test]
    fn bloom_conserves_the_light() {
        let (w, h, ppm) = (241, 201, 1.0);
        let mut flat = TubeImage::new(w, h);
        flat.rgb.iter_mut().for_each(|p| *p = [1.0; 3]);
        // Widest lobe: 9 x 2 mm / sqrt 2 = 12.7 px, its 4 sigma reach 51 px: the centre sees no edge.
        for falloff in [0.0, 0.5, 1.0] {
            let out = bloom(0.3, 2.0, falloff, 0.0).apply(&flat, ppm, (0, 0));
            let c = out.rgb[100 * w + 120];
            assert!(
                c.iter().all(|&v| close(v, 1.0, 1e-9)),
                "falloff {falloff}: centre {c:?}"
            );
            // The edge loses light (no light outside the tube image).
            assert!(out.rgb[100 * w][0] < 1.0 - 0.01);
        }
        let mut dot = TubeImage::new(w, h);
        dot.rgb[100 * w + 120] = [1000.0, 500.0, 250.0];
        for (falloff, threshold, tint) in [
            (0.0, 0.0, [1.0; 3]),
            (1.0, 0.0, [1.0, 0.5, 0.0]),
            (0.5, 2.0, [1.0; 3]),
            (1.0, 50.0, [0.2, 1.0, 0.7]),
        ] {
            let mut o = bloom(0.4, 2.0, falloff, threshold);
            o.bloom.tint = tint;
            let out = o.apply(&dot, ppm, (0, 0));
            for (c, (a, b)) in sums(&dot).iter().zip(sums(&out)).enumerate() {
                assert!(
                    close(b / a, 1.0, 1e-9),
                    "{falloff} {threshold} {tint:?} channel {c}: {}",
                    b / a
                );
            }
            assert!(out.rgb.iter().all(|p| p.iter().all(|&v| v >= -1e-9)));
        }
        // Halation, glare and bloom all at 0.8: normalised to a third each, the direct weight 0.
        let mut o = bloom(0.8, 2.0, 1.0, 0.0);
        o.halation_weight = 0.8;
        o.glare_weight = 0.8;
        o.glass.thickness_mm = 6.0;
        let out = o.apply(&dot, ppm, (0, 0));
        assert!(out.rgb.iter().all(|p| p.iter().all(|&v| v >= -1e-9)), "negative light");
        for (a, b) in sums(&dot).iter().zip(sums(&out)) {
            assert!(close(b / a, 1.0, 1e-6), "{}", b / a);
        }
    }

    /// The radius scales the bloom: an impulse's bloom with one lobe (falloff 0) has the second moment of a
    /// Gaussian of 1/e radius r (2 sigma^2 = r^2 px^2, plus the target pixel box, 2 / 12), so doubling r
    /// quadruples it; the falloff widens it (the lobes at 3 r and 9 r).
    #[test]
    fn bloom_radius_scaling_and_falloff() {
        let (w, h, ppm) = (301, 301, 1.0);
        let mut dot = TubeImage::new(w, h);
        dot.rgb[150 * w + 150] = [1.0; 3];
        let moment = |o: Optics| {
            let out = o.apply(&dot, ppm, (0, 0));
            // The bloom alone: out minus the direct part (1 - w) at the impulse.
            let (mut m0, mut m2) = (0.0, 0.0);
            for y in 0..h {
                for x in 0..w {
                    let mut v = out.rgb[y * w + x][0];
                    if (x, y) == (150, 150) {
                        v -= 1.0 - o.bloom.weight;
                    }
                    let r2 = (x as f64 - 150.0).powi(2) + (y as f64 - 150.0).powi(2);
                    m0 += v;
                    m2 += v * r2;
                }
            }
            (m0, m2 / m0)
        };
        for r in [2.0, 4.0, 8.0] {
            let (m0, var) = moment(bloom(0.5, r, 0.0, 0.0));
            assert!(close(m0, 0.5, 1e-9), "{r}: {m0}");
            assert!(close(var, r * r + 1.0 / 6.0, 1e-6 * r * r), "{r}: {var}");
        }
        let (_, narrow) = moment(bloom(0.5, 2.0, 0.0, 0.0));
        let (_, wide) = moment(bloom(0.5, 2.0, 1.0, 0.0));
        // f = 1: weights 0.5, 0.3, 0.2 on r^2, 9 r^2, 81 r^2.
        let want = 4.0 * (0.5 + 0.3 * 9.0 + 0.2 * 81.0) + 1.0 / 6.0;
        assert!(
            wide > 10.0 * narrow && close(wide, want, 1e-4 * want),
            "{narrow} {wide} vs {want}"
        );
    }

    /// The lobe weights sum to 1 for every falloff (clamped to [0, 1]), and the pixel kernel of the mixture
    /// sums to 1 when it is not cut.
    #[test]
    fn bloom_falloff_mixture_sums_to_one() {
        for f in [-1.0, 0.0, 0.1, 0.5, 0.9, 1.0, 3.0, f64::NAN] {
            let a = bloom_lobe_weights(f);
            assert!(close(a.iter().sum::<f64>(), 1.0, 1e-15), "{f}: {a:?}");
            assert!(a.iter().all(|&x| (0.0..=1.0).contains(&x)));
        }
        assert_eq!(bloom_lobe_weights(0.0), [1.0, 0.0, 0.0]);
        for (r, f) in [(1.0, 0.0), (1.0, 1.0), (0.4, 0.5)] {
            let sig = bloom_sigmas_px(r, 2.0);
            let (_, k) = gaussian_mixture_px(&bloom_lobe_weights(f), &sig, 10_000);
            assert!(
                close(k.iter().sum::<f64>(), 1.0, 1e-6),
                "{r} {f}: {}",
                k.iter().sum::<f64>()
            );
        }
        assert!(close(bloom_sigmas_px(10.0, 1.0)[2], 90.0 / SQRT_2, 1e-12));
    }

    /// The threshold's soft knee: no feed below t / 2, all of the excess above 3 t / 2, continuous with a
    /// continuous slope at both ends of the knee, q in [0, 1] and the excess q m non-decreasing. A flat field
    /// below the knee gets no bloom; a highlight above it glows while the mid-tone around it does not.
    #[test]
    fn bloom_threshold_knee() {
        let t = 1.0;
        assert_eq!(bloom_feed(0.3, 0.0), 1.0);
        assert_eq!(bloom_feed(0.0, 0.0), 1.0);
        assert_eq!(bloom_feed(0.49, t), 0.0);
        assert_eq!(bloom_feed(0.0, t), 0.0);
        assert!(close(bloom_feed(2.0, t), 0.5, 1e-15));
        let e = |m: f64| bloom_feed(m, t) * m;
        for edge in [0.5, 1.5] {
            assert!(close(e(edge - 1e-9), e(edge + 1e-9), 1e-8), "continuous at {edge}");
            let slope = |m: f64| (e(m + 1e-6) - e(m - 1e-6)) / 2e-6;
            assert!(close(slope(edge - 1e-4), slope(edge + 1e-4), 1e-3), "C1 at {edge}");
        }
        let mut last = 0.0;
        for i in 0..400 {
            let m = f64::from(i) * 0.01;
            let q = bloom_feed(m, t);
            assert!((0.0..=1.0).contains(&q), "{m}: {q}");
            assert!(e(m) >= last - 1e-15, "{m}");
            last = e(m);
        }
        // A mid-grey field (0.4, under the knee at 0.5): unchanged.
        let (w, h, ppm) = (121, 101, 1.0);
        let mut pic = TubeImage::new(w, h);
        pic.rgb.iter_mut().for_each(|p| *p = [0.4; 3]);
        let o = bloom(0.3, 2.0, 0.0, 1.0);
        assert_eq!(o.apply(&pic, ppm, (0, 0)), pic, "mid-tones do not bloom");
        // A 9 x 9 highlight at 3 in it: its surround brightens, the light it gives away leaves it.
        for y in 46..55 {
            for x in 56..65 {
                pic.rgb[y * w + x] = [3.0; 3];
            }
        }
        let out = o.apply(&pic, ppm, (0, 0));
        assert!(
            out.rgb[50 * w + 66][0] > 0.4 + 0.01,
            "no glow: {}",
            out.rgb[50 * w + 66][0]
        );
        assert!(out.rgb[50 * w + 60][0] < 3.0, "the highlight keeps all its light");
        assert!(
            close(out.rgb[5 * w + 5][0], 0.4, 1e-12),
            "far from the highlight: unchanged"
        );
        // The knee's average: a constant on whole blocks stays constant (edges held); a step is smoothed.
        let mut c = TubeImage::new(32, 16);
        c.rgb.iter_mut().for_each(|p| *p = [0.7; 3]);
        for k in 1..4 {
            assert!(
                pyramid_bilinear(&c, k).rgb.iter().all(|p| close(p[1], 0.7, 1e-15)),
                "level {k}"
            );
        }
        let mut step = TubeImage::new(32, 16);
        for y in 0..16 {
            for x in 16..32 {
                step.rgb[y * 32 + x] = [1.0; 3];
            }
        }
        let s = pyramid_bilinear(&step, 2);
        assert!(s.rgb[8 * 32 + 15][0] > 0.0 && s.rgb[8 * 32 + 16][0] < 1.0 && s.rgb[8 * 32 + 31][0] == 1.0);
        // The knee level: half the dominant sigma, at least level 1.
        assert_eq!(bloom_knee_level(0.5), 1);
        assert!(pyramid_sigma(bloom_knee_level(40.0)) <= 20.0);
        assert!(pyramid_sigma(bloom_knee_level(40.0) + 1) > 20.0);
    }

    /// The vignette at its defaults is the 0.2.0 law v^(rho^2), bit for bit (the recorded reference is that
    /// closed form with the same rounding steps), inside the raster and in a margin; disabled, corner 1 or floor
    /// 1 give 1.
    #[test]
    fn vignette_defaults_reproduce_the_old_law_exactly() {
        let (w, h, margin) = (97usize, 61usize, (6usize, 4usize));
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let rw = w as f64 - 2.0 * margin.0 as f64;
        let rh = h as f64 - 2.0 * margin.1 as f64;
        let hd2 = (rw * rw + rh * rh) / 4.0;
        for corner in [0.85, 0.5, 0.01, 1.0] {
            let v = Vignette::physical(corner);
            for y in 0..h {
                for x in 0..w {
                    let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
                    let old = corner.powf((dx * dx + dy * dy) / hd2);
                    let new = v.gain(dx, dy, rw / 2.0, rh / 2.0);
                    assert_eq!(new.to_bits(), old.to_bits(), "{corner} at {x} {y}");
                }
            }
        }
        // Through the model: the flat field at the defaults.
        let mut flat = TubeImage::new(w, h);
        flat.rgb.iter_mut().for_each(|p| *p = [1.0; 3]);
        let out = Optics {
            vignette: Vignette::default(),
            ..off()
        }
        .apply(&flat, 1.0, margin);
        let (dx, dy) = (0.5 - cx, 0.5 - cy);
        assert_eq!(
            out.rgb[0][1].to_bits(),
            0.85f64.powf((dx * dx + dy * dy) / hd2).to_bits()
        );
        for v in [
            Vignette {
                enable: false,
                ..Vignette::default()
            },
            Vignette::physical(1.0),
            Vignette {
                floor: 1.0,
                corner: 0.0,
                ..Vignette::default()
            },
        ] {
            assert!(v.is_neutral());
            assert_eq!(v.gain(30.0, 20.0, 40.0, 25.0), 1.0, "{v:?}");
        }
        assert!(!Vignette::default().is_neutral());
    }

    /// The shape controls: the gain never rises from the centre outward along any ray (every combination of
    /// start, exponent, softness and roundness), is 1 inside the start radius, reaches floor + (1 - floor) v
    /// at the corner, and a floor of 0 with a corner of 0 leaves the corner black (1e-4 of the centre).
    #[test]
    fn vignette_shape_is_monotonic_and_reaches_the_corner() {
        let (hw, hh) = (64.0, 48.0);
        for start in [0.0, 0.3, 0.7, 0.95] {
            for exponent in [0.25, 0.5, 2.0, 8.0, 16.0] {
                for softness in [0.0, 0.2, 1.0] {
                    for roundness in [0.0, 0.5, 1.0] {
                        let v = Vignette {
                            corner: 0.05,
                            start,
                            exponent,
                            softness,
                            roundness,
                            ..Vignette::default()
                        };
                        let corner = v.gain(hw, hh, hw, hh);
                        assert!(close(corner, 0.05, 1e-12), "{v:?}: corner {corner}");
                        for angle in 0..12 {
                            let a = f64::from(angle) * PI / 6.0 + 0.1;
                            let mut last = 1.0;
                            for i in 0..=100 {
                                let s = f64::from(i) / 100.0 * 1.2;
                                let g = v.gain(s * hw * a.cos(), s * hh * a.sin(), hw, hh);
                                assert!(g <= last + 1e-12, "{v:?} angle {a} step {i}: {g} > {last}");
                                assert!((0.0..=1.0).contains(&g));
                                last = g;
                            }
                        }
                        // Inside the start radius: no darkening.
                        if start > 0.0 {
                            let g = v.gain(0.9 * start * hw * 0.7, 0.9 * start * hh * 0.7, hw, hh);
                            assert_eq!(g, 1.0, "{v:?}");
                        }
                    }
                }
            }
        }
        let black = Vignette {
            corner: 0.0,
            ..Vignette::default()
        };
        assert!(black.gain(hw, hh, hw, hh) <= 1e-4 + 1e-15);
        let floor = Vignette {
            corner: 0.0,
            floor: 0.2,
            ..Vignette::default()
        };
        assert!(close(floor.gain(hw, hh, hw, hh), 0.2 + 0.8e-4, 1e-12));
        assert!(close(floor.gain(0.0, 0.0, hw, hh), 1.0, 1e-15));
        // Roundness 0 follows the aspect: the middle of the top edge and of the side edge darken alike.
        let aspect = Vignette {
            roundness: 0.0,
            corner: 0.3,
            ..Vignette::default()
        };
        assert!(close(aspect.gain(hw, 0.0, hw, hh), aspect.gain(0.0, hh, hw, hh), 1e-12));
        let round = Vignette::physical(0.3);
        assert!(round.gain(hw, 0.0, hw, hh) < round.gain(0.0, hh, hw, hh));
    }

    /// The centre offset moves the dark centre: the gain is symmetric about the moved centre, and the offsets
    /// (+a, b) and (-a, b) are mirror images.
    #[test]
    fn vignette_centre_offset_symmetry() {
        let (hw, hh) = (80.0, 60.0);
        let v = Vignette {
            corner: 0.2,
            start: 0.2,
            softness: 0.3,
            roundness: 0.4,
            centre: [0.2, -0.1],
            ..Vignette::default()
        };
        let mirror = Vignette {
            centre: [-0.2, -0.1],
            ..v
        };
        let (ox, oy) = (0.2 * hw, -0.1 * hh);
        for (dx, dy) in [(10.0, 5.0), (-30.0, 22.0), (55.0, -40.0), (0.0, 0.0)] {
            let a = v.gain(ox + dx, oy + dy, hw, hh);
            let b = v.gain(ox - dx, oy - dy, hw, hh);
            assert!(close(a, b, 1e-12), "{dx} {dy}: {a} {b}");
            assert!(close(v.gain(dx, dy, hw, hh), mirror.gain(-dx, dy, hw, hh), 1e-12));
        }
        assert_eq!(v.gain(ox, oy, hw, hh), 1.0, "no darkening at the moved centre");
    }
}
