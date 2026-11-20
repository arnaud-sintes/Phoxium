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
//! 4. **Vignetting** (CRT.md "Uniformity": corners 70 to 90 % of the centre). Dominant optical cause:
//!    thicker (tinted) glass toward the edge, T_edge = T_centre^(t_edge / t_centre). With a thickness that
//!    grows with the square of the distance from the centre, the relative transmission is
//!    v^(rho^2), rho = distance from the image centre over the half diagonal of the raster rectangle, v
//!    = `vignette_corner` (the corner over centre ratio).
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
//! out_c = v^(rho^2) tint_c G_c + A_c.
//!
//! Boundary: no light exists outside the tube image (zero padding); light the diffusion or the halo
//! carries past the image edge is lost, as in the `screen` stage.

use std::f64::consts::PI;

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
    pub vignette_corner: f64,
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
        let sigma_px = self.diffusion_sigma_mm * px_per_mm;
        let d = if self.diffusion_weight > 0.0 && sigma_px > 0.0 {
            let g = diffuse(img, sigma_px);
            let wd = self.diffusion_weight;
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
        let halo = if self.halation_weight > 0.0 {
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
        let (wh, wg) = (self.halation_weight, self.glare_weight);
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let rw = w as f64 - 2.0 * margin.0 as f64;
        let rh = h as f64 - 2.0 * margin.1 as f64;
        let hd2 = (rw * rw + rh * rh) / 4.0;
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
                let v = vignette(self.vignette_corner, (dx * dx + dy * dy) / hd2);
                for c in 0..3 {
                    let hv = halo.as_ref().map_or(0.0, |hi| hi.rgb[i][c]);
                    let g = (1.0 - wh - wg) * d.rgb[i][c] + wh * hv + wg * mean[c];
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
            vignette_corner: 1.0,
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
            vignette_corner: 0.8,
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
}
