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

use crate::gpu::FrameConstants;
use crate::params::{Params, RASTER_ASPECT_FILL_TUBE};

/// How the picture takes its shape on the tube (`params.raster_aspect`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RasterAspect {
    /// Aspect = source width x PAR / source height.
    Par(f64),
    /// The picture fills the whole visible area.
    FillTube,
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
    /// Whole host pixels per source row (when at least 1).
    pub integer: bool,
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
            } else {
                RasterAspect::Par(f64::from(params.source_par))
            },
            line_multiplier: params.line_multiplier.max(1) as u32,
            integer,
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
    };
    if !(nw > 0.0 && nh > 0.0 && tw > 0.0 && th > 0.0 && vw > 0.0 && vh > 0.0 && i.height_fraction > 0.0) {
        return empty;
    }
    let aspect = match i.aspect {
        RasterAspect::Par(par) => nw * par / nh,
        RasterAspect::FillTube => vw / vh,
    };
    let (wr, hr) = if aspect <= vw / vh {
        (vh * aspect, vh)
    } else {
        (vw, vw / aspect)
    };
    let mut s = (tw / wr).min(i.height_fraction * th / hr);
    if i.integer {
        let row_px = s * hr / nh;
        if row_px >= 1.0 {
            // The epsilon keeps an exact whole factor (such as 1440 / 290 x 290 / 240 = 6) from falling
            // to the one below through rounding.
            s = (row_px + 1e-9).floor() * nh / hr;
        }
    }
    let (w, h) = ((wr * s).round(), (hr * s).round());
    let (x0, y0) = (((tw - w) / 2.0).floor(), ((th - h) / 2.0).floor());
    Placement {
        rect_px: [x0 as f32, y0 as f32, (x0 + w) as f32, (y0 + h) as f32],
        px_per_mm: s,
        line_pitch_mm: hr / (nh * f64::from(i.line_multiplier.max(1))),
        display_aspect: aspect,
        raster_mm: (wr, hr),
    }
}

impl FrameConstants {
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
            // `amp_samples` (a preset sets it from its bandwidth, `presets::recommended_amp_k`).
            amp_k: params.amp_samples.max(1),
            ..Self::new(native, target, integer)
        }
    }

    /// Grows the tube image around the placed raster by the spot's reach (T-37, review of T-36b): 4 sigma
    /// of the spot at twice the peak current (`BEAM_REACH_CURRENT` and `BEAM_REACH_SIGMAS` of `beam.glsl`;
    /// the tail's sigma when a tail is on) plus the largest misconvergence (static plus radial, mm), rounded
    /// up to whole pixels, per side and per axis, limited by the target on each axis (the raster stays
    /// centred). The beam is then evaluated over the margin, so the skirt past the raster's ends is drawn
    /// instead of cut; `raster_margin` tells the shaders where the raster is. Unplaced constants are
    /// returned unchanged.
    ///
    /// Not yet applied by `Pipeline::record` (T-37 report): with the `screen` slot bypassed, the
    /// nearest-neighbour resample would stretch the raster over the margin, and the surround tests of
    /// `gpu_identity` and `stage_source` check the tube rectangle without margin. Tests inject it.
    pub fn with_skirt_margin(mut self, params: &Params) -> Self {
        if self.px_per_mm.is_nan() || self.px_per_mm <= 0.0 {
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

    fn pvm(native: (u32, u32), target: (u32, u32), aspect: RasterAspect) -> PlacementInput {
        PlacementInput {
            native,
            target,
            visible_mm: (290.0 * 4.0 / 3.0, 290.0),
            height_fraction: 1.0,
            aspect,
            line_multiplier: 1,
            integer: false,
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
