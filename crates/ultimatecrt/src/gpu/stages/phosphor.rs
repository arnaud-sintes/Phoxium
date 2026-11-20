//! Slot `phosphor` (linear light, tube -> tube): persistence as a frame feedback (T-41; CRT.md rule 8,
//! "Decay and persistence"; QUESTIONS PHY-12), `shaders/phosphor.comp`, parameters `params/phosphor.toml`.
//! Ageing and burn-in (T-42, T-43) come later in the same slot.
//!
//! The input is the light of the frame without persistence (frame-integrated). Per channel the impulse
//! response is the Kuhn (2002) P22 fit of `crt_model::phosphor` (red purely exponential, green and blue
//! with a power-law tail, `phosphor_tail_strength_rgb` scales the tail), discretised for a frame period
//! T (the fit rate, see "Time base" below) by [`crt_model::phosphor::DecayFit::frame_feedback`]: four
//! geometric terms (rho_j, c_j) whose trail sum_j c_j rho_j^(k - 1) equals E_k / E_0 exactly at k = 1, 2,
//! 3 and follows it within about 1 % up to 1 s (60 Hz; 2.5 % at 120 Hz), and the weight w0 = 1 / (1 + sum_j c_j / (1 - rho_j))
//! that makes a static picture keep its brightness (steady-state gain 1). w0 is recomputed here from
//! the f32 constants the GPU receives, so the gain is 1 for the values actually used. The fits are
//! recomputed only when the fit rate or a tail strength changes; the real-time step is applied per frame.
//!
//! History: three RGBA32F storage images of the tube size (12 floats per texel: 4 terms x 3 channels),
//! owned by the stage (ARCHITECTURE 3.7), in their own descriptor set (set 1), read and written in
//! place by the shader, allocated on first use and on a tube size change, freed while persistence is
//! off. FP32, not FP16: the slow term has rho close to 1 (0.980 at 60 Hz), and truncating FP16 stores
//! (the dev GPU without `round_f16`) would lose up to one FP16 step (2^-10 relative) per frame, a
//! steady-state bias of up to 2^-10 / (1 - rho) = 4.8 % of that term; with `round_f16` the error would be
//! unbiased but still a random walk of the same order, so the history stays FP32 (the output store is
//! rounded by `round_f16`). The history is cleared (the shader does not read it, `mode` 1, and primes it
//! to the steady state of the frame's input so a static picture shows y = x at once) after an
//! allocation, after [`Stage::clear_history`] (source size change, new capture session:
//! `Renderer::clear_history`), and when the stage did not run on the previous frame. An abandoned frame
//! ([`Stage::abandon`]) gives the pending clear and layout transition back to the next frame.
//!
//! **Time base (review R-10, R-11).** `phosphor_frame_rate_hz` 0, the default, is real time: the trail
//! decays by the time that really passed between two output frames, so it lasts as long on a 144 Hz
//! display as on a 60 Hz one. The fit is made for the automatic rate, the trimmed mean of the last
//! [`RATE_WINDOW`] measured frame periods (`FrameConstants::frame_time_ms`; periods above
//! [`crt_model::phosphor::MAX_FRAME_STEP_MS`] are pauses, dropped from the estimate, and the trim drops
//! stalls and dropped frames), clamped to 10 to 240 Hz, whole hertz, and refitted only when the estimate
//! moves by more than [`RATE_HYSTERESIS`] or settles on a new value (a refit restarts the history, primed
//! to the picture). Each frame then applies the fit for its own
//! measured period, clamped to `MAX_FRAME_STEP_MS` ([`FrameFeedback::at_step`]: rho_j' = rho_j^(dt / T),
//! c_j' keeps each term's share of the steady state, so a static picture keeps its brightness under any
//! frame jitter). The step of frame m (its measured period, the time since frame m - 1) decays the history
//! that frame m leaves to frame m + 1: the period of frame m + 1 is not known yet, so the decay lags the
//! measured periods by one frame, which changes the time of a trail by at most one frame's jitter. A
//! positive value is the manual override and the previous behaviour: the trail decays per output frame
//! with a fixed period 1 / rate, whatever time really passed (deterministic; the stage tests use it). The
//! fits are cached per (rate, channel, tail strength).
//!
//! The stage records its own barriers on its history images (they are not pipeline images): a layout
//! transition after an allocation, otherwise a compute-to-compute memory barrier that orders this
//! frame's read-modify-write after the previous frame's, across frames in flight (one queue).
//!
//! Saturation (T-40, `phosphor_saturation`, `phosphor_saturation_rgb`; CRT.md "Saturation and ageing"):
//! L = I / (1 + s I) per channel on the light of each tube pixel, BEFORE the persistence history. The
//! efficiency droop depends on the instantaneous beam current, so the frame's light is compressed first;
//! the history then holds light that was already emitted, which decays without saturating again. With the
//! recurrence of gain 1 the steady state of a static picture is exactly f(x), and a single excitation
//! keeps the persistence ratios E_k / E_0 whatever its level. The saturation sits in this slot (after
//! `screen`, not inside it) because the persistence stage is the only one that is not memoryless, and the
//! order within it matters. Approximation, stated: with the area-averaged mask the light of a tube pixel
//! is the mean over several phosphor elements that are individually brighter (by 1 / fill factor), so
//! s is a per-pixel effective strength, not the strength of one element; so its effect depends on the mask
//! tier, the host resolution and the headroom mode (known deviation, review R-12, listed in DESIGN section
//! 12; not fixed for now, see `shaders/include/saturation.glsl`). A negative or NaN strength acts as 0 (the
//! shader clamps it, review R-05; oracle `crt_model::phosphor::effective_saturation_strength`). Saturation on, persistence
//! off: `shaders/saturation.comp`; both on: `shaders/phosphor.comp` (same function, one pass); a change of
//! `phosphor_saturation` restarts the persistence history. Both off (the defaults): the copy pass below.
//!
//! `phosphor_persistence` and `phosphor_saturation` off (the defaults): the stage is an exact identity
//! ([`Stage::is_identity`]) and the pipeline skips the slot like a disabled one: no pass, no image, no
//! barrier, the next slot reads the `screen` image, so the chain stays bit exact and the full-image copy
//! is not paid. `prepare` then sees `runs` false (history freed, clear armed), so persistence on again
//! behaves like the off to on transition. The copy pass (`COPY_SPV`) remains for a stage that runs while
//! both are off, which no frame does any more; it is kept because the pass is the stage's standard one.

use std::collections::HashMap;

use ash::vk;
use crt_model::phosphor::{
    frame_step_ms, is_rate_sample, DecayFit, FrameFeedback, FEEDBACK_TERMS, KUHN_BLUE, KUHN_GREEN, KUHN_RED,
    TRAIL_HORIZON_MS,
};

use super::{binding, compute_pipeline, push, ComputePass, Domain, Registration, Stage, GROUP};
use crate::gpu::headless::GpuImage;
use crate::gpu::{barrier, Context, FrameConstants};
use crate::params::Params;
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/phosphor.comp.spv"));
/// The exact copy used while persistence is off (`shaders/resample.comp`, same-size input and output).
const COPY_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/resample.comp.spv"));
/// Saturation alone (`shaders/saturation.comp`), used while persistence is off and saturation is on.
const SATURATION_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/saturation.comp.spv"));

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

/// The push constants (mode 0) of a frame rate and per-channel tail strengths, decaying per frame of
/// period 1 / rate (the manual mode): the recurrence of [`DecayFit::frame_feedback`] per channel, rounded
/// to f32, with w0 recomputed from the rounded values (exact steady-state gain 1 of the recurrence the GPU
/// runs). The rate is clamped to [1, 1000] Hz and the strengths to [0, 10] (the soft ranges of
/// `params/phosphor.toml` are narrower).
#[cfg(test)]
fn weights(rate_hz: f32, tail: [f32; 3]) -> Push {
    let mut p = Push::default();
    for (ch, &s) in tail.iter().enumerate() {
        set_channel(&mut p, ch, per_frame_f32(&channel_fit(rate_hz, ch, s)));
    }
    p
}

/// (w0, rho, c) of one channel.
type ChannelWeights = (f32, [f32; 4], [f32; 4]);

/// The f64 fit of one channel at a frame rate. The four-term fit (a grid search when the channel has a
/// power-law tail) depends on the rate and on the tail strength s: s changes the shape of the trail
/// r_k = (X_k + s T_k) / (X_0 + s T_0) (exponential part X, tail part T), not only its scale, so the
/// rates must be refitted; the stage caches the result per (rate, channel, strength) instead.
fn channel_fit(rate_hz: f32, ch: usize, tail: f32) -> FrameFeedback {
    let scale = if tail.is_finite() {
        f64::from(tail).clamp(0.0, 10.0)
    } else {
        1.0
    };
    FITS[ch]
        .with_tail_scale(scale)
        .frame_feedback(fit_period_ms(rate_hz), TRAIL_HORIZON_MS)
}

/// The frame period, ms, of a fit rate (clamped to [1, 1000] Hz).
fn fit_period_ms(rate_hz: f32) -> f64 {
    1000.0 / f64::from(rate_hz.clamp(1.0, 1000.0))
}

/// The fit as the GPU receives it, decaying per frame: rho and c rounded to f32, w0 recomputed from the
/// rounded values (steady-state gain 1 of the recurrence the GPU runs).
fn per_frame_f32(fb: &FrameFeedback) -> ChannelWeights {
    let rho = fb.rho.map(|x| x as f32);
    let c = fb.c.map(|x| x as f32);
    let sum: f64 = (0..4).map(|j| f64::from(c[j]) / (1.0 - f64::from(rho[j]))).sum();
    ((1.0 / (1.0 + sum)) as f32, rho, c)
}

/// The fit `fb` (made for the period `fit_ms`) for one frame that lasted `step_ms`, as the GPU receives it
/// (real time, [`FrameFeedback::at_step`]): rho' rounded to f32 first, then c' = g (1 - rho') from the
/// rounded rho' and the f64 share g = c / (1 - rho) of the fit, so the steady state stays g x for every
/// frame step; w0 from the rounded values (a term whose rho' rounds to 1 counts with its share g).
fn real_time_f32(fb: &FrameFeedback, fit_ms: f64, step_ms: f64) -> ChannelWeights {
    let at = fb.at_step(fit_ms, step_ms);
    let mut rho = [0.0_f32; FEEDBACK_TERMS];
    let mut c = [0.0_f32; FEEDBACK_TERMS];
    let mut sum = 0.0;
    for j in 0..FEEDBACK_TERMS {
        let g = fb.c[j] / (1.0 - fb.rho[j]);
        rho[j] = at.rho[j] as f32;
        let open = 1.0 - f64::from(rho[j]);
        c[j] = (g * open) as f32;
        sum += if open > 0.0 { f64::from(c[j]) / open } else { g };
    }
    ((1.0 / (1.0 + sum)) as f32, rho, c)
}

fn set_channel(p: &mut Push, ch: usize, (w0, rho, c): ChannelWeights) {
    p.w0[ch] = w0;
    p.rho[ch] = rho;
    p.c[ch] = c;
}

/// Range of the automatic trail frame rate (`phosphor_frame_rate_hz` = 0), Hz.
const AUTO_RATE_HZ: (f32, f32) = (10.0, 240.0);
/// Rate of the automatic mode before a frame period was measured, Hz.
const AUTO_RATE_DEFAULT_HZ: f32 = 60.0;
/// Number of measured frame periods the automatic rate is estimated from.
const RATE_WINDOW: usize = 31;
/// Periods dropped at each end of the sorted window (a full window): the estimate is the mean of the middle
/// 15, so up to 8 stalls or dropped frames in the window do not move it, and a period that alternates
/// between two values (frame pacing) gives their mean, not one of them.
const RATE_TRIM: usize = 8;
/// Relative change of the automatic rate estimate that refits the trail at once. A smaller change refits
/// only once the estimate has held the same whole-hertz value for a full window (a settled new rate). The
/// per-frame real-time step already follows the measured period, so the fit rate only needs to be close;
/// a refit (a grid search, up to a few ms, and a history restart) must not happen on frame-time jitter.
const RATE_HYSTERESIS: f32 = 0.05;

/// The automatic rate: the last [`RATE_WINDOW`] valid frame periods and the rate the fit was made for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct AutoRate {
    periods_ms: [f32; RATE_WINDOW],
    len: usize,
    next: usize,
    /// The rate of the current fit, Hz (0: none yet).
    fit_hz: f32,
    /// A whole-hertz estimate other than the fit rate, and for how many frames in a row it was seen.
    candidate_hz: f32,
    candidate_frames: usize,
}

impl AutoRate {
    /// Adds a measured frame period when it is a rate sample ([`is_rate_sample`]: a measurement no longer
    /// than `MAX_FRAME_STEP_MS`); pauses and missing measurements are dropped.
    fn observe(&mut self, frame_ms: f32) {
        if is_rate_sample(f64::from(frame_ms)) {
            self.periods_ms[self.next] = frame_ms;
            self.next = (self.next + 1) % RATE_WINDOW;
            self.len = (self.len + 1).min(RATE_WINDOW);
        }
    }

    /// The trimmed mean period (a quarter of the samples dropped at each end, [`RATE_TRIM`] in a full
    /// window) as a rate, clamped to [`AUTO_RATE_HZ`] and rounded to whole hertz; [`AUTO_RATE_DEFAULT_HZ`]
    /// before any sample.
    fn estimate_hz(&self) -> f32 {
        if self.len == 0 {
            return AUTO_RATE_DEFAULT_HZ;
        }
        let mut v = self.periods_ms;
        let v = &mut v[..self.len];
        v.sort_by(f32::total_cmp);
        let trim = (self.len * RATE_TRIM) / RATE_WINDOW;
        let mid = &v[trim..self.len - trim];
        let mean = mid.iter().map(|&x| f64::from(x)).sum::<f64>() / mid.len() as f64;
        ((1000.0 / mean) as f32).clamp(AUTO_RATE_HZ.0, AUTO_RATE_HZ.1).round()
    }

    /// The fit rate after this frame's estimate: the estimate when there is no fit yet, when it moved by
    /// more than [`RATE_HYSTERESIS`], or when it held one other value for [`RATE_WINDOW`] frames in a row;
    /// otherwise the current fit rate.
    fn fit_rate_hz(&mut self) -> f32 {
        let est = self.estimate_hz();
        if est == self.fit_hz {
            self.candidate_frames = 0;
        } else if est == self.candidate_hz {
            self.candidate_frames += 1;
        } else {
            self.candidate_hz = est;
            self.candidate_frames = 1;
        }
        if self.fit_hz <= 0.0
            || (est - self.fit_hz).abs() > RATE_HYSTERESIS * self.fit_hz
            || self.candidate_frames >= RATE_WINDOW
        {
            self.fit_hz = est;
            self.candidate_frames = 0;
        }
        self.fit_hz
    }
}
/// The trail frame rate of a frame: `manual_hz` when positive; else (automatic, value 0) the fit rate of
/// [`AutoRate`] after observing `frame_ms`. The measurement is observed in both modes, so switching to
/// automatic starts from the current display rate.
fn trail_rate_hz(manual_hz: f32, frame_ms: f32, auto: &mut AutoRate) -> f32 {
    auto.observe(frame_ms);
    if manual_hz > 0.0 {
        return manual_hz;
    }
    auto.fit_rate_hz()
}
#[derive(Default)]
struct Phosphor {
    /// The copy pass (persistence off); its set layout is the standard set 0 of both pipelines.
    pass: ComputePass,
    /// Saturation without persistence (the same standard set layout as `pass`).
    saturation_pass: ComputePass,
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
    /// The fits of the current frame and the inputs they were computed from (rate bits, tail bits).
    fits: [FrameFeedback; 3],
    fits_key: Option<(u32, [u32; 3])>,
    /// Channel fits per (rate bits, channel, tail strength bits).
    fit_cache: HashMap<(u32, usize, u32), FrameFeedback>,
    /// Number of channel fits computed (cache misses), for the tests.
    fits_computed: usize,
    /// The automatic rate (measured frame periods, rate of the fit).
    auto: AutoRate,
    /// Decided by `prepare` for the frame being recorded: persistence on, saturation on.
    on: bool,
    saturate: bool,
    /// Saturation of the previous frame (a change restarts the history, which holds saturated light).
    saturate_before: bool,
    push: Push,
    /// The history images are new (layout `UNDEFINED`): the next record transitions them.
    fresh: bool,
    transition: bool,
}

impl Phosphor {
    /// The channel fits through the per-channel cache: a change of one tail strength refits that channel
    /// only, and a rate or strength seen before costs no fit.
    fn cached_fits(&mut self, rate_hz: f32, tail: [f32; 3]) -> [FrameFeedback; 3] {
        std::array::from_fn(|ch| {
            let fits = &mut self.fits_computed;
            *self
                .fit_cache
                .entry((rate_hz.to_bits(), ch, tail[ch].to_bits()))
                .or_insert_with(|| {
                    *fits += 1;
                    channel_fit(rate_hz, ch, tail[ch])
                })
        })
    }

    /// `weights` through the cache (manual mode, per frame of period 1 / rate).
    #[cfg(test)]
    fn cached_weights(&mut self, rate_hz: f32, tail: [f32; 3]) -> Push {
        let mut p = Push::default();
        for (ch, fb) in self.cached_fits(rate_hz, tail).iter().enumerate() {
            set_channel(&mut p, ch, per_frame_f32(fb));
        }
        p
    }

    /// The push constants (mode 0) of a frame whose measured period is `frame_ms` (time base in the module
    /// doc), and whether the automatic mode refitted for a new rate (the history must restart: its terms
    /// belong to the old fit). Manual (`manual_hz` > 0): the fit of that rate, per frame. Automatic: the fit
    /// of the automatic rate for this frame's step, clamped ([`frame_step_ms`]; no measurement, the first
    /// frame, steps by the fit period).
    fn frame_weights(&mut self, manual_hz: f32, tail: [f32; 3], frame_ms: f32) -> (Push, bool) {
        let rate = trail_rate_hz(manual_hz, frame_ms, &mut self.auto);
        let key = (rate.to_bits(), tail.map(f32::to_bits));
        let refit = self.fits_key.is_some_and(|k| k.0 != key.0) && manual_hz <= 0.0;
        if self.fits_key != Some(key) {
            self.fits = self.cached_fits(rate, tail);
            self.fits_key = Some(key);
        }
        let mut p = Push::default();
        let fit_ms = fit_period_ms(rate);
        for (ch, fb) in self.fits.iter().enumerate() {
            let w = if manual_hz > 0.0 {
                per_frame_f32(fb)
            } else {
                let step = frame_step_ms(f64::from(frame_ms)).unwrap_or(fit_ms);
                real_time_f32(fb, fit_ms, step)
            };
            set_channel(&mut p, ch, w);
        }
        (p, refit)
    }

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
        self.saturation_pass.create(d, SATURATION_SPV, 0)?;
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

    /// Persistence and saturation both off: the exact copy, no history. The pipeline skips the slot (no
    /// pass) and calls `prepare` with `runs` false, which frees the history and arms the clear, so
    /// switching persistence on later starts like the first time (primed to the picture).
    fn is_identity(&self, params: &Params, _fc: &FrameConstants) -> bool {
        !params.phosphor_persistence && !params.phosphor_saturation
    }

    unsafe fn prepare(&mut self, ctx: &Context, fc: &FrameConstants, params: &Params, runs: bool) -> Res<()> {
        let d = &ctx.device;
        self.on = runs && params.phosphor_persistence;
        self.saturate = runs && params.phosphor_saturation;
        if self.saturate != self.saturate_before {
            // The history holds the light as it was saturated (or not): start it again.
            self.reset = true;
            self.saturate_before = self.saturate;
        }
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
        let (weights, refit) = self.frame_weights(
            params.phosphor_frame_rate_hz,
            params.phosphor_tail_strength_rgb,
            fc.frame_time_ms,
        );
        self.reset |= refit;
        self.push = Push {
            mode: u32::from(self.reset),
            ..weights
        };
        self.transition = std::mem::take(&mut self.fresh);
        self.reset = false;
        Ok(())
    }

    fn clear_history(&mut self) {
        self.reset = true;
    }

    fn abandon(&mut self) {
        // The abandoned frame neither transitioned the new images nor wrote the history it was to
        // clear: the next frame does both again.
        if self.on {
            self.fresh |= self.transition;
            self.reset |= self.push.mode != 0;
        }
        self.transition = false;
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        if !self.on {
            let pass = if self.saturate {
                &self.saturation_pass
            } else {
                &self.pass
            };
            pass.dispatch(d, cb, set, &[], size);
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
        self.saturation_pass.destroy(d);
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

    /// The cache gives the uncached weights, and a change of one tail strength refits that channel
    /// only; going back to seen values refits nothing.
    #[test]
    fn cached_weights_match_and_refit_one_channel() {
        let mut s = Phosphor::default();
        let a = s.cached_weights(60.0, [1.0; 3]);
        assert_eq!(a, weights(60.0, [1.0; 3]));
        assert_eq!(s.fits_computed, 3);
        let b = s.cached_weights(60.0, [1.0, 0.5, 1.0]);
        assert_eq!(b, weights(60.0, [1.0, 0.5, 1.0]));
        assert_eq!(s.fits_computed, 4, "only green refitted");
        assert_eq!(s.cached_weights(60.0, [1.0; 3]), a);
        assert_eq!(s.fits_computed, 4, "seen values: no fit");
        let _ = s.cached_weights(50.0, [1.0; 3]);
        assert_eq!(s.fits_computed, 7, "a new rate refits every channel");
    }

    /// Automatic rate: manual values win, 0 follows the median measured period (clamped, whole hertz),
    /// 60 Hz before any measurement; a lone dropped frame does not move it, a real rate change does after
    /// half the window, jitter within the hysteresis does not refit.
    #[test]
    fn automatic_rate() {
        let mut a = AutoRate::default();
        assert_eq!(trail_rate_hz(0.0, 0.0, &mut a), AUTO_RATE_DEFAULT_HZ);
        let mut a = AutoRate::default();
        assert_eq!(trail_rate_hz(0.0, 1000.0 / 144.0, &mut a), 144.0);
        // Dropped frames (13.9 ms) and a stall (200 ms) do not move a 144 Hz estimate.
        for k in 0..60 {
            let ms = match k % 10 {
                3 => 2000.0 / 144.0,
                7 if k == 37 => 200.0,
                _ => 1000.0 / 144.0,
            };
            assert_eq!(trail_rate_hz(0.0, ms, &mut a), 144.0, "frame {k}");
        }
        assert_eq!(trail_rate_hz(75.0, 20.0, &mut a), 75.0, "manual override");
        // The display goes to 60 Hz: followed, and settled on exactly 60 Hz.
        let mut n = 0;
        while trail_rate_hz(0.0, 1000.0 / 60.0, &mut a) != 60.0 {
            n += 1;
            assert!(n < 3 * RATE_WINDOW, "60 Hz not followed");
        }
        println!("60 Hz followed after {n} frames");
        // Frame pacing (alternating 16.3 / 17.1 ms) and alternating 0.8 / 1.2 periods at 144 Hz keep the
        // fit: no refit on jitter.
        for k in 0..100 {
            let ms = if k % 2 == 0 { 16.3 } else { 17.1 };
            assert_eq!(trail_rate_hz(0.0, ms, &mut a), 60.0, "jitter refits at frame {k}");
        }
        let mut b = AutoRate::default();
        let p = 1000.0 / 144.0;
        let mut fits = Vec::new();
        for k in 0..200 {
            let ms = if k % 2 == 0 { 0.8 * p } else { 1.2 * p };
            fits.push(trail_rate_hz(0.0, ms, &mut b));
        }
        let settled = &fits[RATE_WINDOW..];
        assert!(
            settled.windows(2).all(|w| w[0] == w[1]),
            "refits under jitter: {fits:?}"
        );
        assert!((settled[0] - 144.0).abs() <= 3.0, "{}", settled[0]);
        let mut slow = AutoRate::default();
        assert_eq!(trail_rate_hz(0.0, 120.0, &mut slow), AUTO_RATE_HZ.0);
        let mut fast = AutoRate::default();
        assert_eq!(trail_rate_hz(0.0, 1.0, &mut fast), AUTO_RATE_HZ.1);
    }

    /// Review R-10: one 600 s period (a 10 minute minimise) is a pause, not a rate sample: the estimate and
    /// the fit rate stay where they were (within 1 Hz), and the frame's decay step is the clamp, 250 ms.
    #[test]
    fn long_gap_is_not_a_rate_sample() {
        let mut s = Phosphor::default();
        for _ in 0..RATE_WINDOW {
            let _ = s.frame_weights(0.0, [1.0; 3], 1000.0 / 144.0);
        }
        let before = s.auto;
        let (gap, refit) = s.frame_weights(0.0, [1.0; 3], 600_000.0);
        assert!(!refit);
        assert_eq!(s.auto.periods_ms, before.periods_ms, "the pause is not a sample");
        assert!((s.auto.estimate_hz() - 144.0).abs() <= 1.0 && s.auto.fit_hz == 144.0);
        let (clamped, _) = s.frame_weights(0.0, [1.0; 3], 250.0);
        assert_eq!(gap, clamped, "a 10 minute gap decays like 250 ms");
    }

    /// Simulates the shader's recurrence (f64 on the f32 push constants) for a static picture 1 over
    /// `lit` frames, then black frames; returns (time since the last lit frame began, output) of the
    /// green channel per black frame, all frames through `frame_weights` with their measured periods.
    fn simulate(manual_hz: f32, lit_ms: &[f32], black_ms: &[f32]) -> Vec<(f64, f64)> {
        let mut st = Phosphor::default();
        let ch = 1;
        let mut s = [0.0_f64; 4];
        let mut primed = false;
        let mut run = |st: &mut Phosphor, s: &mut [f64; 4], dt: f32, x: f64| {
            let (p, refit) = st.frame_weights(manual_hz, [1.0; 3], dt);
            // The constants the GPU receives, as the oracle recurrence.
            let fb = FrameFeedback {
                w0: f64::from(p.w0[ch]),
                rho: p.rho[ch].map(f64::from),
                c: p.c[ch].map(f64::from),
            };
            if !primed || refit {
                // The cleared history (first frame, or a refit) is primed to the steady state (mode 1).
                *s = fb.steady_history(x);
                primed = true;
            }
            fb.step(s, x)
        };
        for &dt in lit_ms {
            let y = run(&mut st, &mut s, dt, 1.0);
            assert!((y - 1.0).abs() < 1e-6, "static picture {y} ({dt} ms)");
        }
        // The frame after the last lit one decays by its own measured period.
        let mut t = 0.0;
        let mut out = Vec::new();
        let mut prev = *lit_ms.last().unwrap();
        for &dt in black_ms {
            t += f64::from(prev);
            out.push((t, run(&mut st, &mut s, dt, 0.0)));
            prev = dt;
        }
        out
    }

    fn at(curve: &[(f64, f64)], ms: f64) -> f64 {
        curve
            .iter()
            .find(|(t, _)| (t - ms).abs() < 1e-3)
            .unwrap_or_else(|| {
                panic!(
                    "no sample at {ms} ms: {:?}",
                    curve.iter().map(|c| c.0).collect::<Vec<_>>()
                )
            })
            .1
    }

    /// Review R-11 (stage side of `crt_model::phosphor` `trail_is_real_time_at_60_and_144_hz`): in the default
    /// automatic mode the green trail after a static picture is switched off, at the same time since the last
    /// lit frame began, agrees within 1 % of the steady state at 60 Hz, at 144 Hz and at 144 Hz with jitter
    /// (and a static picture stays at 1 throughout); the manual 60 Hz mode at 144 Hz frames, the previous
    /// default, is the 2.4 times shorter trail.
    #[test]
    fn trail_is_real_time_through_the_push_constants() {
        let (p60, p144) = (1000.0_f32 / 60.0, 1000.0_f32 / 144.0);
        let c60 = simulate(0.0, &[p60; 20], &[p60; 40]);
        let c144 = simulate(0.0, &[p144; 40], &[p144; 80]);
        let jitter: Vec<f32> = (0..80)
            .map(|k| if k % 2 == 0 { 0.8 * p144 } else { 1.2 * p144 })
            .collect();
        let lit: Vec<f32> = (0..40)
            .map(|k| if k % 2 == 0 { 0.8 * p144 } else { 1.2 * p144 })
            .collect();
        let cj = simulate(0.0, &lit, &jitter);
        let old = simulate(60.0, &[p144; 40], &[p144; 80]);
        for ms in [1000.0 / 12.0, 250.0] {
            let (a, b, j, o) = (at(&c60, ms), at(&c144, ms), at(&cj, ms), at(&old, ms));
            println!("{ms:.1} ms: 60 Hz {a:.5}, 144 Hz {b:.5}, jittered {j:.5}, manual 60 Hz at 144 Hz {o:.5}");
            assert!((a - b).abs() < 0.01 && (a - j).abs() < 0.01, "{ms} ms");
            assert!((a - o).abs() > 0.03, "{ms} ms: the old behaviour is separated");
        }
    }

    /// Real-time weights in f32 keep the steady-state gain at 1 for any step (rounded constants, f64
    /// check), including a step so short that rho rounds to 1.
    #[test]
    fn real_time_weights_have_unit_gain() {
        for rate in [60.0, 144.0] {
            for ch in 0..3 {
                let fb = channel_fit(rate, ch, 1.0);
                for step in [1e-4, 0.5, 6.0, 16.7, 40.0, 250.0] {
                    let (w0, rho, c) = real_time_f32(&fb, fit_period_ms(rate), step);
                    let g: f64 = (0..4)
                        .map(|j| {
                            let open = 1.0 - f64::from(rho[j]);
                            if open > 0.0 {
                                f64::from(c[j]) / open
                            } else {
                                fb.c[j] / (1.0 - fb.rho[j])
                            }
                        })
                        .sum();
                    let gain = f64::from(w0) * (1.0 + g);
                    assert!((gain - 1.0).abs() < 1e-6, "{rate} Hz ch {ch} step {step}: {gain}");
                    assert!(rho.iter().chain(&c).all(|v| v.is_finite() && *v >= 0.0));
                }
            }
        }
    }
}
