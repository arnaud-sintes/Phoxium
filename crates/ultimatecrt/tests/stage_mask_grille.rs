//! GPU tests of the aperture-grille mask (T-39a, `mask_eval` in `shaders/include/mask.glsl`, parameters
//! `params/mask.toml`), headless, validation on, zero validation messages.
//!
//! Every test injects its own `FrameConstants` and a beam-current image after `gun`, runs `screen` alone
//! and compares the tube image with the `crt-model` f64 oracle (`crt_model::mask`, T-38) and with numbers
//! derived by hand from `CRT.md` (below). Nothing here repeats the shader's formula.
//!
//! Set-up. The beam is taken out of the picture by leaving the line pitch at 0 while px per mm is set:
//! `beam_eval` then returns the amp sample under the pixel (its documented fallback, ARCHITECTURE 3.7), so
//! with a uniform current I the tube image is I x mask. The oracle works in host pixels: pitch = px per mm
//! x pitch in mm (the f32 product, which is the pitch the shader uses), stripe = fill x pitch, edge =
//! softness x pitch, host pixel (x, y) = the rectangle [x - c, x + 1 - c] x [y, y + 1], c = half the tube
//! width: the grille's origin (a triad start) is the tube centre (T-37; it was the left edge of the tube
//! rectangle before), y from the top of the tube rectangle; damper wires at the oracle's fractions of the visible height (`damper_positions_mm`)
//! minus the rows of the visible area above the tube rectangle, (visible height - rectangle height) / 2
//! (the picture is centred in the visible area, T-30).
//!
//! Tolerance. Chain images are FP16 (11 significant bits, one step = 2^-10 of the value at most). How a
//! float is rounded when a stage stores it was left to the device (the dev machine truncated); since
//! T-fp16 every stage stores `round_f16(value)`, to nearest on any device, and [`Stored::detect`] asserts
//! it: stored / computed - 1 lies in [-2^-11, 2^-11] (it was [-2^-10, 0] on the dev machine). On top of that the shader's own f32 arithmetic gets a budget
//! ([`f32_budget`]): the ends of a pixel inside its triad are known to about 2e-7 triad (a few f32 ulp of
//! 1), which moves an area mean by at most that over (pixel width x fill), 4e-7 x px per triad / fill; a
//! damper wire is placed with the rounding of its row coordinate, 2^-22 of the visible height in px,
//! times the stripe peak 1 / fill; plus 2e-6. Each oracle comparison is repeated with several beam
//! currents, so that the FP16 grid falls differently each time and the FP16 window cannot hide
//! an error of its own size; the plan's acceptance (+/- 0.5 %) is five times wider than one FP16 step.
//!
//! Hand derivations (CRT.md sections in brackets; an independent model re-derives them):
//!
//! D1. Host pixels per triad [Visible size and triad counts; Host-display resolution budget].
//!     PVM-20M4U, 290 mm visible height on 1440 rows: 1440 / 290 = 4.965517 px per mm; pitch 0.31 mm:
//!     4.965517 x 0.31 = 1.539310 px per triad. 26 in TV, 528 mm wide on 1920 px: 1920 / 528 = 3.636364
//!     px per mm; pitch 0.80 mm: 2.909091 = 32 / 11 px per triad.
//! D2. Host-grid moire [Moire: beat of two pitches p1 p2 / |p1 - p2|]. Pixel pitch 1 px against the triad
//!     pitch t px: t / |t - 1|. PVM: 1.539310 / 0.539310 = 2.85422 px (the T-16 early look measured 2.85).
//!     At exactly 1.54: 1.54 / 0.54 = 2.85185 px. Equivalently the triad frequency 1 / t = 0.649642
//!     cycles per px is above the Nyquist limit 0.5 and aliases to 1 - 0.649642 = 0.350358, period 2.85422.
//!     TV at 2.909091 px per triad: 1 / t = 0.34375 is below 0.5, no alias: the rendered period is the
//!     triad itself, 2.909 px (T-16 measured 2.91). Zoomed tube at 16 px per triad: period 16 px.
//! D3. Flat white [Beam transmission; crt-model conventions]. The pixel value is the area mean of the
//!     emitter divided by the coverage, and the coverage of a grille colour is stripe / pitch, so the
//!     mean over a whole number of triads is exactly 1 per channel. Whole numbers: 77 px = 50 triads at
//!     1.54, 32 px = 11 triads at 32 / 11, 3 px at 3.0, 22 px = 5 triads at 4.4, 8 px at 8, 16 px at 16.
//!     Over 4096 px at 1.539310 (2660.9 triads, not whole) the incomplete triad moves the mean by at
//!     most one triad in 2661: 3.8e-4.
//! D4. One px per triad: every host pixel holds one whole period, so every value is 1 (no structure).
//! D5. Three px per triad, fill 0.25, softness 0.07 [Aperture grille geometry; stripe centres (c + 1/2)
//!     p / 3]. In triad units pixel k is [k / 3, (k + 1) / 3]. The red stripe with its ramps spans
//!     1/6 +/- (0.125 + 0.035) = [0.00667, 0.32667], inside pixel 0; green and blue likewise inside
//!     pixels 1 and 2. A pixel that holds a whole stripe reads 0.25 / (1/3 x 0.25) = 3, the others 0:
//!     columns repeat (3, 0, 0), (0, 3, 0), (0, 0, 3). This is the hard jump at 3.0: just below, tier C
//!     shows 1 everywhere.
//! D6. Sixteen px per triad, fill 0.25, hard edges. Stripes are 4 px wide, centred at 2.6667, 8 and
//!     13.3333 px: red [0.6667, 4.6667], green [6, 10], blue [11.3333, 15.3333]. A pixel fully inside a
//!     stripe reads 1 / 0.25 = 4; partially, overlap x 4. Red: pixel 0 has 0.3333 -> 1.3333, pixels 1 to
//!     3 -> 4, pixel 4 has 0.6667 -> 2.6667. Green: pixels 6 to 9 -> 4. Blue: pixel 11 has 0.6667 ->
//!     2.6667, pixels 12 to 14 -> 4, pixel 15 has 0.3333 -> 1.3333. Everything else 0. Centroids over
//!     pixel centres: red (0.5 x 1.3333 + (1.5 + 2.5 + 3.5) x 4 + 4.5 x 2.6667) / 16 = 2.6667, green 8,
//!     blue 13.3333: order R, G, B, spaced p / 3 = 5.3333 px.
//! D7. Damper wires [Aperture grille geometry: 10 to 30 um, at 2/3 of the height for one wire, 1/3 and 2/3
//!     for two]. At 5 px per mm a 0.02 mm shadow is 0.1 px: a row that holds the whole shadow loses 10 %
//!     (value 0.9), in every channel and in every tier; a shadow centred on a row boundary takes 5 % from
//!     each of the two rows (it is symmetric); other rows are untouched. Positions: 301 rows = 60.2 mm,
//!     two wires at 100.333 and 200.667 px, rows 100 and 200; 300 rows = 60 mm, one wire at 200.0 px, rows
//!     199 and 200 at 0.95; three wires [E] on 302 rows at 75.5, 151.0 and 226.5 px: rows 75 and 226 at
//!     0.9, rows 150 and 151 at 0.95. Visible height 90.3 mm (451.5 px) around a 300-row rectangle: 75.75
//!     rows above it, two wires at 150.5 - 75.75 = 74.75 and 301.0 - 75.75 = 225.25, rows 74 and 225.
//! D8. Parameter limits [crt-model `validate`: stripe + edge <= p / 3]. Fill 0.27 with softness 0.10 is
//!     drawn with softness 1/3 - 0.27 = 0.0633.
//! D9. Tier C [Host-display resolution budget; Moire: threshold 1.4 %]. Below 3 px per triad the mask is
//!     averaged over one full period: every value is 1 (flat-field ripple 0, under 1.4 %), while the
//!     area integral at the same density has a ripple far above 1.4 % (the moire of D2). T-39c: with the
//!     default blend band (`mask_tier_blend` 0.4) this holds below 2.8; from 2.8 to 3.2 the footprint is a
//!     box of (1 - u) t + u px (u = (t - 2.8) / 0.4), the oracle's `host_value` under `mask_tiers`; with
//!     blend 0 the switch is hard at 3.0 as before. The grille has no vertical period (always integrated).
//! D10. Origin at the tube centre (T-37). The hand tables D5 and D6 count pixels from a triad start: their
//!     tubes put one on the left edge as well, since half their width is a whole number of triads (D5: 240 px
//!     wide at 3 px per triad, 120 = 40 triads; D6: 4096 px at 16, 2048 = 128 triads). PAR: the default
//!     placement of 256 x 240 on 2560 x 1440 at full height (1440 / 290 = 4.9655 px per mm either way) is
//!     1536 px wide at PAR 1 and 1920 px at PAR 1.25; the centres are columns 768 and 960 (whole pixels), so
//!     column 768 + d of the first and 960 + d of the second are the same place on the mask: equal values.
//!     The left edges are 192 px apart, 192 / 1.53931 = 124.73 triads (not whole): with the old left-edge
//!     origin the two would differ at the centre.

mod common;

use std::ops::Range;

use crt_model::mask::{
    alias_period_px, damper_positions_mm, tier, AxisTier, Grille, Mask, Rect, Tier, BARTEN_THRESHOLD,
};
use ultimatecrt::gpu::{Domain, FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::{
    ParamValue, Params, MASK_TIER_AUTO, MASK_TIER_INTEGRATED, MASK_TIER_PREFILTERED, MASK_TYPE_DELTA,
    MASK_TYPE_SLOT_MASK, OUTPUT_HEADROOM_MODE_CLIP,
};

/// The defaults with the headroom mode pinned to `clip`: the default `auto` (T-84b) lowers the mask contrast
/// where the mask resolves (mask depth), and these tests compare the physical mask with the oracle.
fn physical_mask() -> Params {
    Params {
        output_headroom_mode: OUTPUT_HEADROOM_MODE_CLIP,
        ..Params::default()
    }
}
use ultimatecrt::placement::mask_tiers;

/// One FP16 step relative to the value, at most: 2^-10 (11 significant bits).
const FP16_STEP: f64 = 1.0 / 1024.0;
/// Beam currents the oracle comparisons are repeated with.
const SCALES: [f32; 4] = [1.0, 0.89, 0.77, 0.67];

/// Window of stored / computed - 1 for a value a stage stores in an FP16 image on this device.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stored {
    lo: f64,
    hi: f64,
}

impl Stored {
    /// Checks the rounding: the current 1 + 0.75 x 2^-10 lies three quarters of the way from 1 to the
    /// next FP16 value. With the tube not placed `screen` is an identity (beam fallback, mask 1), so the
    /// stored value is 1 + 2^-10 when the store rounds to nearest. Since T-fp16 every stage store does
    /// (`round_f16`, whatever the device does), so the window is the symmetric nearest one on any device;
    /// a device or shader that truncated would fail here instead of being tolerated.
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

    /// True when `got` can be the stored form of a computed value within `budget` of `want` (>= 0).
    fn holds(&self, got: f64, want: f64, budget: f64) -> bool {
        let err = got - want;
        err >= self.lo * want - budget && err <= self.hi * want + budget
    }
}

/// Budget of the shader's f32 arithmetic for a frame, absolute, in mask units (module header).
fn f32_budget(fc: &FrameConstants, p: &Params) -> f64 {
    let ppt = px_per_triad(fc, p);
    let fill = f64::from(p.mask_stripe_fill);
    // The CPU decision the shader draws (T-39c): any footprint but a whole period integrates columns.
    let integrated = mask_tiers(fc.px_per_mm, p).horizontal != AxisTier::Prefiltered;
    let (columns, peak) = if integrated {
        (4e-7 * ppt / fill, 1.0 / fill)
    } else {
        (0.0, 1.0)
    };
    let wires = if p.mask_damper_count > 0 {
        f64::from(p.tube_visible_height_mm) * f64::from(fc.px_per_mm) / f64::from(1_u32 << 22) * peak
    } else {
        0.0
    };
    2e-6 + columns + wires
}

/// A tube rectangle of `w` x `h` host pixels at `ppm` px per mm; line pitch 0, so the beam is the amp
/// sample under the pixel (module header).
fn tube(w: u32, h: u32, ppm: f32) -> FrameConstants {
    let mut fc = FrameConstants::new((w, h), (w, h), false);
    assert_eq!(fc.tube_size, [w as i32, h as i32]);
    fc.px_per_mm = ppm;
    fc.line_pitch_mm = 0.0;
    fc
}

/// Grille parameters for `fc`: pitch in mm, area-integrated at any density, visible height = the tube
/// rectangle, everything else at its default (fill 0.25, softness 0.07, no damper wire).
fn grille(fc: &FrameConstants, pitch_mm: f32) -> Params {
    Params {
        mask_pitch_mm: pitch_mm,
        mask_tier: MASK_TIER_INTEGRATED,
        tube_visible_height_mm: fc.tube_size[1] as f32 / fc.px_per_mm,
        ..physical_mask()
    }
}

/// Host pixels per triad: the f32 product, which is the horizontal pitch the shader uses.
fn px_per_triad(fc: &FrameConstants, p: &Params) -> f64 {
    f64::from(fc.px_per_mm * p.mask_pitch_mm)
}

/// The oracle grille of a frame, in host pixels from the top-left of the tube rectangle (module header).
fn oracle(fc: &FrameConstants, p: &Params) -> Mask {
    let ppm = f64::from(fc.px_per_mm);
    let ppt = px_per_triad(fc, p);
    let visible = f64::from(p.tube_visible_height_mm) * ppm;
    let above = 0.5 * (visible - f64::from(fc.tube_size[1]));
    let mask = Mask::Grille(Grille {
        pitch_mm: ppt,
        stripe_mm: f64::from(p.mask_stripe_fill) * ppt,
        edge_mm: f64::from(p.mask_edge_softness) * ppt,
        damper_y_mm: damper_positions_mm(p.mask_damper_count as usize, visible)
            .into_iter()
            .map(|y| y - above)
            .collect(),
        damper_mm: f64::from(p.mask_damper_width_mm) * ppm,
        damper_edge_mm: f64::from(p.mask_damper_edge_mm) * ppm,
    });
    mask.validate().expect("the test tube is a valid grille");
    mask
}

/// The mask's horizontal origin in the tube image (T-37): the tube centre, half the tube width (a half pixel
/// for an odd width). The oracle's x = 0 is there.
fn centre_x(fc: &FrameConstants) -> f64 {
    0.5 * f64::from(fc.tube_size[0])
}

/// Footprint of host pixel (x, y) in the oracle's coordinates: x from the tube centre, y from the top.
fn footprint(fc: &FrameConstants, x: u32, y: u32) -> Rect {
    let x0 = f64::from(x) - centre_x(fc);
    Rect::new(x0, f64::from(y), x0 + 1.0, f64::from(y + 1))
}

/// The oracle's value of every host pixel and channel, rows from the top.
fn oracle_image(fc: &FrameConstants, value: impl Fn(usize, &Rect) -> f64) -> Vec<[f64; 3]> {
    let (w, h) = (fc.tube_size[0] as u32, fc.tube_size[1] as u32);
    (0..h)
        .flat_map(|y| (0..w).map(move |x| footprint(fc, x, y)))
        .map(|q| [value(0, &q), value(1, &q), value(2, &q)])
        .collect()
}

/// The amp-domain beam-current image of `fc` with value `f(amp column, row)` per channel.
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

/// Runs the chain on `amp` injected after `gun` with the slots `slots` on; returns the image `screen` left.
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

/// The mask of a frame: `screen` alone on a uniform beam current `scale`, divided by it (f64).
fn mask_image(gpu: &mut Headless, fc: FrameConstants, params: &Params, scale: f32) -> Vec<[f64; 3]> {
    let amp = currents(&fc, |_, _| [scale; 3]);
    let out = run(gpu, &["screen"], fc, params, &amp);
    assert_eq!(
        (out.width as i32, out.height as i32),
        (fc.tube_size[0], fc.tube_size[1])
    );
    (0..out.height)
        .flat_map(|y| (0..out.width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let [r, g, b, _] = out.pixel(x, y);
            [r, g, b].map(|v| f64::from(v) / f64::from(scale))
        })
        .collect()
}

/// Every pixel and channel of `got` is the stored form of `want` within `budget`. Returns the largest
/// error and the mean signed error over the mean value (bias).
fn assert_image(
    what: &str,
    stored: Stored,
    width: u32,
    got: &[[f64; 3]],
    want: &[[f64; 3]],
    budget: f64,
) -> (f64, f64) {
    assert_eq!(got.len(), want.len(), "{what}: image size");
    let (mut worst, mut sum_err, mut sum_want) = (0.0_f64, 0.0, 0.0);
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        for c in 0..3 {
            let err = g[c] - w[c];
            assert!(g[c] >= 0.0, "{what}: negative light {} at pixel {i}", g[c]);
            assert!(
                stored.holds(g[c], w[c], budget),
                "{what}: pixel ({}, {}) channel {c}: GPU {} vs oracle {} (error {err:.3e}, budget {budget:.1e})",
                i as u32 % width,
                i as u32 / width,
                g[c],
                w[c]
            );
            worst = worst.max(err.abs());
            sum_err += err;
            sum_want += w[c];
        }
    }
    (worst, sum_err / sum_want)
}

/// Renders the mask with every beam current of [`SCALES`] and compares each image with `want`. Returns
/// the image of the first current (1.0).
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
        let (worst, bias) = assert_image(&format!("{what} current {scale}"), stored, width, &got, want, budget);
        line.push_str(&format!(" [{scale}: max {worst:.1e}, bias {bias:+.1e}]"));
        if first.is_empty() {
            first = got;
        }
    }
    println!("{line} f32 budget {budget:.1e}");
    first
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

/// Channel `c` of row `y`, columns `xs`.
fn row(img: &[[f64; 3]], width: u32, y: u32, c: usize, xs: Range<u32>) -> Vec<f64> {
    xs.map(|x| img[(y * width + x) as usize][c]).collect()
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// Michelson ripple (max - min) / (max + min).
fn ripple(v: &[f64]) -> f64 {
    let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
    (hi - lo) / (hi + lo)
}

/// Period in samples of the strongest non-constant sinusoid of `v`: the maximum of the periodogram
/// |sum (v - mean) exp(-2 pi i f x)|^2 over 0 < f <= 0.5, on a grid of 1 / (4 N) refined by ternary search.
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

/// Least-squares displacement, in host pixels, of the rendered stripes against the oracle's over the
/// columns `xs` of row 0: a pattern moved by +d reads want(x - d) = want(x) - d x slope, with the slope
/// taken from the oracle by a central difference. `None` when the row has no structure. It averages the
/// FP16 noise over the columns, so it resolves a drift of the stripe phase far below one FP16 step. The
/// oracle's x is measured from `origin` (the tube centre, [`centre_x`]).
fn stripe_displacement(got: &[[f64; 3]], mask: &Mask, xs: Range<u32>, origin: f64) -> Option<f64> {
    let h = 1e-3;
    let (mut num, mut den) = (0.0, 0.0);
    for x in xs {
        let x0 = f64::from(x) - origin;
        for (c, value) in got[x as usize].iter().enumerate() {
            let at = |d: f64| mask.area_mean(c, &Rect::new(x0 + d, 0.0, x0 + 1.0 + d, 1.0));
            let slope = (at(h) - at(-h)) / (2.0 * h);
            num -= (value - at(0.0)) * slope;
            den += slope * slope;
        }
    }
    (den > 1.0).then(|| num / den)
}

/// Random tubes: 0.3 to 24 px per triad (log-uniform; below 1 a pixel spans several triads), 2 to 40 px per mm, fill 0.20 to 0.27, softness 0 to its limit, 0 to 3
/// damper wires of 10 to 30 um with a penumbra, a visible height taller than the rectangle. Every host
/// pixel of a 384 x 40 rectangle (a random rectangle of the mask plane each: the phase of a pixel in its
/// triad and against the wires differs for every pixel and case) equals the oracle's `area_mean`, for
/// every beam current of [`SCALES`]. Plus the limit case D8.
#[test]
fn host_pixels_equal_the_oracle_area_mean_on_random_tubes() {
    let test = "stage_mask_grille random";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let mut rng = Lcg(39);
    let (w, h) = (384, 40);
    for case in 0..16 {
        let ppt = 0.3 * 80.0_f64.powf(rng.next());
        let ppm = 2.0 + 38.0 * rng.next();
        let fc = tube(w, h, ppm as f32);
        let fill = 0.20 + 0.07 * rng.next();
        let soft = 0.999 * rng.next() * (1.0 / 3.0 - fill).min(0.10);
        let wire = 0.01 + 0.02 * rng.next();
        let p = Params {
            mask_pitch_mm: (ppt / ppm) as f32,
            mask_stripe_fill: fill as f32,
            mask_edge_softness: soft as f32,
            mask_damper_count: case % 4,
            mask_damper_width_mm: wire as f32,
            mask_damper_edge_mm: (wire * rng.next()) as f32,
            mask_tier: MASK_TIER_INTEGRATED,
            tube_visible_height_mm: ((f64::from(h) + 30.0 * rng.next()) / ppm) as f32,
            ..physical_mask()
        };
        let mask = oracle(&fc, &p);
        let want = oracle_image(&fc, |c, q| mask.area_mean(c, q));
        let what = format!(
            "{test} case {case} ({:.3} px per triad, fill {fill:.3}, softness {soft:.3}, {} wires)",
            px_per_triad(&fc, &p),
            case % 4
        );
        assert_matches(&mut gpu, &what, fc, &p, &want);
    }

    // D8: fill 0.27 with softness 0.10 is drawn with softness 1/3 - 0.27.
    let fc = tube(w, h, 4.0);
    let asked = Params {
        mask_stripe_fill: 0.27,
        mask_edge_softness: 0.10,
        ..grille(&fc, 2.0)
    };
    let limit = 1.0 / 3.0 - f64::from(asked.mask_stripe_fill);
    assert!((limit - 0.0633).abs() < 5e-5);
    let drawn = Params {
        mask_edge_softness: (limit - 1e-7) as f32,
        ..asked
    };
    let mask = oracle(&fc, &drawn);
    let want = oracle_image(&fc, |c, q| mask.area_mean(c, q));
    assert_matches(&mut gpu, &format!("{test} softness limit"), fc, &asked, &want);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The densities of PLAN T-39a on a 4096 px wide rectangle, default stripe shape, area integral forced
/// (below 3 px per triad the default tier would prefilter): every pixel against the oracle, flat-white
/// mean 1 over whole periods (D3), at the left and at the right end of the rectangle, and no drift of the
/// stripe phase in the last 512 columns.
#[test]
fn plan_densities_flat_white_and_no_phase_drift() {
    let test = "stage_mask_grille densities";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (4096_u32, 2_u32);
    // Name, px per mm, pitch in mm, px per triad by hand (D1), whole-period window in px (D3).
    let cases: [(&str, f32, f32, f64, Option<u32>); 8] = [
        ("PVM-20M4U on 1440 rows", 1440.0 / 290.0, 0.31, 1.539_310, None),
        ("1.54", 4.0, 0.385, 1.54, Some(77)),
        ("26 in TV on 1920 px", 1920.0 / 528.0, 0.8, 2.909_091, Some(32)),
        ("1.0", 4.0, 0.25, 1.0, Some(1)),
        ("3.0", 4.0, 0.75, 3.0, Some(3)),
        ("4.4", 4.0, 1.1, 4.4, Some(22)),
        ("8", 4.0, 2.0, 8.0, Some(8)),
        ("16", 32.0, 0.5, 16.0, Some(16)),
    ];
    for (name, ppm, pitch, ppt_hand, window) in cases {
        let fc = tube(w, h, ppm);
        let p = grille(&fc, pitch);
        let ppt = px_per_triad(&fc, &p);
        assert!((ppt / ppt_hand - 1.0).abs() < 1e-6, "{name}: {ppt} px per triad");
        let mask = oracle(&fc, &p);
        let want = oracle_image(&fc, |c, q| mask.area_mean(c, q));
        let what = format!("{test} {name}");
        let got = assert_matches(&mut gpu, &what, fc, &p, &want);
        // A mean of stored values is off by one FP16 step at most, plus the f32 budget.
        let slack = FP16_STEP + f32_budget(&fc, &p);
        for c in 0..3 {
            match window {
                Some(n) => {
                    // Whole periods at the left end, and the last whole-period window at the right end.
                    let blocks = (1024 / n).max(1) * n;
                    let left = mean(&row(&got, w, 0, c, 0..blocks));
                    let right = mean(&row(&got, w, 1, c, w - blocks..w));
                    println!("{what}: channel {c} flat-white mean {left:.6} (left), {right:.6} (right)");
                    for m in [left, right] {
                        assert!((m - 1.0).abs() <= slack, "{what}: channel {c} mean {m}");
                    }
                }
                None => {
                    // 4096 px are 2660.9 triads: the incomplete triad moves the mean by at most 1 / 2661.
                    let m = mean(&row(&got, w, 0, c, 0..w));
                    println!("{what}: channel {c} flat-white mean {m:.6} over the row");
                    assert!((m - 1.0).abs() <= 1.0 / 2661.0 + slack, "{what}: channel {c} mean {m}");
                }
            }
        }
        match stripe_displacement(&got, &mask, w - 512..w, centre_x(&fc)) {
            Some(d) => {
                println!("{what}: stripes displaced by {d:+.2e} px in the last 512 columns");
                assert!(d.abs() <= 1e-4, "{what}: stripe phase drift {d} px");
            }
            // No slope to fit: at one px per triad there is no structure at all (D4), and at three every
            // stripe lies inside its pixel with a margin of 0.00667 triad = 0.02 px on each side (D5).
            None => assert!(name == "1.0" || name == "3.0", "{what}: no stripe edge to fit"),
        }
        if name == "1.0" {
            assert!(want.iter().flatten().all(|v| (v - 1.0).abs() < 1e-6));
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// D2: the period measured on a rendered flat field (area integral forced) equals the oracle's alias
/// period of the triad frequency on the host grid, and the hand values: 2.854 px for a PVM-20M4U on 1440
/// rows, 2.852 px at 1.54, 2.909 px for the 26 in TV on 1920 px; 16 px on the zoomed tube (PLAN T-39a:
/// dominant period of R = p +/- 1 %). Every channel has the same period.
#[test]
fn moire_period_of_a_rendered_flat_field() {
    let test = "stage_mask_grille moire";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (960_u32, 1_u32);
    for (name, ppm, pitch, period_hand, tolerance) in [
        ("PVM-20M4U on 1440 rows", 1440.0_f32 / 290.0, 0.31_f32, 2.854_22, 0.002),
        ("1.54", 4.0, 0.385, 2.851_85, 0.002),
        ("26 in TV on 1920 px", 1920.0 / 528.0, 0.8, 2.909_09, 0.002),
        ("16", 32.0, 0.5, 16.0, 0.01),
    ] {
        let fc = tube(w, h, ppm);
        let p = grille(&fc, pitch);
        let alias = alias_period_px(px_per_triad(&fc, &p), 1);
        assert!(
            (alias / period_hand - 1.0).abs() < 1e-5,
            "{name}: oracle {alias} vs hand {period_hand}"
        );
        let got = mask_image(&mut gpu, fc, &p, 1.0);
        for c in 0..3 {
            let period = dominant_period(&row(&got, w, 0, c, 0..w));
            println!("{test} {name}: channel {c} period {period:.4} px (oracle {alias:.4}, hand {period_hand})");
            assert!(
                (period / alias - 1.0).abs() <= tolerance,
                "{test} {name}: channel {c} period {period} vs {alias}"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// D6: 16 px per triad, hard edges, fill 0.25: the hand table of a period, the same in the first and in
/// the last period of a 4096 px rectangle; stripe order R, G, B with centroids at 2.6667, 8 and 13.3333
/// px of the period; no pixel lit in two channels.
#[test]
fn stripe_positions_and_colour_order() {
    let test = "stage_mask_grille stripes";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (4096_u32, 1_u32);
    let fc = tube(w, h, 32.0);
    let p = Params {
        mask_edge_softness: 0.0,
        ..grille(&fc, 0.5)
    };
    assert_eq!(px_per_triad(&fc, &p), 16.0);
    let (third, two_thirds) = (4.0 / 3.0, 8.0 / 3.0);
    let mut table = [[0.0_f64; 3]; 16];
    for (x, v) in [(0, third), (1, 4.0), (2, 4.0), (3, 4.0), (4, two_thirds)] {
        table[x][0] = v;
    }
    for cell in &mut table[6..10] {
        cell[1] = 4.0;
    }
    for (x, v) in [(11, two_thirds), (12, 4.0), (13, 4.0), (14, 4.0), (15, third)] {
        table[x][2] = v;
    }
    let want: Vec<[f64; 3]> = (0..w).map(|x| table[(x % 16) as usize]).collect();
    let got = assert_matches(&mut gpu, test, fc, &p, &want);
    for px in &got {
        assert!(
            px.iter().filter(|&&v| v > 0.0).count() <= 1,
            "{test}: two channels lit in {px:?}"
        );
    }
    for start in [0, w - 16] {
        let centroid = |c: usize| {
            let v = row(&got, w, 0, c, start..start + 16);
            v.iter().enumerate().map(|(i, s)| (i as f64 + 0.5) * s).sum::<f64>() / v.iter().sum::<f64>()
        };
        let (r, g, b) = (centroid(0), centroid(1), centroid(2));
        println!("{test}: period at {start}: centroids R {r:.4} G {g:.4} B {b:.4} px");
        for (got, want) in [(r, 8.0 / 3.0), (g, 8.0), (b, 40.0 / 3.0)] {
            assert!((got - want).abs() <= 0.002, "{test}: centroid {got} vs {want}");
        }
        assert!(r < g && g < b);
        assert!((g - r - 16.0 / 3.0).abs() <= 0.004 && (b - g - 16.0 / 3.0).abs() <= 0.004);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// D10: the grille phase at the tube centre is unchanged when PAR changes. The placement of 256 x 240 on
/// 2560 x 1440 at PAR 1 and PAR 1.25 (application code, `FrameConstants::placed`) gives the same px per mm
/// and tubes 1536 and 1920 px wide; each width is rendered (2 rows, area integral forced, PVM-20M4U pitch,
/// 1.539 px per triad) and matches the oracle centred on its tube; the 128 columns around the two centres
/// are equal value for value, while the first columns of the two tubes differ (the left edges are not 124.73
/// triads apart by a whole number: the origin is not the left edge any more).
#[test]
fn grille_phase_at_the_tube_centre_is_unchanged_when_par_changes() {
    let test = "stage_mask_grille par";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let mut tubes = Vec::new();
    let mut ppm = None;
    for (par, width) in [(1.0_f32, 1536_u32), (1.25, 1920)] {
        let mut p = physical_mask();
        assert!(p.set("source_par", ParamValue::F32(par)));
        let placed = FrameConstants::placed((256, 240), (2560, 1440), false, &p);
        assert_eq!(placed.tube_size, [width as i32, 1440], "PAR {par}");
        assert!(
            (placed.px_per_mm - 1440.0 / 290.0).abs() < 1e-5,
            "PAR {par}: full height"
        );
        assert_eq!(
            *ppm.get_or_insert(placed.px_per_mm),
            placed.px_per_mm,
            "PAR {par}: same px per mm"
        );
        let fc = tube(width, 2, placed.px_per_mm);
        let p = Params {
            mask_tier: MASK_TIER_INTEGRATED,
            tube_visible_height_mm: 2.0 / placed.px_per_mm,
            ..p
        };
        assert!((px_per_triad(&fc, &p) - 1.539_310).abs() < 1e-5);
        let mask = oracle(&fc, &p);
        let want = oracle_image(&fc, |c, q| mask.area_mean(c, q));
        let got = assert_matches(&mut gpu, &format!("{test} PAR {par}"), fc, &p, &want);
        tubes.push((width, got));
    }
    let (wa, a) = &tubes[0];
    let (wb, b) = &tubes[1];
    for d in -64_i64..64 {
        let xa = (i64::from(*wa / 2) + d) as usize;
        let xb = (i64::from(*wb / 2) + d) as usize;
        assert_eq!(a[xa], b[xb], "{test}: column {d:+} from the centre");
    }
    assert_ne!(a[..16], b[..16], "{test}: left edges at different phases");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The rows of `got` at column 0 and their whole-period means: every row of `dimmed` (row, value) reads
/// its value, every other row 1 (D7). `period` is the triad pitch in whole px (1 in tier C); `slack` is one
/// FP16 step plus the f32 budget (a mean of stored values is off by one step at most).
fn assert_damper_rows(
    what: &str,
    got: &[[f64; 3]],
    (w, h): (u32, u32),
    period: u32,
    dimmed: &[(u32, f64)],
    slack: f64,
) {
    for y in 0..h {
        let want = dimmed.iter().find(|(r, _)| *r == y).map_or(1.0, |(_, v)| *v);
        for c in 0..3 {
            let m = mean(&row(got, w, y, c, 0..(w / period) * period));
            // PLAN T-39a asks for the attenuation within 10 % of itself (0.01 here); this is 10 times
            // tighter.
            assert!(
                (m - want).abs() <= slack,
                "{what}: row {y} channel {c} mean {m} vs {want} (attenuation {} vs {})",
                1.0 - m,
                1.0 - want
            );
        }
    }
    // No vertical modulation outside the damper rows: those rows are the same image (PLAN: variance < 1e-4).
    let clear: Vec<u32> = (0..h).filter(|y| dimmed.iter().all(|(r, _)| r != y)).collect();
    let first = row(got, w, clear[0], 0, 0..w);
    for &y in &clear {
        assert_eq!(
            row(got, w, y, 0, 0..w),
            first,
            "{what}: row {y} differs from row {}",
            clear[0]
        );
    }
}

/// D7: a 20 um damper wire at 5 px per mm dims the host row that holds it by 10 %, in every channel, in
/// tier C (default tier at 1.55 px per triad) and with the area integral (4 px per triad, whole-period
/// means); a wire on a row boundary dims both rows by 5 %; one, two and three wires sit at the oracle's
/// fractions of the visible height, measured from the top of the visible area.
#[test]
fn damper_wires_dim_their_host_row() {
    let test = "stage_mask_grille dampers";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let w = 64_u32;
    // Rows, visible height in mm, wires, dimmed rows (D7).
    type Case = (u32, f32, i32, &'static [(u32, f64)]);
    let cases: [Case; 4] = [
        (301, 60.2, 2, &[(100, 0.9), (200, 0.9)]),
        (300, 60.0, 1, &[(199, 0.95), (200, 0.95)]),
        (302, 60.4, 3, &[(75, 0.9), (150, 0.95), (151, 0.95), (226, 0.9)]),
        (300, 90.3, 2, &[(74, 0.9), (225, 0.9)]),
    ];
    for (h, visible_mm, wires, dimmed) in cases {
        let fc = tube(w, h, 5.0);
        // (pitch in mm, tier, whole triad period in px): 0.31 mm is 1.55 px per triad, tier C by default;
        // 0.8 mm is 4 px per triad, area-integrated by default.
        for (pitch, period) in [(0.31_f32, 1_u32), (0.8, 4)] {
            let p = Params {
                mask_pitch_mm: pitch,
                mask_damper_count: wires,
                mask_damper_width_mm: 0.02,
                mask_damper_edge_mm: 0.01,
                tube_visible_height_mm: visible_mm,
                ..physical_mask()
            };
            assert_eq!(p.mask_tier, MASK_TIER_AUTO);
            let ppt = px_per_triad(&fc, &p);
            assert_eq!(tier(ppt) == Tier::C, period == 1);
            let what = format!("{test} {h} rows, {visible_mm} mm, {wires} wires, {ppt:.2} px per triad");
            let mask = oracle(&fc, &p);
            let tiers = mask_tiers(fc.px_per_mm, &p);
            let want = oracle_image(&fc, |c, q| mask.host_value(c, q, tiers, 1.0));
            let got = assert_matches(&mut gpu, &what, fc, &p, &want);
            assert_damper_rows(&what, &got, (w, h), period, dimmed, FP16_STEP + f32_budget(&fc, &p));
            let dim: Vec<String> = dimmed
                .iter()
                .map(|(y, _)| format!("row {y}: {:.4}", mean(&row(&got, w, *y, 1, 0..(w / period) * period))))
                .collect();
            println!("{what}: {}", dim.join(", "));
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Tiers (D9, D5, D4, T-39c). Hard switch (`mask_tier_blend` = 0, the behaviour before T-39c): below 3 px
/// per triad every value is exactly 1 (ripple 0, under the 1.4 % threshold) and equals the oracle's
/// `host_value`; from 3.0 the area integral appears at once, at 3.0 as pure primaries of value 3 (D5).
/// Default blend band (0.4, 2.8 to 3.2 px per triad): unchanged at 1.0 and 1.54 (exactly 1) and from 4.4
/// (structure); intentionally changed at 2.9, 2.99 and 3.0, which lie in the band: the footprint is a box
/// of (1 - u) t + u px (crt-model `axis_tier`, t px per triad, u = (t - 2.8) / 0.4), so the values are
/// the oracle's box means, between flat and the full structure (at 2.9: box 2.425 px, ripple about 0.49 by
/// the hand bound of crt-model `blend_band_is_continuous_and_monotone`). Forced area integral at 1.54: the
/// moire ripple is far above 1.4 % (so the tier C check is not vacuous), and at 1.0 px per triad every value
/// is 1. Forced prefilter at 8 px per triad: 1 everywhere.
#[test]
fn tiers_follow_the_oracle() {
    let test = "stage_mask_grille tiers";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (240_u32, 2_u32);
    let fc = tube(w, h, 4.0);
    let ones = vec![[1.0_f64; 3]; (w * h) as usize];
    // (pitch, horizontal class, in the default blend band).
    let cases = [
        (0.25_f32, Tier::C, false),
        (0.385, Tier::C, false),
        (0.725, Tier::C, true),
        (0.7475, Tier::C, true),
        (0.75, Tier::B, true),
        (1.1, Tier::B, false),
        (1.5, Tier::A, false),
        (2.0, Tier::A, false),
    ];
    for blend in [0.0_f32, 0.4] {
        for (pitch, expect, in_band) in cases {
            let p = Params {
                mask_tier: MASK_TIER_AUTO,
                mask_tier_blend: blend,
                ..grille(&fc, pitch)
            };
            let ppt = px_per_triad(&fc, &p);
            assert_eq!(tier(ppt), expect, "{ppt} px per triad");
            let tiers = mask_tiers(fc.px_per_mm, &p);
            let blended = matches!(tiers.horizontal, AxisTier::Blended { .. });
            assert_eq!(blended, blend > 0.0 && in_band, "{ppt}: {tiers:?}");
            let mask = oracle(&fc, &p);
            let want = oracle_image(&fc, |c, q| mask.host_value(c, q, tiers, 1.0));
            let what = format!("{test} auto at {ppt:.4} px per triad, blend {blend}");
            let got = assert_matches(&mut gpu, &what, fc, &p, &want);
            let r = ripple(&row(&got, w, 0, 0, 0..w));
            println!("{what}: tier {expect:?}, {:?}, red ripple {r:.4}", tiers.horizontal);
            if blended {
                // In the band: neither flat nor the full structure (the values are the oracle's above).
                assert!(r > 0.0 && r < 1.0, "{what}: ripple {r}");
                continue;
            }
            if expect == Tier::C {
                assert_eq!(got, ones, "{what}: tier C is exactly 1");
                assert!(r < BARTEN_THRESHOLD);
            } else {
                assert!(r > 0.5, "{what}: the resolved mask has structure");
            }
            if pitch == 0.75 {
                // D5: pure primaries of value 3.
                for (x, px) in got.iter().take(w as usize).enumerate() {
                    let mut hand = [0.0; 3];
                    hand[x % 3] = 3.0;
                    for c in 0..3 {
                        assert!(
                            (px[c] - hand[c]).abs() <= 3.0 * FP16_STEP + 1e-5,
                            "{what}: column {x} is {px:?}"
                        );
                    }
                }
            }
        }
    }

    // Forced area integral below 3 px per triad: host-grid moire, far above the Barten threshold.
    let p = grille(&fc, 0.385);
    let got = mask_image(&mut gpu, fc, &p, 1.0);
    let r = ripple(&row(&got, w, 0, 1, 0..w));
    println!("{test} integrated at 1.54 px per triad: green ripple {r:.4}");
    assert!(r > 10.0 * BARTEN_THRESHOLD);
    // D4: at one px per triad the area integral itself is flat.
    assert_matches(
        &mut gpu,
        &format!("{test} integrated at 1.0"),
        fc,
        &grille(&fc, 0.25),
        &ones,
    );
    // Forced prefilter on a resolvable mask: its mean only.
    let p = Params {
        mask_tier: MASK_TIER_PREFILTERED,
        ..grille(&fc, 2.0)
    };
    assert_eq!(mask_image(&mut gpu, fc, &p, 1.0), ones);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Amp column under the centre of host column `x` (image contract, ARCHITECTURE 3.7).
fn amp_col(fc: &FrameConstants, x: u32) -> u32 {
    let (aw, _) = fc.domain_size(Domain::Amp);
    (((f64::from(x) + 0.5) * f64::from(aw) / f64::from(fc.tube_size[0] as u32)).floor() as u32).min(aw - 1)
}

/// Bypass. With the mask switched off, with the tube not placed (px per mm 0) and with the `screen` slot
/// off, the tube image is exactly the beam image: here the amp sample under each host pixel, bit for bit
/// (the currents are FP16 values), although the mask parameters ask for a resolved grille with wires.
/// Slot mask and delta differ from the grille (T-39b).
#[test]
fn bypass_is_an_exact_identity() {
    let test = "stage_mask_grille bypass";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (96_u32, 30_u32);
    let fc = tube(w, h, 4.0);
    // Distinct per column, row and channel, all multiples of 1 / 64 (exact in FP16).
    let value = |x: u32, y: u32| [1, 2, 3].map(|k| (16 + (x * k + y * 5 + k) % 97) as f32 / 64.0);
    let amp = currents(&fc, value);
    let on = Params {
        mask_damper_count: 2,
        ..grille(&fc, 2.0)
    };
    let beam: Vec<f32> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let [r, g, b] = value(amp_col(&fc, x), y);
            [r, g, b, 1.0]
        })
        .collect();

    // The mask is on and visible with these parameters (otherwise the checks below prove nothing).
    let masked = run(&mut gpu, &["screen"], fc, &on, &amp);
    assert_ne!(masked.rgba, beam);

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

    // Default parameters at a density below 3 px per triad (no wires by default): exactly the beam.
    let defaults = Params::default();
    assert!(defaults.mask_enabled && defaults.mask_damper_count == 0);
    assert_eq!(tier(px_per_triad(&fc, &defaults)), Tier::C);
    assert_eq!(
        run(&mut gpu, &["screen"], fc, &defaults, &amp).rgba,
        beam,
        "defaults, tier C"
    );

    // T-39b: the two other types have their own masks (no fallback to the grille any more), and switching
    // type and back leaves the grille image unchanged (stage_mask_slot_delta.rs tests them).
    for other in [MASK_TYPE_SLOT_MASK, MASK_TYPE_DELTA] {
        let p = Params { mask_type: other, ..on };
        assert_ne!(run(&mut gpu, &["screen"], fc, &p, &amp).rgba, masked.rgba);
    }
    assert_eq!(run(&mut gpu, &["screen"], fc, &on, &amp).rgba, masked.rgba);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Product form L_c = M_c B_c with the real beam (line pitch set: scanlines) and currents that differ per
/// column, row and channel: the tube image with the mask equals the image without it times the oracle's
/// mask, per pixel and channel, within one FP16 step (both images are stored with the same rounding, so
/// their errors differ by one step at most) and the f32 budget.
#[test]
fn screen_is_mask_times_beam() {
    let test = "stage_mask_grille product";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let mut fc = FrameConstants::new((8, 16), (256, 512), false);
    assert_eq!(fc.tube_size, [256, 512]);
    fc.px_per_mm = 4.0;
    fc.line_pitch_mm = 8.0;
    let amp = currents(&fc, |x, y| {
        [1, 2, 3].map(|k| 0.2 + 0.05 * ((x * k + y * 3 + k) % 13) as f32)
    });
    let on = Params {
        mask_damper_count: 2,
        mask_damper_width_mm: 0.03,
        beam_fwhm_mid_mm: 2.5,
        ..grille(&fc, 1.3)
    };
    let off = Params {
        mask_enabled: false,
        ..on
    };
    let mask = oracle(&fc, &on);
    let budget = f32_budget(&fc, &on);
    let with = run(&mut gpu, &["screen"], fc, &on, &amp);
    let without = run(&mut gpu, &["screen"], fc, &off, &amp);
    let mut worst = 0.0_f64;
    for y in 0..512 {
        for x in 0..256 {
            let (a, b) = (with.pixel(x, y), without.pixel(x, y));
            for c in 0..3 {
                let want = f64::from(b[c]) * mask.area_mean(c, &footprint(&fc, x, y));
                let err = (f64::from(a[c]) - want).abs();
                assert!(
                    // The absolute floor covers the FP16 subnormal step 2^-24 = 6e-8: since T-36b the
                    // horizontal spot has very small tails, and the beam image's own store error (one
                    // step) is multiplied by the mask (up to 1 / stripe fill = 4) in the product.
                    err <= 1.01 * FP16_STEP * want + budget * f64::from(b[c]) + 3e-7,
                    "{test}: pixel ({x}, {y}) channel {c}: {} vs beam {} x mask = {want}",
                    a[c],
                    b[c]
                );
                worst = worst.max(err);
            }
        }
    }
    println!("{test}: largest error {worst:.2e}");
    // The beam image has scanlines and the mask has stripes: neither is flat here.
    assert!(ripple(&(0..512).map(|y| f64::from(without.pixel(3, y)[0])).collect::<Vec<_>>()) > 0.05);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Gate 10 (informative, nothing asserted on the time): GPU time of `screen` for a 1920 x 1440 tube
/// rectangle (4:3 at full height of a 2560 x 1440 host), PVM-20M4U mask with two damper wires: mask off,
/// prefiltered (default tier at 1.54 px per triad) and area-integrated. The three variants are rendered in
/// turn, several rounds, and the shortest time of each is kept (the GPU is shared with other work).
#[test]
fn full_frame_gpu_time_is_reported() {
    let test = "stage_mask_grille timing";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let mut fc = FrameConstants::new((320, 240), (1920, 1440), false);
    assert_eq!(fc.tube_size, [1920, 1440]);
    fc.px_per_mm = 1440.0 / 290.0;
    fc.line_pitch_mm = 290.0 / 240.0;
    let amp = currents(&fc, |_, _| [0.5; 3]);
    let variants = [
        ("mask off", false, MASK_TIER_AUTO),
        ("tier C (default)", true, MASK_TIER_AUTO),
        ("area-integrated", true, MASK_TIER_INTEGRATED),
    ];
    let mut best = [f64::MAX; 3];
    for _ in 0..4 {
        for (i, (_, enabled, tier)) in variants.iter().enumerate() {
            let p = Params {
                mask_enabled: *enabled,
                mask_tier: *tier,
                mask_damper_count: 2,
                ..physical_mask()
            };
            run(&mut gpu, &["screen"], fc, &p, &amp);
            if let Some(ms) = gpu.last_timings().and_then(|t| t.stage_ms("screen")) {
                best[i] = best[i].min(ms);
            }
        }
    }
    for ((name, _, _), ms) in variants.iter().zip(best) {
        if ms < f64::MAX {
            println!("{test}: screen {ms:.3} ms, {name} (1920 x 1440, best of 4)");
        } else {
            println!("{test}: no GPU timestamps on this device");
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}
