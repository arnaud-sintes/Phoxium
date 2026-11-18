//! Slot `phosphor` (linear light, tube -> tube): persistence as a frame feedback (T-41; CRT.md rule 8,
//! "Decay and persistence"; QUESTIONS PHY-12), `shaders/phosphor.comp`, parameters `params/phosphor.toml`.
//! Ageing and burn-in (T-42, T-43) come later in the same slot.
//!
//! The input is the light of the frame without persistence (frame-integrated). Per channel the impulse
//! response is the Kuhn (2002) P22 fit of `crt_model::phosphor` (red purely exponential, green and blue
//! with a power-law tail, `phosphor_tail_strength_rgb` scales the tail), discretised per output frame
//! period T = 1 / `phosphor_frame_rate_hz` by [`crt_model::phosphor::DecayFit::frame_feedback`]: four
//! geometric terms (rho_j, c_j) whose trail sum_j c_j rho_j^(k - 1) equals E_k / E_0 exactly at k = 1, 2,
//! 3 and follows it within about 1 % up to 1 s (60 Hz; 2.5 % at 120 Hz), and the weight w0 = 1 / (1 + sum_j c_j / (1 - rho_j))
//! that makes a static picture keep its brightness (steady-state gain 1). w0 is recomputed here from
//! the f32 constants the GPU receives, so the gain is 1 for the values actually used. The weights are
//! recomputed only when the frame rate or a tail strength changes.
//!
//! History: three RGBA32F storage images of the tube size (12 floats per texel: 4 terms x 3 channels),
//! owned by the stage (ARCHITECTURE 3.7), in their own descriptor set (set 1), read and written in
//! place by the shader, allocated on first use and on a tube size change, freed while persistence is
//! off. FP32, not FP16: the slow term has rho close to 1 (0.980 at 60 Hz), and FP16 stores, which
//! truncate on the dev GPU, would lose up to one FP16 step (2^-10 relative) per frame, a steady-state
//! bias of up to 2^-10 / (1 - rho), about 4 % of that term. The history is cleared (the shader does not
//! read it, `mode` 1) after an allocation, after [`Stage::clear_history`] (source size change, new
//! capture session: `Renderer::clear_history`), and when the stage did not run on the previous frame.
//!
//! The stage records its own barriers on its history images (they are not pipeline images): a layout
//! transition after an allocation, otherwise a compute-to-compute memory barrier that orders this
//! frame's read-modify-write after the previous frame's, across frames in flight (one queue).
//!
//! `phosphor_persistence` off (the default): the pass is the exact copy of `resample.comp` (same size,
//! FP16 texels copied without arithmetic), so the chain stays bit exact; the slot's enable flag off
//! skips it as before.

use ash::vk;
use crt_model::phosphor::{DecayFit, FrameFeedback, KUHN_BLUE, KUHN_GREEN, KUHN_RED, TRAIL_HORIZON_MS};

use super::{binding, compute_pipeline, push, ComputePass, Domain, Registration, Stage, GROUP};
use crate::gpu::headless::GpuImage;
use crate::gpu::{barrier, Context, FrameConstants};
use crate::params::Params;
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/phosphor.comp.spv"));
/// The exact copy used while persistence is off (`shaders/resample.comp`, same-size input and output).
const COPY_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/resample.comp.spv"));

/// Format of the history images.
pub(in crate::gpu) const HISTORY_FORMAT: vk::Format = vk::Format::R32G32B32A32_SFLOAT;
/// History images: 3 x RGBA = 4 terms x 3 channels.
const HISTORY_IMAGES: usize = 3;
/// Decay fits per channel, red, green, blue (CRT.md "Decay and persistence", Kuhn 2002). [S][D]
const FITS: [DecayFit<'static>; 3] = [KUHN_RED, KUHN_GREEN, KUHN_BLUE];

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "phosphor",
    input: Domain::Tube,
    output: Domain::Tube,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Phosphor>::default()
}

/// Push constants of `phosphor.comp` (std430, 116 bytes, no padding).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(in crate::gpu) struct Push {
    /// rgb: weight w0 per channel; a unused.
    pub w0: [f32; 4],
    /// Per channel (r, g, b): rho of terms 0 to 3.
    pub rho: [[f32; 4]; 3],
    /// Per channel (r, g, b): c of terms 0 to 3.
    pub c: [[f32; 4]; 3],
    /// 0: history valid, 1: history cleared (not read).
    pub mode: u32,
}

/// The push constants (mode 0) of a frame rate and per-channel tail strengths: the recurrence of
/// [`DecayFit::frame_feedback`] per channel, rounded to f32, with w0 recomputed from the rounded
/// values (exact steady-state gain 1 of the recurrence the GPU runs). The rate is clamped to [1, 1000]
/// Hz and the strengths to [0, 10] (the soft ranges of `params/phosphor.toml` are narrower).
pub(in crate::gpu) fn weights(rate_hz: f32, tail: [f32; 3]) -> Push {
    let frame_ms = 1000.0 / f64::from(rate_hz.clamp(1.0, 1000.0));
    let mut p = Push::default();
    for ch in 0..3 {
        let scale = f64::from(tail[ch]).clamp(0.0, 10.0);
        let fb: FrameFeedback = FITS[ch]
            .with_tail_scale(scale)
            .frame_feedback(frame_ms, TRAIL_HORIZON_MS);
        let rho = fb.rho.map(|x| x as f32);
        let c = fb.c.map(|x| x as f32);
        let sum: f64 = (0..4).map(|j| f64::from(c[j]) / (1.0 - f64::from(rho[j]))).sum();
        p.w0[ch] = (1.0 / (1.0 + sum)) as f32;
        p.rho[ch] = rho;
        p.c[ch] = c;
    }
    p
}

#[derive(Default)]
struct Phosphor {
    /// The copy pass (persistence off); its set layout is the standard set 0 of both pipelines.
    pass: ComputePass,
    /// Set 1 of the persistence pipeline: the three history images.
    history_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    pool: vk::DescriptorPool,
    history_set: vk::DescriptorSet,
    history: [GpuImage; HISTORY_IMAGES],
    /// Size of the history images, (0, 0) when none.
    history_size: (u32, u32),
    /// The history must not be read by the next run.
    reset: bool,
    /// Push constants of the current weights and the inputs they were computed from.
    weights: Push,
    weights_key: Option<(u32, [u32; 3])>,
    /// Decided by `prepare` for the frame being recorded.
    on: bool,
    push: Push,
    /// The history images are new (layout `UNDEFINED`): the next record transitions them.
    fresh: bool,
    transition: bool,
}

impl Phosphor {
    /// # Safety
    /// The GPU no longer uses the history images.
    unsafe fn free_history(&mut self, d: &ash::Device) {
        for h in &mut self.history {
            h.destroy(d);
        }
        self.history_size = (0, 0);
    }

    /// # Safety
    /// `ctx` is live, the old images are freed and the set is not in use.
    unsafe fn alloc_history(&mut self, ctx: &Context, size: (u32, u32)) -> Res<()> {
        let extent = vk::Extent2D {
            width: size.0,
            height: size.1,
        };
        for h in &mut self.history {
            *h = GpuImage::create(ctx, extent, HISTORY_FORMAT, vk::ImageUsageFlags::STORAGE)?;
        }
        let infos: Vec<[vk::DescriptorImageInfo; 1]> = self
            .history
            .iter()
            .map(|h| {
                [vk::DescriptorImageInfo::default()
                    .image_view(h.view)
                    .image_layout(vk::ImageLayout::GENERAL)]
            })
            .collect();
        let writes: Vec<vk::WriteDescriptorSet> = infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(self.history_set)
                    .dst_binding(i as u32)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(info)
            })
            .collect();
        ctx.device.update_descriptor_sets(&writes, &[]);
        self.history_size = size;
        self.fresh = true;
        self.reset = true;
        Ok(())
    }
}

impl Stage for Phosphor {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        self.pass.create(d, COPY_SPV, 0)?;
        let s = vk::ShaderStageFlags::COMPUTE;
        let bindings: Vec<_> = (0..HISTORY_IMAGES as u32)
            .map(|b| binding(b, vk::DescriptorType::STORAGE_IMAGE, s))
            .collect();
        self.history_layout =
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?;
        // The standard set 0 (shared with the copy pass, so the pipeline's sets fit both) and set 1.
        let sets = [self.pass.set_layout, self.history_layout];
        let ranges = [vk::PushConstantRange::default()
            .stage_flags(s)
            .offset(0)
            .size(size_of::<Push>() as u32)];
        self.layout = d.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&sets)
                .push_constant_ranges(&ranges),
            None,
        )?;
        self.pipeline = compute_pipeline(d, self.layout, SPV)?;
        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(HISTORY_IMAGES as u32)];
        self.pool = d.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes),
            None,
        )?;
        let layouts = [self.history_layout];
        self.history_set = d.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.pool)
                .set_layouts(&layouts),
        )?[0];
        self.reset = true;
        Ok(())
    }

    unsafe fn prepare(&mut self, ctx: &Context, fc: &FrameConstants, params: &Params, runs: bool) -> Res<()> {
        let d = &ctx.device;
        self.on = runs && params.phosphor_persistence;
        self.transition = false;
        if !self.on {
            // The history no longer follows the picture.
            self.reset = true;
            if !params.phosphor_persistence && self.history_size != (0, 0) {
                d.device_wait_idle()?;
                self.free_history(d);
            }
            return Ok(());
        }
        let size = fc.domain_size(Domain::Tube);
        if size != self.history_size {
            d.device_wait_idle()?;
            self.free_history(d);
            self.alloc_history(ctx, size)?;
        }
        let tail = params.phosphor_tail_strength_rgb;
        let key = (params.phosphor_frame_rate_hz.to_bits(), tail.map(f32::to_bits));
        if self.weights_key != Some(key) {
            self.weights = weights(params.phosphor_frame_rate_hz, tail);
            self.weights_key = Some(key);
        }
        self.push = Push {
            mode: u32::from(self.reset),
            ..self.weights
        };
        self.transition = std::mem::take(&mut self.fresh);
        self.reset = false;
        Ok(())
    }

    fn clear_history(&mut self) {
        self.reset = true;
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        if !self.on {
            self.pass.dispatch(d, cb, set, &[], size);
            return;
        }
        let compute = vk::PipelineStageFlags2::COMPUTE_SHADER;
        let rw = vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE;
        if self.transition {
            for h in &self.history {
                barrier(
                    d,
                    cb,
                    h.image,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::GENERAL,
                    vk::PipelineStageFlags2::NONE,
                    vk::AccessFlags2::NONE,
                    compute,
                    rw,
                );
            }
        } else {
            // The previous frame's read-modify-write of the history before this frame's.
            let m = [vk::MemoryBarrier2::default()
                .src_stage_mask(compute)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(compute)
                .dst_access_mask(rw)];
            d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
        }
        d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, self.pipeline);
        d.cmd_bind_descriptor_sets(
            cb,
            vk::PipelineBindPoint::COMPUTE,
            self.layout,
            0,
            &[set, self.history_set],
            &[],
        );
        push(d, cb, self.layout, vk::ShaderStageFlags::COMPUTE, &self.push);
        d.cmd_dispatch(cb, size.0.div_ceil(GROUP), size.1.div_ceil(GROUP), 1);
    }

    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.free_history(d);
        d.destroy_descriptor_pool(self.pool, None);
        d.destroy_pipeline(self.pipeline, None);
        d.destroy_pipeline_layout(self.layout, None);
        d.destroy_descriptor_set_layout(self.history_layout, None);
        self.pass.destroy(d);
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The push block is 116 bytes: w0 at 0, rho at 16, c at 64, mode at 112 (std430 of the shader).
    #[test]
    fn push_layout_matches_the_shader() {
        assert_eq!(size_of::<Push>(), 116);
        assert_eq!(std::mem::offset_of!(Push, rho), 16);
        assert_eq!(std::mem::offset_of!(Push, c), 64);
        assert_eq!(std::mem::offset_of!(Push, mode), 112);
        let glsl = include_str!("../../../../../shaders/phosphor.comp");
        for m in ["vec4 w0;", "vec4 rho[3];", "vec4 c[3];", "uint mode;"] {
            assert!(glsl.contains(m), "{m}");
        }
    }

    /// The f32 weights keep the steady-state gain at 1 (f64 check of the rounded constants).
    #[test]
    fn weights_have_unit_steady_state_gain() {
        for rate in [50.0, 60.0, 120.0] {
            let p = weights(rate, [1.0; 3]);
            for ch in 0..3 {
                let sum: f64 = (0..4)
                    .map(|j| f64::from(p.c[ch][j]) / (1.0 - f64::from(p.rho[ch][j])))
                    .sum();
                let gain = f64::from(p.w0[ch]) * (1.0 + sum);
                assert!((gain - 1.0).abs() < 1e-6, "{rate} Hz channel {ch}: {gain}");
            }
        }
    }
}
