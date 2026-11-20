//! Tube placement in the host output (T-30: QUESTIONS SRC-3, SRC-4, OUT-7, PHY-3).
//!
//! All lengths of the model are tube millimetres. This module maps the tube to host pixels once per frame
//! on the CPU: it decides the picture shape (pixel aspect ratio or the whole visible area), the size of the
//! picture in the output (fitted into the output at the height fraction, optionally by whole pixels per
//! source row), and fills `FrameConstants`: tube rectangle, host pixels per mm and scanline pitch in mm.
//!
//! The picture is a rectangle of the visible area, and the tube rectangle of `FrameConstants` is that
//! picture (everything of the output outside it is the black surround). Until T-37 adds overscan and
//! geometry, the raster fills the tube rectangle, as on a tube whose size controls are set to fit.
//!
//! Derivations (the oracle is `crt_model::tube`, an independent f64 copy of the maths):
//! 1. Picture aspect A = native_w x PAR / native_h (`par`) or tube width / tube height (`fill_tube`).
//! 2. Picture in mm: full height Ht and width Ht A, or, when A exceeds the tube aspect, full width Wt and
//!    height Wt / A. Its height Hr is the raster height.
//! 3. Scale s (host px per mm) = min(target_w / Wr, height_fraction x target_h / Hr).
//! 4. Whole pixels per source row (`integer`, only when at least 1): s = floor(s Hr / native_h) native_h / Hr.
//! 5. Scanline pitch P = Hr / (native_h x line_multiplier).
//! 6. Rectangle: size round(Wr s) x round(Hr s), centred with floor, corners whole pixels.
//!
//! With the defaults (`par`, PAR 1, fraction 1) this is the native raster letterboxed into the output,
//! exactly as before T-30 ([`crate::gpu::tube_rect_letterbox`]), with the physical scale added.
//!
//! # Host scale modes (T-96, PLAN D-13, D-18)
//!
//! `source_scale_mode` = `tube` (the default) is the derivation above. The other modes size the picture
//! against the actual output (no host resolution is assumed anywhere) by k host pixels per source row,
//! with the picture aspect A of step 1 and the raster (Wr, Hr) of step 2 unchanged:
//! - `native`: k = 1; `integer`: k = `source_scale` (1 to 10), 0 = the largest whole number that fits;
//!   `fit`: the largest whole number that fits; `fit_free`: k = min(target_w / (native_h A), target_h /
//!   native_h), non-integer (the picture fills the output on one axis; the scanlines and the mask no longer
//!   fall on whole host pixels).
//! - A whole k that does not fit falls to the largest that does; when not even k = 1 fits (the source is
//!   larger than the output) the free fit is used. Both are reported ([`Fit`]); `fit` falling back to the
//!   free fit below 1 is its documented behaviour and reports [`Fit::Fits`].
//! - Picture: round(k native_h A) x k native_h host px, centred with floor (or at the top-left corner with
//!   `source_center` off). With A = native_w / native_h (PAR 1, `par`) the width is k native_w: every source
//!   pixel is exactly k x k host pixels in the whole-number modes.
//! - The physical model stays consistent: s = k native_h / Hr host px per mm (picture height over raster
//!   height), so the scanline pitch P = Hr / (native_h x line_multiplier) is still k / line_multiplier host
//!   px and the mask pitch in host px per triad is `mask_pitch_mm` x s: it follows the picture size, not the
//!   tube's. `tube_height_fraction` applies to the `tube` mode only.
//!
//! The oracle of the host modes is `crt_model::tube::host_rows_scale` (an independent counting search).

use std::fmt;

use crt_model::mask::{AxisTier, AxisTiers, Delta, Grille, Mask, Slot, TierMode, TierRule};

use crate::gpu::FrameConstants;
use crate::params::{
    Params, MASK_TIER_INTEGRATED, MASK_TIER_PREFILTERED, MASK_TYPE_DELTA, MASK_TYPE_SLOT_MASK, RASTER_ASPECT_FILL_TUBE,
    SOURCE_SCALE_MODE_FIT, SOURCE_SCALE_MODE_FIT_FREE, SOURCE_SCALE_MODE_INTEGER, SOURCE_SCALE_MODE_NATIVE,
};

/// How the picture takes its shape on the tube (`params.raster_aspect`, `params.source_display_aspect`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RasterAspect {
    /// Aspect = source width x PAR / source height.
    Par(f64),
    /// The picture fills the whole visible area.
    FillTube,
    /// A forced display aspect (width / height), whatever the source size (T-96).
    Display(f64),
}

/// How large the picture is in the output (`params.source_scale_mode`, T-96).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ScaleMode {
    /// Sized by the tube (visible area and height fraction), the physical default.
    #[default]
    Tube,
    /// One source row on one host pixel.
    Native,
    /// `n` host pixels per source row; 0 = the largest whole number that fits.
    Integer(u32),
    /// The largest whole number that fits; the free fit below 1.
    Fit,
    /// The largest non-integer scale that fits.
    FitFree,
}

impl ScaleMode {
    /// The mode of the parameter table: `source_scale_mode` and `source_scale` (clamped to 0..=10).
    pub fn from_params(p: &Params) -> Self {
        match p.source_scale_mode {
            SOURCE_SCALE_MODE_NATIVE => ScaleMode::Native,
            SOURCE_SCALE_MODE_INTEGER => ScaleMode::Integer(p.source_scale.clamp(0, 10) as u32),
            SOURCE_SCALE_MODE_FIT => ScaleMode::Fit,
            SOURCE_SCALE_MODE_FIT_FREE => ScaleMode::FitFree,
            _ => ScaleMode::Tube,
        }
    }

    /// Whether the mode asks for whole host pixels per source row.
    pub fn is_whole(self) -> bool {
        matches!(self, ScaleMode::Native | ScaleMode::Integer(_) | ScaleMode::Fit)
    }
}

impl fmt::Display for ScaleMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScaleMode::Tube => write!(f, "tube"),
            ScaleMode::Native => write!(f, "native"),
            ScaleMode::Integer(0) => write!(f, "integer auto"),
            ScaleMode::Integer(n) => write!(f, "integer x{n}"),
            ScaleMode::Fit => write!(f, "fit"),
            ScaleMode::FitFree => write!(f, "fit_free"),
        }
    }
}

/// The host-scale options of [`PlacementInput`] (T-96). The default is the `tube` mode, centred.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostFit {
    pub mode: ScaleMode,
    /// Centre the picture (else the top-left corner).
    pub center: bool,
}

impl Default for HostFit {
    fn default() -> Self {
        Self {
            mode: ScaleMode::Tube,
            center: true,
        }
    }
}

/// Whether the requested scale fitted the output (T-96 readout).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// As requested.
    Fits,
    /// The requested whole factor was too large: the largest that fits is used.
    Clamped { requested: u32, used: u32 },
    /// The output is smaller than the native picture: a free (non-integer) fit below 1 is used.
    Downscaled,
}

/// Inputs of [`place`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacementInput {
    /// Native raster (source pixels after crop and divisor).
    pub native: (u32, u32),
    /// Output size in pixels.
    pub target: (u32, u32),
    /// Visible area of the tube face in mm (width, height).
    pub visible_mm: (f64, f64),
    /// Fraction of the output height the picture may occupy.
    pub height_fraction: f64,
    pub aspect: RasterAspect,
    /// Scanlines per source row (1 or 2).
    pub line_multiplier: u32,
    /// Whole host pixels per source row (when at least 1), `tube` mode only (`--integer`).
    pub integer: bool,
    /// Host scale mode and alignment (T-96); the default is the `tube` mode, centred.
    pub fit: HostFit,
}

impl PlacementInput {
    /// The inputs of a frame from the parameter table (`source.toml`, `tube.toml`).
    pub fn from_params(params: &Params, native: (u32, u32), target: (u32, u32), integer: bool) -> Self {
        let height = f64::from(params.tube_visible_height_mm);
        Self {
            native,
            target,
            visible_mm: (height * f64::from(params.tube_aspect), height),
            height_fraction: f64::from(params.tube_height_fraction),
            aspect: if params.raster_aspect == RASTER_ASPECT_FILL_TUBE {
                RasterAspect::FillTube
            } else if params.source_display_aspect > 0.0 {
                RasterAspect::Display(f64::from(params.source_display_aspect))
            } else {
                RasterAspect::Par(f64::from(params.source_par))
            },
            line_multiplier: params.line_multiplier.max(1) as u32,
            integer,
            fit: HostFit {
                mode: ScaleMode::from_params(params),
                center: params.source_center,
            },
        }
    }
}

/// Result of [`place`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Tube rectangle in output pixels: x0, y0, x1, y1 (exclusive), whole pixels.
    pub rect_px: [f32; 4],
    /// Host pixels per tube mm.
    pub px_per_mm: f64,
    /// Scanline pitch in tube mm.
    pub line_pitch_mm: f64,
    /// Displayed picture aspect (width / height).
    pub display_aspect: f64,
    /// Picture size in mm (the raster).
    pub raster_mm: (f64, f64),
    /// Host pixels per source row (T-96): whole in the whole-number modes when it fits.
    pub row_px: f64,
    /// Whether the requested scale fitted the output (T-96).
    pub fit: Fit,
}

/// Places the tube picture in the output (derivation in the module documentation). A zero native or
/// target size, or a non-positive length, gives an empty rectangle and zero scale (the pipeline refuses
/// an empty tube domain).
pub fn place(i: &PlacementInput) -> Placement {
    let (nw, nh) = (f64::from(i.native.0), f64::from(i.native.1));
    let (tw, th) = (f64::from(i.target.0), f64::from(i.target.1));
    let (vw, vh) = i.visible_mm;
    let empty = Placement {
        rect_px: [0.0; 4],
        px_per_mm: 0.0,
        line_pitch_mm: 0.0,
        display_aspect: 0.0,
        raster_mm: (0.0, 0.0),
        row_px: 0.0,
        fit: Fit::Fits,
    };
    if !(nw > 0.0 && nh > 0.0 && tw > 0.0 && th > 0.0 && vw > 0.0 && vh > 0.0 && i.height_fraction > 0.0) {
        return empty;
    }
    let aspect = match i.aspect {
        RasterAspect::Par(par) => nw * par / nh,
        RasterAspect::FillTube => vw / vh,
        RasterAspect::Display(a) => a,
    };
    if !(aspect > 0.0 && aspect.is_finite()) {
        return empty;
    }
    let (wr, hr) = if aspect <= vw / vh {
        (vh * aspect, vh)
    } else {
        (vw, vw / aspect)
    };
    let (s, fit) = if i.fit.mode == ScaleMode::Tube {
        let mut s = (tw / wr).min(i.height_fraction * th / hr);
        if i.integer {
            let row_px = s * hr / nh;
            if row_px >= 1.0 {
                // The epsilon keeps an exact whole factor (such as 1440 / 290 x 290 / 240 = 6) from falling
                // to the one below through rounding.
                s = (row_px + 1e-9).floor() * nh / hr;
            }
        }
        (s, Fit::Fits)
    } else {
        let (k, fit) = host_row_px(i.fit.mode, nh, aspect, (tw, th));
        (k * nh / hr, fit)
    };
    let (w, h) = ((wr * s).round(), (hr * s).round());
    let (x0, y0) = if i.fit.center {
        (((tw - w) / 2.0).floor(), ((th - h) / 2.0).floor())
    } else {
        (0.0, 0.0)
    };
    Placement {
        rect_px: [x0 as f32, y0 as f32, (x0 + w) as f32, (y0 + h) as f32],
        px_per_mm: s,
        line_pitch_mm: hr / (nh * f64::from(i.line_multiplier.max(1))),
        display_aspect: aspect,
        raster_mm: (wr, hr),
        row_px: s * hr / nh,
        fit,
    }
}

/// Host pixels per source row of a host scale mode (not `tube`) for `nh` source rows at display aspect
/// `aspect` in an output of `target` px, and whether it fitted (module documentation, T-96).
fn host_row_px(mode: ScaleMode, nh: f64, aspect: f64, target: (f64, f64)) -> (f64, Fit) {
    let free = (target.0 / (nh * aspect)).min(target.1 / nh);
    // As in the `tube` integer case, the epsilon keeps an exact whole factor (2160 / 240 = 9).
    let largest = (free + 1e-9).floor();
    let whole = |want: f64| {
        if want <= largest {
            (want, Fit::Fits)
        } else if largest >= 1.0 {
            (
                largest,
                Fit::Clamped {
                    requested: want as u32,
                    used: largest as u32,
                },
            )
        } else {
            (free, Fit::Downscaled)
        }
    };
    match mode {
        ScaleMode::Native => whole(1.0),
        ScaleMode::Integer(n) if n >= 1 => whole(f64::from(n)),
        ScaleMode::Integer(_) if largest >= 1.0 => (largest, Fit::Fits),
        ScaleMode::Integer(_) => (free, Fit::Downscaled),
        ScaleMode::Fit if largest >= 1.0 => (largest, Fit::Fits),
        ScaleMode::Fit | ScaleMode::FitFree | ScaleMode::Tube => (free, Fit::Fits),
    }
}

/// The resolved placement of a frame for the readouts (T-96: `--stats` at start, the settings menu of
/// T-53): what the picture is in the output and what the tube's physical numbers become on this host.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Readout {
    pub mode: ScaleMode,
    pub native: (u32, u32),
    pub target: (u32, u32),
    /// Host pixels per source pixel (columns, rows): the picture size over the native size.
    pub px_per_source_px: (f64, f64),
    /// Every source pixel is a whole number of host pixels on both axes.
    pub integer_aligned: bool,
    /// Picture size in host px (the raster, without the beam's skirt margin).
    pub picture_px: (u32, u32),
    /// Top-left corner of the picture in the output.
    pub offset_px: (u32, u32),
    pub px_per_mm: f64,
    /// Scanline pitch in host px.
    pub line_px: f64,
    /// Mask pitch of the current preset in host px per triad (`mask_pitch_mm` x px per mm).
    pub px_per_triad: f64,
    /// The mask tier decision at this scale (`mask_tiers`).
    pub tiers: AxisTiers,
    pub fit: Fit,
}

/// The resolved placement of a `native` raster in a `target` output with the parameters (T-96), as the
/// live pipeline places it ([`FrameConstants::placed`]).
pub fn readout(params: &Params, native: (u32, u32), target: (u32, u32), integer: bool) -> Readout {
    let input = PlacementInput::from_params(params, native, target, integer);
    let p = place(&input);
    let (w, h) = (
        (p.rect_px[2] - p.rect_px[0]) as u32,
        (p.rect_px[3] - p.rect_px[1]) as u32,
    );
    let per = |a: u32, n: u32| if n > 0 { f64::from(a) / f64::from(n) } else { 0.0 };
    let px = (per(w, native.0), per(h, native.1));
    let whole = |v: f64| v >= 1.0 && v.fract() == 0.0;
    Readout {
        mode: input.fit.mode,
        native,
        target,
        px_per_source_px: px,
        integer_aligned: whole(px.0) && whole(px.1),
        picture_px: (w, h),
        offset_px: (p.rect_px[0] as u32, p.rect_px[1] as u32),
        px_per_mm: p.px_per_mm,
        line_px: p.line_pitch_mm * p.px_per_mm,
        px_per_triad: f64::from(params.mask_pitch_mm) * p.px_per_mm,
        tiers: mask_tiers(p.px_per_mm as f32, params),
        fit: p.fit,
    }
}

impl fmt::Display for Readout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tier = |t: AxisTier| match t {
            AxisTier::Integrated => "integrated".to_string(),
            AxisTier::Blended { box_px } => format!("blended (box {box_px:.2} px)"),
            AxisTier::Prefiltered => "prefiltered".to_string(),
        };
        write!(
            f,
            "placement: {} | native {}x{} in {}x{} | picture {}x{} at ({}, {}) | {:.4} x {:.4} px per source px{} | \
             {:.4} px/mm | scanline {:.3} px | {:.3} px per triad | mask tier h {} v {}",
            self.mode,
            self.native.0,
            self.native.1,
            self.target.0,
            self.target.1,
            self.picture_px.0,
            self.picture_px.1,
            self.offset_px.0,
            self.offset_px.1,
            self.px_per_source_px.0,
            self.px_per_source_px.1,
            if self.integer_aligned { " (integer)" } else { "" },
            self.px_per_mm,
            self.line_px,
            self.px_per_triad,
            tier(self.tiers.horizontal),
            tier(self.tiers.vertical),
        )?;
        match self.fit {
            Fit::Fits => Ok(()),
            Fit::Clamped { requested, used } => {
                write!(f, " | x{requested} does not fit this output: x{used} used")
            }
            Fit::Downscaled => write!(
                f,
                " | the output is smaller than the native picture: downscaled (not integer)"
            ),
        }
    }
}

/// The mask geometry of the parameter table as the `crt-model` oracle sees it, in tube mm (T-39c): the
/// periods and shapes of `mask.toml` (no damper wires: they do not enter the tier decision). The source of
/// the tier decision and of the mask readouts, so both see the same f64 numbers.
pub fn mask_model(p: &Params) -> Mask {
    let pitch = f64::from(p.mask_pitch_mm);
    let edge = f64::from(p.mask_edge_softness) * pitch;
    match p.mask_type {
        MASK_TYPE_SLOT_MASK => Mask::Slot(Slot {
            pitch_mm: pitch,
            stripe_mm: f64::from(p.mask_stripe_fill) * pitch,
            edge_mm: edge,
            slot_period_mm: f64::from(p.mask_slot_period) * pitch,
            bridge_fraction: f64::from(p.mask_slot_bridge),
        }),
        MASK_TYPE_DELTA => Mask::Delta(Delta {
            pitch_mm: pitch,
            dot_mm: f64::from(p.mask_dot_fill) * pitch,
            edge_mm: edge,
        }),
        _ => Mask::Grille(Grille {
            pitch_mm: pitch,
            stripe_mm: f64::from(p.mask_stripe_fill) * pitch,
            edge_mm: edge,
            damper_y_mm: Vec::new(),
            damper_mm: 0.0,
            damper_edge_mm: 0.0,
        }),
    }
}

/// The tier rule of the parameter table: `mask_tier` and `mask_tier_blend`.
pub fn mask_tier_rule(p: &Params) -> TierRule {
    TierRule {
        mode: match p.mask_tier {
            MASK_TIER_INTEGRATED => TierMode::Integrated,
            MASK_TIER_PREFILTERED => TierMode::Prefiltered,
            _ => TierMode::Auto,
        },
        blend: f64::from(p.mask_tier_blend),
    }
}

/// The per-axis mask tiers at `px_per_mm` host px per tube mm (T-39c): decided once, on the CPU, in f64 from
/// the f32 frame and parameter values (`crt_model::mask::Mask::axis_tiers`). The shader draws what this
/// returns ([`FrameConstants::with_mask_tiers`]) and the readouts report it, so they cannot disagree (an f32
/// product of px per mm and pitch can fall on the other side of a limit than the f64 one).
pub fn mask_tiers(px_per_mm: f32, p: &Params) -> AxisTiers {
    mask_model(p).axis_tiers(f64::from(px_per_mm), mask_tier_rule(p))
}

/// The footprint widths `FrameConstants::mask_box_px` encodes for an axis tier: 0 for a whole period
/// (prefiltered, drawn in closed form), 1 for the host pixel (integrated), the box width in host px between.
pub fn mask_box_code(t: AxisTier) -> f32 {
    match t {
        AxisTier::Integrated => 1.0,
        AxisTier::Blended { box_px } => box_px as f32,
        AxisTier::Prefiltered => 0.0,
    }
}

impl FrameConstants {
    /// Fills `mask_box_px` from [`mask_tiers`] (T-39c). The pipeline applies it to every frame it records,
    /// live or injected, so the decision is always the CPU's. Unplaced constants (px per mm not positive:
    /// the mask is off) get the host pixel on both axes.
    pub fn with_mask_tiers(mut self, params: &Params) -> Self {
        self.mask_box_px = if self.px_per_mm > 0.0 {
            let t = mask_tiers(self.px_per_mm, params);
            [mask_box_code(t.horizontal), mask_box_code(t.vertical)]
        } else {
            [1.0, 1.0]
        };
        self
    }

    /// The constants of a `native` raster placed into `target` by the tube parameters ([`place`]): tube
    /// rectangle and size, `px_per_mm` and `line_pitch_mm` filled; K = `amp_samples` (default 4), frame index
    /// and time 0.
    pub fn placed(native: (u32, u32), target: (u32, u32), integer: bool, params: &Params) -> Self {
        let p = place(&PlacementInput::from_params(params, native, target, integer));
        Self {
            tube_rect_px: p.rect_px,
            tube_size: [
                (p.rect_px[2] - p.rect_px[0]) as i32,
                (p.rect_px[3] - p.rect_px[1]) as i32,
            ],
            px_per_mm: p.px_per_mm as f32,
            line_pitch_mm: p.line_pitch_mm as f32,
            white_gain: crate::white::white_gains(params),
            // `amp_samples` (a preset sets it from its bandwidth, `presets::recommended_amp_k`), limited to what the
            // host pixels resolve in the fast and low quality tiers (T-48, `quality::amp_k`).
            amp_k: crate::quality::amp_k(
                params,
                f64::from(p.rect_px[2] - p.rect_px[0]) / f64::from(native.0.max(1)),
            ),
            ..Self::new(native, target, integer)
        }
    }

    /// Grows the tube image around the placed raster by the spot's reach (T-37, review of T-36b): 4 sigma
    /// of the spot at twice the peak current (`BEAM_REACH_CURRENT` and `BEAM_REACH_SIGMAS` of `beam.glsl`;
    /// the tail's sigma when a tail is on) plus the largest misconvergence (static plus radial, mm), rounded
    /// up to whole pixels, per side and per axis, limited by the target on each axis (the raster stays
    /// centred). The beam is then evaluated over the margin, so the skirt past the raster's ends is drawn
    /// instead of cut; `raster_margin` tells the shaders where the raster is. Unplaced constants (px per mm
    /// or line pitch not positive: the beam falls back to the nearest amp sample, which has no skirt) are
    /// returned unchanged.
    ///
    /// The live pipeline applies it only when the `screen` slot runs (`Pipeline::record`): a bypassed
    /// `screen` is a nearest-neighbour resample that would stretch the raster over the margin. The tube
    /// rectangle `tube_rect_px` grows with the image, so `encode` draws the margin (the skirt) and the black
    /// surround starts past it.
    pub fn with_skirt_margin(mut self, params: &Params) -> Self {
        let positive = |v: f32| v > 0.0; // false for NaN
        if !positive(self.px_per_mm) || !positive(self.line_pitch_mm) {
            return self;
        }
        let growth = crt_model::beam::SpotGrowth {
            fwhm_mid: f64::from(params.beam_fwhm_mid_mm),
            fwhm_peak: f64::from(params.beam_fwhm_peak_mm),
            n: f64::from(params.beam_growth_exponent),
        };
        let mut sigma_mm = growth.fwhm(2.0).max(growth.fwhm(0.0)) / 2.354_820_045;
        if params.beam_tail_weight > 0.0 {
            sigma_mm *= f64::from(params.beam_tail_sigma_ratio).max(1.0);
        }
        let conv = |s: f32, d: f32, r: f32| f64::from(s.abs().max(d.abs()) + r.abs());
        let conv_mm = conv(
            params.geometry_conv_red_dx_mm,
            params.geometry_conv_red_dy_mm,
            params.geometry_conv_red_radial_mm,
        )
        .max(conv(
            params.geometry_conv_green_dx_mm,
            params.geometry_conv_green_dy_mm,
            params.geometry_conv_green_radial_mm,
        ))
        .max(conv(
            params.geometry_conv_blue_dx_mm,
            params.geometry_conv_blue_dy_mm,
            params.geometry_conv_blue_radial_mm,
        ));
        let want = ((4.0 * sigma_mm + conv_mm) * f64::from(self.px_per_mm)).ceil();
        if !want.is_finite() || want <= 0.0 {
            return self;
        }
        let want = want.min(4096.0) as i32;
        let r = self.tube_rect_px;
        let room = |lo: f32, hi: f32, size: i32| (lo as i32).min(size - hi as i32).max(0);
        let m = [
            want.min(room(r[0], r[2], self.target_size[0])),
            want.min(room(r[1], r[3], self.target_size[1])),
        ];
        self.tube_rect_px = [
            r[0] - m[0] as f32,
            r[1] - m[1] as f32,
            r[2] + m[0] as f32,
            r[3] + m[1] as f32,
        ];
        self.tube_size = [self.tube_size[0] + 2 * m[0], self.tube_size[1] + 2 * m[1]];
        self.raster_margin = m;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::tube_rect_letterbox;
    use crate::params::ParamValue;

    /// T-39c: the mask footprint codes of the frame. Default parameters (PVM grille, 0.31 mm) placed on
    /// 2560 x 1440 at full height: 1440 / 290 x 0.31 = 1.5393 px per triad, prefiltered horizontally (code 0),
    /// no vertical period (code 1). Unplaced: 1 and 1. Slot defaults at 4 px per mm with p = 0.75 mm (3.0 px
    /// per triad, u = 0.5): horizontal box 0.5 x 3 + 0.5 = 2 px, vertical V / 2 = 1.25 px: prefiltered (0).
    #[test]
    fn mask_box_codes_from_the_parameter_table() {
        let p = Params::default();
        let fc = FrameConstants::placed((256, 240), (2560, 1440), false, &p).with_mask_tiers(&p);
        assert_eq!(fc.mask_box_px, [0.0, 1.0]);
        let unplaced = FrameConstants::new((256, 240), (2560, 1440), false).with_mask_tiers(&p);
        assert_eq!(unplaced.mask_box_px, [1.0, 1.0]);
        let slot = Params {
            mask_type: MASK_TYPE_SLOT_MASK,
            mask_pitch_mm: 0.75,
            ..Params::default()
        };
        let mut fc = FrameConstants::new((256, 240), (2560, 1440), false);
        fc.px_per_mm = 4.0;
        assert_eq!(fc.with_mask_tiers(&slot).mask_box_px, [2.0, 0.0]);
    }

    fn pvm(native: (u32, u32), target: (u32, u32), aspect: RasterAspect) -> PlacementInput {
        PlacementInput {
            native,
            target,
            visible_mm: (290.0 * 4.0 / 3.0, 290.0),
            height_fraction: 1.0,
            aspect,
            line_multiplier: 1,
            integer: false,
            fit: HostFit::default(),
        }
    }

    /// The defaults reproduce the letterbox of T-20a exactly, over aspects wider and narrower than the
    /// tube, integer and fractional scale, so the T-20a and T-20b tests and the live picture stay valid.
    #[test]
    fn default_placement_is_the_native_letterbox() {
        let params = Params::default();
        let natives = [
            (256, 240),
            (256, 4),
            (320, 180),
            (640, 480),
            (160, 144),
            (256, 224),
            (8, 4),
            (1, 1),
        ];
        let targets = [
            (256, 240),
            (1920, 1080),
            (2560, 1440),
            (300, 300),
            (128, 240),
            (16, 8),
            (256, 4),
            (1000, 77),
        ];
        for n in natives {
            for t in targets {
                for integer in [false, true] {
                    let want = tube_rect_letterbox(n, t, integer);
                    let got = place(&PlacementInput::from_params(&params, n, t, integer)).rect_px;
                    assert_eq!(got, want, "native {n:?} target {t:?} integer {integer}");
                }
            }
        }
    }

    /// 2560 x 1440, PVM-20M4U class 290 mm high 4:3, raster filling the tube: 1440 / 290 = 4.9655 px/mm,
    /// 1920 px wide (386.667 x 4.9655), 240 lines 290 / 240 = 1.2083 mm apart, 6 px per line.
    #[test]
    fn pvm_20m4u_fills_1440() {
        let p = place(&pvm((256, 240), (2560, 1440), RasterAspect::FillTube));
        assert!((p.px_per_mm - 1440.0 / 290.0).abs() < 1e-12);
        assert!((p.px_per_mm - 4.966).abs() / 4.966 < 1e-3);
        assert!((p.line_pitch_mm - 1.208_333).abs() < 1e-5);
        assert_eq!(p.rect_px, [320.0, 0.0, 2240.0, 1440.0]);
        assert!((p.display_aspect - 4.0 / 3.0).abs() < 1e-12);
        // Host pixels per triad of a 0.31 mm pitch: 4.9655 x 0.31 = 1.5393.
        assert!((p.px_per_mm * 0.31 - 1.5393).abs() < 1e-4);
        let double = place(&PlacementInput {
            line_multiplier: 2,
            ..pvm((256, 240), (2560, 1440), RasterAspect::FillTube)
        });
        assert!((double.line_pitch_mm - 0.604_167).abs() < 1e-5);
        assert_eq!(double.line_pitch_mm * 2.0, p.line_pitch_mm);
    }

    /// PAR 8:7 on 256 x 240: aspect 292.571 / 240 = 1.2190 (+-0.5 %); at 1440 px high the rectangle is
    /// round(290 x 1.219048 x 4.965517) = round(1755.4) = 1755 wide, and 1755 / 1440 = 1.21875.
    #[test]
    fn nes_par_8_7() {
        let p = place(&pvm((256, 240), (2560, 1440), RasterAspect::Par(8.0 / 7.0)));
        assert!((p.display_aspect - 1.219).abs() / 1.219 < 5e-3, "{}", p.display_aspect);
        let w = p.rect_px[2] - p.rect_px[0];
        let h = p.rect_px[3] - p.rect_px[1];
        assert_eq!((w, h), (1755.0, 1440.0));
        assert!((f64::from(w / h) - 1.219).abs() / 1.219 < 5e-3);
        assert_eq!(p.rect_px[0], 402.0, "centred: floor((2560 - 1755) / 2)");
    }

    /// Height fraction 0.5: 720 / 290 = 2.4828 px/mm, rectangle 960 x 720 centred; the scanline pitch is
    /// tube physics and does not change.
    #[test]
    fn height_fraction_and_surround() {
        let mut i = pvm((256, 240), (2560, 1440), RasterAspect::FillTube);
        i.height_fraction = 0.5;
        let p = place(&i);
        assert!((p.px_per_mm - 720.0 / 290.0).abs() < 1e-12);
        assert_eq!(p.rect_px, [800.0, 360.0, 1760.0, 1080.0]);
        assert!((p.line_pitch_mm - 290.0 / 240.0).abs() < 1e-12);
    }

    /// 16:9 pixel art (320 x 180) on the 4:3 tube is limited by the width: 217.5 mm high, 180 lines at
    /// 1.2083 mm; 2560 / 386.667 = 6.6207 px/mm fills the output exactly.
    #[test]
    fn wide_picture_on_a_4_3_tube() {
        let p = place(&pvm((320, 180), (2560, 1440), RasterAspect::Par(1.0)));
        assert!((p.raster_mm.1 - 217.5).abs() < 1e-9);
        assert!((p.line_pitch_mm - 1.208_333).abs() < 1e-5);
        assert!((p.px_per_mm - 6.620_69).abs() < 1e-5);
        assert_eq!(p.rect_px, [0.0, 0.0, 2560.0, 1440.0]);
    }

    /// Whole pixels per source row. 2560 x 1440, 240 rows: 6.0 px per row, kept. At 1400 px high the
    /// fractional 5.8333 drops to 5: scale 5 x 240 / 290 = 4.1379 px/mm, picture 1200 high; for the
    /// square-pixel 256 x 240 it is 1280 wide. Below 1 px per row the scale is left alone.
    #[test]
    fn integer_scale() {
        let mut i = pvm((256, 240), (2560, 1440), RasterAspect::FillTube);
        i.integer = true;
        assert_eq!(place(&i).rect_px[3], 1440.0);
        let p = place(&PlacementInput {
            integer: true,
            target: (2560, 1400),
            ..pvm((256, 240), (2560, 1400), RasterAspect::Par(1.0))
        });
        assert!((p.px_per_mm - 5.0 * 240.0 / 290.0).abs() < 1e-12);
        assert_eq!(p.rect_px, [640.0, 100.0, 1920.0, 1300.0]);
        let small = PlacementInput {
            integer: true,
            ..pvm((256, 240), (128, 120), RasterAspect::Par(1.0))
        };
        assert_eq!(place(&small).rect_px, tube_rect_letterbox((256, 240), (128, 120), true));
    }

    /// T-96: the mode comes from the table (`source_scale` clamped to 0..=10), the readout line names the
    /// mode, the integer alignment and the fit note (256 x 240 x10 on 1920 x 1080: 4 fits, 1024 x 960 at
    /// (448, 60), the hand case of `tests/placement_modes.rs`).
    #[test]
    fn host_mode_from_the_table_and_readout_text() {
        use crate::params::{SOURCE_SCALE_MODE_FIT_FREE, SOURCE_SCALE_MODE_INTEGER};
        let mut p = Params::default();
        assert_eq!(ScaleMode::from_params(&p), ScaleMode::Tube);
        p.source_scale_mode = SOURCE_SCALE_MODE_INTEGER;
        p.source_scale = 42;
        assert_eq!(ScaleMode::from_params(&p), ScaleMode::Integer(10));
        let r = readout(&p, (256, 240), (1920, 1080), false);
        assert_eq!(r.fit, Fit::Clamped { requested: 10, used: 4 });
        let text = r.to_string();
        assert!(
            text.contains("integer x10") && text.contains("1024x960 at (448, 60)"),
            "{text}"
        );
        assert!(
            text.contains("(integer)") && text.contains("x10 does not fit"),
            "{text}"
        );
        p.source_scale_mode = SOURCE_SCALE_MODE_FIT_FREE;
        let text = readout(&p, (256, 240), (1920, 1080), false).to_string();
        assert!(text.contains("fit_free") && !text.contains("(integer)"), "{text}");
        // The default (tube) readout of the T-39c case: 1.5393 px per triad on 2560 x 1440.
        let r = readout(&Params::default(), (256, 240), (2560, 1440), false);
        assert!((r.px_per_triad - 1440.0 / 290.0 * 0.31).abs() < 1e-6);
        assert!(matches!(r.tiers.horizontal, AxisTier::Prefiltered));
    }

    #[test]
    fn degenerate_inputs_give_an_empty_tube() {
        for (n, t) in [((0, 240), (100, 100)), ((256, 240), (0, 100)), ((256, 0), (100, 0))] {
            let p = place(&pvm(n, t, RasterAspect::FillTube));
            assert_eq!(p.rect_px, [0.0; 4]);
            assert_eq!(p.px_per_mm, 0.0);
        }
    }

    /// `FrameConstants::placed` fills px per mm and the scanline pitch from the parameters, switching
    /// `raster_aspect` and `line_multiplier` through the table.
    #[test]
    fn frame_constants_from_the_parameter_table() {
        let mut params = Params::default();
        let fc = FrameConstants::placed((256, 240), (2560, 1440), false, &params);
        assert_eq!(fc.tube_size, [1536, 1440], "default: par 1 keeps the native aspect");
        assert!((fc.px_per_mm - 4.9655).abs() < 1e-3);
        assert!((fc.line_pitch_mm - 1.2083).abs() < 1e-3);
        assert_eq!(fc.native_size, [256, 240]);
        assert_eq!(fc.amp_k, FrameConstants::new((256, 240), (2560, 1440), false).amp_k);
        assert_eq!(fc.amp_k, 4, "amp_samples defaults to the pipeline K");
        assert!(params.set("amp_samples", ParamValue::I32(16)));
        assert_eq!(
            FrameConstants::placed((256, 240), (2560, 1440), false, &params).amp_k,
            16
        );
        assert!(params.set("amp_samples", ParamValue::I32(0)));
        assert_eq!(
            FrameConstants::placed((256, 240), (2560, 1440), false, &params).amp_k,
            1,
            "never below 1"
        );
        assert!(params.set("amp_samples", ParamValue::I32(4)));
        assert!(params.set("raster_aspect", ParamValue::Enum(RASTER_ASPECT_FILL_TUBE)));
        assert!(params.set("line_multiplier", ParamValue::I32(2)));
        assert!(params.set("source_par", ParamValue::F32(8.0 / 7.0)));
        let fc = FrameConstants::placed((256, 240), (2560, 1440), false, &params);
        assert_eq!(fc.tube_size, [1920, 1440]);
        assert!((fc.line_pitch_mm - 0.6042).abs() < 1e-4);
        assert_eq!(fc.tube_rect_px, [320.0, 0.0, 2240.0, 1440.0]);
    }
}
