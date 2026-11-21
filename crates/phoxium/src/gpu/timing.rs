//! GPU timestamps per stage (T-21, TEC-8, PRF-1 measurement).
//!
//! Every frame in flight has its own timestamp query pool. The pipeline writes one timestamp
//! (`vkCmdWriteTimestamp2`, `ALL_COMMANDS`) before the frame's first command, one after every segment
//! that was recorded and one at the end of the command buffer. The segments, in recording order, are
//! `upload` (staged frame or injected image copy), `decode`, every chain slot, `encode`, `ui` (the settings
//! menu and overlay pass of T-52a, recorded only while one of them is shown). Because the
//! marks are chained, the time of a segment is the difference to the previous written mark, so the sum
//! of the segments never exceeds the frame time; the rest of the frame (target barriers, the readback
//! copies of a headless frame) is in the frame time only.
//!
//! The results are never waited for: the pool of a frame slot is read when the CPU is about to reuse the
//! slot (`Pipeline::wait_slot`, whose fence wait proves the frame N-k finished, k = frames in flight), or
//! by the headless renderer after its device idle. So the numbers lag the CPU by at most
//! [`super::FRAMES_IN_FLIGHT`] frames and reading them adds no stall.
//!
//! A segment that was not recorded (a same-domain slot that is off or a placeholder, `upload` without a
//! new frame) has `ms: None`. A domain-changing slot that is off still runs the nearest resample copy:
//! it is timed and flagged `bypassed`. A segment lasts from the previous mark, so a stall at a pipeline
//! barrier or, for `encode`, on the swapchain acquire semaphore is counted in the segment that follows it.
//!
//! Time of a tick is `timestampPeriod` ns; only the `timestampValidBits` low bits of a value are
//! meaningful, so differences are taken modulo 2^valid_bits. A device or queue family without timestamp
//! support turns the feature off ([`GpuTimer::unavailable`] holds the reason, no query is recorded).

use ash::vk;

use super::context::Context;
use super::FRAMES_IN_FLIGHT;
use crate::Res;

/// GPU time of one segment of a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageTiming {
    /// `upload`, `decode`, a chain slot name or `encode`.
    pub name: &'static str,
    /// GPU milliseconds; `None` when the segment was not recorded in the frame (or, in an average, in
    /// any of its frames).
    pub ms: Option<f64>,
    /// The slot was off (or a placeholder) and ran the resample copy of a domain change instead of its
    /// stage: `ms` is the cost of that copy, not of the stage.
    pub bypassed: bool,
}

/// GPU timings of one frame, or the mean of several ([`TimingAverager::take`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameTimings {
    /// Frames this value covers: 1 for a single frame.
    pub frames: u32,
    /// GPU milliseconds from the start of the frame's command buffer to its end.
    pub frame_ms: f64,
    /// Every segment of the pipeline in recording order (`upload`, `decode`, the chain slots,
    /// `encode`), whether it ran or not.
    pub stages: Vec<StageTiming>,
}

impl FrameTimings {
    /// The milliseconds of the segment `name`, `None` when it did not run (or does not exist).
    pub fn stage_ms(&self, name: &str) -> Option<f64> {
        self.stages.iter().find(|s| s.name == name).and_then(|s| s.ms)
    }

    /// Sum of the segments that ran. Never above [`FrameTimings::frame_ms`] for a single frame.
    pub fn sum_ms(&self) -> f64 {
        self.stages.iter().filter_map(|s| s.ms).sum()
    }

    /// The `--stats` line: `gpu 0.31 ms | upload - | decode 0.05 | amp 0.02 (bypass) | ...`; `-` is a
    /// segment that did not run.
    pub fn stats_line(&self) -> String {
        let mut out = format!("gpu {:.2} ms", self.frame_ms);
        for s in &self.stages {
            match s.ms {
                Some(ms) if s.bypassed => out.push_str(&format!(" | {} {ms:.3} (bypass)", s.name)),
                Some(ms) => out.push_str(&format!(" | {} {ms:.3}", s.name)),
                None => out.push_str(&format!(" | {} -", s.name)),
            }
        }
        out
    }

    /// The values of the `gpu_*` CSV columns of [`csv_columns`], each preceded by a comma; empty
    /// values for `None`, or for every column without timings.
    pub fn csv_values(this: Option<&Self>, names: &[&str]) -> String {
        let mut out = String::new();
        match this {
            Some(t) => {
                out.push_str(&format!(",{:.3}", t.frame_ms));
                for n in names {
                    match t.stage_ms(n) {
                        Some(ms) => out.push_str(&format!(",{ms:.3}")),
                        None => out.push(','),
                    }
                }
            }
            None => out.push_str(&",".repeat(names.len() + 1)),
        }
        out
    }
}

/// The names of the CSV columns the GPU timings add to the diagnostics log: `gpu_frame_ms` then
/// `gpu_<segment>_ms` per segment, each preceded by a comma.
pub fn csv_columns(names: &[&str]) -> String {
    let mut out = String::from(",gpu_frame_ms");
    for n in names {
        out.push_str(&format!(",gpu_{n}_ms"));
    }
    out
}

/// Accumulates [`FrameTimings`] and returns their mean (the per-second average of `--stats`, the log and
/// the overlay).
#[derive(Default)]
pub struct TimingAverager {
    frames: u32,
    frame_sum: f64,
    /// Per segment: name, sum of ms, frames it ran, frames it ran bypassed.
    stages: Vec<(&'static str, f64, u32, u32)>,
}

impl TimingAverager {
    pub fn push(&mut self, t: &FrameTimings) {
        self.frames += 1;
        self.frame_sum += t.frame_ms;
        for s in &t.stages {
            let i = match self.stages.iter().position(|e| e.0 == s.name) {
                Some(i) => i,
                None => {
                    self.stages.push((s.name, 0.0, 0, 0));
                    self.stages.len() - 1
                }
            };
            if let Some(ms) = s.ms {
                let e = &mut self.stages[i];
                e.1 += ms;
                e.2 += 1;
                e.3 += u32::from(s.bypassed);
            }
        }
    }

    /// The mean of the frames pushed since the last call (a segment's mean is over the frames it ran
    /// in, `None` when it never ran), then resets. `None` without any frame.
    pub fn take(&mut self) -> Option<FrameTimings> {
        if self.frames == 0 {
            return None;
        }
        let n = f64::from(self.frames);
        let out = FrameTimings {
            frames: self.frames,
            frame_ms: self.frame_sum / n,
            stages: self
                .stages
                .iter()
                .map(|&(name, sum, ran, bypassed)| StageTiming {
                    name,
                    ms: (ran > 0).then(|| sum / f64::from(ran)),
                    bypassed: ran > 0 && bypassed == ran,
                })
                .collect(),
        };
        self.frames = 0;
        self.frame_sum = 0.0;
        for e in &mut self.stages {
            (e.1, e.2, e.3) = (0.0, 0, 0);
        }
        Some(out)
    }
}

/// What a device needs for timestamps: the milliseconds per tick and the mask of the valid bits, or the
/// reason it cannot do them. Pure, so that the refusals are unit tested.
pub(super) fn tick_scale(
    timestamps_on_all_queues: bool,
    valid_bits: u32,
    period_ns: f32,
) -> Result<(f64, u64), String> {
    if !timestamps_on_all_queues {
        return Err("the device does not support timestamps on its graphics and compute queues".into());
    }
    if valid_bits == 0 {
        return Err("the graphics queue family has no valid timestamp bits".into());
    }
    if !(period_ns > 0.0 && period_ns.is_finite()) {
        return Err(format!(
            "the device reports an unusable timestamp period ({period_ns} ns)"
        ));
    }
    let mask = if valid_bits >= 64 {
        u64::MAX
    } else {
        (1u64 << valid_bits) - 1
    };
    Ok((f64::from(period_ns) * 1e-6, mask))
}

/// Milliseconds between two raw timestamps of a `mask`-bit counter (wrap-around safe).
pub(super) fn ticks_to_ms(start: u64, end: u64, mask: u64, ms_per_tick: f64) -> f64 {
    (end.wrapping_sub(start) & mask) as f64 * ms_per_tick
}

/// Mark state of a timestamp of the frame being recorded.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mark {
    #[default]
    Unwritten,
    Run,
    Bypassed,
}

/// One frame slot's query pool and the state of its marks.
#[derive(Default)]
struct TimerSlot {
    pool: vk::QueryPool,
    /// Marks: 0 = start, 1 + k = end of segment k, last = end of the frame.
    marks: Vec<Mark>,
    /// The slot was submitted and its results were not read yet.
    pending: bool,
}

/// The timestamp queries of the pipeline (one pool per frame slot).
pub(super) struct GpuTimer {
    /// Segment names in recording order: `upload`, `decode`, the chain slots, `encode`, `ui`.
    names: Vec<&'static str>,
    ms_per_tick: f64,
    mask: u64,
    unavailable: Option<String>,
    slots: [TimerSlot; FRAMES_IN_FLIGHT],
    /// Raw results (value, availability) of one read, reused.
    scratch: Vec<u64>,
}

/// Segment index of the staged frame or injected image copy.
pub(super) const UPLOAD: usize = 0;
/// Segment index of `decode`.
pub(super) const DECODE: usize = 1;
/// Segment index of chain slot `i`.
pub(super) const fn chain(i: usize) -> usize {
    2 + i
}

impl GpuTimer {
    /// A timer for the chain slots `chain`; [`GpuTimer::create`] makes the Vulkan objects.
    pub fn new(chain: impl IntoIterator<Item = &'static str>) -> Self {
        let mut names = vec!["upload", "decode"];
        names.extend(chain);
        names.push("encode");
        names.push("ui");
        Self {
            names,
            ms_per_tick: 0.0,
            mask: 0,
            unavailable: Some("not created".into()),
            slots: Default::default(),
            scratch: Vec::new(),
        }
    }

    /// Segment index of `encode`.
    pub fn encode(&self) -> usize {
        self.names.len() - 2
    }

    /// Segment index of the UI pass (T-52a), after `encode`; not recorded while the UI is hidden.
    pub fn ui(&self) -> usize {
        self.names.len() - 1
    }

    /// Segment names in recording order (the columns of the diagnostics log).
    pub fn names(&self) -> &[&'static str] {
        &self.names
    }

    /// The reason timestamps are off, `None` when they work.
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// Creates the query pools, or records why the device cannot (not an error).
    ///
    /// # Safety
    /// `ctx` is live; the caller destroys the timer with [`GpuTimer::destroy`] before `ctx`.
    pub unsafe fn create(&mut self, ctx: &Context) -> Res<()> {
        let limits = ctx.instance.get_physical_device_properties(ctx.physical).limits;
        let family = ctx
            .instance
            .get_physical_device_queue_family_properties(ctx.physical)
            .get(ctx.queue_family as usize)
            .map_or(0, |f| f.timestamp_valid_bits);
        match tick_scale(
            limits.timestamp_compute_and_graphics == vk::TRUE,
            family,
            limits.timestamp_period,
        ) {
            Ok((ms, mask)) => {
                (self.ms_per_tick, self.mask, self.unavailable) = (ms, mask, None);
            }
            Err(reason) => {
                self.unavailable = Some(reason);
                return Ok(());
            }
        }
        let marks = self.names.len() + 2;
        for s in &mut self.slots {
            s.marks = vec![Mark::Unwritten; marks];
            s.pool = ctx.device.create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(marks as u32),
                None,
            )?;
        }
        self.scratch = vec![0; 2 * marks];
        Ok(())
    }

    fn write(&mut self, d: &ash::Device, cb: vk::CommandBuffer, slot: usize, mark: usize, state: Mark) {
        let s = &mut self.slots[slot];
        // SAFETY: `cb` is recording (the callers' contract) and `mark` is below the pool's query count.
        unsafe {
            d.cmd_write_timestamp2(cb, vk::PipelineStageFlags2::ALL_COMMANDS, s.pool, mark as u32);
        }
        s.marks[mark] = state;
    }

    /// Starts the frame: resets the slot's queries and writes the start mark. No-op without timestamps.
    ///
    /// # Safety
    /// `cb` is recording on `d`, outside a render pass; the slot's previous results were read or are
    /// not needed (the previous use finished: fence waited).
    pub unsafe fn begin(&mut self, d: &ash::Device, cb: vk::CommandBuffer, slot: usize) {
        if self.unavailable.is_some() {
            return;
        }
        let s = &mut self.slots[slot];
        s.pending = false;
        s.marks.fill(Mark::Unwritten);
        d.cmd_reset_query_pool(cb, s.pool, 0, s.marks.len() as u32);
        self.write(d, cb, slot, 0, Mark::Run);
    }

    /// Writes the end mark of segment `segment` (`bypassed`: the slot ran its resample copy).
    ///
    /// # Safety
    /// As [`GpuTimer::begin`], after it, outside a render pass; `segment` is a valid segment index.
    pub unsafe fn mark(&mut self, d: &ash::Device, cb: vk::CommandBuffer, slot: usize, segment: usize, bypassed: bool) {
        if self.unavailable.is_some() {
            return;
        }
        let state = if bypassed { Mark::Bypassed } else { Mark::Run };
        self.write(d, cb, slot, 1 + segment, state);
    }

    /// Writes the end-of-frame mark.
    ///
    /// # Safety
    /// As [`GpuTimer::mark`].
    pub unsafe fn finish(&mut self, d: &ash::Device, cb: vk::CommandBuffer, slot: usize) {
        if self.unavailable.is_some() {
            return;
        }
        let last = self.slots[slot].marks.len() - 1;
        self.write(d, cb, slot, last, Mark::Run);
    }

    /// The frame recorded into `slot` was submitted: its results can be read once its fence signals.
    pub fn submitted(&mut self, slot: usize) {
        if self.unavailable.is_none() {
            self.slots[slot].pending = true;
        }
    }

    /// Reads the results of the last submission of `slot`, if there is one not read yet. Never waits:
    /// the caller knows the submission finished (fence waited, or device idle).
    ///
    /// # Safety
    /// `slot`'s last submission has completed on the GPU.
    pub unsafe fn harvest(&mut self, d: &ash::Device, slot: usize) -> Option<FrameTimings> {
        if self.unavailable.is_some() || !std::mem::take(&mut self.slots[slot].pending) {
            return None;
        }
        let s = &self.slots[slot];
        let n = s.marks.len();
        // 64-bit values with an availability word after each: 16 bytes per query. An unavailable query
        // (a mark that was not written) makes the call return NOT_READY but fills the others.
        let r = (d.fp_v1_0().get_query_pool_results)(
            d.handle(),
            s.pool,
            0,
            n as u32,
            n * 16,
            self.scratch.as_mut_ptr().cast(),
            16,
            vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WITH_AVAILABILITY,
        );
        if r != vk::Result::SUCCESS && r != vk::Result::NOT_READY {
            return None;
        }
        let (mask, scale) = (self.mask, self.ms_per_tick);
        let at = |k: usize| (self.scratch[2 * k + 1] != 0).then(|| self.scratch[2 * k]);
        let (start, end) = (at(0)?, at(n - 1)?);
        let mut prev = start;
        let mut stages = Vec::with_capacity(n - 2);
        for (k, name) in self.names.iter().enumerate() {
            let (state, t) = (s.marks[1 + k], at(1 + k));
            let ms = match (state, t) {
                (Mark::Unwritten, _) | (_, None) => None,
                (_, Some(t)) => {
                    let ms = ticks_to_ms(prev, t, mask, scale);
                    prev = t;
                    Some(ms)
                }
            };
            stages.push(StageTiming {
                name,
                ms,
                bypassed: ms.is_some() && state == Mark::Bypassed,
            });
        }
        Some(FrameTimings {
            frames: 1,
            frame_ms: ticks_to_ms(start, end, mask, scale),
            stages,
        })
    }

    /// Destroys the query pools (null ones are no-ops). Idempotent.
    ///
    /// # Safety
    /// The GPU no longer uses the pools.
    pub unsafe fn destroy(&mut self, d: &ash::Device) {
        for s in &mut self.slots {
            d.destroy_query_pool(s.pool, None);
            s.pool = vk::QueryPool::null();
            s.pending = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_scale_refuses_devices_without_timestamps() {
        assert!(tick_scale(false, 64, 1.0).unwrap_err().contains("does not support"));
        assert!(tick_scale(true, 0, 1.0).unwrap_err().contains("valid timestamp bits"));
        for p in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(tick_scale(true, 64, p).unwrap_err().contains("period"), "{p}");
        }
        let (ms, mask) = tick_scale(true, 36, 52.0).unwrap();
        assert!((ms - 52.0e-6).abs() < 1e-12);
        assert_eq!(mask, (1u64 << 36) - 1);
        assert_eq!(tick_scale(true, 64, 1.0).unwrap().1, u64::MAX);
    }

    #[test]
    fn ticks_convert_with_the_period_and_wrap_at_the_valid_bits() {
        // 1_000_000 ticks of 1 ns = 1 ms.
        assert!((ticks_to_ms(10, 1_000_010, u64::MAX, 1e-6) - 1.0).abs() < 1e-12);
        // 36 valid bits: a counter that wrapped between the marks.
        let mask = (1u64 << 36) - 1;
        let (a, b) = (mask - 499_999, 500_000);
        assert!((ticks_to_ms(a, b, mask, 1e-6) - 1.0).abs() < 1e-12);
        // Upper garbage bits above the valid ones are ignored.
        assert!((ticks_to_ms(5 | (7 << 40), 1_000_005 | (9 << 40), mask, 1e-6) - 1.0).abs() < 1e-12);
    }

    fn sample(frame_ms: f64, stages: &[(&'static str, Option<f64>, bool)]) -> FrameTimings {
        FrameTimings {
            frames: 1,
            frame_ms,
            stages: stages
                .iter()
                .map(|&(name, ms, bypassed)| StageTiming { name, ms, bypassed })
                .collect(),
        }
    }

    #[test]
    fn stats_line_has_one_value_per_segment() {
        let t = sample(
            0.5,
            &[
                ("upload", None, false),
                ("decode", Some(0.1), false),
                ("amp", Some(0.02), true),
                ("encode", Some(0.2), false),
            ],
        );
        assert_eq!(
            t.stats_line(),
            "gpu 0.50 ms | upload - | decode 0.100 | amp 0.020 (bypass) | encode 0.200"
        );
        assert_eq!(t.stage_ms("decode"), Some(0.1));
        assert_eq!(t.stage_ms("upload"), None);
        assert_eq!(t.stage_ms("nope"), None);
        assert!((t.sum_ms() - 0.32).abs() < 1e-12);
    }

    #[test]
    fn csv_columns_and_values_line_up() {
        let names = ["upload", "decode", "encode"];
        assert_eq!(
            csv_columns(&names),
            ",gpu_frame_ms,gpu_upload_ms,gpu_decode_ms,gpu_encode_ms"
        );
        let t = sample(
            0.5,
            &[
                ("upload", None, false),
                ("decode", Some(0.1), false),
                ("encode", Some(0.25), false),
            ],
        );
        assert_eq!(FrameTimings::csv_values(Some(&t), &names), ",0.500,,0.100,0.250");
        // Without timings: as many empty values as columns.
        let blank = FrameTimings::csv_values(None, &names);
        assert_eq!(blank, ",,,,");
        assert_eq!(blank.matches(',').count(), csv_columns(&names).matches(',').count());
    }

    /// A slot whose stage really runs (the test dummy) is timed and not flagged bypassed; turning it off
    /// makes it absent. Validation silent; needs a device, skipped without one unless `PHOXIUM_REQUIRE_GPU`.
    #[test]
    fn a_running_stage_is_timed_and_a_disabled_one_is_absent() {
        use crate::gpu::stages::{amp, dummy, gun, optics, phosphor, screen, signal, Registration};
        use crate::gpu::{Headless, Settings, SourceFrame, TargetFormat};
        use crate::params::Params;
        use crate::scaling::View;
        const CHAIN: &[Registration] = &[
            signal::SLOT,
            amp::SLOT,
            gun::SLOT,
            screen::SLOT,
            dummy::SLOT,
            phosphor::SLOT,
            optics::SLOT,
        ];
        let required = crate::gpu::gpu_required();
        let validation = crate::gpu::validation_layer_available();
        assert!(!required || validation, "PHOXIUM_REQUIRE_GPU=1 but no validation layer");
        let mut gpu = match Headless::with_chain(validation, CHAIN) {
            Ok(g) => g,
            Err(e) => {
                assert!(!required, "PHOXIUM_REQUIRE_GPU=1 but no headless device: {e}");
                eprintln!("timing dummy test skipped: {e}");
                return;
            }
        };
        if let Some(why) = gpu.timing_unavailable() {
            assert!(!required, "PHOXIUM_REQUIRE_GPU=1 but no timestamps: {why}");
            eprintln!("timing dummy test skipped: {why}");
            return;
        }
        let counter = gpu.validation_counter();
        let (w, h) = (512u32, 512u32);
        let bgra: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 7, (i >> 8) as u8, 255]).collect();
        let render = |gpu: &mut Headless| {
            gpu.render(
                Some(SourceFrame {
                    width: w,
                    height: h,
                    bgra: &bgra,
                }),
                View::full(w, h),
                &Settings::identity(),
                &Params::default(),
                (w, h),
                TargetFormat::Bgra8Unorm,
            )
            .expect("render");
            gpu.last_timings().expect("timings").clone()
        };
        let on = render(&mut gpu);
        let dummy_on = on.stages.iter().find(|s| s.name == "dummy").expect("dummy segment");
        assert!(dummy_on.ms.is_some_and(|ms| ms > 0.0), "{dummy_on:?}");
        assert!(!dummy_on.bypassed);
        gpu.set_stage_enabled("dummy", false).unwrap();
        let off = render(&mut gpu);
        assert_eq!(off.stage_ms("dummy"), None);
        assert!(!off.stages.iter().find(|s| s.name == "dummy").unwrap().bypassed);
        drop(gpu);
        assert_eq!(counter.count(), 0, "validation messages (printed above)");
    }

    #[test]
    fn the_averager_means_over_the_frames_a_segment_ran() {
        let mut a = TimingAverager::default();
        assert_eq!(a.take(), None);
        a.push(&sample(
            1.0,
            &[
                ("decode", Some(0.2), false),
                ("amp", None, false),
                ("encode", Some(0.4), false),
            ],
        ));
        a.push(&sample(
            3.0,
            &[
                ("decode", Some(0.4), false),
                ("amp", Some(0.1), true),
                ("encode", Some(0.6), false),
            ],
        ));
        let m = a.take().expect("two frames");
        assert_eq!(m.frames, 2);
        assert!((m.frame_ms - 2.0).abs() < 1e-12);
        assert!((m.stage_ms("decode").unwrap() - 0.3).abs() < 1e-12);
        assert!((m.stage_ms("encode").unwrap() - 0.5).abs() < 1e-12);
        // `amp` ran in one frame only: its mean is over that frame, flagged bypassed.
        assert_eq!(m.stage_ms("amp"), Some(0.1));
        assert!(m.stages[1].bypassed);
        // Taking resets.
        assert_eq!(a.take(), None);
        a.push(&sample(
            1.0,
            &[("decode", None, false), ("amp", None, false), ("encode", None, false)],
        ));
        let m = a.take().unwrap();
        assert_eq!(m.stage_ms("decode"), None);
        assert_eq!(m.frames, 1);
    }
}
