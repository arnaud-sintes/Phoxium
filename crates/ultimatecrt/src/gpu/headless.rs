//! Headless renderer: runs the GPU pipeline without any window or swapchain, into an offscreen
//! image, and reads the result back to the CPU (TEC-7, ARCHITECTURE A16).
//!
//! Used by the GPU tests (`tests/gpu_*.rs`) and, later, by offline renders. It drives the same
//! [`Pipeline`] as the window renderer (same stages, frames in flight, reallocation). Every call is
//! synchronous: it submits one frame (or a burst of frames in flight), waits for the last fence and
//! returns the pixels. It allocates its targets and readback buffers per call, which is fine off the
//! render loop.

use super::pipeline::{full_copy, FrameConstants, ImgRef, Pipeline, Settings, Target, INJECT_FORMAT};
use super::timing::FrameTimings;
use super::*;
use crate::params::Params;

/// Format of the offscreen target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetFormat {
    /// `R16G16B16A16_SFLOAT`, the internal format of the pipeline (A10).
    Rgba16F,
    /// `R32G32B32A32_SFLOAT`, for tests that need more precision than FP16.
    Rgba32F,
    /// `B8G8R8A8_UNORM`, the swapchain format (A7): values read back as `code / 255`.
    Bgra8Unorm,
}

impl TargetFormat {
    fn vk(self) -> vk::Format {
        match self {
            TargetFormat::Rgba16F => vk::Format::R16G16B16A16_SFLOAT,
            TargetFormat::Rgba32F => vk::Format::R32G32B32A32_SFLOAT,
            TargetFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        }
    }

    fn bytes_per_pixel(self) -> usize {
        match self {
            TargetFormat::Rgba16F => 8,
            TargetFormat::Rgba32F => 16,
            TargetFormat::Bgra8Unorm => 4,
        }
    }
}

/// RGBA image of `f32` values, rows from top to bottom, 4 values per pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageF32 {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<f32>,
}

impl ImageF32 {
    /// The RGBA value of pixel (`x`, `y`). Panics outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        assert!(x < self.width && y < self.height, "pixel ({x}, {y}) outside the image");
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }
}

/// Result of one headless frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    /// The output target, as written by `encode`.
    pub output: ImageF32,
    /// The native raster written by `decode` (RGBA16F, converted exactly to `f32`); empty when `decode`
    /// did not run (no source, or an injected image).
    pub native: ImageF32,
    /// Every chain slot that ran or was bypassed in the frame, in order, with the image it left for the
    /// next one (a bypassed same-domain slot shows its input). Slots before an injected image are absent.
    pub stages: Vec<(&'static str, ImageF32)>,
    /// Pixels the output transform clipped in this frame (T-46).
    pub clip: ClipCounts,
}

impl Rendered {
    /// The image left by the chain slot `name`, if it was part of the frame.
    pub fn stage(&self, name: &str) -> Option<&ImageF32> {
        self.stages.iter().find(|(n, _)| *n == name).map(|(_, i)| i)
    }
}

/// Offscreen renderer on a surface-less [`Context`].
pub struct Headless {
    pipeline: Pipeline,
    /// Which images are read back with the target ([`Headless::set_read_stages`]).
    read_stages: StageReadback,
    /// Zero-copy (A29): the shared slots held (the source, and those read by frames in flight), as the window
    /// renderer holds them.
    shared: crate::zerocopy::InFlight<crate::capture::SharedFrame, FRAMES_IN_FLIGHT>,
    /// Destroyed last, by the field drop after `Drop for Headless`.
    ctx: Context,
}

impl Headless {
    /// Creates a headless context ([`Context::new_headless`]) and the pipeline.
    /// With `validation`, the validation layer must be installed and its messages are counted
    /// ([`Headless::validation_counter`]).
    pub fn new(validation: bool) -> Res<Self> {
        Self::from_context(Context::new_headless(validation)?)
    }

    /// Creates the pipeline on an existing headless context. Lets a caller tell a missing device
    /// (Context::new_headless fails) from a failure to create the renderer objects (this fails).
    pub fn from_context(ctx: Context) -> Res<Self> {
        // SAFETY: `ctx` is live; `Drop for Headless` destroys the pipeline before the context.
        let pipeline = unsafe { Pipeline::new(&ctx)? };
        Ok(Self {
            pipeline,
            read_stages: StageReadback::All,
            shared: Default::default(),
            ctx,
        })
    }

    /// Which images the next calls read back along with the target (default [`StageReadback::All`]). The
    /// frame renders the same in every mode; only the copies to the host and their conversion are left
    /// out. For callers that look at the output (or the encode input) only and render many large frames:
    /// at 1280 x 1080 the native raster and the chain images are about 8 million texels per frame,
    /// converted to `f32` and copied once per routed slot.
    pub fn set_read_stages(&mut self, mode: StageReadback) {
        self.read_stages = mode;
    }

    /// The Vulkan context (device, queue, chosen GPU).
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// Shared count of validation messages, readable after `self` is dropped.
    pub fn validation_counter(&self) -> ValidationCounter {
        self.ctx.validation_counter()
    }

    /// Uploads a BGRA8 (UNORM) image, as captured, runs the pipeline with identity transfers over its
    /// full raster (no crop, divisor 1) into a `format` target of the same size, and reads the target
    /// back. Code k becomes k / 255 (rounded to the target format), alpha is 1.
    pub fn passthrough(&mut self, width: u32, height: u32, bgra: &[u8], format: TargetFormat) -> Res<ImageF32> {
        let frame = SourceFrame { width, height, bgra };
        let r = self.render(
            Some(frame),
            View::full(width, height),
            &Settings::identity(),
            &Params::identity_gun(),
            (width, height),
            format,
        )?;
        Ok(r.output)
    }

    /// Runs one frame of the pipeline: uploads `frame` if given (otherwise the previous upload is
    /// reused), decodes the native raster of `view`, encodes into a `target`-sized image of `format`,
    /// and reads back the target and the native raster.
    pub fn render(
        &mut self,
        frame: Option<SourceFrame<'_>>,
        view: View,
        settings: &Settings,
        params: &Params,
        target: (u32, u32),
        format: TargetFormat,
    ) -> Res<Rendered> {
        self.render_burst(&[(frame, view)], settings, params, target, format)
    }

    /// Runs `frames` back to back like the window loop: each frame waits only for its own frame slot
    /// ([`super::FRAMES_IN_FLIGHT`] frames in flight), into its own target image; only the last frame
    /// is read back (target and native raster). Exercises the synchronisation between frames in
    /// flight under the validation layer.
    pub fn render_burst(
        &mut self,
        frames: &[(Option<SourceFrame<'_>>, View)],
        settings: &Settings,
        params: &Params,
        target: (u32, u32),
        format: TargetFormat,
    ) -> Res<Rendered> {
        let frames: Vec<_> = frames.iter().map(|&(f, v)| (f, v, params)).collect();
        self.render_burst_params(&frames, settings, target, format)
    }

    /// [`Headless::render_burst`] with the parameters of each frame: the frames flip parameters while
    /// they overlap in flight (identity stages skipped and run again, ARCHITECTURE 3.7).
    pub fn render_burst_params(
        &mut self,
        frames: &[(Option<SourceFrame<'_>>, View, &Params)],
        settings: &Settings,
        target: (u32, u32),
        format: TargetFormat,
    ) -> Res<Rendered> {
        self.render_frames(frames, settings, target, format, None)
    }

    /// [`Headless::render`] with the settings menu and overlay (T-52a) drawn over the output after `encode`
    /// ([`UiPass`], `crate::ui::Ui`), when `ui` has something to draw. `ui` must have been created on
    /// [`Headless::context`] for `format`'s Vulkan format (`Bgra8Unorm` for the swapchain format).
    // The frame's inputs plus the UI, as `render`.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_ui(
        &mut self,
        frame: Option<SourceFrame<'_>>,
        view: View,
        settings: &Settings,
        params: &Params,
        target: (u32, u32),
        format: TargetFormat,
        ui: &mut dyn UiPass,
    ) -> Res<Rendered> {
        self.render_frames(&[(frame, view, params)], settings, target, format, Some(ui))
    }

    /// Zero-copy (A29): runs `frames` back to back like [`Headless::render_burst`], each one sampling its shared
    /// texture in place (imported once per pool), and reads back the last one. Each frame waits only for its own
    /// frame slot; the shared slot a frame read stays held until that frame's fence was waited (as in the window
    /// renderer), so a test that publishes new frames meanwhile exercises the slot reuse rule. A shared frame that
    /// cannot be used is an error here (the window renderer falls back instead).
    pub fn render_shared_burst(
        &mut self,
        frames: &[(crate::capture::SharedFrame, View)],
        settings: &Settings,
        params: &Params,
        target: (u32, u32),
        format: TargetFormat,
    ) -> Res<Rendered> {
        if target.0 == 0 || target.1 == 0 || frames.is_empty() {
            return Err("headless render: empty target or no frame".into());
        }
        let mut s = Scratch::default();
        // SAFETY: as in `render_frames`.
        unsafe {
            let out = self.run_shared(&mut s, frames, settings, params, target, format);
            let _ = self.ctx.device.device_wait_idle();
            self.pipeline.collect_all(&self.ctx);
            // Every frame finished: only the source stays held.
            for k in 0..FRAMES_IN_FLIGHT {
                self.shared.frame_done(k);
            }
            if out.is_err() {
                self.pipeline.drop_external_source();
                self.shared.clear_current();
            }
            s.destroy(&self.ctx.device);
            out.map_err(|e| super::lift(e, "rendering a frame"))
        }
    }

    /// [`Headless::render_shared_burst`] of one frame.
    pub fn render_shared(
        &mut self,
        frame: &crate::capture::SharedFrame,
        view: View,
        settings: &Settings,
        params: &Params,
        target: (u32, u32),
        format: TargetFormat,
    ) -> Res<Rendered> {
        self.render_shared_burst(&[(frame.clone(), view)], settings, params, target, format)
    }

    /// Shared slots the renderer holds now (the source, and the frames in flight): the release-by-fence rule.
    pub fn shared_holds(&self) -> usize {
        self.shared.held()
    }

    /// Body of [`Headless::render_shared_burst`].
    ///
    /// # Safety
    /// As [`Headless::run`].
    unsafe fn run_shared(
        &mut self,
        s: &mut Scratch,
        frames: &[(crate::capture::SharedFrame, View)],
        settings: &Settings,
        params: &Params,
        target: (u32, u32),
        format: TargetFormat,
    ) -> Res<Rendered> {
        let ctx = &self.ctx;
        let extent = vk::Extent2D {
            width: target.0,
            height: target.1,
        };
        let (last, earlier) = frames.split_last().expect("frames is not empty");
        for (frame, view) in earlier {
            let t = self.next_target(s, extent, format)?;
            self.pipeline.wait_slot(ctx)?;
            let slot = self.pipeline.slot();
            self.shared.frame_done(slot);
            if self.pipeline.stage_external(ctx, frame.pool(), frame.slot())? {
                self.shared.drain();
            }
            self.shared.set_current(frame.clone());
            self.pipeline.record(ctx, *view, settings, params, &t)?;
            self.pipeline.submit(ctx, &[], &[], &[])?;
            self.shared.submitted(slot, self.pipeline.submitted_external());
        }
        let (frame, view) = last;
        let t = self.next_target(s, extent, format)?;
        self.pipeline.wait_slot(ctx)?;
        let slot = self.pipeline.slot();
        self.shared.frame_done(slot);
        if self.pipeline.stage_external(ctx, frame.pool(), frame.slot())? {
            self.shared.drain();
        }
        self.shared.set_current(frame.clone());
        let cb = self.pipeline.record(ctx, *view, settings, params, &t)?;
        let r = self.read_back_or_abandon(s, cb, &t, format);
        self.shared.submitted(slot, self.pipeline.submitted_external());
        r
    }

    /// The Vulkan format of a [`TargetFormat`] (to create a `crate::ui::Ui` for the headless target).
    pub fn vk_format(format: TargetFormat) -> vk::Format {
        format.vk()
    }

    /// Body of [`Headless::render_burst_params`] and [`Headless::render_with_ui`]: `ui` draws over the last
    /// frame only.
    fn render_frames(
        &mut self,
        frames: &[(Option<SourceFrame<'_>>, View, &Params)],
        settings: &Settings,
        target: (u32, u32),
        format: TargetFormat,
        ui: Option<&mut dyn UiPass>,
    ) -> Res<Rendered> {
        if target.0 == 0 || target.1 == 0 || frames.is_empty() {
            return Err("headless render: empty target or no frame".into());
        }
        let mut s = Scratch::default();
        // SAFETY: every handle is created on `self.ctx.device`. `run` waits for the fence of its last
        // submission before returning Ok; on any path the device is idled before `s` is destroyed, so
        // the GPU no longer uses the scratch objects.
        unsafe {
            let out = self.run(&mut s, frames, settings, target, format, ui);
            let _ = self.ctx.device.device_wait_idle();
            self.pipeline.collect_all(&self.ctx);
            s.destroy(&self.ctx.device);
            out.map_err(|e| super::lift(e, "rendering a frame"))
        }
    }

    /// Body of [`Headless::render_burst`]; every object it creates goes into `s`.
    ///
    /// # Safety
    /// `s` must be empty (null handles); the caller idles the device and destroys `s` afterwards.
    unsafe fn run(
        &mut self,
        s: &mut Scratch,
        frames: &[(Option<SourceFrame<'_>>, View, &Params)],
        settings: &Settings,
        target: (u32, u32),
        format: TargetFormat,
        ui: Option<&mut dyn UiPass>,
    ) -> Res<Rendered> {
        let ctx = &self.ctx;
        let extent = vk::Extent2D {
            width: target.0,
            height: target.1,
        };

        // Every frame but the last: recorded and submitted without waiting for the GPU.
        let (last, earlier) = frames.split_last().expect("frames is not empty");
        for (frame, view, params) in earlier {
            let t = self.next_target(s, extent, format)?;
            self.pipeline.wait_slot(ctx)?;
            if let Some(f) = frame {
                self.pipeline.stage_source(ctx, f.width, f.height, f.bgra)?;
                self.shared.clear_current();
            }
            self.pipeline.record(ctx, *view, settings, params, &t)?;
            self.pipeline.submit(ctx, &[], &[], &[])?;
        }

        let (frame, view, params) = last;
        let t = self.next_target(s, extent, format)?;
        self.pipeline.wait_slot(ctx)?;
        if let Some(f) = frame {
            self.pipeline.stage_source(ctx, f.width, f.height, f.bgra)?;
            self.shared.clear_current();
        }
        let cb = self.pipeline.record_ui(ctx, *view, settings, params, &t, ui)?;
        self.read_back_or_abandon(s, cb, &t, format)
    }

    /// Stage-test helper (T-20b): renders one frame through the chain with the given input, the
    /// injected `FrameConstants` of `run` (image sizes follow them, see [`FrameConstants::domain_size`])
    /// and the current enable flags ([`Headless::set_stage_enabled`], [`Headless::enable_only`]), into
    /// a target of `run.frame.target_size`, and reads back the target and every chain image.
    pub fn render_stages(&mut self, run: &StageRun<'_>) -> Res<Rendered> {
        let target = (
            run.frame.target_size[0].max(0) as u32,
            run.frame.target_size[1].max(0) as u32,
        );
        if target.0 == 0 || target.1 == 0 {
            return Err("headless render: empty target".into());
        }
        let mut s = Scratch::default();
        // SAFETY: as in `render_burst`: the device is idled before `s` is destroyed.
        unsafe {
            let out = self.run_stages(&mut s, run, target);
            let _ = self.ctx.device.device_wait_idle();
            self.pipeline.collect_all(&self.ctx);
            s.destroy(&self.ctx.device);
            out.map_err(|e| super::lift(e, "rendering a frame"))
        }
    }

    /// The GPU-side notice in force after the last frame, as [`super::Renderer::gpu_notice`].
    pub fn gpu_notice(&self) -> Option<String> {
        self.pipeline.notice()
    }

    /// The notices that appeared since the previous call, as [`super::Renderer::take_gpu_notices`].
    pub fn take_gpu_notices(&mut self) -> Vec<String> {
        self.pipeline.take_notices()
    }

    /// The image limits the pipeline applies (the device's, `Context::image_limits`, unless replaced).
    pub fn image_limits(&self) -> crate::quality::ImageLimits {
        self.pipeline.image_limits()
    }

    /// Replaces the image limits the pipeline applies (tests: the limits of a smaller device, R-02).
    pub fn set_image_limits(&mut self, limits: crate::quality::ImageLimits) {
        self.pipeline.set_image_limits(limits);
    }

    /// Body of [`Headless::render_stages`].
    ///
    /// # Safety
    /// As [`Headless::run`].
    unsafe fn run_stages(&mut self, s: &mut Scratch, run: &StageRun<'_>, target: (u32, u32)) -> Res<Rendered> {
        let ctx = &self.ctx;
        let extent = vk::Extent2D {
            width: target.0,
            height: target.1,
        };
        let t = self.next_target(s, extent, run.format)?;
        self.pipeline.wait_slot(ctx)?;
        let view = match run.input {
            StageInput::Source { frame, view } => {
                self.pipeline.stage_source(ctx, frame.width, frame.height, frame.bgra)?;
                view
            }
            StageInput::Image { after, image } => {
                let at = if after == "decode" {
                    0
                } else {
                    let names = self.pipeline.stage_names();
                    names
                        .iter()
                        .position(|n| *n == after)
                        .ok_or_else(|| format!("no stage slot named `{after}`"))?
                        + 1
                };
                self.pipeline.stage_injected(ctx, at, image)?;
                let n = (
                    run.frame.native_size[0].max(0) as u32,
                    run.frame.native_size[1].max(0) as u32,
                );
                View::full(n.0, n.1)
            }
        };
        let cb = self
            .pipeline
            .record_with(ctx, view, run.frame, run.settings, run.params, &t)?;
        self.read_back_or_abandon(s, cb, &t, run.format)
    }

    /// [`Headless::read_back`], giving the recorded frame up when it fails before the submission (the
    /// pipeline then keeps its staged frame and image states as if the frame had not been recorded).
    ///
    /// # Safety
    /// As [`Headless::read_back`].
    unsafe fn read_back_or_abandon(
        &mut self,
        s: &mut Scratch,
        cb: vk::CommandBuffer,
        t: &Target,
        format: TargetFormat,
    ) -> Res<Rendered> {
        let r = self.read_back(s, cb, t, format);
        if r.is_err() {
            self.pipeline.abandon();
        }
        r
    }

    /// Records the readback of the target and of every image routed in the frame just recorded into
    /// `cb`, submits, waits and converts them.
    ///
    /// # Safety
    /// `cb` was returned by the pipeline's last `record`, still recording, into `t`; `s` holds `t`.
    unsafe fn read_back(
        &mut self,
        s: &mut Scratch,
        cb: vk::CommandBuffer,
        t: &Target,
        format: TargetFormat,
    ) -> Res<Rendered> {
        let ctx = &self.ctx;
        let d = &ctx.device;
        let target = (t.extent.width, t.extent.height);
        let out_bytes = target.0 as usize * target.1 as usize * format.bytes_per_pixel();
        s.readback = HostBuffer::create(ctx, out_bytes, vk::BufferUsageFlags::TRANSFER_DST)?;
        let dst = t.image;

        // Read back the target: -> transfer source, copy to a host-visible buffer.
        barrier(
            d,
            cb,
            dst,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
        );
        d.cmd_copy_image_to_buffer(
            cb,
            dst,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            s.readback.buffer,
            &[full_copy(target)],
        );
        // Read back every image the frame routed (decode output and chain images), each once; the
        // pipeline records the barriers from its tracked state. Fewer when the caller asked for less.
        let routed: Vec<(&'static str, ImgRef)> = match self.read_stages {
            StageReadback::All => self.pipeline.routed().to_vec(),
            StageReadback::Last => self
                .pipeline
                .routed()
                .iter()
                .rev()
                .find(|(n, _)| *n != "decode")
                .copied()
                .into_iter()
                .collect(),
            StageReadback::None => Vec::new(),
        };
        let mut images: Vec<(ImgRef, (u32, u32), vk::Format)> = Vec::new();
        for &(_, r) in &routed {
            if images.iter().any(|(o, _, _)| *o == r) {
                continue;
            }
            let (image, size, fmt) = self.pipeline.prepare_readback(d, cb, r);
            let buf = HostBuffer::create(
                ctx,
                size.0 as usize * size.1 as usize * texel_bytes(fmt),
                vk::BufferUsageFlags::TRANSFER_DST,
            )?;
            d.cmd_copy_image_to_buffer(
                cb,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buf.buffer,
                &[full_copy(size)],
            );
            s.image_readbacks.push(buf);
            images.push((r, size, fmt));
        }
        super::pipeline::barrier_to_host(d, cb, true);

        let fence = self.pipeline.submit(ctx, &[], &[], &[])?;
        super::wait_fences(d, &[fence], "a frame")?;

        // The readback memory is HOST_COHERENT: after the HOST_READ barrier and the fence wait, the
        // copies are visible to the host without an invalidate.
        let bytes = std::slice::from_raw_parts(s.readback.mapped as *const u8, out_bytes);
        let rgba = match format {
            TargetFormat::Rgba16F => half_to_f32(bytes),
            TargetFormat::Rgba32F => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&b| f32::from_le_bytes(b))
                .collect(),
            TargetFormat::Bgra8Unorm => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]].map(|c| f32::from(c) / 255.0))
                .collect(),
        };
        let output = ImageF32 {
            width: target.0,
            height: target.1,
            rgba,
        };
        let read: Vec<(ImgRef, ImageF32)> = images
            .iter()
            .zip(&s.image_readbacks)
            .map(|(&(r, (w, h), fmt), buf)| {
                let bytes =
                    std::slice::from_raw_parts(buf.mapped as *const u8, w as usize * h as usize * texel_bytes(fmt));
                let rgba = if fmt == INJECT_FORMAT {
                    bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|&b| f32::from_le_bytes(b))
                        .collect()
                } else {
                    half_to_f32(bytes)
                };
                (
                    r,
                    ImageF32 {
                        width: w,
                        height: h,
                        rgba,
                    },
                )
            })
            .collect();
        let get = |r: ImgRef| read.iter().find(|(o, _)| *o == r).map(|(_, i)| i.clone());
        let native = routed
            .iter()
            .find(|(n, _)| *n == "decode")
            .and_then(|&(_, r)| get(r))
            .unwrap_or(ImageF32 {
                width: 0,
                height: 0,
                rgba: Vec::new(),
            });
        let stages = routed
            .iter()
            .filter(|(n, _)| *n != "decode")
            .filter_map(|&(n, r)| get(r).map(|i| (n, i)))
            .collect();
        let clip = self.pipeline.last_clip_counts();
        Ok(Rendered {
            output,
            native,
            stages,
            clip,
        })
    }

    /// SDR headroom readout of the last frame recorded (T-84), as [`super::Renderer::headroom`].
    pub fn last_headroom(&self) -> Option<crt_model::output::Headroom> {
        self.pipeline.headroom()
    }

    /// GPU timings (T-21) of the last frame rendered, readback copies included in `frame_ms` only. Every
    /// render call reads the timestamps of its frames after the device idle, so after a call this is
    /// that call's last frame. `None` before any render or without timestamp support.
    pub fn last_timings(&self) -> Option<&FrameTimings> {
        self.pipeline.timings()
    }

    /// Mean GPU timings of the frames rendered since the previous call, then restarts the average.
    pub fn take_timing_average(&mut self) -> Option<FrameTimings> {
        self.pipeline.take_timing_average()
    }

    /// Why GPU timestamps are off, `None` when they work.
    pub fn timing_unavailable(&self) -> Option<&str> {
        self.pipeline.timing_unavailable()
    }

    /// Names of the timed segments in order: `upload`, `decode`, the chain slots, `encode`, `ui`.
    pub fn timing_names(&self) -> Vec<&'static str> {
        self.pipeline.timing_names()
    }

    /// Chain slots that recorded a pass in the last frame planned (a stage or a resample copy; `decode`
    /// and `encode` are not counted): drops when identity stages are skipped.
    pub fn last_chain_passes(&self) -> usize {
        self.pipeline.chain_passes()
    }

    /// Chain slots the last frame planned routed around: disabled, placeholder or identity stages that
    /// keep their domain (nothing recorded, no image, no barrier).
    pub fn last_skipped_slots(&self) -> Vec<&'static str> {
        self.pipeline.skipped_slots()
    }

    /// Names of the chain slots, in order (`signal`, `amp`, `gun`, `screen`, `phosphor`, `optics`).
    pub fn stage_names(&self) -> Vec<&'static str> {
        self.pipeline.stage_names()
    }

    /// Turns the chain slot `name` on or off (off: bypassed, exact identity).
    pub fn set_stage_enabled(&mut self, name: &str, on: bool) -> Res<()> {
        self.pipeline.set_enabled(name, on)
    }

    /// The enable flag of the chain slot `name`.
    pub fn stage_enabled(&self, name: &str) -> Option<bool> {
        self.pipeline.is_enabled(name)
    }

    /// Clears the history of the stages that keep one (persistence, T-41), as the window loop does on
    /// a source size change or a new capture session.
    pub fn clear_history(&mut self) {
        self.pipeline.clear_history();
    }

    /// Enables exactly the chain slots of `names` (the stage subset under test), every other slot off.
    pub fn enable_only(&mut self, names: &[&str]) -> Res<()> {
        for n in names {
            if self.pipeline.is_enabled(n).is_none() {
                return Err(format!("no stage slot named `{n}`").into());
            }
        }
        for n in self.pipeline.stage_names() {
            self.pipeline.set_enabled(n, names.contains(&n))?;
        }
        Ok(())
    }

    /// The pipeline (the `tube_state::WearHost` implementation of `gpu/mod.rs`).
    pub(super) fn pipeline_mut(&mut self) -> &mut Pipeline {
        &mut self.pipeline
    }

    /// The pipeline and its context.
    pub(super) fn parts(&mut self) -> (&mut Pipeline, &Context) {
        (&mut self.pipeline, &self.ctx)
    }

    /// A headless renderer whose pipeline runs `chain` instead of [`super::stages::CHAIN`].
    #[cfg(test)]
    pub(super) fn with_chain(validation: bool, chain: &'static [super::stages::Registration]) -> Res<Self> {
        let ctx = Context::new_headless(validation)?;
        // SAFETY: as in `new`.
        let pipeline = unsafe { Pipeline::with_chain(&ctx, chain)? };
        Ok(Self {
            pipeline,
            read_stages: StageReadback::All,
            shared: Default::default(),
            ctx,
        })
    }
}

/// Bytes per texel of the images read back.
fn texel_bytes(format: vk::Format) -> usize {
    if format == INJECT_FORMAT {
        16
    } else {
        8
    }
}

/// The images a headless call reads back besides the target ([`Headless::set_read_stages`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageReadback {
    /// The native raster ([`Rendered::native`]) and the image of every routed chain slot
    /// ([`Rendered::stages`]), the default.
    All,
    /// Only the image of the last routed chain slot, the encode input ([`Rendered::stages`] has that one
    /// entry, [`Rendered::native`] is empty).
    Last,
    /// None: [`Rendered::native`] is empty and [`Rendered::stages`] has no entry.
    None,
}

/// Where a stage test's input enters the chain ([`Headless::render_stages`]).
#[derive(Clone, Copy, Debug)]
pub enum StageInput<'a> {
    /// A captured BGRA8 frame, uploaded and decoded over the native raster of `view`.
    Source { frame: SourceFrame<'a>, view: View },
    /// A float image (FP32 on the GPU) that replaces the output of `after`: `"decode"` (native domain)
    /// or a chain slot name (that slot's output domain). Its size must be the domain size of the
    /// injected `FrameConstants`; `decode` and the slots up to `after` do not run.
    Image { after: &'a str, image: &'a ImageF32 },
}

/// One stage-test frame ([`Headless::render_stages`]).
#[derive(Clone, Copy, Debug)]
pub struct StageRun<'a> {
    pub input: StageInput<'a>,
    /// The injected constants, used as given (start from [`FrameConstants::new`]); the target is
    /// `frame.target_size`.
    pub frame: FrameConstants,
    pub settings: &'a Settings,
    pub params: &'a Params,
    pub format: TargetFormat,
}

/// A captured frame handed to [`Headless::render`]: BGRA8, rows top to bottom, tightly packed.
#[derive(Clone, Copy, Debug)]
pub struct SourceFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub bgra: &'a [u8],
}

/// Little-endian binary16 values to `f32`, exactly.
fn half_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| f16_to_f32(u16::from_le_bytes(b)))
        .collect()
}

impl Drop for Headless {
    fn drop(&mut self) {
        // SAFETY: after `device_wait_idle` the GPU uses none of the pipeline's objects; the device and
        // instance go last with the field drop of `ctx`.
        unsafe {
            let _ = self.ctx.device.device_wait_idle();
            self.pipeline.destroy(&self.ctx);
        }
    }
}

impl Headless {
    /// Creates a new target image of `extent` and `format` in `s` and returns it.
    ///
    /// # Safety
    /// `s` is destroyed by the caller once the GPU no longer uses it.
    unsafe fn next_target(&self, s: &mut Scratch, extent: vk::Extent2D, format: TargetFormat) -> Res<Target> {
        let image = GpuImage::create(
            &self.ctx,
            extent,
            format.vk(),
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        let t = Target {
            image: image.image,
            view: image.view,
            extent,
            format: format.vk(),
        };
        s.targets.push(image);
        Ok(t)
    }
}

/// Objects of one headless call. Null handles by default; `destroy` releases what was created.
#[derive(Default)]
struct Scratch {
    /// One target image per frame (frames in flight never share a target).
    targets: Vec<GpuImage>,
    readback: HostBuffer,
    /// One buffer per routed image read back.
    image_readbacks: Vec<HostBuffer>,
}

impl Scratch {
    /// # Safety
    /// The GPU must have finished every use of the objects (device idle).
    unsafe fn destroy(&mut self, d: &ash::Device) {
        for b in &mut self.image_readbacks {
            b.destroy(d);
        }
        self.image_readbacks.clear();
        self.readback.destroy(d);
        for t in &mut self.targets {
            t.destroy(d);
        }
        self.targets.clear();
    }
}

/// Device-local 2D image with one mip level, one layer, and its colour view.
#[derive(Default)]
pub(super) struct GpuImage {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
}

impl GpuImage {
    /// Creates the image (initial layout `UNDEFINED`), binds device-local memory and creates the view.
    /// On error, the partly created objects are destroyed.
    ///
    /// # Safety
    /// `ctx` must be live; the caller destroys the result with [`GpuImage::destroy`] once the GPU
    /// no longer uses it.
    pub unsafe fn create(
        ctx: &Context,
        extent: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Res<Self> {
        let d = &ctx.device;
        let mut g = Self {
            image: d.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )?,
            ..Self::default()
        };
        let r = (|| -> Res<()> {
            let req = d.get_image_memory_requirements(g.image);
            g.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(ctx.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?),
                None,
            )?;
            d.bind_image_memory(g.image, g.memory, 0)?;
            g.view = d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(g.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(color_range()),
                None,
            )?;
            Ok(())
        })();
        match r {
            Ok(()) => Ok(g),
            Err(e) => {
                g.destroy(d);
                Err(e)
            }
        }
    }

    /// Destroys the view, image and memory (null handles are no-ops) and resets them to null.
    ///
    /// # Safety
    /// The GPU must have finished every use of the image.
    pub unsafe fn destroy(&mut self, d: &ash::Device) {
        d.destroy_image_view(self.view, None);
        d.destroy_image(self.image, None);
        d.free_memory(self.memory, None);
        *self = Self::default();
    }
}

/// Host-visible, host-coherent buffer, persistently mapped (host-cached memory preferred, for reads).
pub(super) struct HostBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub mapped: *mut u8,
}

impl Default for HostBuffer {
    fn default() -> Self {
        Self {
            buffer: vk::Buffer::null(),
            memory: vk::DeviceMemory::null(),
            mapped: std::ptr::null_mut(),
        }
    }
}

impl HostBuffer {
    /// Creates a buffer of `size` bytes and maps all of it. On error, the partly created objects
    /// are destroyed.
    ///
    /// # Safety
    /// `ctx` must be live; the caller destroys the result with [`HostBuffer::destroy`] once the GPU
    /// no longer uses it, and accesses `mapped` only within `size` bytes.
    pub unsafe fn create(ctx: &Context, size: usize, usage: vk::BufferUsageFlags) -> Res<Self> {
        let d = &ctx.device;
        let mut b = Self {
            buffer: d.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size as u64)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?,
            ..Self::default()
        };
        let r = (|| -> Res<()> {
            let req = d.get_buffer_memory_requirements(b.buffer);
            let coherent = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let index = ctx
                .memory_type(req.memory_type_bits, coherent | vk::MemoryPropertyFlags::HOST_CACHED)
                .or_else(|_| ctx.memory_type(req.memory_type_bits, coherent))?;
            b.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(index),
                None,
            )?;
            d.bind_buffer_memory(b.buffer, b.memory, 0)?;
            b.mapped = d.map_memory(b.memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())? as *mut u8;
            Ok(())
        })();
        match r {
            Ok(()) => Ok(b),
            Err(e) => {
                b.destroy(d);
                Err(e)
            }
        }
    }

    /// Destroys the buffer and frees its memory (which unmaps it); null handles are no-ops.
    ///
    /// # Safety
    /// The GPU must have finished every use of the buffer; `mapped` must not be used afterwards.
    pub unsafe fn destroy(&mut self, d: &ash::Device) {
        d.destroy_buffer(self.buffer, None);
        d.free_memory(self.memory, None);
        *self = Self::default();
    }
}

/// Device-local buffer (no mapping): for data the GPU writes with atomics or reads often. A storage buffer in
/// host memory turns every atomic into a bus round trip (the `encode` clip counters cost up to 3.5 ms per frame
/// at 1920 x 1080 there, performance work 2026-11-20).
#[derive(Default)]
pub(super) struct DeviceBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
}

impl DeviceBuffer {
    /// Creates a buffer of `size` bytes in device-local memory. On error, the partly created objects are
    /// destroyed.
    ///
    /// # Safety
    /// `ctx` must be live; the caller destroys the result with [`DeviceBuffer::destroy`] once the GPU no longer
    /// uses it.
    pub unsafe fn create(ctx: &Context, size: usize, usage: vk::BufferUsageFlags) -> Res<Self> {
        let d = &ctx.device;
        let mut b = Self {
            buffer: d.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size as u64)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?,
            ..Self::default()
        };
        let r = (|| -> Res<()> {
            let req = d.get_buffer_memory_requirements(b.buffer);
            b.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(ctx.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?),
                None,
            )?;
            d.bind_buffer_memory(b.buffer, b.memory, 0)?;
            Ok(())
        })();
        match r {
            Ok(()) => Ok(b),
            Err(e) => {
                b.destroy(d);
                Err(e)
            }
        }
    }

    /// Destroys the buffer and frees its memory; null handles are no-ops.
    ///
    /// # Safety
    /// The GPU must have finished every use of the buffer.
    pub unsafe fn destroy(&mut self, d: &ash::Device) {
        d.destroy_buffer(self.buffer, None);
        d.free_memory(self.memory, None);
        *self = Self::default();
    }
}

/// Converts an IEEE 754 binary16 value to `f32` (exact: every half value is an `f32` value).
pub(super) fn f16_to_f32(h: u16) -> f32 {
    let negative = h & 0x8000 != 0;
    let exp = u32::from((h >> 10) & 0x1f);
    let man = u32::from(h & 0x3ff);
    let sign = if negative { 1u32 << 31 } else { 0 };
    let bits = match (exp, man) {
        (0, 0) => sign,
        (0, _) => {
            // Subnormal: man * 2^-24.
            let v = man as f32 / 16_777_216.0;
            return if negative { -v } else { v };
        }
        (31, _) => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 112) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_convert_exactly() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert!(f16_to_f32(0x8000).is_sign_negative());
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert_eq!(f16_to_f32(0x0400), 2f32.powi(-14));
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x83ff), -1023.0 * 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
        // 1/3 rounds to 0x3555 = 1365/4096.
        assert_eq!(f16_to_f32(0x3555), 1365.0 / 4096.0);
    }

    /// A frame that is recorded but never submitted (a failure between `record` and `submit`) must not
    /// consume the staged upload nor leave the tracked image states ahead of the GPU: the next frame,
    /// without any new upload, decodes the staged frame, with zero validation messages. Needs the
    /// validation layer and a device; skipped without them unless `UCRT_REQUIRE_GPU=1`.
    #[test]
    fn a_recorded_frame_that_is_never_submitted_is_abandoned_cleanly() {
        let require = crate::gpu::gpu_required();
        let validation = crate::gpu::validation_layer_available();
        assert!(
            validation || !require,
            "UCRT_REQUIRE_GPU=1 but the validation layer is not installed"
        );
        let mut gpu = match Headless::new(validation) {
            Ok(g) => g,
            Err(e) => {
                assert!(!require, "UCRT_REQUIRE_GPU=1 but no headless Vulkan device: {e}");
                eprintln!("abandoned frame test skipped: {e}");
                return;
            }
        };
        let counter = gpu.validation_counter();
        // The test is about frame state, not the physics: bypass every chain slot (exact identity).
        gpu.enable_only(&[]).expect("bypass every slot");
        let (w, h) = (3u32, 2u32);
        let bgra: Vec<u8> = (0..w * h)
            .flat_map(|i| [i as u8 * 40 + 3, i as u8 * 30 + 7, i as u8 * 20 + 11, 255])
            .collect();
        let (view, settings, params) = (View::full(w, h), Settings::identity(), Params::default());
        // SAFETY: the scratch objects are destroyed after the device is idle; the pipeline and the
        // context are the ones of `gpu`.
        unsafe {
            let ctx = &gpu.ctx;
            let mut s = Scratch::default();
            let extent = vk::Extent2D { width: w, height: h };
            let t = gpu
                .next_target(&mut s, extent, TargetFormat::Bgra8Unorm)
                .expect("target");
            gpu.pipeline.wait_slot(ctx).expect("wait");
            gpu.pipeline.stage_source(ctx, w, h, &bgra).expect("stage");
            gpu.pipeline.record(ctx, view, &settings, &params, &t).expect("record");
            // The frame is dropped here, as after a failed submission.
            gpu.pipeline.abandon();
            let _ = ctx.device.device_wait_idle();
            s.destroy(&ctx.device);
        }
        let r = gpu
            .render(None, view, &settings, &params, (w, h), TargetFormat::Bgra8Unorm)
            .expect("render without new upload");
        for i in 0..w * h {
            let p = r.output.pixel(i % w, i / w);
            let got = [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8);
            let k = i as u8;
            assert_eq!(got, [k * 20 + 11, k * 30 + 7, k * 40 + 3], "pixel {i}");
        }
        drop(gpu);
        assert_eq!(counter.count(), 0, "validation messages");
    }

    /// The first frame of new phosphor history images and of a cold ABL state is abandoned: the next
    /// frame still transitions the images and clears the history and the state (`Stage::abandon`), so
    /// it shows the static picture unchanged (primed history, ABL gain 1) with zero validation messages.
    #[test]
    fn an_abandoned_first_frame_keeps_the_stage_resets() {
        let require = crate::gpu::gpu_required();
        let validation = crate::gpu::validation_layer_available();
        assert!(
            validation || !require,
            "UCRT_REQUIRE_GPU=1 but the validation layer is not installed"
        );
        let mut gpu = match Headless::new(validation) {
            Ok(g) => g,
            Err(e) => {
                assert!(!require, "UCRT_REQUIRE_GPU=1 but no headless Vulkan device: {e}");
                eprintln!("abandoned stage frame test skipped: {e}");
                return;
            }
        };
        let counter = gpu.validation_counter();
        gpu.enable_only(&["abl", "phosphor"]).expect("enable");
        let (w, h) = (3u32, 2u32);
        let bgra: Vec<u8> = (0..w * h)
            .flat_map(|i| [i as u8 * 40 + 3, i as u8 * 30 + 7, i as u8 * 20 + 11, 255])
            .collect();
        let (view, settings) = (View::full(w, h), Settings::identity());
        let params = crate::params::Params {
            phosphor_persistence: true,
            abl_enable: true,
            ..crate::params::Params::default()
        };
        // SAFETY: as in the test above.
        unsafe {
            let ctx = &gpu.ctx;
            let mut s = Scratch::default();
            let extent = vk::Extent2D { width: w, height: h };
            let t = gpu
                .next_target(&mut s, extent, TargetFormat::Bgra8Unorm)
                .expect("target");
            gpu.pipeline.wait_slot(ctx).expect("wait");
            gpu.pipeline.stage_source(ctx, w, h, &bgra).expect("stage");
            gpu.pipeline.record(ctx, view, &settings, &params, &t).expect("record");
            gpu.pipeline.abandon();
            let _ = ctx.device.device_wait_idle();
            s.destroy(&ctx.device);
        }
        for frame in 0..3 {
            let r = gpu
                .render(None, view, &settings, &params, (w, h), TargetFormat::Bgra8Unorm)
                .expect("render after the abandoned frame");
            for i in 0..w * h {
                let p = r.output.pixel(i % w, i / w);
                let got = [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8);
                let k = i as u8;
                assert_eq!(got, [k * 20 + 11, k * 30 + 7, k * 40 + 3], "frame {frame} pixel {i}");
            }
        }
        drop(gpu);
        assert_eq!(counter.count(), 0, "validation messages");
    }

    /// A headless renderer with validation when the layer is installed, `None` (skip) without a device unless
    /// `UCRT_REQUIRE_GPU=1`.
    fn gpu_or_skip(test: &str) -> Option<Headless> {
        let require = crate::gpu::gpu_required();
        let validation = crate::gpu::validation_layer_available();
        assert!(
            validation || !require,
            "UCRT_REQUIRE_GPU=1 but the validation layer is not installed"
        );
        match Headless::new(validation) {
            Ok(g) => Some(g),
            Err(e) => {
                assert!(!require, "UCRT_REQUIRE_GPU=1 but no headless Vulkan device: {e}");
                eprintln!("{test} skipped: {e}");
                None
            }
        }
    }

    /// R-27: a source reallocation that fails half way (the second slot's staging buffer) leaves no size without
    /// its buffers: the same size fails again (reallocated again, never a copy through a null mapping), and once
    /// the allocation works the frame renders normally, with zero validation messages.
    #[test]
    fn a_failed_source_reallocation_leaves_no_null_staging() {
        let Some(mut gpu) = gpu_or_skip("failed source reallocation test") else {
            return;
        };
        let counter = gpu.validation_counter();
        gpu.enable_only(&[]).expect("bypass every slot");
        let (w, h) = (5u32, 3u32);
        let bgra: Vec<u8> = (0..w * h)
            .flat_map(|i| [i as u8 * 9 + 1, i as u8 * 7 + 2, i as u8 * 5 + 3, 255])
            .collect();
        // SAFETY: the pipeline and the context are the ones of `gpu`; no frame is recorded in between.
        unsafe {
            let ctx = &gpu.ctx;
            gpu.pipeline.wait_slot(ctx).expect("wait");
            for slot in [1, 0] {
                gpu.pipeline.fail_staging_alloc = Some(slot);
                assert!(
                    gpu.pipeline.stage_source(ctx, w, h, &bgra).is_err(),
                    "slot {slot} fault"
                );
                assert!(
                    gpu.pipeline.stage_source(ctx, w, h, &bgra).is_err(),
                    "slot {slot} fault, same size again"
                );
                assert_eq!(
                    gpu.pipeline.source_allocation(),
                    ((0, 0), 0),
                    "no size without its buffers, no buffer left"
                );
            }
            gpu.pipeline.fail_staging_alloc = None;
        }
        let (view, settings, params) = (View::full(w, h), Settings::identity(), Params::default());
        let frame = SourceFrame {
            width: w,
            height: h,
            bgra: &bgra,
        };
        let r = gpu
            .render(Some(frame), view, &settings, &params, (w, h), TargetFormat::Bgra8Unorm)
            .expect("render once the allocation works");
        for i in 0..w * h {
            let p = r.output.pixel(i % w, i / w);
            let got = [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8);
            let k = i as u8;
            assert_eq!(got, [k * 5 + 3, k * 7 + 2, k * 9 + 1], "pixel {i}");
        }
        drop(gpu);
        assert_eq!(counter.count(), 0, "validation messages");
    }

    /// R-50: the clip counters read before any frame finished are zero in every frame slot.
    #[test]
    fn clip_counters_start_at_zero() {
        let Some(gpu) = gpu_or_skip("clip counter test") else {
            return;
        };
        // SAFETY: no frame was ever submitted, so no slot is in use.
        let (current, last) = unsafe { (gpu.pipeline.clip_counts(), gpu.pipeline.last_clip_counts()) };
        for c in [current, last] {
            assert_eq!((c.over_white, c.out_of_gamut), (0, 0));
        }
    }

    #[test]
    fn image_pixel_reads_rows_top_to_bottom() {
        let img = ImageF32 {
            width: 2,
            height: 2,
            rgba: (0..16).map(|v| v as f32).collect(),
        };
        assert_eq!(img.pixel(1, 0), [4.0, 5.0, 6.0, 7.0]);
        assert_eq!(img.pixel(0, 1), [8.0, 9.0, 10.0, 11.0]);
    }
}
