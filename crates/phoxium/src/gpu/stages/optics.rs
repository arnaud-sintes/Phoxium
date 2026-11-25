//! Slot `optics` (linear light, tube -> tube): phosphor-layer diffusion, halation, veiling glare, vignette,
//! glass tint and ambient reflection (T-45; CRT.md rule 7, "Glass, halation, glare and reflections";
//! QUESTIONS PHY-10), shaders `shaders/optics*.comp` and `shaders/include/optics.glsl`, parameters
//! `params/optics.toml`, oracle `crt_model::optics` (the model and its derivation).
//!
//! # Passes
//!
//! 1. **Pyramid** (when the halo or the glare is on): level 1 = 2 x 2 box means of the input
//!    (`optics_down_input.comp`), level k = 2 x 2 box means of level k - 1 (`optics_down.comp`), zero outside,
//!    down to a 1 x 1 top, whose value times 4^n / (W H) is the image mean (the glare).
//! 2. **Halo**, by `optics_halo_method` (the cost study, QUESTIONS PHY-10, chose `kernel`):
//!    - `kernel`: the exact halo kernel of `crt_model::optics::Halo` (ring at 1.747 t, Fresnel haze, glass
//!      absorption, truncated at 99 % of its energy) as cell masses on one pyramid level l, the finest whose
//!      texels keep the kernel radius within `optics_halo_taps` texels (`optics_kernel.comp`, a direct sum);
//!      the composite reads it bilinearly at each host pixel;
//!    - `pyramid`: the halo as a least-squares mix (signed amplitudes, unit sum) of the pyramid levels read
//!      bilinearly, each taken as a Gaussian of variance (4^k - 1) / 12 + 4^k / 6 px^2 (the box chain plus
//!      the bilinear tent); it cannot form a ring (bloom-like);
//!    - `gaussians`: a least-squares mix of four Gaussians (sigma 0.3, 0.6, 0.9, 1.2 times the ring radius)
//!      at full resolution, separable (`optics_gauss_h.comp`, `optics_gauss_v.comp`); slow, comparison only.
//! 3. **Bloom** (when on; owner request 2026-11-20, model `crt_model::optics::Bloom`): each of the three lobes
//!    (Gaussians of 1/e radii r, 3 r, 9 r, the ones of weight 0 skipped) runs on one pyramid level l, the finest
//!    whose remaining blur fits [`BLOOM_R_MAX`] texels ([`bloom_lobe`], the same selection by radius in pixels
//!    as the kernel method): the level read bilinearly is already a Gaussian of variance [`pyramid_sigma`]^2,
//!    a separable blur on the level (`optics_bloom_blur.comp`, horizontal into a scratch image, then vertical)
//!    adds the rest. Without a threshold the lobes read the stage's pyramid; with one, a feed pyramid of q L
//!    first (`optics_bloom_feed.comp` for its level 1, q from the stage's pyramid level k_q, then
//!    `optics_down.comp`) up to the coarsest lobe level. The `low` quality tier allows 4 texels instead of 8
//!    and a reach of 2.5 sigma instead of 3 (a coarser level per lobe); the other tiers use the full plan.
//! 4. **Diffusion** (when on): the horizontal half of the separable sum into a full-resolution image
//!    (`optics_diffuse.comp`, radius at most 16 px).
//! 5. **Composite** (`optics.comp`): the vertical half of the diffusion, the halo, the glare, the bloom (the fed
//!    light leaves the direct image, the three lobes read bilinearly come back), the vignette, the tint and the
//!    ambient term.
//!
//! The kernel masses and the diffusion weights are computed on the CPU when their inputs change and written
//! into a host-visible storage buffer (set 1, binding 18), after idling the device (a frame in flight may read
//! it): a parameter drag pays one idle per change. Everything else comes from the `Params` buffer or the push
//! constants.
//!
//! # Images
//!
//! The stage owns its pyramid levels, the kernel halo (at level l), the gaussians method's three
//! per-channel sum images and its full-resolution halo, and a 1 x 1 stand-in for the unused bindings; all
//! `rgba16f` storage images (not chain images), allocated by `prepare` for the frame's plan and freed when the
//! plan no longer needs them or the slot does not run. The stage records its own barriers: a layout transition
//! of new images, a memory barrier from the previous frame's accesses, and one between consecutive passes.
//!
//! # Identity
//!
//! `optics_enable` off (the default, D-6, D-24), an unplaced tube (px per mm 0: the model is in mm), or
//! every effect at its neutral value (no halo, glare, diffusion or bloom, a neutral vignette, tint and ambient):
//! [`Stage::is_identity`], the pipeline skips the slot (bit exact). Each effect alone makes the stage run.

use ash::vk;
use crt_model::beam::box_gaussian_overlap;
pub use crt_model::optics::pyramid_sigma;
use crt_model::optics::{bloom_knee_level, bloom_lobe_weights, bloom_sigmas_px, Glass, Halo, Vignette};

use super::{binding, compute_pipeline, standard_set_layout, ComputePass, Domain, Registration, Stage};
use crate::gpu::headless::{GpuImage, HostBuffer};
use crate::gpu::{barrier, Context, FrameConstants};
use crate::params::{Params, OPTICS_HALO_METHOD_GAUSSIANS, OPTICS_HALO_METHOD_PYRAMID};
use crate::quality::{self, Tier};
use crate::Res;

const COMPOSITE_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/optics.comp.spv"));
const DOWN_INPUT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/optics_down_input.comp.spv"));
const DOWN_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/optics_down.comp.spv"));
const KERNEL_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/optics_kernel.comp.spv"));
const GAUSS_H_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/optics_gauss_h.comp.spv"));
const GAUSS_V_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/optics_gauss_v.comp.spv"));
const DIFFUSE_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/optics_diffuse.comp.spv"));
const BLOOM_FEED_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/optics_bloom_feed.comp.spv"));
const BLOOM_BLUR_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/optics_bloom_blur.comp.spv"));

/// Largest pyramid depth (tube images up to 16384 px).
const MAX_LEVELS: usize = 14;
/// Pyramid levels the composite can read (`level1` to `level12` of `optics.comp`).
const PYRAMID_BINDINGS: usize = 12;
/// Bindings of set 1 (the passes that write images use storage images 0 to 4 and the buffer at 18; the composite
/// reads sampled images 2, 4 to 17 and 19 to 21 (the bloom lobes) and the buffer at 18).
const BINDINGS: u32 = 22;
const BUFFER_BINDING: u32 = 18;
/// Storage images of the set 1 of the passes that write images (bindings 0 to 4).
const STORAGE_BINDINGS: u32 = 5;
/// The images the composite reads as sampled images (the composite's set 1 layout; `optics.comp`): the diffusion
/// and halo images, the top, the pyramid levels 1 to 12 and the three bloom lobes.
fn composite_bindings() -> Vec<u32> {
    [2, 4]
        .into_iter()
        .chain(5..=17)
        .chain(BLOOM_BINDING as u32..BLOOM_BINDING as u32 + 3)
        .collect()
}
/// First binding of the three bloom lobes read by the composite.
const BLOOM_BINDING: usize = 19;
/// Largest diffusion radius in px (`OPT_DIFF_MAX`) and the start of the kernel quadrant (`OPT_KERNEL_OFFSET`).
pub const DIFF_MAX: usize = 16;
const KERNEL_OFFSET: usize = DIFF_MAX + 1;
/// Largest kernel radius in texels (`optics_halo_taps` is clamped to it; `OPT_TAPS_MAX_Q` of `optics.glsl`).
const TAPS_MAX: usize = 48;
/// The bloom blur weights in the buffer (`OPT_BLOOM_OFFSET`), [`BLOOM_R_MAX`] + 1 per lobe.
const BLOOM_OFFSET: usize = KERNEL_OFFSET + (TAPS_MAX + 2) * (TAPS_MAX + 2);
/// Largest bloom blur radius in texels of the lobe's level (`OPT_BLOOM_R_MAX`): the level of a lobe is the
/// finest whose remaining blur fits it.
pub const BLOOM_R_MAX: usize = 8;
/// The radius allowed in the `low` quality tier (a level coarser per lobe, about a quarter of the texels).
pub const BLOOM_R_LOW: usize = 4;
const BUFFER_FLOATS: usize = BLOOM_OFFSET + 3 * (BLOOM_R_MAX + 1);
/// Descriptor sets of set 1: one per pyramid level, the kernel, the two Gaussian passes, the composite, the
/// diffusion, one per feed pyramid level, and the two blur passes of each bloom lobe.
const SET_KERNEL: usize = MAX_LEVELS;
const SET_GAUSS_H: usize = MAX_LEVELS + 1;
const SET_GAUSS_V: usize = MAX_LEVELS + 2;
const SET_COMPOSITE: usize = MAX_LEVELS + 3;
const SET_DIFFUSE: usize = MAX_LEVELS + 4;
const SET_FEED: usize = MAX_LEVELS + 5;
const SET_BLOOM_H: usize = 2 * MAX_LEVELS + 5;
const SET_BLOOM_V: usize = 2 * MAX_LEVELS + 8;
const SETS: usize = 2 * MAX_LEVELS + 11;
const FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;
const GROUP: (u32, u32) = (8, 8);
/// Largest Gaussian reach of the gaussians method, px.
const GAUSS_RADIUS_MAX: i32 = 2048;
/// Largest number of taps per pixel and per pass (2 r + 1) of the gaussians method (review D-12): above it,
/// the frame takes the `pyramid` method instead, with a notice. The reach is 3.6 times the ring radius
/// (1.747 t): the default glass (12 mm) needs 751 taps on a 1440-line 19 in tube (4.966 px/mm, the cost
/// study of QUESTIONS PHY-10) and 379 in the tests at 2.5 px/mm, which stay on the method; the soft maximum
/// (30 mm) on that tube needs about 1900 and the hard maximum about 4100 (the GPU-timeout risk), which fall
/// back. The method is a slow comparison method in any case (`params/optics.toml`).
pub const GAUSS_TAPS_MAX: i32 = 1025;
/// Sigmas of the gaussians method relative to the ring radius.
const GAUSS_SIGMAS: [f64; 4] = [0.3, 0.6, 0.9, 1.2];

/// The sigmas (px) of the gaussians method for `halo` at `ppm` px per mm.
fn gauss_sigmas_px(h: &Halo, ppm: f64) -> Vec<f64> {
    let ring = h.ring_radius_mm() * ppm;
    GAUSS_SIGMAS.iter().map(|f| (f * ring).max(0.5)).collect()
}

/// The reach (px, 3 sigma of the widest Gaussian) of the gaussians method for `halo` at `ppm` px per mm.
fn gauss_radius_px(h: &Halo, ppm: f64) -> i32 {
    let sig = gauss_sigmas_px(h, ppm);
    ((3.0 * sig[3]).ceil() as i32).min(GAUSS_RADIUS_MAX)
}

/// Push constants of every optics pass (`OpticsPush` of `optics.glsl`, std430, 112 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Push {
    info: [i32; 4],
    scal: [f32; 4],
    w: [[f32; 4]; 4],
    aux: [i32; 4],
}

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "optics",
    input: Domain::Tube,
    output: Domain::Tube,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Optics>::default()
}

/// The halo method of the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Kernel,
    Pyramid,
    Gaussians,
}

/// What a frame records, decided on the CPU from the parameters and constants ([`plan`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Tube image size.
    pub size: (u32, u32),
    /// Pyramid levels (0: no pyramid), level n is 1 x 1.
    pub levels: usize,
    /// Halo method, `None` when the halo is off.
    pub method: Option<Method>,
    /// Kernel method: the level l and the kernel radius in texels.
    pub kernel_level: usize,
    pub kernel_radius: usize,
    /// Diffusion radius in px (0: none) and sigma in px.
    pub diffusion_radius: usize,
    pub diffusion_sigma_px: f64,
    /// Image mean = top texel x this (4^n / (W H)).
    pub mean_scale: f64,
    /// The `gaussians` method was asked for but needs more than [`GAUSS_TAPS_MAX`] taps: `method` is
    /// `Pyramid` instead.
    pub gauss_fallback: bool,
    /// The bloom, `None` when it is off.
    pub bloom: Option<BloomPlan>,
}

/// One bloom lobe on the GPU: its pyramid level, the remaining blur on that level (radius and sigma in
/// texels) and its amplitude (`crt_model::optics::bloom_lobe_weights`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomLobe {
    pub level: usize,
    pub radius: usize,
    pub sigma_texels: f64,
    pub amp: f64,
}

/// The bloom of a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct BloomPlan {
    /// The lobes at r, 3 r, 9 r; `None` for a lobe of weight 0.
    pub lobes: [Option<BloomLobe>; 3],
    /// The pyramid level whose bilinear read gives the knee brightness (0: no threshold, q = 1).
    pub knee_level: usize,
    /// Levels of the feed pyramid of q L (0: no threshold, the lobes read the stage's pyramid).
    pub feed_levels: usize,
}

/// The level, blur radius (texels) and blur sigma (texels) of a bloom lobe of sigma `sigma_px` on a pyramid of
/// `levels` levels: the finest level l >= 1 whose remaining blur, sigma t with t^2 = (sigma^2 -
/// [`pyramid_sigma`] (l)^2) / 4^l - 1 / 6 (the pixel-box overlap of the weights adds 1 / 6 texel^2), has a
/// radius ceil(`reach` t) of at most `max_r` texels (the coarsest level, radius cut to `max_r`, when none
/// fits). A lobe narrower than level 1 read bilinearly gets no blur (radius 0): it is then slightly wider than
/// asked (sigma 0.96 px at least).
pub fn bloom_lobe(sigma_px: f64, levels: usize, max_r: usize, reach: f64) -> (usize, usize, f64) {
    let mut out = (1, 0, 0.0);
    for l in 1..=levels.max(1) {
        let var = (sigma_px * sigma_px - pyramid_sigma(l).powi(2)) / 4f64.powi(l as i32) - 1.0 / 6.0;
        let t = if var > 0.0 { var.sqrt() } else { 0.0 };
        let r = if t < 0.05 { 0 } else { (reach * t).ceil() as usize };
        out = (l, r.min(max_r), t);
        if r <= max_r {
            break;
        }
    }
    out
}

/// The vignette of the parameters (the model's [`Vignette`]).
pub fn vignette(p: &Params) -> Vignette {
    Vignette {
        enable: p.optics_vignette_enable,
        corner: f64::from(p.optics_vignette_corner),
        start: f64::from(p.optics_vignette_start),
        exponent: f64::from(p.optics_vignette_exponent),
        softness: f64::from(p.optics_vignette_softness),
        roundness: f64::from(p.optics_vignette_roundness),
        centre: p.optics_vignette_centre_xy.map(f64::from),
        floor: f64::from(p.optics_vignette_floor),
    }
}

/// The bloom plan of the parameters on a pyramid of `levels` levels at `px_per_mm`, `None` when the bloom is
/// off (weight 0, every tint channel 0, or no valid radius).
fn bloom_plan(p: &Params, px_per_mm: f64, levels: usize) -> Option<BloomPlan> {
    let positive = |v: f32| v > 0.0; // false for NaN
    let on = positive(p.optics_bloom_weight)
        && p.optics_bloom_tint_rgb.iter().any(|&t| positive(t))
        && positive(p.optics_bloom_radius_mm)
        && p.optics_bloom_radius_mm.is_finite();
    if !on {
        return None;
    }
    let (max_r, reach) = if quality::tier(p) == Tier::Low {
        (BLOOM_R_LOW, 2.5)
    } else {
        (BLOOM_R_MAX, 3.0)
    };
    let sig = bloom_sigmas_px(f64::from(p.optics_bloom_radius_mm), px_per_mm);
    let amps = bloom_lobe_weights(f64::from(p.optics_bloom_falloff));
    let lobes: [Option<BloomLobe>; 3] = std::array::from_fn(|j| {
        (amps[j] > 0.0).then(|| {
            let (level, radius, sigma_texels) = bloom_lobe(sig[j], levels, max_r, reach);
            BloomLobe {
                level,
                radius,
                sigma_texels,
                amp: amps[j],
            }
        })
    });
    let knee_level = if positive(p.optics_bloom_threshold) {
        bloom_knee_level(sig[0]).clamp(1, levels.clamp(1, PYRAMID_BINDINGS))
    } else {
        0
    };
    let feed_levels = if knee_level > 0 {
        lobes.iter().flatten().map(|l| l.level).max().unwrap_or(1)
    } else {
        0
    };
    Some(BloomPlan {
        lobes,
        knee_level,
        feed_levels,
    })
}

/// Pyramid level sizes: ceil halvings of `size`, `levels` of them.
fn level_sizes(size: (u32, u32), levels: usize) -> Vec<(u32, u32)> {
    let mut s = size;
    (0..levels)
        .map(|_| {
            s = (s.0.div_ceil(2).max(1), s.1.div_ceil(2).max(1));
            s
        })
        .collect()
}

/// The glass of the parameters, `None` when it is not a valid glass (the halo is then off).
fn glass(p: &Params) -> Option<Glass> {
    let g = Glass {
        thickness_mm: f64::from(p.optics_glass_thickness_mm),
        index: f64::from(p.optics_glass_index),
        transmission: f64::from(p.optics_glass_transmission),
    };
    let ok = g.thickness_mm.is_finite()
        && g.thickness_mm > 0.0
        && g.index.is_finite()
        && g.index > 1.0
        && g.transmission.is_finite()
        && g.transmission > 0.0
        && g.transmission <= 1.0;
    ok.then_some(g)
}

/// Diffusion sigma in px when the diffusion is on (weight > 0 and a sigma of at least 0.05 px).
fn diffusion_sigma(p: &Params, fc: &FrameConstants) -> Option<f64> {
    let s = f64::from(p.optics_diffusion_mm) * f64::from(fc.px_per_mm);
    (p.optics_diffusion_weight > 0.0 && s.is_finite() && s >= 0.05).then_some(s)
}

/// The frame's plan; `None` when the stage is an identity (off, unplaced, or every effect neutral).
pub fn plan(p: &Params, fc: &FrameConstants, size: (u32, u32), halo: Option<&Halo>) -> Option<Plan> {
    let positive = |v: f32| v > 0.0; // false for NaN
    if !p.optics_enable || !positive(fc.px_per_mm) || size.0 == 0 || size.1 == 0 {
        return None;
    }
    let diff = diffusion_sigma(p, fc);
    let halo_on = positive(p.optics_halation_weight) && glass(p).is_some();
    let glare_on = positive(p.optics_glare_weight);
    let bloom_on = bloom_plan(p, f64::from(fc.px_per_mm), 1).is_some();
    let neutral = !halo_on
        && !glare_on
        && !bloom_on
        && diff.is_none()
        && vignette(p).is_neutral()
        && p.optics_tint_rgb == [1.0; 3]
        && !positive(p.optics_ambient_lux);
    if neutral {
        return None;
    }
    let mut method = halo_on.then_some(match p.optics_halo_method {
        OPTICS_HALO_METHOD_PYRAMID => Method::Pyramid,
        OPTICS_HALO_METHOD_GAUSSIANS => Method::Gaussians,
        _ => Method::Kernel,
    });
    // The gaussians method's cost grows with the reach (review D-12): above the tap budget, the pyramid.
    // Without the halo at hand (the identity test), the method does not change the identity verdict.
    let gauss_fallback = method == Some(Method::Gaussians)
        && halo.is_some_and(|h| 2 * gauss_radius_px(h, f64::from(fc.px_per_mm)) + 1 > GAUSS_TAPS_MAX);
    if gauss_fallback {
        method = Some(Method::Pyramid);
    }
    let levels = if halo_on || glare_on || bloom_on {
        let m = size.0.max(size.1).max(2);
        ((32 - (m - 1).leading_zeros()) as usize).min(MAX_LEVELS)
    } else {
        0
    };
    let (mut kernel_level, mut kernel_radius) = (0, 0);
    if method == Some(Method::Kernel) {
        let h = halo.expect("plan: the kernel method needs the halo");
        let support = h.support_mm * f64::from(fc.px_per_mm);
        let taps = (p.optics_halo_taps.max(2) as usize).min(TAPS_MAX) as f64;
        let mut l = 1;
        while l < levels && support / f64::from(1u32 << l) + 0.5 > taps {
            l += 1;
        }
        kernel_level = l;
        kernel_radius = ((support / f64::from(1u32 << l) + 0.5).ceil() as usize).min(TAPS_MAX);
    }
    let (diffusion_radius, diffusion_sigma_px) = match diff {
        Some(s) => (((4.0 * s).ceil() as usize + 1).min(DIFF_MAX), s),
        None => (0, 0.0),
    };
    Some(Plan {
        size,
        levels,
        method,
        kernel_level,
        kernel_radius,
        diffusion_radius,
        diffusion_sigma_px,
        mean_scale: 4f64.powi(levels as i32) / (f64::from(size.0) * f64::from(size.1)),
        gauss_fallback,
        bloom: bloom_plan(p, f64::from(fc.px_per_mm), levels),
    })
}

/// Kernel cell masses of `halo` on a level of 2^`level` px cells at `px_per_mm`, radius `r` texels: the
/// quadrant (r + 1)^2 (index dy (r + 1) + dx), each the integral of the density over the cell at offset
/// (dx, dy) cells from the target cell centre (m x m midpoint rule, m = 4 to 16), normalised so that the full
/// (2r + 1)^2 kernel sums to 1.
pub fn kernel_cells(halo: &Halo, px_per_mm: f64, level: usize, r: usize) -> Vec<f64> {
    let s = f64::from(1u32 << level);
    let m = (2.0 * s).clamp(4.0, 16.0) as usize;
    let mut q = vec![0.0; (r + 1) * (r + 1)];
    let mut total = 0.0;
    for dy in 0..=r {
        for dx in 0..=r {
            let mut acc = 0.0;
            for j in 0..m {
                for i in 0..m {
                    let x = (dx as f64 - 0.5 + (i as f64 + 0.5) / m as f64) * s;
                    let y = (dy as f64 - 0.5 + (j as f64 + 0.5) / m as f64) * s;
                    acc += halo.density((x * x + y * y).sqrt() / px_per_mm);
                }
            }
            let mass = acc * (s / px_per_mm).powi(2) / (m * m) as f64;
            q[dy * (r + 1) + dx] = mass;
            let mult = if dx == 0 { 1.0 } else { 2.0 } * if dy == 0 { 1.0 } else { 2.0 };
            total += mult * mass;
        }
    }
    if total > 0.0 {
        for v in &mut q {
            *v /= total;
        }
    }
    q
}

/// Diffusion weights w(d), d = 0 ..= r: the mass a source pixel puts into a pixel d away along one axis under
/// a Gaussian of `sigma_px` (`box_gaussian_overlap`), normalised to w(0) + 2 sum w(d) = 1 (the truncation).
pub fn diffusion_weights(sigma_px: f64, r: usize) -> Vec<f64> {
    let mut w: Vec<f64> = (0..=r)
        .map(|d| box_gaussian_overlap(-0.5, 0.5, d as f64 - 0.5, d as f64 + 0.5, sigma_px))
        .collect();
    let sum = w[0] + 2.0 * w[1..].iter().sum::<f64>();
    for v in &mut w {
        *v /= sum;
    }
    w
}

/// Least-squares amplitudes (unit sum) of radially symmetric unit-area Gaussians of `sigmas_px` that best fit
/// the halo kernel of `halo` at `px_per_mm` in the 2D L2 sense (radial grid out to the support, a small ridge
/// for the nearly collinear bases).
pub fn gaussian_fit(halo: &Halo, px_per_mm: f64, sigmas_px: &[f64]) -> Vec<f64> {
    let n = sigmas_px.len();
    let support = halo.support_mm * px_per_mm;
    let steps = 4000;
    let dr = support / steps as f64;
    let g = |s: f64, r: f64| (-0.5 * r * r / (s * s)).exp() / (2.0 * std::f64::consts::PI * s * s);
    let mut c = vec![0.0; n];
    for i in 0..=steps {
        let r = i as f64 * dr;
        let wt = if i == 0 || i == steps { 0.5 } else { 1.0 } * 2.0 * std::f64::consts::PI * r * dr;
        let k = halo.density(r / px_per_mm) / (px_per_mm * px_per_mm);
        for (j, &s) in sigmas_px.iter().enumerate() {
            c[j] += wt * k * g(s, r);
        }
    }
    // [G 1; 1^T 0] [a; lambda] = [c; 1], G_ij = 1 / (2 pi (s_i^2 + s_j^2)).
    let dim = n + 1;
    let mut a = vec![vec![0.0; dim + 1]; dim];
    let mut trace = 0.0;
    for i in 0..n {
        for j in 0..n {
            a[i][j] = 1.0 / (2.0 * std::f64::consts::PI * (sigmas_px[i].powi(2) + sigmas_px[j].powi(2)));
        }
        trace += a[i][i];
    }
    for (i, row) in a.iter_mut().enumerate().take(n) {
        row[i] += 1e-6 * trace / n as f64;
        row[n] = 1.0;
        row[dim] = c[i];
    }
    a[n][..n].fill(1.0);
    a[n][dim] = 1.0;
    // Gaussian elimination with partial pivoting.
    for col in 0..dim {
        let piv = (col..dim)
            .max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))
            .unwrap_or(col);
        a.swap(col, piv);
        let d = a[col][col];
        if d.abs() < 1e-300 {
            continue;
        }
        for row in 0..dim {
            if row != col {
                let f = a[row][col] / d;
                let pivot = a[col].clone();
                for (x, p) in a[row][col..=dim].iter_mut().zip(&pivot[col..=dim]) {
                    *x -= f * p;
                }
            }
        }
    }
    (0..n).map(|i| a[i][dim] / a[i][i]).collect()
}

/// Key of the CPU-computed weights (kernel cells, diffusion weights, fits, bloom blurs): all inputs as bits.
#[derive(Clone, Debug, PartialEq)]
struct WeightKey {
    glass: [u32; 3],
    px_per_mm: u32,
    plan: (Option<Method>, usize, usize, usize, u64),
    levels: usize,
    bloom: [(usize, u64); 3],
}

/// The bloom blur weights of a lobe: `diffusion_weights` of its sigma and radius ([1] without a blur).
fn bloom_weights(lobe: &BloomLobe) -> Vec<f64> {
    if lobe.radius == 0 || lobe.sigma_texels <= 0.0 {
        vec![1.0]
    } else {
        diffusion_weights(lobe.sigma_texels, lobe.radius)
    }
}

/// What the images were allocated for: (size, levels, kernel level, gaussians, diffusion, bloom feed levels,
/// bloom lobe levels (0: lobe off)).
type AllocKey = ((u32, u32), usize, usize, bool, bool, usize, [usize; 3]);

/// The allocation key of `plan`.
fn alloc_key(plan: &Plan) -> AllocKey {
    let bloom = plan.bloom.as_ref();
    (
        plan.size,
        plan.levels,
        if plan.method == Some(Method::Kernel) {
            plan.kernel_level
        } else {
            0
        },
        plan.method == Some(Method::Gaussians),
        plan.diffusion_radius > 0,
        bloom.map_or(0, |b| b.feed_levels),
        std::array::from_fn(|j| bloom.and_then(|b| b.lobes[j]).map_or(0, |l| l.level)),
    )
}

#[derive(Default)]
struct Optics {
    /// The composite pass's standard set layout (the pipelines use [`Optics::layout`]).
    pass: ComputePass,
    set1_layout: vk::DescriptorSetLayout,
    /// The composite's own set 1 layout and pipeline layout (sampled images, `composite_bindings`).
    set1c_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    layout_composite: vk::PipelineLayout,
    composite: vk::Pipeline,
    down_input: vk::Pipeline,
    down: vk::Pipeline,
    kernel: vk::Pipeline,
    gauss_h: vk::Pipeline,
    gauss_v: vk::Pipeline,
    diffuse: vk::Pipeline,
    bloom_feed: vk::Pipeline,
    bloom_blur: vk::Pipeline,
    pool: vk::DescriptorPool,
    sets: Vec<vk::DescriptorSet>,
    /// Owned images: pyramid levels (index k - 1), kernel halo, Gaussian sums (3), full-resolution halo, and
    /// the 1 x 1 stand-in.
    levels: Vec<GpuImage>,
    halo_low: GpuImage,
    gauss: [GpuImage; 3],
    halo_full: GpuImage,
    /// The horizontal half of the diffusion (full resolution).
    diff: GpuImage,
    /// The bloom: the feed pyramid of q L (index k - 1), and per lobe the scratch of its horizontal blur and
    /// the blurred lobe (both at the lobe's level).
    feed: Vec<GpuImage>,
    bloom_tmp: [GpuImage; 3],
    bloom_img: [GpuImage; 3],
    dummy: GpuImage,
    /// Allocation key of the images: (size, levels, kernel level when the kernel method runs, gaussians,
    /// diffusion).
    alloc_key: Option<AllocKey>,
    buffer: HostBuffer,
    weight_key: Option<WeightKey>,
    /// Cached halo of the glass bits.
    halo: Option<([u32; 3], Halo)>,
    /// The frame's plan and pushes.
    plan: Option<Plan>,
    pyramid_amps: [f32; 12],
    gauss_sigmas: [f32; 4],
    gauss_amps: [f32; 4],
    gauss_radius: i32,
    /// The current plan is a gaussians fallback whose notice was given.
    gauss_fallback_noticed: bool,
    /// Images created since the last recorded frame: the next record transitions them.
    fresh: bool,
    transition: bool,
}

impl Optics {
    /// The halo of the parameters' glass (cached on the glass bits), `None` for an invalid glass.
    fn halo_for(&mut self, p: &Params) -> Option<Halo> {
        let g = glass(p)?;
        let key = [
            p.optics_glass_thickness_mm.to_bits(),
            p.optics_glass_index.to_bits(),
            p.optics_glass_transmission.to_bits(),
        ];
        match self.halo {
            Some((k, h)) if k == key => Some(h),
            _ => {
                let h = Halo::new(g);
                self.halo = Some((key, h));
                Some(h)
            }
        }
    }

    /// # Safety
    /// The GPU no longer uses the images.
    unsafe fn free_images(&mut self, d: &ash::Device) {
        for img in &mut self.levels {
            img.destroy(d);
        }
        self.levels.clear();
        self.halo_low.destroy(d);
        for img in &mut self.gauss {
            img.destroy(d);
        }
        self.halo_full.destroy(d);
        self.diff.destroy(d);
        for img in &mut self.feed {
            img.destroy(d);
        }
        self.feed.clear();
        for img in self.bloom_tmp.iter_mut().chain(self.bloom_img.iter_mut()) {
            img.destroy(d);
        }
        self.alloc_key = None;
    }

    /// Allocates the images of `plan` and writes every descriptor set.
    ///
    /// # Safety
    /// `ctx` is live, the old images are freed and no set is in use.
    unsafe fn alloc_images(&mut self, ctx: &Context, plan: &Plan) -> Res<()> {
        let make = |size: (u32, u32)| {
            GpuImage::create(
                ctx,
                vk::Extent2D {
                    width: size.0,
                    height: size.1,
                },
                FORMAT,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED,
            )
        };
        let sizes = level_sizes(plan.size, plan.levels);
        for &s in &sizes {
            self.levels.push(make(s)?);
        }
        let kernel = plan.method == Some(Method::Kernel);
        if kernel {
            self.halo_low = make(sizes[plan.kernel_level - 1])?;
        }
        let gaussians = plan.method == Some(Method::Gaussians);
        if gaussians {
            for img in &mut self.gauss {
                *img = make(plan.size)?;
            }
            self.halo_full = make(plan.size)?;
        }
        let diffusion = plan.diffusion_radius > 0;
        if diffusion {
            self.diff = make(plan.size)?;
        }
        if let Some(b) = &plan.bloom {
            for &s in &level_sizes(plan.size, b.feed_levels) {
                self.feed.push(make(s)?);
            }
            for (j, lobe) in b.lobes.iter().enumerate() {
                if let Some(l) = lobe {
                    let s = sizes[l.level - 1];
                    self.bloom_tmp[j] = make(s)?;
                    self.bloom_img[j] = make(s)?;
                }
            }
        }
        self.alloc_key = Some(alloc_key(plan));
        self.fresh = true;
        // Descriptor sets: every binding the dummy, then the pass's own images.
        let dummy = self.dummy.view;
        let level = |k: usize| self.levels.get(k.wrapping_sub(1)).map_or(dummy, |i| i.view);
        let feed = |k: usize| self.feed.get(k.wrapping_sub(1)).map_or(dummy, |i| i.view);
        let or_dummy = |img: &GpuImage| {
            if img.view == vk::ImageView::null() {
                dummy
            } else {
                img.view
            }
        };
        let mut views = vec![[dummy; BINDINGS as usize]; SETS];
        if let Some(b) = &plan.bloom {
            for k in 1..=b.feed_levels {
                views[SET_FEED + k - 1][0] = if k == 1 { level(b.knee_level) } else { feed(k - 1) };
                views[SET_FEED + k - 1][1] = feed(k);
            }
            for (j, lobe) in b.lobes.iter().enumerate() {
                if let Some(l) = lobe {
                    views[SET_BLOOM_H + j][0] = if b.feed_levels > 0 {
                        feed(l.level)
                    } else {
                        level(l.level)
                    };
                    views[SET_BLOOM_H + j][1] = self.bloom_tmp[j].view;
                    views[SET_BLOOM_V + j][0] = self.bloom_tmp[j].view;
                    views[SET_BLOOM_V + j][1] = self.bloom_img[j].view;
                    views[SET_COMPOSITE][BLOOM_BINDING + j] = self.bloom_img[j].view;
                }
            }
        }
        for k in 1..=plan.levels {
            if k >= 2 {
                views[k - 1][0] = level(k - 1);
            }
            views[k - 1][1] = level(k);
        }
        if kernel {
            views[SET_KERNEL][0] = level(plan.kernel_level);
            views[SET_KERNEL][1] = or_dummy(&self.halo_low);
        }
        if gaussians {
            views[SET_GAUSS_H][1] = self.gauss[0].view;
            views[SET_GAUSS_H][2] = self.gauss[1].view;
            views[SET_GAUSS_H][3] = self.gauss[2].view;
            views[SET_GAUSS_V][0] = self.gauss[0].view;
            views[SET_GAUSS_V][2] = self.gauss[1].view;
            views[SET_GAUSS_V][3] = self.gauss[2].view;
            views[SET_GAUSS_V][4] = self.halo_full.view;
        }
        if diffusion {
            views[SET_DIFFUSE][1] = self.diff.view;
            views[SET_COMPOSITE][2] = self.diff.view;
        }
        let c = &mut views[SET_COMPOSITE];
        c[4] = if kernel {
            or_dummy(&self.halo_low)
        } else if gaussians {
            self.halo_full.view
        } else {
            dummy
        };
        c[5] = if plan.levels > 0 { level(plan.levels) } else { dummy };
        for k in 1..=PYRAMID_BINDINGS.min(plan.levels) {
            c[5 + k] = level(k);
        }
        self.write_sets(&ctx.device, &views);
        Ok(())
    }

    /// # Safety
    /// No set is in use; the views and the buffer are live.
    unsafe fn write_sets(&self, d: &ash::Device, views: &[[vk::ImageView; BINDINGS as usize]]) {
        let infos: Vec<Vec<[vk::DescriptorImageInfo; 1]>> = views
            .iter()
            .map(|set| {
                set.iter()
                    .map(|&v| {
                        [vk::DescriptorImageInfo::default()
                            .image_view(v)
                            .image_layout(vk::ImageLayout::GENERAL)]
                    })
                    .collect()
            })
            .collect();
        let buf = [vk::DescriptorBufferInfo::default()
            .buffer(self.buffer.buffer)
            .range(vk::WHOLE_SIZE)];
        let composite = composite_bindings();
        let mut writes = Vec::new();
        for (s, set_infos) in infos.iter().enumerate() {
            // The composite reads its images as sampled images; the other passes have storage images 0 to 4.
            let (bindings, ty): (Vec<u32>, vk::DescriptorType) = if s == SET_COMPOSITE {
                (composite.clone(), vk::DescriptorType::SAMPLED_IMAGE)
            } else {
                ((0..STORAGE_BINDINGS).collect(), vk::DescriptorType::STORAGE_IMAGE)
            };
            for b in bindings {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(self.sets[s])
                        .dst_binding(b)
                        .descriptor_type(ty)
                        .image_info(&set_infos[b as usize]),
                );
            }
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(self.sets[s])
                    .dst_binding(BUFFER_BINDING)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(&buf),
            );
        }
        d.update_descriptor_sets(&writes, &[]);
    }

    /// Computes the CPU weights of the plan when their inputs changed (kernel cells and diffusion weights into
    /// the buffer after idling the device; fits into the push constants).
    ///
    /// # Safety
    /// `ctx` is live and the buffer is mapped with [`BUFFER_FLOATS`] floats.
    unsafe fn update_weights(&mut self, ctx: &Context, p: &Params, fc: &FrameConstants, plan: &Plan) -> Res<()> {
        let key = WeightKey {
            glass: [
                p.optics_glass_thickness_mm.to_bits(),
                p.optics_glass_index.to_bits(),
                p.optics_glass_transmission.to_bits(),
            ],
            px_per_mm: fc.px_per_mm.to_bits(),
            plan: (
                plan.method,
                plan.kernel_level,
                plan.kernel_radius,
                plan.diffusion_radius,
                plan.diffusion_sigma_px.to_bits(),
            ),
            levels: plan.levels,
            bloom: std::array::from_fn(|j| {
                plan.bloom
                    .as_ref()
                    .and_then(|b| b.lobes[j])
                    .map_or((0, 0), |l| (l.radius, l.sigma_texels.to_bits()))
            }),
        };
        if self.weight_key.as_ref() == Some(&key) {
            return Ok(());
        }
        let ppm = f64::from(fc.px_per_mm);
        let halo = if plan.method.is_some() { self.halo_for(p) } else { None };
        let mut buf = vec![0f32; BUFFER_FLOATS];
        if plan.diffusion_radius > 0 {
            for (i, w) in diffusion_weights(plan.diffusion_sigma_px, plan.diffusion_radius)
                .into_iter()
                .enumerate()
            {
                buf[i] = w as f32;
            }
        }
        if let Some(b) = &plan.bloom {
            for (j, lobe) in b.lobes.iter().enumerate() {
                if let Some(l) = lobe {
                    let at = BLOOM_OFFSET + j * (BLOOM_R_MAX + 1);
                    for (i, w) in bloom_weights(l).into_iter().enumerate() {
                        buf[at + i] = w as f32;
                    }
                }
            }
        }
        self.pyramid_amps = [0.0; 12];
        self.gauss_amps = [0.0; 4];
        if let Some(h) = halo {
            match plan.method {
                Some(Method::Kernel) => {
                    let q = kernel_cells(&h, ppm, plan.kernel_level, plan.kernel_radius);
                    for (i, v) in q.into_iter().enumerate() {
                        buf[KERNEL_OFFSET + i] = v as f32;
                    }
                }
                Some(Method::Pyramid) => {
                    let n = plan.levels.min(PYRAMID_BINDINGS);
                    let sig: Vec<f64> = (1..=n).map(pyramid_sigma).collect();
                    for (i, a) in gaussian_fit(&h, ppm, &sig).into_iter().enumerate() {
                        self.pyramid_amps[i] = a as f32;
                    }
                }
                Some(Method::Gaussians) => {
                    let sig = gauss_sigmas_px(&h, ppm);
                    for (i, a) in gaussian_fit(&h, ppm, &sig).into_iter().enumerate() {
                        self.gauss_amps[i] = a as f32;
                        self.gauss_sigmas[i] = sig[i] as f32;
                    }
                    self.gauss_radius = gauss_radius_px(&h, ppm);
                }
                None => {}
            }
        }
        // A frame in flight may read the buffer.
        ctx.device.device_wait_idle()?;
        if self.buffer.mapped.is_null() {
            return Err("optics: the weight buffer is missing".into());
        }
        std::ptr::copy_nonoverlapping(buf.as_ptr().cast::<u8>(), self.buffer.mapped, BUFFER_FLOATS * 4);
        self.weight_key = Some(key);
        Ok(())
    }

    /// # Safety
    /// As [`Stage::record`].
    #[allow(clippy::too_many_arguments)]
    unsafe fn dispatch(
        &self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        set0: vk::DescriptorSet,
        set1: usize,
        pipeline: vk::Pipeline,
        push: &Push,
        size: (u32, u32),
        group: (u32, u32),
    ) {
        let bind = vk::PipelineBindPoint::COMPUTE;
        d.cmd_bind_pipeline(cb, bind, pipeline);
        let layout = if set1 == SET_COMPOSITE {
            self.layout_composite
        } else {
            self.layout
        };
        d.cmd_bind_descriptor_sets(cb, bind, layout, 0, &[set0, self.sets[set1]], &[]);
        super::push(d, cb, layout, vk::ShaderStageFlags::COMPUTE, push);
        d.cmd_dispatch(cb, size.0.div_ceil(group.0), size.1.div_ceil(group.1), 1);
    }
}

/// A compute-to-compute memory barrier: the previous pass's writes before the next pass's reads and writes.
unsafe fn pass_barrier(d: &ash::Device, cb: vk::CommandBuffer) {
    let compute = vk::PipelineStageFlags2::COMPUTE_SHADER;
    let m = [vk::MemoryBarrier2::default()
        .src_stage_mask(compute)
        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
        .dst_stage_mask(compute)
        .dst_access_mask(
            vk::AccessFlags2::SHADER_STORAGE_READ
                | vk::AccessFlags2::SHADER_STORAGE_WRITE
                | vk::AccessFlags2::SHADER_SAMPLED_READ,
        )];
    d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
}

impl Stage for Optics {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        let s = vk::ShaderStageFlags::COMPUTE;
        self.pass.set_layout = standard_set_layout(d)?;
        // Set 1 of the passes that write images: the storage images 0 to 4 and the weights buffer.
        let b: Vec<_> = (0..STORAGE_BINDINGS)
            .map(|i| binding(i, vk::DescriptorType::STORAGE_IMAGE, s))
            .chain([binding(BUFFER_BINDING, vk::DescriptorType::STORAGE_BUFFER, s)])
            .collect();
        self.set1_layout =
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&b), None)?;
        // Set 1 of the composite: its images as sampled images and the weights buffer (22 storage images in one
        // layout exceed the 16 per stage of an Intel UHD 770; sampled images are limited to 200 there).
        let bc: Vec<_> = composite_bindings()
            .into_iter()
            .map(|i| binding(i, vk::DescriptorType::SAMPLED_IMAGE, s))
            .chain([binding(BUFFER_BINDING, vk::DescriptorType::STORAGE_BUFFER, s)])
            .collect();
        self.set1c_layout =
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bc), None)?;
        let ranges = [vk::PushConstantRange::default()
            .stage_flags(s)
            .offset(0)
            .size(size_of::<Push>() as u32)];
        let set_layouts = [self.pass.set_layout, self.set1_layout];
        self.layout = d.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&set_layouts)
                .push_constant_ranges(&ranges),
            None,
        )?;
        let set_layouts_c = [self.pass.set_layout, self.set1c_layout];
        self.layout_composite = d.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&set_layouts_c)
                .push_constant_ranges(&ranges),
            None,
        )?;
        self.composite = compute_pipeline(d, self.layout_composite, COMPOSITE_SPV)?;
        self.down_input = compute_pipeline(d, self.layout, DOWN_INPUT_SPV)?;
        self.down = compute_pipeline(d, self.layout, DOWN_SPV)?;
        self.kernel = compute_pipeline(d, self.layout, KERNEL_SPV)?;
        self.gauss_h = compute_pipeline(d, self.layout, GAUSS_H_SPV)?;
        self.gauss_v = compute_pipeline(d, self.layout, GAUSS_V_SPV)?;
        self.diffuse = compute_pipeline(d, self.layout, DIFFUSE_SPV)?;
        self.bloom_feed = compute_pipeline(d, self.layout, BLOOM_FEED_SPV)?;
        self.bloom_blur = compute_pipeline(d, self.layout, BLOOM_BLUR_SPV)?;
        let sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(STORAGE_BINDINGS * (SETS as u32 - 1)),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(composite_bindings().len() as u32),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(SETS as u32),
        ];
        self.pool = d.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default()
                .max_sets(SETS as u32)
                .pool_sizes(&sizes),
            None,
        )?;
        let mut layouts = vec![self.set1_layout; SETS];
        layouts[SET_COMPOSITE] = self.set1c_layout;
        self.sets = d.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.pool)
                .set_layouts(&layouts),
        )?;
        Ok(())
    }

    fn is_identity(&self, params: &Params, fc: &FrameConstants) -> bool {
        // The halo is not needed to decide (only its kernel radius is).
        let size = fc.domain_size(Domain::Tube);
        let p = Params {
            optics_halation_weight: if glass(params).is_some() {
                params.optics_halation_weight
            } else {
                0.0
            },
            optics_halo_method: OPTICS_HALO_METHOD_PYRAMID,
            ..*params
        };
        plan(&p, fc, size, None).is_none()
    }

    unsafe fn prepare(&mut self, ctx: &Context, fc: &FrameConstants, params: &Params, runs: bool) -> Res<()> {
        let d = &ctx.device;
        self.transition = false;
        let size = fc.domain_size(Domain::Tube);
        let halo = if runs && params.optics_enable && params.optics_halation_weight > 0.0 {
            self.halo_for(params)
        } else {
            None
        };
        self.plan = if runs {
            plan(params, fc, size, halo.as_ref())
        } else {
            None
        };
        // Review D-12: one notice when the gaussians method falls back to the pyramid (not every frame).
        let fallback = self.plan.as_ref().is_some_and(|pl| pl.gauss_fallback);
        if fallback && !self.gauss_fallback_noticed {
            crate::params::notify(format!(
                "optics: the 'gaussians' halo method would need more than {GAUSS_TAPS_MAX} taps per pixel at this \
                 glass thickness and resolution; the 'pyramid' method is used instead"
            ));
        }
        self.gauss_fallback_noticed = fallback;
        let Some(plan) = self.plan.clone() else {
            if self.alloc_key.is_some() {
                d.device_wait_idle()?;
                self.free_images(d);
            }
            return Ok(());
        };
        // R-27: each object is made when it is missing, so a failure between the two (the buffer after the dummy)
        // is retried by the next frame instead of leaving a null mapping that `update_weights` would write to.
        if self.dummy.image == vk::Image::null() || self.buffer.mapped.is_null() {
            if self.dummy.image == vk::Image::null() {
                self.dummy = GpuImage::create(
                    ctx,
                    vk::Extent2D { width: 1, height: 1 },
                    FORMAT,
                    vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED,
                )?;
            }
            if self.buffer.mapped.is_null() {
                self.buffer = HostBuffer::create(ctx, BUFFER_FLOATS * 4, vk::BufferUsageFlags::STORAGE_BUFFER)?;
                self.weight_key = None;
            }
            self.fresh = true;
            self.alloc_key = None;
        }
        let key = alloc_key(&plan);
        if self.alloc_key != Some(key) {
            d.device_wait_idle()?;
            self.free_images(d);
            self.alloc_images(ctx, &plan)?;
        }
        self.update_weights(ctx, params, fc, &plan)?;
        self.transition = std::mem::take(&mut self.fresh);
        Ok(())
    }

    fn abandon(&mut self) {
        self.fresh |= self.transition;
        self.transition = false;
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        let Some(plan) = &self.plan else {
            return;
        };
        let compute = vk::PipelineStageFlags2::COMPUTE_SHADER;
        let rw = vk::AccessFlags2::SHADER_STORAGE_READ
            | vk::AccessFlags2::SHADER_STORAGE_WRITE
            | vk::AccessFlags2::SHADER_SAMPLED_READ;
        if self.transition {
            let all = self
                .levels
                .iter()
                .chain([&self.halo_low, &self.halo_full, &self.diff, &self.dummy])
                .chain(self.gauss.iter())
                .chain(self.feed.iter())
                .chain(self.bloom_tmp.iter())
                .chain(self.bloom_img.iter())
                .filter(|i| i.image != vk::Image::null());
            for img in all {
                barrier(
                    d,
                    cb,
                    img.image,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::GENERAL,
                    vk::PipelineStageFlags2::NONE,
                    vk::AccessFlags2::NONE,
                    compute,
                    rw,
                );
            }
        } else {
            // The previous frame's reads and writes of the stage's images before this frame's.
            let m = [vk::MemoryBarrier2::default()
                .src_stage_mask(compute)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(compute)
                .dst_access_mask(rw)];
            d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
        }
        let none = Push::default();
        let sizes = level_sizes(plan.size, plan.levels);
        for (k, &s) in sizes.iter().enumerate() {
            let pipe = if k == 0 { self.down_input } else { self.down };
            self.dispatch(d, cb, set, k, pipe, &none, s, GROUP);
            pass_barrier(d, cb);
        }
        match plan.method {
            Some(Method::Kernel) => {
                let push = Push {
                    info: [plan.kernel_radius as i32, 0, 0, 0],
                    ..none
                };
                let s = sizes[plan.kernel_level - 1];
                self.dispatch(d, cb, set, SET_KERNEL, self.kernel, &push, s, GROUP);
                pass_barrier(d, cb);
            }
            Some(Method::Gaussians) => {
                let push = Push {
                    info: [self.gauss_radius, 0, 0, 0],
                    w: [self.gauss_sigmas, self.gauss_amps, [0.0; 4], [0.0; 4]],
                    ..none
                };
                self.dispatch(d, cb, set, SET_GAUSS_H, self.gauss_h, &push, size, GROUP);
                pass_barrier(d, cb);
                self.dispatch(d, cb, set, SET_GAUSS_V, self.gauss_v, &push, size, GROUP);
                pass_barrier(d, cb);
            }
            _ => {}
        }
        if let Some(b) = &plan.bloom {
            // The feed pyramid of q L (with a threshold), then each lobe's two blur halves on its level.
            let feed_sizes = level_sizes(plan.size, b.feed_levels);
            for (k, &s) in feed_sizes.iter().enumerate() {
                let (pipe, push) = if k == 0 {
                    let push = Push {
                        info: [0, b.knee_level as i32, 0, 0],
                        ..none
                    };
                    (self.bloom_feed, push)
                } else {
                    (self.down, none)
                };
                self.dispatch(d, cb, set, SET_FEED + k, pipe, &push, s, GROUP);
                pass_barrier(d, cb);
            }
            for (j, lobe) in b.lobes.iter().enumerate() {
                if let Some(l) = lobe {
                    let s = sizes[l.level - 1];
                    let off = (BLOOM_OFFSET + j * (BLOOM_R_MAX + 1)) as i32;
                    for (dir, set1) in [(0, SET_BLOOM_H + j), (1, SET_BLOOM_V + j)] {
                        let push = Push {
                            info: [l.radius as i32, dir, off, 0],
                            ..none
                        };
                        self.dispatch(d, cb, set, set1, self.bloom_blur, &push, s, GROUP);
                        pass_barrier(d, cb);
                    }
                }
            }
        }
        if plan.diffusion_radius > 0 {
            let push = Push {
                info: [plan.diffusion_radius as i32, 0, 0, 0],
                ..none
            };
            self.dispatch(d, cb, set, SET_DIFFUSE, self.diffuse, &push, size, GROUP);
            pass_barrier(d, cb);
        }
        let mode = match plan.method {
            None => 0,
            Some(Method::Kernel) => 1,
            Some(Method::Pyramid) => 2,
            Some(Method::Gaussians) => 3,
        };
        let a = self.pyramid_amps;
        let bloom = plan.bloom.as_ref();
        let lobe = |j: usize| bloom.and_then(|b| b.lobes[j]);
        let amp = |j: usize| lobe(j).map_or(0.0, |l| l.amp as f32);
        let packed = (0..3).fold(0i32, |acc, j| {
            acc | ((lobe(j).map_or(0, |l| l.level) as i32) << (8 * j))
        });
        let push = Push {
            info: [
                mode,
                plan.kernel_level as i32,
                plan.diffusion_radius as i32,
                plan.levels.min(PYRAMID_BINDINGS) as i32,
            ],
            scal: [plan.mean_scale as f32, amp(0), amp(1), amp(2)],
            w: [
                [a[0], a[1], a[2], a[3]],
                [a[4], a[5], a[6], a[7]],
                [a[8], a[9], a[10], a[11]],
                [0.0; 4],
            ],
            aux: [
                i32::from(bloom.is_some()),
                packed,
                bloom.map_or(0, |b| b.knee_level as i32),
                0,
            ],
        };
        self.dispatch(d, cb, set, SET_COMPOSITE, self.composite, &push, size, GROUP);
    }

    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.free_images(d);
        self.dummy.destroy(d);
        self.buffer.destroy(d);
        d.destroy_descriptor_pool(self.pool, None);
        for p in [
            self.composite,
            self.down_input,
            self.down,
            self.kernel,
            self.gauss_h,
            self.gauss_v,
            self.diffuse,
            self.bloom_feed,
            self.bloom_blur,
        ] {
            d.destroy_pipeline(p, None);
        }
        d.destroy_pipeline_layout(self.layout, None);
        d.destroy_pipeline_layout(self.layout_composite, None);
        d.destroy_descriptor_set_layout(self.set1_layout, None);
        d.destroy_descriptor_set_layout(self.set1c_layout, None);
        self.pass.destroy(d);
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The push block is 112 bytes (std430 of `optics.glsl`); the buffer layout constants match the shader's.
    #[test]
    fn push_layout_matches_the_shader() {
        assert_eq!(size_of::<Push>(), 112);
        assert_eq!(std::mem::offset_of!(Push, scal), 16);
        assert_eq!(std::mem::offset_of!(Push, w), 32);
        assert_eq!(std::mem::offset_of!(Push, aux), 96);
        let glsl = include_str!("../../../../../shaders/include/optics.glsl").replace("\r\n", "\n");
        assert!(glsl.contains("ivec4 info;\n    vec4 scal;\n    vec4 w[4];\n    ivec4 aux;"));
        assert!(glsl.contains(&format!("#define OPT_DIFF_MAX {DIFF_MAX}")));
        assert!(glsl.contains(&format!("#define OPT_TAPS_MAX_Q {TAPS_MAX}")));
        assert!(glsl.contains(&format!("#define OPT_BLOOM_R_MAX {BLOOM_R_MAX}")));
        let kernel = include_str!("../../../../../shaders/optics_kernel.comp");
        assert!(kernel.contains(&format!("#define OPT_TAPS_MAX {TAPS_MAX}")));
        let composite = include_str!("../../../../../shaders/optics.comp");
        assert!(composite.contains(&format!("binding = {BLOOM_BINDING}) uniform texture2D bloom0")));
    }

    /// A bloom lobe runs on the finest level whose remaining blur fits the radius budget: level 1 read
    /// bilinearly for a lobe narrower than it, deeper levels for wider lobes, the total variance (level plus
    /// blur) equal to the lobe's, and the `low` tier one level coarser with a smaller radius.
    #[test]
    fn bloom_lobe_levels_follow_the_radius() {
        assert_eq!(bloom_lobe(0.5, 11, BLOOM_R_MAX, 3.0), (1, 0, 0.0));
        let mut last = 0;
        for s in [2.0, 5.0, 20.0, 60.0, 200.0, 600.0] {
            let (l, r, t) = bloom_lobe(s, 11, BLOOM_R_MAX, 3.0);
            assert!(r <= BLOOM_R_MAX && l >= last, "{s}: {l} {r} {t}");
            if l > 1 {
                // The next finer level would need more than the budget.
                let finer = (s * s - pyramid_sigma(l - 1).powi(2)) / 4f64.powi(l as i32 - 1) - 1.0 / 6.0;
                assert!((3.0 * finer.sqrt()).ceil() as usize > BLOOM_R_MAX, "{s}: level {l}");
            }
            if t > 0.0 {
                let total = pyramid_sigma(l).powi(2) + 4f64.powi(l as i32) * (t * t + 1.0 / 6.0);
                assert!((total - s * s).abs() < 1e-9 * s * s, "{s}: {total}");
            }
            last = l;
            let (ll, rl, _) = bloom_lobe(s, 11, BLOOM_R_LOW, 2.5);
            assert!(ll >= l && rl <= BLOOM_R_LOW, "{s}: low {ll} {rl}");
        }
        // The plan: three lobes at r, 3 r, 9 r for falloff 0.5, one for falloff 0; a threshold adds the feed
        // pyramid up to the coarsest lobe level and the knee level.
        let fc = placed();
        let p = Params {
            optics_enable: true,
            optics_bloom_weight: 0.2,
            optics_bloom_radius_mm: 4.0,
            ..Params::default()
        };
        let b = plan(&p, &fc, (1920, 1440), Some(&Halo::new(glass(&p).unwrap())))
            .unwrap()
            .bloom
            .unwrap();
        assert!(b.lobes.iter().all(Option::is_some) && b.knee_level == 0 && b.feed_levels == 0);
        let levels: Vec<usize> = b.lobes.iter().flatten().map(|l| l.level).collect();
        assert!(levels[0] < levels[1] && levels[1] < levels[2], "{levels:?}");
        let amps: f64 = b.lobes.iter().flatten().map(|l| l.amp).sum();
        assert!((amps - 1.0).abs() < 1e-12);
        let one = bloom_plan(
            &Params {
                optics_bloom_falloff: 0.0,
                optics_bloom_threshold: 0.7,
                ..p
            },
            4.966,
            11,
        )
        .unwrap();
        assert!(one.lobes[0].is_some() && one.lobes[1].is_none() && one.lobes[2].is_none());
        assert!(one.knee_level >= 1 && one.feed_levels == one.lobes[0].unwrap().level);
        // Low tier: never a finer level than the standard plan.
        let low = bloom_plan(
            &Params {
                quality_tier: crate::params::QUALITY_TIER_LOW,
                ..p
            },
            4.966,
            11,
        )
        .unwrap();
        for (a, s) in low.lobes.iter().flatten().zip(b.lobes.iter().flatten()) {
            assert!(a.level >= s.level && a.radius <= BLOOM_R_LOW);
        }
        // Off: weight 0, every tint channel 0, or no radius.
        for q in [
            Params {
                optics_bloom_weight: 0.0,
                ..p
            },
            Params {
                optics_bloom_tint_rgb: [0.0; 3],
                ..p
            },
            Params {
                optics_bloom_radius_mm: f32::NAN,
                ..p
            },
        ] {
            assert!(bloom_plan(&q, 4.966, 11).is_none());
        }
    }

    fn placed() -> FrameConstants {
        let mut fc = FrameConstants::new((320, 240), (1920, 1440), false);
        fc.px_per_mm = 4.966;
        fc.line_pitch_mm = 290.0 / 240.0;
        fc
    }

    /// Off, unplaced or neutral: identity; each effect alone makes the stage run.
    #[test]
    fn identity_rules() {
        let fc = placed();
        let size = (1920, 1440);
        let off = Params::default();
        assert!(plan(&off, &fc, size, None).is_none(), "off by default");
        let on = Params {
            optics_enable: true,
            optics_halation_weight: 0.0,
            optics_glare_weight: 0.0,
            optics_diffusion_weight: 0.0,
            optics_vignette_corner: 1.0,
            ..off
        };
        assert!(plan(&on, &fc, size, None).is_none(), "neutral");
        let mut unplaced = fc;
        unplaced.px_per_mm = 0.0;
        let all = Params {
            optics_enable: true,
            optics_halo_method: OPTICS_HALO_METHOD_PYRAMID,
            ..off
        };
        assert!(plan(&all, &unplaced, size, None).is_none(), "unplaced");
        for p in [
            Params {
                optics_glare_weight: 0.01,
                ..on
            },
            Params {
                optics_vignette_corner: 0.9,
                ..on
            },
            Params {
                optics_tint_rgb: [1.0, 0.9, 1.0],
                ..on
            },
            Params {
                optics_ambient_lux: 10.0,
                ..on
            },
            Params {
                optics_diffusion_weight: 0.1,
                ..on
            },
            Params {
                optics_bloom_weight: 0.1,
                ..on
            },
        ] {
            assert!(plan(&p, &fc, size, None).is_some());
        }
        // The vignette alone runs the stage (the default shape, or any shape), and is neutral when disabled,
        // at a corner of 1 or a floor of 1; the bloom is neutral at weight 0 or with every tint channel at 0.
        let vig = Params {
            optics_vignette_corner: 0.2,
            optics_vignette_start: 0.4,
            optics_vignette_exponent: 4.0,
            ..on
        };
        assert!(plan(&vig, &fc, size, None).is_some());
        for p in [
            Params {
                optics_vignette_enable: false,
                ..vig
            },
            Params {
                optics_vignette_floor: 1.0,
                ..vig
            },
            Params {
                optics_vignette_corner: 1.0,
                ..vig
            },
            Params {
                optics_bloom_weight: 0.3,
                optics_bloom_tint_rgb: [0.0; 3],
                optics_vignette_corner: 1.0,
                ..vig
            },
        ] {
            assert!(plan(&p, &fc, size, None).is_none(), "{p:?}");
        }
        // The bloom alone builds the pyramid and nothing of the halo.
        let glow = Params {
            optics_bloom_weight: 0.2,
            ..on
        };
        let pl = plan(&glow, &fc, size, None).unwrap();
        assert!(pl.levels > 0 && pl.method.is_none() && pl.bloom.is_some());
    }

    /// The kernel level follows the taps: t = 12 mm, T 0.85 at 4.966 px/mm has a support of about 620 px; 24
    /// taps need cells of 32 px (level 5), 48 taps cells of 16 px (level 4). The cells sum to 1.
    #[test]
    fn kernel_level_and_cells() {
        let fc = placed();
        // The halation is opt-in (weight 0 by default): the physical 1.5 % switches the halo on.
        let p = Params {
            optics_enable: true,
            optics_halation_weight: 0.015,
            ..Params::default()
        };
        let h = Halo::new(glass(&p).unwrap());
        let pl = plan(&p, &fc, (1920, 1440), Some(&h)).unwrap();
        assert_eq!(pl.method, Some(Method::Kernel));
        assert_eq!(pl.levels, 11);
        let support = h.support_mm * 4.966;
        assert!(support / 32.0 + 0.5 <= 24.0 && support / 16.0 + 0.5 > 24.0, "{support}");
        assert_eq!(pl.kernel_level, 5);
        let fine = plan(
            &Params {
                optics_halo_taps: 48,
                ..p
            },
            &fc,
            (1920, 1440),
            Some(&h),
        )
        .unwrap();
        assert_eq!(fine.kernel_level, 4);
        let r = pl.kernel_radius;
        let q = kernel_cells(&h, 4.966, pl.kernel_level, r);
        let mut total = 0.0;
        for dy in -(r as i64)..=r as i64 {
            for dx in -(r as i64)..=r as i64 {
                total += q[dy.unsigned_abs() as usize * (r + 1) + dx.unsigned_abs() as usize];
            }
        }
        assert!((total - 1.0).abs() < 1e-12);
        assert!((pl.mean_scale - 4f64.powi(11) / (1920.0 * 1440.0)).abs() < 1e-12);
    }

    /// Review D-12: the gaussians method keeps its method within the tap budget (the default glass on the
    /// 1440-line tube: 751 taps) and falls back to the pyramid above it (30 mm, the soft maximum; the hard
    /// maximum thickness), never above GAUSS_TAPS_MAX taps.
    #[test]
    fn gaussians_method_falls_back_above_the_tap_budget() {
        let fc = placed();
        let ppm = f64::from(fc.px_per_mm);
        let hard_max = crate::params::meta("optics_glass_thickness_mm")
            .unwrap()
            .hard_range
            .unwrap()
            .1 as f32;
        for (t, fallback) in [(12.0, false), (30.0, true), (hard_max, true)] {
            let p = Params {
                optics_enable: true,
                optics_halation_weight: 0.015,
                optics_halo_method: OPTICS_HALO_METHOD_GAUSSIANS,
                optics_glass_thickness_mm: t,
                ..Params::default()
            };
            let h = Halo::new(glass(&p).unwrap());
            let taps = 2 * gauss_radius_px(&h, ppm) + 1;
            let pl = plan(&p, &fc, (1920, 1440), Some(&h)).unwrap();
            assert_eq!(pl.gauss_fallback, fallback, "{t} mm: {taps} taps");
            assert_eq!(taps > GAUSS_TAPS_MAX, fallback, "{t} mm: {taps} taps");
            let want = if fallback { Method::Pyramid } else { Method::Gaussians };
            assert_eq!(pl.method, Some(want), "{t} mm");
        }
        // The default glass at 1440 lines: 751 taps (the module documentation).
        let h = Halo::new(glass(&Params::default()).unwrap());
        assert_eq!(2 * gauss_radius_px(&h, ppm) + 1, 751);
        // Other methods never carry the flag.
        let p = Params {
            optics_enable: true,
            optics_halation_weight: 0.015,
            optics_glass_thickness_mm: hard_max,
            ..Params::default()
        };
        let h = Halo::new(glass(&p).unwrap());
        let pl = plan(&p, &fc, (1920, 1440), Some(&h)).unwrap();
        assert_eq!((pl.method, pl.gauss_fallback), (Some(Method::Kernel), false));
    }

    /// Diffusion weights sum to 1; the fit of a halo by Gaussians sums to 1.
    #[test]
    fn weights_and_fits() {
        let w = diffusion_weights(0.75, 4);
        assert!((w[0] + 2.0 * w[1..].iter().sum::<f64>() - 1.0).abs() < 1e-12);
        let h = Halo::new(Glass {
            thickness_mm: 12.0,
            index: 1.52,
            transmission: 0.85,
        });
        let sig: Vec<f64> = (1..=11).map(pyramid_sigma).collect();
        let a = gaussian_fit(&h, 4.966, &sig);
        assert!((a.iter().sum::<f64>() - 1.0).abs() < 1e-6, "{a:?}");
        assert!((pyramid_sigma(1) - (0.25f64 + 4.0 / 6.0).sqrt()).abs() < 1e-12);
    }
}
