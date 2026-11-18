//! GPU tests of the slot mask and the delta dot mask (T-39b, `mask_slot` and `mask_delta` in
//! `shaders/include/mask.glsl`, parameters `params/mask.toml`), headless, validation on, zero validation
//! messages.
//!
//! Set-up as in `stage_mask_grille.rs`: line pitch 0 with px per mm set, so the tube image is the uniform beam
//! current I times the mask, compared with the `crt-model` f64 oracle (`crt_model::mask`, T-38) and with
//! numbers derived by hand from `CRT.md` (below). The oracle works in host pixels, origin at the top-left of the
//! tube rectangle, host pixel (x, y) = [x, x + 1] x [y, y + 1]:
//! - slot mask: pitch p = px per mm x pitch (the f32 product the shader uses), stripe = fill x p, edge =
//!   softness x p, slot period V = slot period x p (f32 product), bridge fraction as given;
//! - delta: the shader reduces x modulo the cell width X = f32(P x sqrt 3) and y modulo P (P = the f32 product
//!   px per mm x pitch), so its lattice is X wide and P tall, both f32 values. The oracle lattice is built on X
//!   (diagonal pitch X / sqrt 3, in f64) and its rows are matched to the shader's by scaling y by
//!   sigma = P / (X / sqrt 3) = 1 + O(1e-7): pixel (x, y) is the oracle rectangle [x, x + 1] x [y / sigma,
//!   (y + 1) / sigma]. This only lines up the two lattices (no drift over many rows); everything else differs
//!   by O(1e-7) relative. Dot diameter = f32(fill x P), edge = f32(softness x P).
//!
//! Tolerance: FP16 storage window, nearest since T-fp16 ([`Stored::detect`] asserts it), several beam currents, plus an
//! f32 budget ([`f32_budget`]): positions of a pixel inside its period are known to a few f32 ulp of the
//! period (2e-7 period), which moves a value by at most 4e-7 x period in px x the peak value 1 / coverage
//! (slot: horizontal and vertical periods; delta: the cell width); the dot areas are differences of
//! quadrant areas of size r^2, each with a few f32 ulp, 2e-6 x max(r^2, 1) x peak; plus 2e-6.
//!
//! Hand derivations (CRT.md sections in brackets; an independent model re-derives them):
//!
//! E1. Host pixels per triad [Pitch definitions; Delta (dot) triad geometry; Host-display resolution
//!     budget]. Slot: p x px per mm; 4 px/mm with p = 0.385, 0.725, 1.1, 2.0 mm gives 1.54, 2.9, 4.4, 8 px
//!     per triad; 0.25 mm gives 1.0, 0.75 mm gives 3.0; 26 in TV 1920 / 528 x 0.8 = 32 / 11 = 2.909091.
//!     Delta: the triad pitch is the horizontal dot pitch P sqrt(3) / 2 = 0.866 P (vendor "horizontal dot
//!     pitch"; for P = 0.28 mm, 0.242487 mm), so with P = 0.28 mm, px per mm = target / 0.242487:
//!     1.54 -> 6.350853, 2.9 -> 11.959398, 4.4 -> 18.145294, 8 -> 32.991444, 1.0 -> 4.123930, 3.0 ->
//!     12.371791, 16 -> 65.982888 px per mm.
//! E2. Coverage [Beam transmission; Delta: a round hole of diameter h on a cell of area A_t transmits
//!     pi h^2 / (4 A_t)]. Slot: stripe fill x (1 - bridge) = 0.25 x 0.85 = 0.2125 (defaults). Delta: two
//!     dots of each colour per sqrt(3) P x P cell, area of a ramped dot pi (R^2 + s^2 / 12): R = 0.225 P,
//!     s = 0.07 P (defaults), 2 pi (0.050625 + 0.000408333) / sqrt 3 = 0.185129. A lit pixel fully inside
//!     an element reads 1 / coverage: slot 4.705882, delta 5.401655.
//! E3. Flat white [crt-model conventions]. The value is the area mean over the coverage, so the mean over
//!     whole periods is exactly 1 per channel. Slot: the x period is 2 p (two column phases), and the
//!     x-averaged profile has period V / 2 (the two phases are half a period apart); windows of whole 2 p
//!     and whole V / 2 give 1: at 1.54 px per triad 77 px = 25 x 3.08, with V = 2 px (slot period 2 / 1.54
//!     = 1.298701, V / 2 = 1 row); at 2.9, 58 px = 10 x 5.8, V = 2 px (0.689655); at 4.4, 44 px = 5 x 8.8,
//!     V = 4 px (0.909091, V / 2 = 2 rows); at 8, 16 px, V = 6 px (0.75, V / 2 = 3 rows); windows of 12
//!     rows. Delta: the x period is the cell X = 2 x px per triad (3.08, 5.8, 8.8, 16 px: 77, 58, 44 and 16
//!     px are whole), the x-averaged profile has period P / 2 = X / (2 sqrt 3), irrational in px, so over 64
//!     rows the partial row period moves the mean by less than (P / 2) / 64 of the peak row mean; the plan
//!     asks for 1 +/- 0.5 %, and the GPU mean must equal the oracle's area mean of the whole window.
//! E4. Slot element positions, 16 px per triad, hard edges, fill 0.25, slot period 0.75 (V = 12 px),
//!     bridge 1/6 (2 px) [Slot mask geometry: columns staggered by V / 2]. Stripes as on a grille: R [0.6667,
//!     4.6667], G [6, 10], B [11.3333, 15.3333] px of each 16 px column. Even columns (x in [0, 16), [32,
//!     48), ...) have bridges centred on rows 0, 12, 24, ...: rows [11, 13] mod 12 dark, i.e. rows 11 and 0
//!     of each 12; odd columns centred on 6 + 12 k: rows 5 and 6 dark. Peak 1 / (0.25 x 5/6) = 4.8. So
//!     pixel 2 (red, inside its stripe) reads 4.8 on rows 1 to 10 and 0 on rows 0 and 11; pixel 18 (red,
//!     odd column) reads 4.8 except rows 5 and 6; pixel 0 reads 1/3 x 4.8 = 1.6 where lit.
//! E5. Delta lattice, P = 0.28 mm at 100 px per mm (P = 28 px) [Delta (dot) triad geometry table]: same
//!     colour repeat along a row P sqrt 3 = 0.485 mm, same colour between adjacent rows horizontally P sqrt(3)
//!     / 2 = 0.242 mm, row spacing P / 2 = 0.140 mm, adjacent dots of any colour in a row P / sqrt 3 = 0.162
//!     mm; colours R G B left to right along a row; nearest same colour in the next row at distance P, 30
//!     degrees from horizontal. Measured as dot centroids of the rendered channels.
//! E6. Zoomed tube, 16 px per triad [Slot mask geometry; Delta geometry]. Slot, default slot period 0.8333
//!     and bridge 0.15: one column has the vertical period V = 13.333 px; the row means over whole 2 p (32
//!     px) have period V / 2 = 6.667 px (stagger). Delta: X = 32 px, P = 32 / sqrt 3 = 18.475 px; the row
//!     means over whole cells have period P / 2 = 9.2376 px (row spacing); one column of one colour has
//!     period P = 18.475 px (rows j and j + 2 hold the same colour at the same x).
//! E7. Host-grid moire [Moire: beat p1 p2 / |p1 - p2|]: the x profile (mean over 64 rows) has the alias
//!     period of the triad frequency on the host grid, t / |t - 1| for t in (2/3, 2): 1.54 -> 2.85185 px;
//!     2.9 is below Nyquist (1 / 2.9 = 0.345 < 0.5): 2.9 px; 16 px per triad: 16 px. Slot: the 2 p
//!     component of single rows cancels between the two column phases in the row mean; delta: the 3 a
//!     component cancels between rows j and j + 1 (shifted by 1.5 a, half of 3 a).
//! E8. Tier C [Host-display resolution budget; crt-model prefiltered]: below 3 px per triad the mask is
//!     averaged over a box of one period (slot p x V, delta 1.5 a x P), whose mean is 1 at every position:
//!     both types read exactly 1, ripple 0 < 1.4 %, and damper wires (grille only) do not appear. From 3
//!     px per triad the area integral (structure) appears at once for both.

mod common;

use std::ops::Range;

use crt_model::mask::{alias_period_px, tier, Delta, Mask, Rect, Slot, Tier, BARTEN_THRESHOLD};
use ultimatecrt::gpu::{Domain, FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::{
    Params, MASK_TIER_AUTO, MASK_TIER_INTEGRATED, MASK_TYPE_APERTURE_GRILLE, MASK_TYPE_DELTA, MASK_TYPE_SLOT_MASK,
};

/// One FP16 step relative to the value, at most: 2^-10.
const FP16_STEP: f64 = 1.0 / 1024.0;
/// Beam currents the oracle comparisons are repeated with.
const SCALES: [f32; 3] = [1.0, 0.83, 0.67];
/// sqrt(3) as the shader's f32 constant.
const SQRT3_F32: f32 = 1.732_050_8;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Stored {
    lo: f64,
    hi: f64,
}

impl Stored {
    /// Checks the FP16 store rounding (as in `stage_mask_grille.rs`): nearest on any device since T-fp16.
    fn detect(gpu: &mut Headless) -> Self {
        let mut fc = tube(4, 2, 1.0);
        fc.px_per_mm = 0.0;
        let amp = currents(&fc, |_, _| [1.0 + 0.75 / 1024.0; 3]);
        let stored = run(gpu, &["screen"], fc, &Params::default(), &amp).pixel(0, 0)[0];
        assert_eq!(
            stored,
            1.0 + 1.0 / 1024.0,
            "FP16 stage stores must round to nearest (T-fp16)"
        );
        Self {
            lo: -FP16_STEP / 2.0,
            hi: FP16_STEP / 2.0,
        }
    }

    fn holds(&self, got: f64, want: f64, budget: f64) -> bool {
        let err = got - want;
        err >= self.lo * want - budget && err <= self.hi * want + budget
    }
}

fn tube(w: u32, h: u32, ppm: f32) -> FrameConstants {
    let mut fc = FrameConstants::new((w, h), (w, h), false);
    assert_eq!(fc.tube_size, [w as i32, h as i32]);
    fc.px_per_mm = ppm;
    fc.line_pitch_mm = 0.0;
    fc
}

fn slot_params(pitch_mm: f32) -> Params {
    Params {
        mask_type: MASK_TYPE_SLOT_MASK,
        mask_pitch_mm: pitch_mm,
        mask_tier: MASK_TIER_INTEGRATED,
        ..Params::default()
    }
}

fn delta_params(pitch_mm: f32) -> Params {
    Params {
        mask_type: MASK_TYPE_DELTA,
        mask_pitch_mm: pitch_mm,
        mask_tier: MASK_TIER_INTEGRATED,
        ..Params::default()
    }
}

/// The shader's lengths of a frame in host px: (pitch p or P, delta cell width X), f32 products.
fn pitches(fc: &FrameConstants, p: &Params) -> (f32, f32) {
    let pp = fc.px_per_mm * p.mask_pitch_mm;
    (pp, pp * SQRT3_F32)
}

/// Host pixels per (horizontal) triad: p for the slot mask, X / 2 = 0.866 P for delta.
fn px_per_triad(fc: &FrameConstants, p: &Params) -> f64 {
    let (pp, cell) = pitches(fc, p);
    if p.mask_type == MASK_TYPE_DELTA {
        f64::from(cell) / 2.0
    } else {
        f64::from(pp)
    }
}

/// The oracle of a frame (module header) and the y scale sigma that maps host rows to its rows.
struct Oracle {
    mask: Mask,
    sigma: f64,
}

impl Oracle {
    fn new(fc: &FrameConstants, p: &Params) -> Self {
        let (pp, cell) = pitches(fc, p);
        let (mask, sigma) = if p.mask_type == MASK_TYPE_DELTA {
            let pitch = f64::from(cell) / 3.0_f64.sqrt();
            (
                Mask::Delta(Delta {
                    pitch_mm: pitch,
                    dot_mm: f64::from(p.mask_dot_fill * pp),
                    edge_mm: f64::from(p.mask_edge_softness * pp),
                }),
                f64::from(pp) / pitch,
            )
        } else {
            (
                Mask::Slot(Slot {
                    pitch_mm: f64::from(pp),
                    stripe_mm: f64::from(p.mask_stripe_fill * pp),
                    edge_mm: f64::from(p.mask_edge_softness * pp),
                    slot_period_mm: f64::from(p.mask_slot_period * pp),
                    bridge_fraction: f64::from(p.mask_slot_bridge),
                }),
                1.0,
            )
        };
        mask.validate().expect("the test tube is a valid mask");
        Self { mask, sigma }
    }

    fn rect(&self, x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect::new(x, y / self.sigma, x + w, (y + h) / self.sigma)
    }

    fn area_mean(&self, c: usize, x: u32, y: u32) -> f64 {
        self.mask.area_mean(c, &self.rect(f64::from(x), f64::from(y), 1.0, 1.0))
    }

    fn image(&self, fc: &FrameConstants, value: impl Fn(usize, u32, u32) -> f64) -> Vec<[f64; 3]> {
        let (w, h) = (fc.tube_size[0] as u32, fc.tube_size[1] as u32);
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| [value(0, x, y), value(1, x, y), value(2, x, y)])
            .collect()
    }

    fn area_image(&self, fc: &FrameConstants) -> Vec<[f64; 3]> {
        self.image(fc, |c, x, y| self.area_mean(c, x, y))
    }
}

/// Budget of the shader's f32 arithmetic (module header).
fn f32_budget(fc: &FrameConstants, p: &Params) -> f64 {
    let (pp, cell) = pitches(fc, p);
    let (pp, cell) = (f64::from(pp), f64::from(cell));
    if p.mask_type == MASK_TYPE_DELTA {
        let r = 0.5 * f64::from(p.mask_dot_fill) * pp;
        let s = f64::from(p.mask_edge_softness) * pp;
        let peak = cell * pp / (2.0 * std::f64::consts::PI * (r * r + s * s / 12.0));
        2e-6 + (4e-7 * cell + 2e-6 * (r * r).max(1.0)) * peak
    } else {
        let peak = 1.0 / f64::from(p.mask_stripe_fill * (1.0 - p.mask_slot_bridge));
        2e-6 + 4e-7 * (2.0 * pp + f64::from(p.mask_slot_period) * pp) * peak
    }
}

fn currents(fc: &FrameConstants, f: impl Fn(u32, u32) -> [f32; 3]) -> ImageF32 {
    let (w, h) = fc.domain_size(Domain::Amp);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x, y);
            rgba.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

fn run(gpu: &mut Headless, slots: &[&str], fc: FrameConstants, params: &Params, amp: &ImageF32) -> ImageF32 {
    gpu.enable_only(slots).unwrap();
    let settings = Settings::identity();
    let run = StageRun {
        input: StageInput::Image {
            after: "gun",
            image: amp,
        },
        frame: fc,
        settings: &settings,
        params,
        format: TargetFormat::Rgba32F,
    };
    let r = gpu.render_stages(&run).expect("render_stages");
    r.stage("screen").expect("screen is part of the frame").clone()
}

fn mask_image(gpu: &mut Headless, fc: FrameConstants, params: &Params, scale: f32) -> Vec<[f64; 3]> {
    let amp = currents(&fc, |_, _| [scale; 3]);
    let out = run(gpu, &["screen"], fc, params, &amp);
    (0..out.height)
        .flat_map(|y| (0..out.width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let [r, g, b, _] = out.pixel(x, y);
            [r, g, b].map(|v| f64::from(v) / f64::from(scale))
        })
        .collect()
}

/// Compares every pixel and channel of the rendered mask, for every current of [`SCALES`], with `want`.
/// Returns the image of the first current.
fn assert_matches(
    gpu: &mut Headless,
    what: &str,
    fc: FrameConstants,
    params: &Params,
    want: &[[f64; 3]],
) -> Vec<[f64; 3]> {
    let stored = Stored::detect(gpu);
    let budget = f32_budget(&fc, params);
    let width = fc.tube_size[0] as u32;
    let mut first = Vec::new();
    let mut line = format!("{what}:");
    for scale in SCALES {
        let got = mask_image(gpu, fc, params, scale);
        assert_eq!(got.len(), want.len());
        let mut worst = 0.0_f64;
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            for c in 0..3 {
                assert!(g[c] >= 0.0, "{what}: negative light at pixel {i}");
                assert!(
                    stored.holds(g[c], w[c], budget),
                    "{what} current {scale}: pixel ({}, {}) channel {c}: GPU {} vs oracle {} (error {:.3e}, budget {budget:.1e})",
                    i as u32 % width,
                    i as u32 / width,
                    g[c],
                    w[c],
                    g[c] - w[c]
                );
                worst = worst.max((g[c] - w[c]).abs());
            }
        }
        line.push_str(&format!(" [{scale}: max {worst:.1e}]"));
        if first.is_empty() {
            first = got;
        }
    }
    println!("{line} f32 budget {budget:.1e}");
    first
}

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

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

fn ripple(v: &[f64]) -> f64 {
    let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
    (hi - lo) / (hi + lo)
}

/// Mean of channel c over the window xs x ys.
fn window_mean(img: &[[f64; 3]], width: u32, c: usize, xs: Range<u32>, ys: Range<u32>) -> f64 {
    let v: Vec<f64> = ys
        .flat_map(|y| xs.clone().map(move |x| (x, y)))
        .map(|(x, y)| img[(y * width + x) as usize][c])
        .collect();
    mean(&v)
}

/// Period in samples of the strongest non-constant sinusoid of `v` (periodogram maximum, refined).
fn dominant_period(v: &[f64]) -> f64 {
    let m = mean(v);
    let power = |f: f64| {
        let (mut re, mut im) = (0.0, 0.0);
        for (x, s) in v.iter().enumerate() {
            let a = std::f64::consts::TAU * f * x as f64;
            re += (s - m) * a.cos();
            im += (s - m) * a.sin();
        }
        re * re + im * im
    };
    let step = 1.0 / (4.0 * v.len() as f64);
    let steps = (0.5 / step).round() as usize;
    let (mut best, mut best_power) = (step, 0.0);
    for k in 4..=steps {
        let f = k as f64 * step;
        let p = power(f);
        if p > best_power {
            (best, best_power) = (f, p);
        }
    }
    let (mut lo, mut hi) = (best - step, best + step);
    for _ in 0..40 {
        let (a, b) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
        if power(a) < power(b) {
            lo = a;
        } else {
            hi = b;
        }
    }
    2.0 / (lo + hi)
}

/// Random tubes for both types: 0.3 to 24 px per triad (log-uniform), 2 to 40 px per mm, random shape
/// parameters inside the oracle's validity limits. Every host pixel (a random rectangle of the mask plane:
/// the phases differ for every pixel and case) equals the oracle's `area_mean` for every beam current.
#[test]
fn host_pixels_equal_the_oracle_area_mean_on_random_tubes() {
    let test = "stage_mask_slot_delta random";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let mut rng = Lcg(392);
    for case in 0..20 {
        let delta = case % 2 == 1;
        let (w, h) = if delta { (192, 24) } else { (384, 40) };
        let ppt = 0.3 * 80.0_f64.powf(rng.next());
        let ppm = 2.0 + 38.0 * rng.next();
        let fc = tube(w, h, ppm as f32);
        let p = if delta {
            let fill = 0.40 + 0.10 * rng.next();
            let soft = 0.999 * rng.next() * (1.0 / 3.0_f64.sqrt() - fill).min(0.10);
            Params {
                mask_dot_fill: fill as f32,
                mask_edge_softness: soft as f32,
                ..delta_params((ppt / (ppm * 0.75_f64.sqrt())) as f32)
            }
        } else {
            let fill = 0.20 + 0.07 * rng.next();
            let period = 0.6667 + 0.9333 * rng.next();
            let bridge = 0.10 + 0.10 * rng.next();
            let soft = 0.999 * rng.next() * (1.0 / 3.0 - fill).min(0.10).min(bridge * period);
            Params {
                mask_stripe_fill: fill as f32,
                mask_edge_softness: soft as f32,
                mask_slot_period: period as f32,
                mask_slot_bridge: bridge as f32,
                ..slot_params((ppt / ppm) as f32)
            }
        };
        let oracle = Oracle::new(&fc, &p);
        let want = oracle.area_image(&fc);
        let what = format!(
            "{test} case {case} ({}, {:.3} px per triad)",
            if delta { "delta" } else { "slot" },
            px_per_triad(&fc, &p)
        );
        assert_matches(&mut gpu, &what, fc, &p, &want);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The plan's densities (E1) for both types, default shapes, area integral forced: every pixel against the
/// oracle; flat-white mean 1 per channel over whole periods (E3, plan: +/- 0.5 %); at 1.0 px per triad and
/// above 3 the same. Slot windows of whole 2 p and V / 2; delta windows of whole cells over 64 rows.
#[test]
fn plan_densities_and_flat_white() {
    let test = "stage_mask_slot_delta densities";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    // Slot: (name, px per mm, pitch, hand px per triad, slot period, window px, window rows).
    let slots: [(&str, f32, f32, f64, f32, u32, u32); 7] = [
        ("1.54", 4.0, 0.385, 1.54, 2.0 / 1.54, 77, 12),
        ("2.9", 4.0, 0.725, 2.9, 2.0 / 2.9, 58, 12),
        ("26 in TV", 1920.0 / 528.0, 0.8, 32.0 / 11.0, 0.6875, 64, 12),
        ("1.0", 4.0, 0.25, 1.0, 1.0, 2, 12),
        ("3.0", 4.0, 0.75, 3.0, 2.0 / 3.0, 6, 12),
        ("4.4", 4.0, 1.1, 4.4, 4.0 / 4.4, 44, 12),
        ("8", 4.0, 2.0, 8.0, 0.75, 16, 12),
    ];
    let (w, h) = (1024_u32, 12_u32);
    for (name, ppm, pitch, hand, period, wx, wy) in slots {
        let fc = tube(w, h, ppm);
        let p = Params {
            mask_slot_period: period,
            ..slot_params(pitch)
        };
        let ppt = px_per_triad(&fc, &p);
        assert!((ppt / hand - 1.0).abs() < 1e-6, "slot {name}: {ppt}");
        let oracle = Oracle::new(&fc, &p);
        let what = format!("{test} slot {name}");
        let got = assert_matches(&mut gpu, &what, fc, &p, &oracle.area_image(&fc));
        let slack = FP16_STEP + f32_budget(&fc, &p);
        for c in 0..3 {
            let blocks = (512 / wx) * wx;
            for xs in [0..blocks, w - blocks..w] {
                let m = window_mean(&got, w, c, xs.clone(), 0..wy);
                println!("{what}: channel {c} flat-white mean {m:.6} over columns {xs:?}");
                assert!((m - 1.0).abs() <= slack, "{what}: channel {c} mean {m}");
            }
        }
    }
    // Delta: (name, px per mm, hand px per triad, whole-cell window px). P = 0.28 mm.
    let deltas: [(&str, f32, f64, u32); 6] = [
        ("1.54", 6.350_853, 1.54, 77),
        ("2.9", 11.959_398, 2.9, 58),
        ("1.0", 4.123_93, 1.0, 2),
        ("3.0", 12.371_791, 3.0, 6),
        ("4.4", 18.145_294, 4.4, 44),
        ("8", 32.991_444, 8.0, 16),
    ];
    let (w, h) = (264_u32, 64_u32);
    for (name, ppm, hand, wx) in deltas {
        let fc = tube(w, h, ppm);
        let p = delta_params(0.28);
        let ppt = px_per_triad(&fc, &p);
        assert!((ppt / hand - 1.0).abs() < 2e-6, "delta {name}: {ppt}");
        let oracle = Oracle::new(&fc, &p);
        let what = format!("{test} delta {name}");
        let got = assert_matches(&mut gpu, &what, fc, &p, &oracle.area_image(&fc));
        let slack = FP16_STEP + f32_budget(&fc, &p);
        let blocks = (w / wx) * wx;
        // Rows: the window of 32 to 64 rows nearest a whole number of row spacings P / 2 (E3).
        let half = f64::from(pitches(&fc, &p).0) / 2.0;
        let rows = (32..=h)
            .min_by(|a, b| {
                let off = |n: u32| {
                    let k = f64::from(n) / half;
                    (k - k.round()).abs() * half / f64::from(n)
                };
                off(*a).total_cmp(&off(*b))
            })
            .unwrap();
        for c in 0..3 {
            let m = window_mean(&got, w, c, 0..blocks, 0..rows);
            let window = oracle
                .mask
                .area_mean(c, &oracle.rect(0.0, 0.0, f64::from(blocks), f64::from(rows)));
            println!("{what}: channel {c} flat-white mean {m:.6} (oracle window {window:.6})");
            assert!((m - window).abs() <= slack, "{what}: channel {c} mean {m} vs {window}");
            assert!((m - 1.0).abs() <= 0.005, "{what}: channel {c} mean {m}");
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// E4: slot elements at 16 px per triad, hard edges: the hand table of red, green and blue columns of an
/// even and an odd triad column over two slot periods, the stagger V / 2 = 6 rows, peak 4.8.
#[test]
fn slot_positions_per_channel() {
    let test = "stage_mask_slot_delta slot positions";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (64_u32, 24_u32);
    let fc = tube(w, h, 32.0);
    let p = Params {
        mask_edge_softness: 0.0,
        mask_slot_period: 0.75,
        mask_slot_bridge: 1.0 / 6.0,
        ..slot_params(0.5)
    };
    assert_eq!(px_per_triad(&fc, &p), 16.0);
    let peak = 4.8;
    // Value of a pixel by hand: stripe overlap (in px) of colour c in pixel x of its column, times the
    // bridge factor of the row in that column's phase.
    let stripe = |c: usize, x: u32| -> f64 {
        let (lo, hi) = [(2.0 / 3.0, 14.0 / 3.0), (6.0, 10.0), (34.0 / 3.0, 46.0 / 3.0)][c];
        let u = f64::from(x % 16);
        ((u + 1.0).min(hi) - u.max(lo)).max(0.0)
    };
    let lit = |x: u32, y: u32| -> bool {
        let r = y % 12;
        if (x / 16).is_multiple_of(2) {
            r != 0 && r != 11
        } else {
            r != 5 && r != 6
        }
    };
    let want: Vec<[f64; 3]> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| [0, 1, 2].map(|c| if lit(x, y) { stripe(c, x) * peak } else { 0.0 }))
        .collect();
    // The oracle agrees with the hand table (the hand table is the check of the oracle here).
    let oracle = Oracle::new(&fc, &p);
    for (i, px) in oracle.area_image(&fc).iter().enumerate() {
        for c in 0..3 {
            // 1e-6: the bridge fraction 1/6 is an f32 (2 px + 6e-8).
            assert!(
                (px[c] - want[i][c]).abs() < 1e-6,
                "oracle vs hand at {i} c{c}: {} vs {}",
                px[c],
                want[i][c]
            );
        }
    }
    let got = assert_matches(&mut gpu, test, fc, &p, &want);
    let at = |x: u32, y: u32| got[(y * w + x) as usize];
    // Red inside its stripe: even column dark on rows 0 and 11, odd column on rows 5 and 6 (dark: below 1e-3,
    // the f32 bridge 1/6 is 2 px + 6e-8 and leaves a few 1e-7 of light at the edges of a dark row).
    let even: Vec<u32> = (0..12).filter(|&y| at(2, y)[0] < 1e-3).collect();
    let odd: Vec<u32> = (0..12).filter(|&y| at(18, y)[0] < 1e-3).collect();
    println!("{test}: dark rows, even column {even:?}, odd column {odd:?}");
    assert_eq!(even, vec![0, 11]);
    assert_eq!(odd, vec![5, 6]);
    // Order R, G, B along a lit row of each column.
    for start in [0_u32, 16] {
        let centroid = |c: usize| {
            let v: Vec<f64> = (start..start + 16).map(|x| at(x, 3)[c]).collect();
            v.iter().enumerate().map(|(i, s)| (i as f64 + 0.5) * s).sum::<f64>() / v.iter().sum::<f64>()
        };
        let (r, g, b) = (centroid(0), centroid(1), centroid(2));
        println!("{test}: column at {start}: centroids R {r:.4} G {g:.4} B {b:.4}");
        for (got, want) in [(r, 8.0 / 3.0), (g, 8.0), (b, 40.0 / 3.0)] {
            assert!((got - want).abs() <= 0.002);
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Weighted centroids (x, y) in px of the 4-connected lit regions of channel c that do not touch the image
/// border.
fn blobs(img: &[[f64; 3]], (w, h): (u32, u32), c: usize) -> Vec<(f64, f64)> {
    let mut seen = vec![false; img.len()];
    let mut out = Vec::new();
    for start in 0..img.len() {
        if seen[start] || img[start][c] <= 1e-3 {
            continue;
        }
        let (mut stack, mut border, mut sx, mut sy, mut sw) = (vec![start], false, 0.0, 0.0, 0.0);
        seen[start] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let v = img[i][c];
            sx += (f64::from(x) + 0.5) * v;
            sy += (f64::from(y) + 0.5) * v;
            sw += v;
            border |= x == 0 || y == 0 || x == w - 1 || y == h - 1;
            let mut push = |nx: u32, ny: u32| {
                let j = (ny * w + nx) as usize;
                if !seen[j] && img[j][c] > 1e-3 {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(x - 1, y);
            }
            if x + 1 < w {
                push(x + 1, y);
            }
            if y > 0 {
                push(x, y - 1);
            }
            if y + 1 < h {
                push(x, y + 1);
            }
        }
        if !border {
            out.push((sx / sw, sy / sw));
        }
    }
    out
}

/// E5: delta dot positions per channel at P = 0.28 mm, 100 px per mm, hard edges: the CRT.md table (0.485,
/// 0.242, 0.140, 0.162 mm), colour order R G B along a row, same colour in the next row at P and 30 degrees.
#[test]
fn delta_dot_positions_and_distances() {
    let test = "stage_mask_slot_delta delta positions";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (200_u32, 90_u32);
    let ppm = 100.0;
    let fc = tube(w, h, ppm);
    let p = Params {
        mask_edge_softness: 0.0,
        ..delta_params(0.28)
    };
    let got = mask_image(&mut gpu, fc, &p, 1.0);
    let mm = |v: f64| v / f64::from(ppm);
    let dots: Vec<Vec<(f64, f64)>> = (0..3).map(|c| blobs(&got, (w, h), c)).collect();
    for (c, d) in dots.iter().enumerate() {
        println!("{test}: channel {c}: {} whole dots", d.len());
        assert!(d.len() >= 4, "channel {c}: too few dots");
    }
    let same_row = |a: &(f64, f64), b: &(f64, f64)| (a.1 - b.1).abs() < 0.5;
    // Same colour along a row, and between adjacent rows.
    for (c, d) in dots.iter().enumerate() {
        let mut along = f64::MAX;
        let mut next_row = (f64::MAX, 0.0, 0.0);
        for a in d {
            for b in d {
                if b.0 <= a.0 && same_row(a, b) {
                    continue;
                }
                if same_row(a, b) {
                    along = along.min(b.0 - a.0);
                } else if b.1 > a.1 + 0.5 {
                    let dist = (b.0 - a.0).hypot(b.1 - a.1);
                    if dist < next_row.0 {
                        next_row = (dist, (b.0 - a.0).abs(), b.1 - a.1);
                    }
                }
            }
        }
        let (dist, dx, dy) = next_row;
        let angle = dy.atan2(dx).to_degrees();
        println!(
            "{test}: channel {c}: along a row {:.4} mm, next row dx {:.4} dy {:.4} distance {:.4} mm at {angle:.2} deg",
            mm(along),
            mm(dx),
            mm(dy),
            mm(dist)
        );
        assert!((mm(along) - 0.485).abs() < 0.0006);
        assert!((mm(dx) - 0.242).abs() < 0.0006);
        assert!((mm(dy) - 0.140).abs() < 0.0006);
        assert!((mm(dist) - 0.28).abs() < 0.0006);
        assert!((angle - 30.0).abs() < 0.2);
    }
    // Any colour in a row: R G B left to right, spacing 0.162 mm. Row of y = 0.28 mm (row j = 2), R at x =
    // 3 a = 0.485 mm.
    let pick = |c: usize, x: f64, y: f64| {
        *dots[c]
            .iter()
            .min_by(|a, b| (a.0 - x).hypot(a.1 - y).total_cmp(&(b.0 - x).hypot(b.1 - y)))
            .unwrap()
    };
    let (r, g, b) = (pick(0, 48.5, 28.0), pick(1, 64.7, 28.0), pick(2, 80.8, 28.0));
    println!("{test}: row at 0.28 mm: R {r:?} G {g:?} B {b:?} px");
    assert!(same_row(&r, &g) && same_row(&g, &b));
    assert!((mm(g.0 - r.0) - 0.162).abs() < 0.0006 && (mm(b.0 - g.0) - 0.162).abs() < 0.0006);
    assert!((r.0 - 48.497).abs() < 0.05 && (r.1 - 28.0).abs() < 0.05);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// E6: zoomed tube at 16 px per triad (plan: slot vertical period +/- 1 %, delta row spacing P / 2 +/- 1 %).
#[test]
fn zoomed_vertical_periods() {
    let test = "stage_mask_slot_delta vertical periods";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (64_u32, 480_u32);
    // Slot, defaults: V = 0.8333 x 16 = 13.333 px.
    let fc = tube(w, h, 32.0);
    let p = slot_params(0.5);
    let got = mask_image(&mut gpu, fc, &p, 1.0);
    let v = f64::from(p.mask_slot_period) * 16.0;
    let column: Vec<f64> = (0..h).map(|y| got[(y * w + 8) as usize][1]).collect();
    let rows: Vec<f64> = (0..h).map(|y| window_mean(&got, w, 1, 0..32, y..y + 1)).collect();
    let (pc, pr) = (dominant_period(&column), dominant_period(&rows));
    println!("{test}: slot green column period {pc:.4} px (V {v:.4}), row-mean period {pr:.4} px (V / 2)");
    assert!((pc / v - 1.0).abs() <= 0.01);
    assert!((pr / (v / 2.0) - 1.0).abs() <= 0.01);
    // Delta: 16 px per triad, X = 32 px, P = 18.475 px.
    let ppm = 16.0 / (0.28 * 0.75_f32.sqrt());
    let fc = tube(w, h, ppm);
    let p = delta_params(0.28);
    let (pp, cell) = pitches(&fc, &p);
    assert!((f64::from(cell) - 32.0).abs() < 1e-4);
    let got = mask_image(&mut gpu, fc, &p, 1.0);
    let big_p = f64::from(pp);
    for c in 0..3 {
        let rows: Vec<f64> = (0..h).map(|y| window_mean(&got, w, c, 0..32, y..y + 1)).collect();
        let x = [0, 10, 21][c];
        let column: Vec<f64> = (0..h).map(|y| got[(y * w + x) as usize][c]).collect();
        let (pr, pc) = (dominant_period(&rows), dominant_period(&column));
        println!(
            "{test}: delta channel {c}: row-mean period {pr:.4} px (P / 2 = {:.4}), column period {pc:.4} px (P)",
            big_p / 2.0
        );
        assert!((pr / (big_p / 2.0) - 1.0).abs() <= 0.01);
        assert!((pc / big_p - 1.0).abs() <= 0.01);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// E7: the period of the x profile (mean over 64 rows) of a rendered flat field equals the oracle's alias
/// period of the triad frequency on the host grid, and the hand values.
#[test]
fn moire_period_of_a_rendered_flat_field() {
    let test = "stage_mask_slot_delta moire";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (960_u32, 64_u32);
    for (name, ppt_hand, period_hand, tolerance) in [
        ("1.54", 1.54, 2.851_85, 0.002),
        ("2.9", 2.9, 2.9, 0.002),
        ("16", 16.0, 16.0, 0.01),
    ] {
        for delta in [false, true] {
            let (fc, p) = if delta {
                let ppm = (ppt_hand / (0.28 * 0.75_f64.sqrt())) as f32;
                (tube(w, h, ppm), delta_params(0.28))
            } else {
                (tube(w, h, 4.0), slot_params((ppt_hand / 4.0) as f32))
            };
            let ppt = px_per_triad(&fc, &p);
            let alias = alias_period_px(ppt, 1);
            assert!(
                (alias / period_hand - 1.0).abs() < 1e-4,
                "{name}: oracle {alias} vs hand {period_hand}"
            );
            let got = mask_image(&mut gpu, fc, &p, 1.0);
            for c in 0..3 {
                let profile: Vec<f64> = (0..w).map(|x| window_mean(&got, w, c, x..x + 1, 0..h)).collect();
                let period = dominant_period(&profile);
                let kind = if delta { "delta" } else { "slot" };
                println!("{test} {kind} {name}: channel {c} period {period:.4} px (oracle {alias:.4})");
                assert!(
                    (period / alias - 1.0).abs() <= tolerance,
                    "{kind} {name} c{c}: {period} vs {alias}"
                );
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// E8: tier C for both types. Default tier below 3 px per triad: every value exactly 1 (the oracle's
/// `host_value`), ripple 0 < 1.4 %, damper wires (grille only) absent; at 3.0 px per triad both show
/// structure at once. The area integral forced at 2.9 shows host-grid structure above 1.4 % (not vacuous).
#[test]
fn tier_c_slot_and_delta() {
    let test = "stage_mask_slot_delta tiers";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (240_u32, 24_u32);
    let ones = vec![[1.0_f64; 3]; (w * h) as usize];
    for (ppt_hand, expect) in [
        (1.0, Tier::C),
        (1.54, Tier::C),
        (2.9, Tier::C),
        (3.0, Tier::B),
        (4.4, Tier::B),
    ] {
        for delta in [false, true] {
            let (fc, base) = if delta {
                // 3.0 exactly is not representable through P sqrt(3) / 2: aim just above it.
                let ppm = (ppt_hand * 1.000_01 / (0.28 * 0.75_f64.sqrt())) as f32;
                (tube(w, h, ppm), delta_params(0.28))
            } else {
                (tube(w, h, 4.0), slot_params((ppt_hand / 4.0) as f32))
            };
            let p = Params {
                mask_tier: MASK_TIER_AUTO,
                mask_damper_count: 2,
                tube_visible_height_mm: h as f32 / fc.px_per_mm,
                ..base
            };
            let ppt = px_per_triad(&fc, &p);
            assert_eq!(tier(ppt), expect, "{ppt}");
            let oracle = Oracle::new(&fc, &p);
            let kind = if delta { "delta" } else { "slot" };
            let what = format!("{test} {kind} auto at {ppt:.4} px per triad");
            let rect = |x: u32, y: u32| oracle.rect(f64::from(x), f64::from(y), 1.0, 1.0);
            let want = oracle.image(&fc, |c, x, y| oracle.mask.host_value(c, &rect(x, y), ppt));
            let got = assert_matches(&mut gpu, &what, fc, &p, &want);
            let r = ripple(&got.iter().map(|px| px[1]).collect::<Vec<_>>());
            println!("{what}: tier {expect:?}, green ripple {r:.4}");
            if expect == Tier::C {
                assert_eq!(got, ones, "{what}: tier C is exactly 1");
                assert!(r < BARTEN_THRESHOLD);
            } else {
                assert!(r > 0.5, "{what}: the resolved mask has structure");
            }
        }
    }
    for delta in [false, true] {
        let (fc, p) = if delta {
            (tube(w, h, 11.959_398), delta_params(0.28))
        } else {
            (tube(w, h, 4.0), slot_params(0.725))
        };
        let got = mask_image(&mut gpu, fc, &p, 1.0);
        let r = ripple(&got.iter().map(|px| px[1]).collect::<Vec<_>>());
        println!(
            "{test} integrated at 2.9 ({}): green ripple {r:.4}",
            if delta { "delta" } else { "slot" }
        );
        assert!(r > 10.0 * BARTEN_THRESHOLD);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

fn amp_col(fc: &FrameConstants, x: u32) -> u32 {
    let (aw, _) = fc.domain_size(Domain::Amp);
    (((f64::from(x) + 0.5) * f64::from(aw) / f64::from(fc.tube_size[0] as u32)).floor() as u32).min(aw - 1)
}

/// Bypass and the type switch. For both types: mask off, tube not placed and `screen` off give the beam
/// image bit for bit. The three types give three different images (no fallback to the grille any more), and
/// switching type and back leaves the grille image bit for bit unchanged and equal to its own oracle path
/// (`stage_mask_grille.rs` keeps its expected numbers).
#[test]
fn bypass_and_type_switch() {
    let test = "stage_mask_slot_delta bypass";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (96_u32, 30_u32);
    let fc = tube(w, h, 4.0);
    let value = |x: u32, y: u32| [1, 2, 3].map(|k| (16 + (x * k + y * 5 + k) % 97) as f32 / 64.0);
    let amp = currents(&fc, value);
    let beam: Vec<f32> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let [r, g, b] = value(amp_col(&fc, x), y);
            [r, g, b, 1.0]
        })
        .collect();
    let grille = Params {
        mask_type: MASK_TYPE_APERTURE_GRILLE,
        ..slot_params(2.0)
    };
    let first = run(&mut gpu, &["screen"], fc, &grille, &amp);
    let mut images = vec![first.rgba.clone()];
    for on in [slot_params(2.0), delta_params(2.0 / 0.75_f32.sqrt())] {
        let masked = run(&mut gpu, &["screen"], fc, &on, &amp);
        assert_ne!(masked.rgba, beam, "the mask is visible");
        images.push(masked.rgba.clone());
        let off = Params {
            mask_enabled: false,
            ..on
        };
        assert_eq!(
            run(&mut gpu, &["screen"], fc, &off, &amp).rgba,
            beam,
            "mask_enabled = false"
        );
        let mut unplaced = fc;
        unplaced.px_per_mm = 0.0;
        assert_eq!(
            run(&mut gpu, &["screen"], unplaced, &on, &amp).rgba,
            beam,
            "px per mm 0"
        );
        assert_eq!(run(&mut gpu, &[], fc, &on, &amp).rgba, beam, "screen slot off");
    }
    assert_ne!(images[0], images[1], "slot mask drawn as the grille");
    assert_ne!(images[0], images[2], "delta drawn as the grille");
    assert_ne!(images[1], images[2]);
    let again = run(&mut gpu, &["screen"], fc, &grille, &amp);
    assert_eq!(again.rgba, first.rgba, "grille changed after a type switch");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Least-squares displacement, in host px, of the rendered pattern against the oracle's over the samples
/// `ks` (pixel index `pixel(k)`, oracle value `at(c, k, d)` of the pixel moved by d): a pattern moved by +d
/// reads want(k - d) = want(k) - d x slope. Averages the FP16 noise, so it resolves a phase drift far below
/// one FP16 step. `None` without structure.
fn displacement(
    got: &[[f64; 3]],
    ks: Range<u32>,
    pixel: impl Fn(u32) -> usize,
    at: impl Fn(usize, u32, f64) -> f64,
) -> Option<f64> {
    let h = 1e-3;
    let (mut num, mut den) = (0.0, 0.0);
    for k in ks {
        for (c, value) in got[pixel(k)].iter().enumerate() {
            let slope = (at(c, k, h) - at(c, k, -h)) / (2.0 * h);
            num -= (value - at(c, k, 0.0)) * slope;
            den += slope * slope;
        }
    }
    (den > 1.0).then(|| num / den)
}

/// The coordinate reductions do not drift: on a 4096 px row and a 4096 px column, 8 px per triad, both
/// types, the rendered pattern in the last 512 px is where the oracle puts it within 1e-4 px (a phase error
/// of 1e-4 triad changes a pixel by about 0.1 %, PLAN T-39a).
#[test]
fn no_phase_drift_far_from_the_origin() {
    let test = "stage_mask_slot_delta drift";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let n = 4096_u32;
    for delta in [false, true] {
        let kind = if delta { "delta" } else { "slot" };
        for horizontal in [true, false] {
            let (w, h) = if horizontal { (n, 2) } else { (2, n) };
            let (fc, p) = if delta {
                (tube(w, h, (8.0 / (0.28 * 0.75_f64.sqrt())) as f32), delta_params(0.28))
            } else {
                (tube(w, h, 4.0), slot_params(2.0))
            };
            let oracle = Oracle::new(&fc, &p);
            let got = mask_image(&mut gpu, fc, &p, 1.0);
            let d = if horizontal {
                displacement(
                    &got,
                    n - 512..n,
                    |k| k as usize,
                    |c, k, d| oracle.mask.area_mean(c, &oracle.rect(f64::from(k) + d, 0.0, 1.0, 1.0)),
                )
            } else {
                displacement(
                    &got,
                    n - 512..n,
                    |k| (k * 2) as usize,
                    |c, k, d| oracle.mask.area_mean(c, &oracle.rect(0.0, f64::from(k) + d, 1.0, 1.0)),
                )
            }
            .expect("structure to fit");
            let axis = if horizontal { "x" } else { "y" };
            println!("{test} {kind}: displaced by {d:+.2e} px along {axis} in the last 512 px");
            assert!(d.abs() <= 1e-4, "{test} {kind} {axis}: drift {d} px");
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}
