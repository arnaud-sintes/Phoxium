//! The composite video module (T-31, optional, off by default) of the slot `signal` (voltage domain,
//! native -> native, before the video amplifier): composite or S-Video encode, channel, luma/chroma
//! separation and decode (`shaders/composite.comp`, parameters `params/composite.toml`, oracle
//! `crt_model::composite`, tests `tests/stage_composite.rs`).
//!
//! The CPU builds the oracle's [`Composite`] from the parameters ([`model`]) and pushes its derived
//! constants ([`plan`]: subcarrier cycles per pixel, the line advance and this frame's phase, computed
//! in f64 so that the frame counter never loses precision, the ACC gain, the kernel sigmas in samples).
//! The shader runs one workgroup of 64 threads per 64 output pixels of a row (`TILE`), the line simulated
//! in shared memory (28 KiB). `composite_enable` off: the stage is an identity and the pipeline skips it.

use ash::vk;
use crt_model::composite::{Composite, Connection, Separation, Standard, NTSC_LINE_RATE_KHZ, PAL_LINE_RATE_KHZ};
use crt_model::composite::{NTSC_SUBCARRIER_MHZ, PAL_SUBCARRIER_MHZ};

use super::{ComputePass, Stage};
use crate::gpu::{Context, FrameConstants};
use crate::params::{self, Params};
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/composite.comp.spv"));

/// Output pixels per workgroup (`TILE` and `local_size_x` of the shader).
const TILE: u32 = 64;

/// The push constants of `composite.comp` (std430, 48 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Push {
    cycles_per_px: f32,
    line_advance: f32,
    frame_phase: f32,
    acc_gain: f32,
    sigma_ch: f32,
    sigma_a: f32,
    sigma_b: f32,
    box_len: f32,
    demod_angle: f32,
    axis_turn: f32,
    saturation: f32,
    flags: u32,
}

/// Flag bits of [`Push::flags`].
const FLAG_SVIDEO: u32 = 1;
const FLAG_PAL: u32 = 2;
const FLAG_COMB: u32 = 4;

/// The oracle's settings for `params`: the pixel clock of the amplifier (`amp_pixel_clock_mhz`); 0 for the
/// subcarrier or the line rate means the standard's value; non-positive clocks and bandwidths are lifted
/// to small positive values (the shader never sees a division by zero).
pub(crate) fn model(p: &Params) -> Composite {
    let pal = p.composite_standard == params::COMPOSITE_STANDARD_PAL;
    let or = |v: f32, standard: f64| if v > 0.0 { f64::from(v) } else { standard };
    Composite {
        standard: if pal { Standard::Pal } else { Standard::Ntsc },
        connection: if p.composite_connection == params::COMPOSITE_CONNECTION_SVIDEO {
            Connection::SVideo
        } else {
            Connection::Composite
        },
        separation: if p.composite_separation == params::COMPOSITE_SEPARATION_COMB {
            Separation::Comb
        } else {
            Separation::Notch
        },
        pixel_clock_mhz: f64::from(p.amp_pixel_clock_mhz).max(0.1),
        subcarrier_mhz: or(
            p.composite_subcarrier_mhz,
            if pal { PAL_SUBCARRIER_MHZ } else { NTSC_SUBCARRIER_MHZ },
        ),
        line_rate_khz: or(
            p.composite_line_rate_khz,
            if pal { PAL_LINE_RATE_KHZ } else { NTSC_LINE_RATE_KHZ },
        ),
        frame_advance_deg: f64::from(p.composite_frame_advance_deg),
        phase_deg: f64::from(p.composite_phase_deg),
        i_bandwidth_mhz: f64::from(p.composite_i_bandwidth_mhz).max(0.01),
        q_bandwidth_mhz: f64::from(p.composite_q_bandwidth_mhz).max(0.01),
        channel_bandwidth_mhz: f64::from(p.composite_bandwidth_mhz).max(0.0),
        hue_deg: f64::from(p.composite_hue_deg),
        saturation: f64::from(p.composite_saturation),
    }
}

/// The push constants of frame `frame` for `params`.
pub(super) fn plan(p: &Params, frame: u32) -> Push {
    let m = model(p);
    let (sigma_a, sigma_b) = m.demod_sigmas();
    let sigma_ch = if m.connection == Connection::Composite {
        m.sigma_samples(m.channel_bandwidth_mhz)
    } else {
        0.0
    };
    let mut flags = 0;
    if m.connection == Connection::SVideo {
        flags |= FLAG_SVIDEO;
    }
    if m.standard == Standard::Pal {
        flags |= FLAG_PAL;
    }
    if m.separation == Separation::Comb {
        flags |= FLAG_COMB;
    }
    Push {
        cycles_per_px: m.cycles_per_px() as f32,
        line_advance: m.line_advance_cycles() as f32,
        frame_phase: m.frame_phase_cycles(frame) as f32,
        acc_gain: m.acc_gain() as f32,
        sigma_ch: sigma_ch as f32,
        sigma_a: sigma_a as f32,
        sigma_b: sigma_b as f32,
        box_len: m.box_len_samples() as f32,
        demod_angle: (m.axis_turn_rad() + m.hue_deg.to_radians()) as f32,
        axis_turn: m.axis_turn_rad() as f32,
        saturation: m.saturation as f32,
        flags,
    }
}

/// The composite stage (see the module documentation).
#[derive(Default)]
pub(super) struct CompositeStage {
    pass: ComputePass,
    push: Push,
}

impl Stage for CompositeStage {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        self.pass.create(d, SPV, size_of::<Push>() as u32)
    }

    /// Off: the exact identity, the pipeline skips the slot.
    fn is_identity(&self, params: &Params, _fc: &FrameConstants) -> bool {
        !params.composite_enable
    }

    unsafe fn prepare(&mut self, _ctx: &Context, fc: &FrameConstants, params: &Params, runs: bool) -> Res<()> {
        if runs {
            self.push = plan(params, fc.frame_index);
        }
        Ok(())
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        // SAFETY: `Push` is `#[repr(C)]` plain data of 12 four-byte fields, no padding.
        let bytes = std::slice::from_raw_parts((&self.push as *const Push).cast::<u8>(), size_of::<Push>());
        self.pass.dispatch_group(d, cb, set, bytes, size, (TILE, 1));
    }

    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.pass.destroy(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The push block is 48 bytes in the shader's order.
    #[test]
    fn push_layout_matches_the_shader() {
        assert_eq!(size_of::<Push>(), 48);
        assert_eq!(std::mem::offset_of!(Push, box_len), 28);
        assert_eq!(std::mem::offset_of!(Push, flags), 44);
        let glsl = include_str!("../../../../../shaders/composite.comp");
        let order = [
            "float cycles_per_px;",
            "float line_advance;",
            "float frame_phase;",
            "float acc_gain;",
            "float sigma_ch;",
            "float sigma_a;",
            "float sigma_b;",
            "float box_len;",
            "float demod_angle;",
            "float axis_turn;",
            "float saturation;",
            "uint flags;",
        ];
        let pos: Vec<usize> = order
            .iter()
            .map(|m| glsl.find(m).unwrap_or_else(|| panic!("{m}")))
            .collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "push members out of order");
        assert!(glsl.contains("const int TILE = 64;") && glsl.contains("local_size_x = 64"));
        assert_eq!(TILE, 64);
    }

    /// The defaults: off, NTSC composite with a notch at the amplifier's 5.37 MHz clock (2/3 cycle per
    /// pixel, 180 degrees per line and per frame); the frame phase alternates.
    #[test]
    fn defaults_and_plan() {
        let p = Params::default();
        assert!(!p.composite_enable);
        let m = model(&p);
        assert_eq!(
            (m.standard, m.connection, m.separation),
            (Standard::Ntsc, Connection::Composite, Separation::Notch)
        );
        assert_eq!(m.subcarrier_mhz, NTSC_SUBCARRIER_MHZ);
        let push0 = plan(&p, 0);
        let push1 = plan(&p, 1);
        assert!((push0.cycles_per_px - 0.6666).abs() < 1e-3);
        assert!((push0.line_advance - 0.5).abs() < 1e-4);
        assert_eq!((push0.frame_phase, push1.frame_phase), (0.0, 0.5));
        assert_eq!(push0.flags, 0);
        // A frame counter far past f32 precision still alternates exactly.
        assert_eq!(plan(&p, 4_000_000_001).frame_phase, 0.5);
        let pal = Params {
            composite_standard: params::COMPOSITE_STANDARD_PAL,
            composite_separation: params::COMPOSITE_SEPARATION_COMB,
            composite_connection: params::COMPOSITE_CONNECTION_SVIDEO,
            ..p
        };
        assert_eq!(model(&pal).subcarrier_mhz, PAL_SUBCARRIER_MHZ);
        assert_eq!(plan(&pal, 0).flags, FLAG_SVIDEO | FLAG_PAL | FLAG_COMB);
        assert_eq!(plan(&pal, 0).sigma_ch, 0.0, "no channel filter on S-Video");
    }
}
