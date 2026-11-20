//! Zero-copy capture (owner request 2026-11-20, ARCHITECTURE A29, QUESTIONS CAP-2): the pure parts, unit tested
//! without any GPU.
//!
//! The optional GPU-to-GPU path: the capture thread copies the client area of each Windows.Graphics.Capture frame
//! into one of [`POOL_SLOTS`] shared Direct3D 11 textures (`capture.rs`), waits on the GPU for that copy, then
//! publishes the slot; the renderer imports every texture of the pool once as a Vulkan image
//! (`VK_KHR_external_memory_win32`, `gpu/external.rs`) and `decode` samples it directly. No pixel crosses the CPU.
//! Zero-copy is the default ([`CapturePath::ZeroCopy`], owner request 2026-11-20): the session uses it when the
//! platform allows it ([`decide`]) and falls back to the simple CPU copy ([`CapturePath::Cpu`], which works on any
//! adapter pair) otherwise, or on any failure at runtime ([`Fallback`]). The Vulkan device enables the extension
//! whenever it can import the shared textures, so the path can be switched both ways during a session.
//!
//! This module holds what both sides agree on and what can be decided without a device: the setting
//! ([`CapturePath`]), the slot state machine shared by the two threads ([`SlotPool`]), the release-by-fence rule of
//! the render thread ([`InFlight`]), the handle type ([`select_handle`]), the client-area crop ([`clamp_crop`]),
//! the decision whether the path may run ([`decide`]) and the runtime fallback ([`Fallback`]).

use std::fmt;

/// Shared textures per pool: one the capture writes, one the render thread reads in the current frame, one an
/// earlier frame still in flight may read. The capture thread never waits for a slot: when none is free it keeps
/// the frame and copies it as soon as one is released (`capture.rs`, `Worker::pending`).
pub const POOL_SLOTS: usize = 3;

/// Which way captured frames reach the renderer (`[global.capture] path`, `--capture-path`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CapturePath {
    /// The capture texture is copied to a CPU buffer and uploaded (works on every adapter pair).
    Cpu,
    /// The capture texture is copied on the GPU into a shared texture that Vulkan samples directly (the default;
    /// needs one adapter for capture and rendering, else the CPU copy is used).
    #[default]
    ZeroCopy,
}

impl CapturePath {
    /// Every value, in menu order.
    pub const ALL: [CapturePath; 2] = [CapturePath::ZeroCopy, CapturePath::Cpu];

    /// The settings value: `cpu` or `zero_copy`.
    pub fn id(self) -> &'static str {
        match self {
            CapturePath::Cpu => "cpu",
            CapturePath::ZeroCopy => "zero_copy",
        }
    }

    /// Reads a settings or command-line value: `cpu`, `zero_copy` or `zero-copy`, or `auto` (the default, zero-copy
    /// with the automatic fallback), any case, spaces trimmed.
    pub fn parse(text: &str) -> Option<CapturePath> {
        match text.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "cpu" => Some(CapturePath::Cpu),
            "zero_copy" | "zerocopy" | "auto" => Some(CapturePath::ZeroCopy),
            _ => None,
        }
    }

    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            CapturePath::Cpu => "CPU copy (most compatible)",
            CapturePath::ZeroCopy => "Zero-copy (GPU sharing, default)",
        }
    }

    /// The name of the path in force, for the status lines (menu, About).
    pub fn name(self) -> &'static str {
        match self {
            CapturePath::Cpu => "CPU copy",
            CapturePath::ZeroCopy => "Zero-copy (GPU sharing)",
        }
    }

    /// The short name of the `--stats` line, the `--log` column and the About box.
    pub fn short(self) -> &'static str {
        match self {
            CapturePath::Cpu => "cpu",
            CapturePath::ZeroCopy => "zero-copy",
        }
    }
}

impl fmt::Display for CapturePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.short())
    }
}

/// The state of one shared texture of the pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    /// Nobody uses it: the capture may write it.
    Free,
    /// The capture thread is copying a frame into it.
    Writing,
    /// It holds the captured frame `seq`, complete on the GPU, not taken yet.
    Ready(u64),
    /// The render side holds it (taken, or read by a frame still in flight); released by
    /// [`SlotPool::release`].
    Held,
}

/// The slot state machine shared by the capture thread (writer) and the render thread (reader), behind one mutex.
/// Rules: one writer; the newest ready frame wins (an older ready frame goes back to free when a newer one is
/// published, it was never shown); a held slot is never written; the capture prefers a free slot and takes the
/// oldest ready one only when none is free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotPool {
    states: [SlotState; POOL_SLOTS],
}

impl Default for SlotPool {
    fn default() -> Self {
        Self {
            states: [SlotState::Free; POOL_SLOTS],
        }
    }
}

impl SlotPool {
    /// The state of every slot.
    pub fn states(&self) -> [SlotState; POOL_SLOTS] {
        self.states
    }

    /// Capture thread: a slot to write the next frame into (now `Writing`), `None` when every slot is written or
    /// held. A free slot first, else the ready slot holding the oldest frame (overwritten, it was never shown).
    pub fn begin_write(&mut self) -> Option<usize> {
        if self.states.contains(&SlotState::Writing) {
            return None;
        }
        let slot = self.states.iter().position(|s| *s == SlotState::Free).or_else(|| {
            self.states
                .iter()
                .enumerate()
                .filter_map(|(i, s)| match s {
                    SlotState::Ready(seq) => Some((i, *seq)),
                    _ => None,
                })
                .min_by_key(|&(_, seq)| seq)
                .map(|(i, _)| i)
        })?;
        self.states[slot] = SlotState::Writing;
        Some(slot)
    }

    /// Capture thread: the copy into `slot` finished on the GPU; it now holds frame `seq`. Every other ready slot
    /// (an older frame never taken) becomes free. A slot that is not `Writing` is left as it is (false).
    pub fn publish(&mut self, slot: usize, seq: u64) -> bool {
        if self.states.get(slot) != Some(&SlotState::Writing) {
            return false;
        }
        for s in &mut self.states {
            if matches!(s, SlotState::Ready(_)) {
                *s = SlotState::Free;
            }
        }
        self.states[slot] = SlotState::Ready(seq);
        true
    }

    /// Capture thread: the copy into `slot` failed; it is free again.
    pub fn abort_write(&mut self, slot: usize) {
        if self.states.get(slot) == Some(&SlotState::Writing) {
            self.states[slot] = SlotState::Free;
        }
    }

    /// Render side: takes the newest ready frame when it is newer than `after` (the sequence number last taken):
    /// the slot becomes `Held` and (slot, seq) is returned.
    pub fn take(&mut self, after: u64) -> Option<(usize, u64)> {
        let (slot, seq) = self
            .states
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                SlotState::Ready(seq) if *seq > after => Some((i, *seq)),
                _ => None,
            })
            .max_by_key(|&(_, seq)| seq)?;
        self.states[slot] = SlotState::Held;
        Some((slot, seq))
    }

    /// Render side: the slot is no longer read by anybody; the capture may write it again. Only a held slot changes.
    pub fn release(&mut self, slot: usize) {
        if self.states.get(slot) == Some(&SlotState::Held) {
            self.states[slot] = SlotState::Free;
        }
    }

    /// Slots in each state, for the diagnostics: (free, writing, ready, held).
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let n = |f: fn(&SlotState) -> bool| self.states.iter().filter(|s| f(s)).count();
        (
            n(|s| *s == SlotState::Free),
            n(|s| *s == SlotState::Writing),
            n(|s| matches!(s, SlotState::Ready(_))),
            n(|s| *s == SlotState::Held),
        )
    }
}

/// The render thread's rule for releasing what the GPU reads (release by fence): `T` is a hold on a shared slot
/// (dropping it releases the slot, `capture::SharedFrame`). The current source stays held while it is the source;
/// each frame in flight keeps a clone of the hold it read until its fence was waited.
#[derive(Debug)]
pub struct InFlight<T, const F: usize> {
    /// The source of the next frames.
    current: Option<T>,
    /// What each frame slot (the pipeline's frames in flight) read in its last submission.
    reads: [Option<T>; F],
}

impl<T, const F: usize> Default for InFlight<T, F> {
    fn default() -> Self {
        Self {
            current: None,
            reads: std::array::from_fn(|_| None),
        }
    }
}

impl<T: Clone, const F: usize> InFlight<T, F> {
    /// A new source frame: the previous current one is dropped (released, unless a frame in flight read it).
    pub fn set_current(&mut self, hold: T) {
        self.current = Some(hold);
    }

    /// The source is not a shared slot any more (CPU frame, fallback): the current hold goes.
    pub fn clear_current(&mut self) {
        self.current = None;
    }

    /// The current source.
    pub fn current(&self) -> Option<&T> {
        self.current.as_ref()
    }

    /// Frame slot `frame` was submitted; it read the current source when `read_current`.
    pub fn submitted(&mut self, frame: usize, read_current: bool) {
        self.reads[frame] = if read_current { self.current.clone() } else { None };
    }

    /// The fence of frame slot `frame` was waited: what it read is released (the hold dropped).
    pub fn frame_done(&mut self, frame: usize) {
        self.reads[frame] = None;
    }

    /// The device is idle: every hold of the frames in flight goes, and the current one too.
    pub fn drain(&mut self) {
        self.current = None;
        for r in &mut self.reads {
            *r = None;
        }
    }

    /// Holds kept: the current one and those of the frames in flight (clones of one hold count once each).
    pub fn held(&self) -> usize {
        self.current.iter().count() + self.reads.iter().flatten().count()
    }
}

/// The kind of shared handle of the Direct3D 11 textures and the matching Vulkan handle type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// An NT handle (`IDXGIResource1::CreateSharedHandle`, `D3D11_RESOURCE_MISC_SHARED_NTHANDLE`), Vulkan
    /// `D3D11_TEXTURE`. Preferred: the modern kind, with access rights and reference counting.
    Nt,
    /// A global (KMT) handle (`IDXGIResource::GetSharedHandle`, `D3D11_RESOURCE_MISC_SHARED`), Vulkan
    /// `D3D11_TEXTURE_KMT`.
    Kmt,
}

impl HandleKind {
    pub fn name(self) -> &'static str {
        match self {
            HandleKind::Nt => "NT handle (D3D11_TEXTURE)",
            HandleKind::Kmt => "KMT handle (D3D11_TEXTURE_KMT)",
        }
    }
}

/// The handle kind to use from the Vulkan device's answers for B8G8R8A8 images (`ExternalMemoryProbe`:
/// (importable, exportable) per handle type, `None` when the type is not supported): an importable NT handle
/// first, else an importable KMT handle, else none.
pub fn select_handle(nt: Option<(bool, bool)>, kmt: Option<(bool, bool)>) -> Option<HandleKind> {
    if nt.is_some_and(|(import, _)| import) {
        Some(HandleKind::Nt)
    } else if kmt.is_some_and(|(import, _)| import) {
        Some(HandleKind::Kmt)
    } else {
        None
    }
}

/// The client area (x, y, w, h) inside a captured texture of `tex` (width, height), clamped to it: the origin
/// moved inside, the size cut at the texture's edge. Negative values (a window partly off its frame) count as 0.
pub fn clamp_crop(x: i64, y: i64, w: i64, h: i64, tex: (u32, u32)) -> (u32, u32, u32, u32) {
    let x = x.clamp(0, i64::from(tex.0)) as u32;
    let y = y.clamp(0, i64::from(tex.1)) as u32;
    let w = w.clamp(0, i64::from(tex.0 - x)) as u32;
    let h = h.clamp(0, i64::from(tex.1 - y)) as u32;
    (x, y, w, h)
}

/// What the zero-copy path needs, from queries and the session state (no allocation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Support {
    /// The Vulkan device offers `VK_KHR_external_memory_win32`.
    pub extension: bool,
    /// The handle kind the device imports ([`select_handle`]) with the extension enabled on the device (it is,
    /// whenever the device imports one), `None` when it imports neither.
    pub handle: Option<HandleKind>,
    /// The capture device and the Vulkan device are on the same adapter (same LUID); `None` when unknown.
    pub same_adapter: Option<bool>,
}

/// Why the zero-copy path does not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The Vulkan device does not offer `VK_KHR_external_memory_win32`.
    NoExtension,
    /// The Vulkan device imports neither NT nor KMT Direct3D 11 textures in B8G8R8A8.
    NoImport,
    /// Capture and rendering run on different adapters: a texture cannot be shared across adapters.
    CrossAdapter,
    /// The adapters could not be compared (no LUID).
    UnknownAdapter,
    /// A runtime failure (texture creation, handle, import, copy wait, device), with its text.
    Failed(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NoExtension => f.write_str("the Vulkan device does not offer VK_KHR_external_memory_win32"),
            Refusal::NoImport => f.write_str("the Vulkan device cannot import Direct3D 11 textures (B8G8R8A8)"),
            Refusal::CrossAdapter => f.write_str(
                "capture and rendering run on different adapters; sharing a texture needs one adapter (the CPU copy crosses adapters)",
            ),
            Refusal::UnknownAdapter => f.write_str("the capture and rendering adapters could not be compared (no LUID)"),
            Refusal::Failed(why) => write!(f, "zero-copy failed: {why}"),
        }
    }
}

impl Refusal {
    /// A property of this machine and session (extension, import, adapters), not a failure: the default path falls
    /// back without a title notice, and the menu shows the zero-copy choice disabled with the reason.
    pub fn is_platform(&self) -> bool {
        !matches!(self, Refusal::Failed(_))
    }
}

/// Whether the zero-copy path may run (the fallback decision table): `Ok` with the handle kind, or why not. The
/// extension comes first (nothing else matters without it), then the adapter, then import.
pub fn decide(requested: CapturePath, s: Support) -> Result<Option<HandleKind>, Refusal> {
    if requested == CapturePath::Cpu {
        return Ok(None);
    }
    if !s.extension {
        return Err(Refusal::NoExtension);
    }
    match s.same_adapter {
        Some(false) => return Err(Refusal::CrossAdapter),
        None => return Err(Refusal::UnknownAdapter),
        Some(true) => {}
    }
    s.handle.map(Some).ok_or(Refusal::NoImport)
}

/// Why this machine cannot run zero-copy at all ([`decide`] for a zero-copy request), `None` when it can: the menu
/// shows the zero-copy choice disabled with this reason.
pub fn unavailable(s: Support) -> Option<Refusal> {
    decide(CapturePath::ZeroCopy, s).err()
}

/// The path in force for the session: what the setting asks and, after a refusal or a failure, the CPU path with
/// the reason. Each reason is reported once ([`Fallback::refuse`] returns true the first time).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fallback {
    requested: CapturePath,
    reason: Option<Refusal>,
}

impl Fallback {
    pub fn new(requested: CapturePath) -> Self {
        Self {
            requested,
            reason: None,
        }
    }

    /// The setting changed: a new request clears the reason of the previous one (it is tried again).
    pub fn request(&mut self, requested: CapturePath) {
        if requested != self.requested {
            self.requested = requested;
            self.reason = None;
        }
    }

    /// What the setting asks.
    pub fn requested(&self) -> CapturePath {
        self.requested
    }

    /// The path in force: zero-copy only when asked and not refused.
    pub fn active(&self) -> CapturePath {
        if self.requested == CapturePath::ZeroCopy && self.reason.is_none() {
            CapturePath::ZeroCopy
        } else {
            CapturePath::Cpu
        }
    }

    /// Falls back to the CPU path for `why`. True when this is news (log it, show the notice): the first refusal
    /// since the request, while zero-copy is asked.
    pub fn refuse(&mut self, why: Refusal) -> bool {
        if self.requested != CapturePath::ZeroCopy || self.reason.is_some() {
            return false;
        }
        self.reason = Some(why);
        true
    }

    /// Why the CPU path is used although zero-copy is asked, `None` otherwise.
    pub fn reason(&self) -> Option<&Refusal> {
        self.reason.as_ref()
    }

    /// The line the menu, `--stats` and the title show: `zero-copy`, `cpu`, or `cpu (zero-copy refused: ...)`.
    pub fn describe(&self) -> String {
        match &self.reason {
            Some(r) if self.requested == CapturePath::ZeroCopy => format!("cpu (zero-copy off: {r})"),
            _ => self.active().short().to_string(),
        }
    }
}

/// Pool generations (and capture session ids) come from one counter for the whole process: a capture session
/// restarted on another adapter, a reattach or a new source makes pools whose generations never repeat an
/// earlier session's (each session used to count from 1, so a restarted session's first pool looked like the
/// pool the renderer had imported and the renderer kept sampling the old, stale textures). The counter starts far
/// above the small hand-made generations of the tests.
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 32);

/// A new pool generation or capture session id, never returned before in this process.
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Whether the renderer must import `offered` (generation, pool identity: the address of the shared pool, kept
/// alive by the import so it cannot be reused meanwhile) when it holds the imports of `imported`: any difference
/// of either value is a new pool. Pure.
pub fn needs_import(imported: Option<(u64, usize)>, offered: (u64, usize)) -> bool {
    imported != Some(offered)
}

/// Whether the frame `frame` (capture session id, sequence number) needs no upload because the renderer already
/// samples its shared texture (`held`, recorded when it was handed over, and `renderer_shared`): the same frame of
/// the same session only. A new session counts its frames from 1 again: its frame 5 is not the old session's
/// frame 5. Pure.
pub fn already_held(held: Option<(u64, u64)>, frame: (u64, u64), renderer_shared: bool) -> bool {
    renderer_shared && held == Some(frame)
}

/// Running mean of the capture thread's copy cost per frame (µs), read and restarted by the statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CopyCost {
    sum_us: f64,
    n: u64,
    max_us: f64,
}

impl CopyCost {
    pub fn push(&mut self, us: f64) {
        if us.is_finite() && us >= 0.0 {
            self.sum_us += us;
            self.n += 1;
            self.max_us = self.max_us.max(us);
        }
    }

    /// (mean ms, max ms, frames), then restarts; `None` without a frame.
    pub fn take(&mut self) -> Option<(f64, f64, u64)> {
        let out = (self.n > 0).then(|| (self.sum_us / self.n as f64 / 1000.0, self.max_us / 1000.0, self.n));
        *self = Self::default();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn capture_path_names_round_trip() {
        for p in CapturePath::ALL {
            assert_eq!(CapturePath::parse(p.id()), Some(p));
        }
        assert_eq!(CapturePath::parse(" Zero-Copy "), Some(CapturePath::ZeroCopy));
        assert_eq!(CapturePath::parse("CPU"), Some(CapturePath::Cpu));
        assert_eq!(CapturePath::parse("gpu"), None);
        // `auto` is the default: zero-copy with the automatic fallback.
        assert_eq!(CapturePath::parse(" Auto"), Some(CapturePath::ZeroCopy));
        assert_eq!(CapturePath::default(), CapturePath::ZeroCopy);
        assert_eq!(CapturePath::ALL[0], CapturePath::default());
        assert_eq!(CapturePath::ZeroCopy.to_string(), "zero-copy");
    }

    #[test]
    fn pool_writes_publishes_takes_and_releases() {
        let mut p = SlotPool::default();
        let a = p.begin_write().unwrap();
        assert_eq!(a, 0);
        // One writer only.
        assert_eq!(p.begin_write(), None);
        assert!(p.publish(a, 1));
        assert_eq!(p.take(0), Some((0, 1)));
        // Nothing newer.
        assert_eq!(p.take(1), None);
        let b = p.begin_write().unwrap();
        assert_eq!(b, 1);
        assert!(p.publish(b, 2));
        assert_eq!(p.states(), [SlotState::Held, SlotState::Ready(2), SlotState::Free]);
        // Only a frame newer than the one taken is taken.
        assert_eq!(p.take(2), None);
        assert_eq!(p.take(1), Some((1, 2)));
        p.release(0);
        assert_eq!(p.states(), [SlotState::Free, SlotState::Held, SlotState::Free]);
        // Releasing a slot that is not held changes nothing.
        p.release(0);
        p.release(7);
        assert_eq!(p.counts(), (2, 0, 0, 1));
    }

    #[test]
    fn newest_ready_frame_wins_and_older_ones_are_freed() {
        let mut p = SlotPool::default();
        for seq in 1..=5 {
            let s = p.begin_write().unwrap();
            assert!(p.publish(s, seq));
            // Never more than one ready frame.
            assert_eq!(p.counts().2, 1);
        }
        assert_eq!(p.take(0).map(|(_, seq)| seq), Some(5));
    }

    #[test]
    fn capture_takes_the_oldest_ready_slot_when_none_is_free() {
        let mut p = SlotPool::default();
        // Two slots held by the render side (current source and a frame in flight).
        for seq in 1..=2 {
            let s = p.begin_write().unwrap();
            p.publish(s, seq);
            p.take(seq - 1).unwrap();
        }
        let s = p.begin_write().unwrap();
        p.publish(s, 3);
        assert_eq!(p.counts(), (0, 0, 1, 2));
        // The ready frame 3 was never taken: frame 4 overwrites it.
        assert_eq!(p.begin_write(), Some(s));
        p.publish(s, 4);
        assert_eq!(p.take(2), Some((s, 4)));
        // Now all three are held: the capture must wait (it keeps the frame, capture.rs).
        assert_eq!(p.begin_write(), None);
        p.release(0);
        assert_eq!(p.begin_write(), Some(0));
    }

    #[test]
    fn a_failed_write_frees_the_slot_and_publish_needs_a_write() {
        let mut p = SlotPool::default();
        let s = p.begin_write().unwrap();
        p.abort_write(s);
        assert_eq!(p.counts(), (3, 0, 0, 0));
        assert!(!p.publish(s, 1));
        assert_eq!(p.take(0), None);
    }

    /// A hold that counts the live clones, as `capture::SharedFrame` releases its slot when the last one goes.
    #[derive(Clone)]
    struct Hold(#[allow(dead_code)] Arc<usize>, Arc<AtomicUsize>);

    impl Drop for Hold {
        fn drop(&mut self) {
            // The last clone of this slot's hold: count a release.
            if Arc::strong_count(&self.0) == 1 {
                self.1.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn a_slot_is_released_only_after_every_frame_that_read_it_finished() {
        let released = Arc::new(AtomicUsize::new(0));
        let hold = |slot| Hold(Arc::new(slot), released.clone());
        let mut f: InFlight<Hold, 2> = InFlight::default();
        // Frame 0 reads A.
        f.set_current(hold(0));
        f.submitted(0, true);
        // A new source B; frame 1 reads B; A is still read by frame 0 in flight.
        f.set_current(hold(1));
        f.submitted(1, true);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        assert_eq!(f.held(), 3);
        // Frame slot 0's fence: A goes.
        f.frame_done(0);
        assert_eq!(released.load(Ordering::SeqCst), 1);
        // Frame slot 0 again, B still current: B is kept.
        f.submitted(0, true);
        f.frame_done(1);
        f.frame_done(0);
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(f.held(), 1);
        // Back to the CPU path: B goes once no frame reads it.
        f.submitted(1, true);
        f.clear_current();
        assert_eq!(released.load(Ordering::SeqCst), 1);
        f.frame_done(1);
        assert_eq!(released.load(Ordering::SeqCst), 2);
        // A frame that did not read a shared slot keeps nothing.
        f.set_current(hold(2));
        f.submitted(0, false);
        assert_eq!(f.held(), 1);
        f.drain();
        assert_eq!(released.load(Ordering::SeqCst), 3);
        assert_eq!(f.held(), 0);
    }

    /// The pool and the render rule together over many frames: two frames in flight, a capture faster than the
    /// render, never a write into a slot the render side holds, never more than the pool.
    #[test]
    fn pool_and_render_rule_never_overwrite_a_read_slot() {
        let mut pool = SlotPool::default();
        // (slot) held by: current, frame 0, frame 1.
        let mut current: Option<usize> = None;
        let mut reads: [Option<usize>; 2] = [None; 2];
        let mut last = 0;
        let mut dropped = 0;
        let release_unused = |pool: &mut SlotPool, current: Option<usize>, reads: &[Option<usize>; 2], s: usize| {
            if current != Some(s) && !reads.contains(&Some(s)) {
                pool.release(s);
            }
        };
        for frame in 0..300u64 {
            let fs = (frame % 2) as usize;
            // wait_slot: the frame two submissions ago finished.
            if let Some(s) = reads[fs].take() {
                release_unused(&mut pool, current, &reads, s);
            }
            // The capture delivers 0 to 3 frames between two renders.
            for k in 0..(frame % 4) {
                match pool.begin_write() {
                    Some(s) => {
                        assert!(
                            current != Some(s) && !reads.contains(&Some(s)),
                            "write into a read slot"
                        );
                        pool.publish(s, frame * 4 + k + 1);
                    }
                    None => dropped += 1,
                }
            }
            if let Some((s, seq)) = pool.take(last) {
                last = seq;
                if let Some(old) = current.replace(s) {
                    release_unused(&mut pool, current, &reads, old);
                }
            }
            reads[fs] = current;
            let held = pool.counts().3;
            assert!(held <= 2, "the render side holds {held} slots");
        }
        // With two frames in flight the render side holds at most two slots, so a slot is always free or ready.
        assert_eq!(dropped, 0);
    }

    #[test]
    fn handle_kind_prefers_nt_then_kmt() {
        assert_eq!(
            select_handle(Some((true, false)), Some((true, false))),
            Some(HandleKind::Nt)
        );
        assert_eq!(
            select_handle(Some((false, true)), Some((true, false))),
            Some(HandleKind::Kmt)
        );
        assert_eq!(select_handle(None, Some((true, true))), Some(HandleKind::Kmt));
        assert_eq!(select_handle(None, Some((false, true))), None);
        assert_eq!(select_handle(None, None), None);
    }

    #[test]
    fn crop_is_clamped_to_the_texture() {
        assert_eq!(clamp_crop(8, 31, 640, 480, (656, 519)), (8, 31, 640, 480));
        // Larger than what is left: cut at the edge.
        assert_eq!(clamp_crop(8, 31, 700, 600, (656, 519)), (8, 31, 648, 488));
        // Origin outside: moved in, nothing left.
        assert_eq!(clamp_crop(900, 10, 10, 10, (656, 519)), (656, 10, 0, 10));
        // Negative origin or size.
        assert_eq!(clamp_crop(-5, -1, 20, -3, (100, 100)), (0, 0, 20, 0));
        assert_eq!(clamp_crop(0, 0, 10, 10, (0, 0)), (0, 0, 0, 0));
    }

    #[test]
    fn decision_table() {
        let ok = Support {
            extension: true,
            handle: Some(HandleKind::Nt),
            same_adapter: Some(true),
        };
        let none = Support {
            extension: false,
            handle: None,
            same_adapter: None,
        };
        // Explicit CPU: never refused, whatever the machine.
        assert_eq!(decide(CapturePath::Cpu, none), Ok(None));
        assert_eq!(decide(CapturePath::Cpu, ok), Ok(None));
        // The default (zero-copy) on a machine that supports it.
        assert_eq!(decide(CapturePath::default(), ok), Ok(Some(HandleKind::Nt)));
        assert_eq!(
            decide(
                CapturePath::ZeroCopy,
                Support {
                    handle: Some(HandleKind::Kmt),
                    ..ok
                }
            ),
            Ok(Some(HandleKind::Kmt))
        );
        // Not supported: no extension, no import.
        assert_eq!(
            decide(CapturePath::ZeroCopy, Support { extension: false, ..ok }),
            Err(Refusal::NoExtension)
        );
        assert_eq!(decide(CapturePath::ZeroCopy, none), Err(Refusal::NoExtension));
        assert_eq!(
            decide(CapturePath::ZeroCopy, Support { handle: None, ..ok }),
            Err(Refusal::NoImport)
        );
        // Adapter mismatch, or adapters that cannot be compared.
        assert_eq!(
            decide(
                CapturePath::ZeroCopy,
                Support {
                    same_adapter: Some(false),
                    ..ok
                }
            ),
            Err(Refusal::CrossAdapter)
        );
        assert_eq!(
            decide(
                CapturePath::ZeroCopy,
                Support {
                    same_adapter: None,
                    ..ok
                }
            ),
            Err(Refusal::UnknownAdapter)
        );
        // The adapter check comes before the import check.
        assert_eq!(
            decide(
                CapturePath::ZeroCopy,
                Support {
                    handle: None,
                    same_adapter: Some(false),
                    ..ok
                }
            ),
            Err(Refusal::CrossAdapter)
        );
        assert!(Refusal::CrossAdapter.to_string().contains("different adapters"));
        // The platform refusals disable the menu choice; a runtime failure does not.
        assert_eq!(unavailable(ok), None);
        assert_eq!(unavailable(none), Some(Refusal::NoExtension));
        for r in [
            Refusal::NoExtension,
            Refusal::NoImport,
            Refusal::CrossAdapter,
            Refusal::UnknownAdapter,
        ] {
            assert!(r.is_platform(), "{r}");
        }
        assert!(!Refusal::Failed("x".into()).is_platform());
    }

    /// The whole session decision for the default request: supported, not supported, adapter mismatch, explicit
    /// CPU, then a failure injected at runtime and a new request that tries again.
    #[test]
    fn default_session_decision_and_failure_injection() {
        let ok = Support {
            extension: true,
            handle: Some(HandleKind::Nt),
            same_adapter: Some(true),
        };
        let run = |requested: CapturePath, s: Support| {
            let mut f = Fallback::new(requested);
            if let Err(r) = decide(f.requested(), s) {
                f.refuse(r);
            }
            f
        };
        // Supported: zero-copy.
        let f = run(CapturePath::default(), ok);
        assert_eq!((f.active(), f.reason()), (CapturePath::ZeroCopy, None));
        // Not supported: the CPU copy, with the platform reason.
        let f = run(CapturePath::default(), Support { handle: None, ..ok });
        assert_eq!(f.active(), CapturePath::Cpu);
        assert!(f.reason().is_some_and(Refusal::is_platform));
        // Adapter mismatch: the CPU copy.
        let f = run(
            CapturePath::default(),
            Support {
                same_adapter: Some(false),
                ..ok
            },
        );
        assert_eq!(f.reason(), Some(&Refusal::CrossAdapter));
        assert_eq!(f.active(), CapturePath::Cpu);
        // Explicit CPU on a supported machine: the CPU copy, no reason.
        let f = run(CapturePath::Cpu, ok);
        assert_eq!((f.active(), f.reason()), (CapturePath::Cpu, None));
        // Failure injection: zero-copy running, then a runtime failure: the CPU copy at once, reported once.
        let mut f = run(CapturePath::default(), ok);
        assert!(f.refuse(Refusal::Failed("the GPU copy did not finish".into())));
        assert!(!f.refuse(Refusal::Failed("again".into())));
        assert_eq!(f.active(), CapturePath::Cpu);
        // The user switches to CPU and back to zero-copy: tried again.
        f.request(CapturePath::Cpu);
        f.request(CapturePath::ZeroCopy);
        assert_eq!((f.active(), f.reason()), (CapturePath::ZeroCopy, None));
    }

    #[test]
    fn fallback_reports_once_and_a_new_request_retries() {
        let mut f = Fallback::new(CapturePath::ZeroCopy);
        assert_eq!(f.active(), CapturePath::ZeroCopy);
        assert!(f.refuse(Refusal::Failed("import".into())));
        // Logged once.
        assert!(!f.refuse(Refusal::Failed("again".into())));
        assert_eq!(f.active(), CapturePath::Cpu);
        assert!(f.describe().starts_with("cpu (zero-copy off: zero-copy failed: import"));
        // Same request: the reason stays.
        f.request(CapturePath::ZeroCopy);
        assert_eq!(f.active(), CapturePath::Cpu);
        // The CPU path asked: nothing to refuse, nothing reported.
        f.request(CapturePath::Cpu);
        assert_eq!(f.reason(), None);
        assert!(!f.refuse(Refusal::NoImport));
        assert_eq!(f.describe(), "cpu");
        // Asked again: tried again.
        f.request(CapturePath::ZeroCopy);
        assert_eq!(f.active(), CapturePath::ZeroCopy);
        assert_eq!(f.describe(), "zero-copy");
    }

    /// A capture restarted on another adapter (or reattached) makes a new pool: its generation is new for the whole
    /// process, so the renderer imports it instead of sampling the old session's textures; the same pool is not
    /// imported twice.
    #[test]
    fn a_restarted_session_never_reuses_an_imported_pool() {
        // Two sessions each make their first pool.
        let (a, b) = (next_id(), next_id());
        assert_ne!(a, b);
        assert!(a >= 1 << 32 && b > a, "far above the tests' hand-made generations");
        let imported = Some((a, 0x1000));
        assert!(!needs_import(imported, (a, 0x1000)), "same pool: no import");
        assert!(needs_import(imported, (b, 0x2000)), "the new session's pool");
        // Even a repeated generation with another pool (hand-made generations of the tests) is imported.
        assert!(needs_import(imported, (a, 0x2000)));
        assert!(needs_import(None, (a, 0x1000)), "nothing imported yet");
        // Ids are unique across threads (the capture threads make pools concurrently).
        let ids: Vec<u64> = std::thread::scope(|s| {
            let h: Vec<_> = (0..4)
                .map(|_| s.spawn(|| (0..100).map(|_| next_id()).collect::<Vec<_>>()))
                .collect();
            h.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        let set: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(set.len(), ids.len());
    }

    /// The frame whose shared texture the renderer holds is not uploaded again, but a new session's frame with the
    /// same number is (a restart counts from 1 again).
    #[test]
    fn a_held_frame_is_matched_by_session_and_number() {
        let held = Some((7, 5));
        assert!(already_held(held, (7, 5), true));
        assert!(!already_held(held, (8, 5), true), "same number, new session");
        assert!(!already_held(held, (7, 6), true));
        assert!(
            !already_held(held, (7, 5), false),
            "the renderer dropped it (a zero-copy failure)"
        );
        assert!(!already_held(None, (7, 5), true));
    }

    #[test]
    fn copy_cost_mean_and_max() {
        let mut c = CopyCost::default();
        assert_eq!(c.take(), None);
        c.push(100.0);
        c.push(300.0);
        c.push(f64::NAN);
        let (mean, max, n) = c.take().unwrap();
        assert!((mean - 0.2).abs() < 1e-12 && (max - 0.3).abs() < 1e-12 && n == 2);
        assert_eq!(c.take(), None);
    }
}
