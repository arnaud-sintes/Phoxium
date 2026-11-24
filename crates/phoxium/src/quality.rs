//! Quality tiers and the automatic drop on sustained overrun (PLAN T-48, QUESTIONS PRF-2 and OUT-6; parameters
//! `params/quality.toml`, table of costs and errors in ARCHITECTURE 3.7 "Quality tiers").
//!
//! A tier chooses how exactly the model is evaluated, never the physics: the look parameters are the same in
//! every tier, only the evaluation's cost parameters change (which beam path `screen` records, how many amplifier
//! sub-samples, the line-image grid). Pure functions, unit tested; the GPU side reads the tier from the
//! parameters (`gpu::stages::screen::warp_plan`, [`amp_k`] through `FrameConstants::placed`).
//!
//! - `exact`: every stage as written (the output of the build before the tiers, bit for bit).
//! - `standard` (default): the beam of a separable geometry (flat tube, overscan, convergence) uses the line-image
//!   path of the curved tubes (T-91: each raster line's horizontal profile computed once per column instead of
//!   once per tile), on a grid aligned with the host columns when the raster is not scaled (no interpolation).
//! - `fast`: `standard`, plus the amplifier sub-samples limited to what the host pixels resolve ([`amp_k`]).
//! - `low`: `fast`, plus a coarser line-image grid ([`warp_sigma_steps`]) and a beam reach of 3 sigma instead
//!   of 4 ([`beam_reach_sigmas`]).
//!
//! The automatic drop ([`AutoQuality`], `quality_auto`, on by default) lowers the tier in force one step at a
//! time while the GPU frame time stays above the refresh period, and raises it back with hysteresis. It never
//! writes the parameters: the renderer applies it on top of the selected tier ([`effective`]).

use crate::params::{Params, QUALITY_TIER_EXACT, QUALITY_TIER_FAST, QUALITY_TIER_LOW, QUALITY_TIER_STANDARD};

/// A quality tier, from the most exact to the cheapest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Exact,
    Standard,
    Fast,
    Low,
}

impl Tier {
    /// Every tier, most exact first.
    pub const ALL: [Tier; 4] = [Tier::Exact, Tier::Standard, Tier::Fast, Tier::Low];

    /// The tier of a `quality_tier` value (an unknown value is `standard`, the default).
    pub fn from_param(v: u32) -> Tier {
        match v {
            QUALITY_TIER_EXACT => Tier::Exact,
            QUALITY_TIER_FAST => Tier::Fast,
            QUALITY_TIER_LOW => Tier::Low,
            _ => Tier::Standard,
        }
    }

    /// The `quality_tier` value.
    pub fn param(self) -> u32 {
        match self {
            Tier::Exact => QUALITY_TIER_EXACT,
            Tier::Standard => QUALITY_TIER_STANDARD,
            Tier::Fast => QUALITY_TIER_FAST,
            Tier::Low => QUALITY_TIER_LOW,
        }
    }

    /// The name used in the parameter table and the notices.
    pub fn name(self) -> &'static str {
        match self {
            Tier::Exact => "exact",
            Tier::Standard => "standard",
            Tier::Fast => "fast",
            Tier::Low => "low",
        }
    }

    /// `steps` tiers cheaper, at most [`Tier::Low`].
    pub fn lower(self, steps: u32) -> Tier {
        let i = Tier::ALL.iter().position(|&t| t == self).unwrap_or(1) + steps as usize;
        Tier::ALL[i.min(Tier::ALL.len() - 1)]
    }
}

/// The tier the parameters select.
pub fn tier(p: &Params) -> Tier {
    Tier::from_param(p.quality_tier)
}

/// The parameters with the tier lowered by `drop` steps (the automatic drop): only `quality_tier` changes.
pub fn effective(p: &Params, drop: u32) -> Params {
    if drop == 0 {
        return *p;
    }
    Params {
        quality_tier: tier(p).lower(drop).param(),
        ..*p
    }
}

/// The parameter ids of the machine settings: the quality ones and the host display's subpixel layout
/// (`mask_host_subpixel`). [`keep`] copies them; "Reset look overrides" keeps them.
pub const PARAM_IDS: [&str; 3] = ["quality_tier", "quality_auto", "mask_host_subpixel"];

/// Copies the machine settings of `old` into `new` (the quality settings and the host subpixel layout): a preset
/// change (hotkey or menu) keeps them, as they describe the machine, not the tube.
pub fn keep(new: &mut Params, old: &Params) {
    new.quality_tier = old.quality_tier;
    new.quality_auto = old.quality_auto;
    new.mask_host_subpixel = old.mask_host_subpixel;
}

/// Amplifier sub-samples per source pixel (`FrameConstants::amp_k`) for the tier: `amp_samples` (at least 1) in
/// `exact` and `standard`; in `fast` and `low` no more than the host pixels per source pixel `px_per_source_px`
/// (rounded up), and never below the table default 4 (`gpu::AMP_K`) or below the setting when it is lower. A
/// sub-sample narrower than a host pixel is box-filtered by the beam spot (sigma above one host pixel at every
/// preset), so the finer amplifier sampling only moves light within a pixel (measured error in the tier table).
/// The renderer then limits the result to what the GPU can allocate, in every tier ([`clamp_amp_k`], R-02).
pub fn amp_k(p: &Params, px_per_source_px: f64) -> i32 {
    let k = p.amp_samples.max(1);
    let placed = px_per_source_px > 0.0; // false for NaN
    if tier(p) < Tier::Fast || !placed {
        return k;
    }
    let resolved = px_per_source_px.ceil().min(f64::from(i32::MAX)) as i32;
    k.min(resolved.max(crate::gpu::AMP_K as i32))
}

/// What the GPU allows for one chain image (R-02): its largest 2D image side (`maxImageDimension2D`, 16384 on
/// Intel and AMD, 32768 on recent NVIDIA, 4096 at the Vulkan minimum) and the memory budget of one image in bytes
/// (`gpu::Context::image_limits`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageLimits {
    pub max_dim: u32,
    pub budget_bytes: u64,
}

/// Memory budget of one chain image at most (R-02): 256 MiB, 32 Mtexel of RGBA16F. The amp domain has three
/// images of that size (`amp`, `gun`, `abl`).
pub const IMAGE_BUDGET_BYTES: u64 = 256 << 20;

/// Bytes per texel of the chain images (RGBA16F, `gpu::NATIVE_FORMAT`).
pub const CHAIN_TEXEL_BYTES: u64 = 8;

/// The per-image budget of a device whose largest device-local heap is `heap_bytes`: [`IMAGE_BUDGET_BYTES`], or a
/// sixteenth of the heap when that is smaller (a 1 GiB GPU gets 64 MiB per image), at least 16 MiB. A heap of 0
/// (unknown) gives the full budget.
pub fn image_budget(heap_bytes: u64) -> u64 {
    if heap_bytes == 0 {
        return IMAGE_BUDGET_BYTES;
    }
    (heap_bytes / 16).clamp(16 << 20, IMAGE_BUDGET_BYTES)
}

/// The largest amplifier K whose amp-domain image (`native.0 * K` x `native.1`, RGBA16F) fits `limits`: its width
/// within `max_dim` and its bytes within `budget_bytes`. At least 1 (K = 1 is the native raster itself, which the
/// pipeline checks on its own). No overflow for any input.
pub fn amp_k_cap(native: (u32, u32), limits: ImageLimits) -> i32 {
    let w = u64::from(native.0.max(1));
    let h = u64::from(native.1.max(1));
    let by_dim = u64::from(limits.max_dim) / w;
    let by_budget = limits.budget_bytes / w.saturating_mul(h).saturating_mul(CHAIN_TEXEL_BYTES);
    by_dim.min(by_budget).clamp(1, i32::MAX as u64) as i32
}

/// The amplifier K a frame really uses, in every tier (R-02): the tier's K ([`amp_k`], at least 1) limited by
/// [`amp_k_cap`]. The flag is true when the limit lowered it (the renderer tells the user once).
pub fn clamp_amp_k(k: i32, native: (u32, u32), limits: ImageLimits) -> (i32, bool) {
    let k = k.max(1);
    let cap = amp_k_cap(native, limits);
    if k > cap {
        (cap, true)
    } else {
        (k, false)
    }
}

/// Smallest spot sigma at peak current in line-image grid steps (the warp path's sampling rule,
/// `gpu::stages::screen::WARP_SIGMA_STEPS` in the other tiers): `low` accepts a coarser grid (larger
/// Catmull-Rom error on the steepest edges, in the tier table).
pub fn warp_sigma_steps(p: &Params, standard: f64) -> f64 {
    if tier(p) >= Tier::Low {
        0.8
    } else {
        standard
    }
}

/// Reach of the beam sums in spot sigmas (`BEAM_REACH_SIGMAS` of `beam.glsl`, the skirt margin, the warp
/// path's pad): 4 in every tier but `low`, 3 there (the Gaussian mass beyond 3 sigma is 0.13 % per side).
pub fn beam_reach_sigmas(p: &Params) -> f64 {
    if tier(p) >= Tier::Low {
        3.0
    } else {
        4.0
    }
}

/// What the automatic drop did in one step ([`AutoQuality::feed`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// The tier in force dropped to `to`: the GPU frame time `gpu_ms` stayed above the refresh period
    /// `period_ms` for [`AutoQuality::OVER_SECS`] seconds.
    Dropped { to: Tier, gpu_ms: f64, period_ms: f64 },
    /// The tier in force came back up to `to` after a quiet stretch.
    Raised { to: Tier },
    /// The automatic drop was switched off while active: the selected tier `to` applies again.
    Off { to: Tier },
}

impl Change {
    /// The notice shown in the title, the console and the overlay.
    pub fn notice(&self) -> String {
        match self {
            Change::Dropped { to, gpu_ms, period_ms } => format!(
                "quality lowered to {} (GPU {gpu_ms:.1} ms over the {period_ms:.1} ms refresh)",
                to.name()
            ),
            Change::Raised { to } => format!("quality back to {}", to.name()),
            Change::Off { to } => format!("automatic quality off: {}", to.name()),
        }
    }
}

/// The automatic drop on sustained overrun (OUT-6): fed once per second with the mean GPU frame time and the
/// refresh period. Over the period for [`Self::OVER_SECS`] seconds in a row: one tier lower (until `low`). Back up
/// one tier after `wait` seconds in a row below [`Self::RAISE_FRACTION`] of the period; `wait` starts at
/// [`Self::RAISE_SECS`] and doubles (up to [`Self::MAX_WAIT_SECS`]) when a drop follows a raise within
/// [`Self::RELAPSE_SECS`], so a tier that does not fit is not retried every few seconds. A raise also needs the
/// tier above to be predicted to fit: at each drop the mean GPU time of the tier left is kept, and with the time the
/// lower tier took when it settled it gives the cost ratio between the two; the raise needs the current time times
/// that ratio to stay under [`Self::FIT_FRACTION`] of the period, else only after [`Self::MAX_WAIT_SECS`] seconds
/// of headroom in a row (a live run showed the plain headroom rule flapping every 10 s on a source whose standard
/// tier costs twice the period). Pure: no clock, no I/O.
#[derive(Clone, Debug)]
pub struct AutoQuality {
    drop: u32,
    over: u32,
    under: u32,
    wait: u32,
    /// Seconds fed so far, and the value at the last raise.
    t: u64,
    raised_at: Option<u64>,
    /// One entry per tier dropped, the last one for the next raise.
    probes: Vec<Probe>,
}

/// What a drop remembers about the tier it left.
#[derive(Clone, Copy, Debug)]
struct Probe {
    /// The mean GPU time of the tier left, ms (the second the drop was decided on).
    high_ms: f64,
    /// The GPU time of the lower tier once settled (its second valid second), ms.
    low_ms: Option<f64>,
    /// Valid seconds seen at the lower tier.
    seen: u32,
}

impl Probe {
    /// The time the tier above would take now, ms: the current time times the cost ratio of the two tiers.
    fn predicted_high_ms(&self, gpu_ms: f64) -> Option<f64> {
        let low = self.low_ms?;
        Some(gpu_ms * (self.high_ms / low).max(1.0))
    }
}

impl Default for AutoQuality {
    fn default() -> Self {
        Self {
            drop: 0,
            over: 0,
            under: 0,
            wait: Self::RAISE_SECS,
            t: 0,
            raised_at: None,
            probes: Vec::new(),
        }
    }
}

impl AutoQuality {
    /// Seconds over the refresh period before a drop.
    pub const OVER_SECS: u32 = 3;
    /// Seconds with headroom before the first raise.
    pub const RAISE_SECS: u32 = 10;
    /// A second counts as headroom when the GPU time is below this fraction of the period (35 % headroom: the
    /// next tier up costs up to about 1.5 times more, tier table).
    pub const RAISE_FRACTION: f64 = 0.65;
    /// A drop this soon after a raise doubles the wait before the next raise.
    pub const RELAPSE_SECS: u64 = 30;
    pub const MAX_WAIT_SECS: u32 = 160;
    /// The tier above is predicted to fit when its predicted GPU time is below this fraction of the period.
    pub const FIT_FRACTION: f64 = 0.9;

    /// Tiers dropped below the selected one.
    pub fn drop(&self) -> u32 {
        self.drop
    }

    /// The output moved onto another monitor (another refresh rate): the streaks measured against the old period
    /// start again, and so does the wait before a raise (a relapse on the old monitor says nothing about this
    /// one). The tiers dropped so far stay until the new monitor's own seconds raise them. Each second is still
    /// compared with the refresh period of the monitor the output is on at that second.
    pub fn monitor_changed(&mut self) {
        self.over = 0;
        self.under = 0;
        self.wait = Self::RAISE_SECS;
        self.raised_at = None;
    }

    /// The tier in force for the selected tier `user`.
    pub fn effective(&self, user: Tier) -> Tier {
        user.lower(self.drop)
    }

    /// One second: `enabled` (`quality_auto`), the selected tier, the mean GPU frame time of the second and the
    /// refresh period (both ms; a period or time that is not positive and finite counts as neither over nor
    /// under). Returns what changed.
    pub fn feed(&mut self, enabled: bool, user: Tier, gpu_ms: f64, period_ms: f64) -> Option<Change> {
        self.t += 1;
        if !enabled {
            self.over = 0;
            self.under = 0;
            self.wait = Self::RAISE_SECS;
            self.raised_at = None;
            self.probes.clear();
            if self.drop > 0 {
                self.drop = 0;
                return Some(Change::Off { to: user });
            }
            return None;
        }
        let valid = gpu_ms.is_finite() && gpu_ms > 0.0 && period_ms.is_finite() && period_ms > 0.0;
        let over = valid && gpu_ms > period_ms;
        let under = valid && gpu_ms < Self::RAISE_FRACTION * period_ms;
        self.over = if over { self.over + 1 } else { 0 };
        self.under = if under { self.under + 1 } else { 0 };
        // The lower tier settles for a second before its time is taken as the reference of the cost ratio.
        if valid {
            if let Some(probe) = self.probes.last_mut().filter(|p| p.low_ms.is_none()) {
                probe.seen += 1;
                if probe.seen >= 2 {
                    probe.low_ms = Some(gpu_ms);
                }
            }
        }
        let current = self.effective(user);
        if self.over >= Self::OVER_SECS && current < Tier::Low {
            self.over = 0;
            self.under = 0;
            self.drop += 1;
            self.probes.push(Probe {
                high_ms: gpu_ms,
                low_ms: None,
                seen: 0,
            });
            if self.raised_at.is_some_and(|r| self.t - r <= Self::RELAPSE_SECS) {
                self.wait = (self.wait * 2).min(Self::MAX_WAIT_SECS);
            }
            return Some(Change::Dropped {
                to: self.effective(user),
                gpu_ms,
                period_ms,
            });
        }
        let fits = self
            .probes
            .last()
            .and_then(|p| p.predicted_high_ms(gpu_ms))
            .is_none_or(|predicted| predicted < Self::FIT_FRACTION * period_ms);
        if self.drop > 0 && self.under >= self.wait && (fits || self.under >= Self::MAX_WAIT_SECS) {
            self.under = 0;
            self.over = 0;
            self.drop -= 1;
            self.probes.pop();
            self.raised_at = Some(self.t);
            return Some(Change::Raised {
                to: self.effective(user),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    /// The host subpixel layout describes the monitor, so a preset change keeps it, like the quality settings.
    #[test]
    fn a_preset_change_keeps_the_host_subpixel_layout() {
        let old = Params {
            mask_host_subpixel: crate::params::MASK_HOST_SUBPIXEL_BGR,
            ..Params::default()
        };
        let mut new = Params::default();
        assert_eq!(new.mask_host_subpixel, crate::params::MASK_HOST_SUBPIXEL_OFF);
        keep(&mut new, &old);
        assert_eq!(new.mask_host_subpixel, crate::params::MASK_HOST_SUBPIXEL_BGR);
        assert!(PARAM_IDS.contains(&"mask_host_subpixel"));
    }

    use super::*;

    #[test]
    fn tiers_round_trip_and_lower() {
        for t in Tier::ALL {
            assert_eq!(Tier::from_param(t.param()), t);
        }
        assert_eq!(tier(&Params::default()), Tier::Standard, "the default tier");
        assert!(Params::default().quality_auto, "the automatic drop is on by default");
        assert_eq!(Tier::Exact.lower(1), Tier::Standard);
        assert_eq!(Tier::Standard.lower(2), Tier::Low);
        assert_eq!(Tier::Fast.lower(5), Tier::Low);
        assert_eq!(Tier::Low.lower(0), Tier::Low);
    }

    /// The tier changes only `quality_tier`: every other parameter is kept.
    #[test]
    fn effective_changes_the_tier_only() {
        let p = Params {
            exposure: 1.7,
            amp_samples: 16,
            ..Params::default()
        };
        assert_eq!(effective(&p, 0), p);
        let e = effective(&p, 1);
        assert_eq!(tier(&e), Tier::Fast);
        assert_eq!(
            Params {
                quality_tier: p.quality_tier,
                ..e
            },
            p
        );
    }

    #[test]
    fn amp_k_is_capped_in_fast_and_low_only() {
        let p = |t: Tier, k: i32| Params {
            quality_tier: t.param(),
            amp_samples: k,
            ..Params::default()
        };
        assert_eq!(amp_k(&p(Tier::Exact, 16), 4.5), 16);
        assert_eq!(amp_k(&p(Tier::Standard, 16), 4.5), 16);
        assert_eq!(amp_k(&p(Tier::Fast, 16), 4.5), 5);
        assert_eq!(amp_k(&p(Tier::Low, 16), 6.0), 6);
        assert_eq!(amp_k(&p(Tier::Fast, 16), 2.0), 4, "never below the default K");
        assert_eq!(amp_k(&p(Tier::Fast, 2), 9.0), 2, "never above the setting");
        assert_eq!(amp_k(&p(Tier::Fast, 0), 9.0), 1);
        assert_eq!(amp_k(&p(Tier::Fast, 16), 0.0), 16, "unplaced: the setting");
        assert_eq!(amp_k(&p(Tier::Fast, 16), 40.0), 16);
    }

    /// R-02: K is limited by the GPU's largest image side, per vendor, in every tier.
    #[test]
    fn amp_k_is_clamped_to_the_device_image_limit() {
        // A short raster, so only the image side limits K (budget unlimited here).
        let lim = |max_dim| ImageLimits {
            max_dim,
            budget_bytes: u64::MAX,
        };
        let native = (2560, 240);
        // NVIDIA 32768, Intel and AMD 16384, the Vulkan minimum 4096.
        assert_eq!(clamp_amp_k(16, native, lim(32768)), (12, true));
        assert_eq!(clamp_amp_k(16, native, lim(16384)), (6, true));
        assert_eq!(clamp_amp_k(16, native, lim(4096)), (1, true));
        assert_eq!(
            clamp_amp_k(4, native, lim(16384)),
            (4, false),
            "within the limit: unchanged"
        );
        assert_eq!(clamp_amp_k(6, native, lim(16384)), (6, false), "exactly at the limit");
        // A 1280-wide source on the Intel iGPU: K 16 would be 20480 px wide.
        assert_eq!(clamp_amp_k(16, (1280, 720), lim(16384)), (12, true));
        // Floor of 1, even when the native raster alone is wider than the limit.
        assert_eq!(clamp_amp_k(16, (5000, 100), lim(4096)), (1, true));
        assert_eq!(clamp_amp_k(0, (256, 240), lim(16384)), (1, false));
        assert_eq!(clamp_amp_k(-3, (256, 240), lim(16384)), (1, false));
        // No overflow for absurd inputs.
        assert_eq!(clamp_amp_k(i32::MAX, (1, 1), lim(u32::MAX)), (i32::MAX, false));
        assert_eq!(clamp_amp_k(i32::MAX, (u32::MAX, u32::MAX), lim(u32::MAX)), (1, true));
        assert_eq!(amp_k_cap((0, 0), lim(16384)), 16384, "an empty raster counts as 1 px");
        // Every quality tier is clamped (the tier's K first, then the limit).
        for t in Tier::ALL {
            let p = Params {
                quality_tier: t.param(),
                amp_samples: 16,
                ..Params::default()
            };
            let k = amp_k(&p, 1.0);
            assert_eq!(clamp_amp_k(k, native, lim(16384)).0, k.min(6), "{t:?}");
        }
    }

    /// R-02: the memory budget per amp-domain image.
    #[test]
    fn amp_k_is_clamped_to_the_memory_budget() {
        let lim = ImageLimits {
            max_dim: 32768,
            budget_bytes: IMAGE_BUDGET_BYTES,
        };
        // 2560 x 1440 x 8 bytes is 29.5 MB per K: 256 MiB holds 9.
        assert_eq!(clamp_amp_k(16, (2560, 1440), lim), (9, true));
        // 1920 x 1080: 16.6 MB per K, 16 fit (265 MB of 268 MB).
        assert_eq!(clamp_amp_k(16, (1920, 1080), lim), (16, false));
        // 3840 x 2160: 66 MB per K, 4 fit.
        assert_eq!(clamp_amp_k(16, (3840, 2160), lim), (4, true));
        // A typical pixel-art raster is far from both limits.
        assert_eq!(clamp_amp_k(16, (320, 240), lim), (16, false));
        // A small GPU heap lowers the budget.
        assert_eq!(image_budget(0), IMAGE_BUDGET_BYTES);
        assert_eq!(image_budget(8 << 30), IMAGE_BUDGET_BYTES);
        assert_eq!(image_budget(1 << 30), 64 << 20);
        assert_eq!(image_budget(64 << 20), 16 << 20, "never below 16 MiB");
        let small = ImageLimits {
            max_dim: 16384,
            budget_bytes: image_budget(1 << 30),
        };
        assert_eq!(clamp_amp_k(16, (1920, 1080), small), (4, true));
    }

    fn run(a: &mut AutoQuality, user: Tier, secs: u32, gpu: f64) -> Vec<Change> {
        (0..secs).filter_map(|_| a.feed(true, user, gpu, 16.7)).collect()
    }

    /// A move from a 60 Hz monitor to a 144 Hz one: two seconds over the 60 Hz period then the move do not add up
    /// to a drop with one second at 144 Hz; the new monitor's own three seconds over its 6.9 ms period drop one
    /// tier. Each second is judged against the period of its own monitor.
    #[test]
    fn a_monitor_change_restarts_the_streaks_against_the_new_period() {
        let mut a = AutoQuality::default();
        let user = Tier::Standard;
        // 17 ms at 60 Hz: over, twice.
        assert_eq!(a.feed(true, user, 17.0, 16.7), None);
        assert_eq!(a.feed(true, user, 17.0, 16.7), None);
        a.monitor_changed();
        // 8 ms at 144 Hz: over; one second only since the move.
        assert_eq!(a.feed(true, user, 8.0, 6.94), None);
        assert_eq!(a.feed(true, user, 8.0, 6.94), None);
        assert!(matches!(
            a.feed(true, user, 8.0, 6.94),
            Some(Change::Dropped { period_ms, .. }) if (period_ms - 6.94).abs() < 1e-9
        ));
        assert_eq!(a.drop(), 1);
        // Without the reset the old monitor's seconds would have counted: a drop one second earlier.
        let mut b = AutoQuality::default();
        b.feed(true, user, 17.0, 16.7);
        b.feed(true, user, 17.0, 16.7);
        assert!(b.feed(true, user, 8.0, 6.94).is_some());
        // The relapse back-off of the old monitor is forgotten too; the drop stays.
        a.wait = AutoQuality::MAX_WAIT_SECS;
        a.monitor_changed();
        assert_eq!((a.wait, a.drop()), (AutoQuality::RAISE_SECS, 1));
    }

    #[test]
    fn drops_after_three_seconds_over_and_stops_at_low() {
        let mut a = AutoQuality::default();
        assert!(run(&mut a, Tier::Standard, 2, 20.0).is_empty());
        // A second under the period resets the count.
        assert!(run(&mut a, Tier::Standard, 1, 15.0).is_empty());
        assert!(run(&mut a, Tier::Standard, 2, 20.0).is_empty());
        let c = run(&mut a, Tier::Standard, 1, 20.0);
        assert_eq!(
            c,
            vec![Change::Dropped {
                to: Tier::Fast,
                gpu_ms: 20.0,
                period_ms: 16.7
            }]
        );
        assert!(c[0].notice().contains("fast"));
        assert_eq!(a.effective(Tier::Standard), Tier::Fast);
        let c = run(&mut a, Tier::Standard, 3, 20.0);
        assert!(matches!(c[..], [Change::Dropped { to: Tier::Low, .. }]));
        assert!(run(&mut a, Tier::Standard, 30, 20.0).is_empty(), "nothing below low");
        assert_eq!(a.drop(), 2);
    }

    #[test]
    fn raises_with_hysteresis_and_backs_off_after_a_relapse() {
        let mut a = AutoQuality::default();
        run(&mut a, Tier::Standard, 3, 20.0);
        assert_eq!(a.effective(Tier::Standard), Tier::Fast);
        // Between 65 % and 100 % of the period: no raise, however long.
        assert!(run(&mut a, Tier::Standard, 60, 14.0).is_empty());
        // Headroom for 9 s, one busy second, then 10 s: raised once the 10 s are in a row.
        assert!(run(&mut a, Tier::Standard, 9, 8.0).is_empty());
        assert!(run(&mut a, Tier::Standard, 1, 12.0).is_empty());
        assert!(run(&mut a, Tier::Standard, 9, 8.0).is_empty());
        assert_eq!(
            run(&mut a, Tier::Standard, 1, 8.0),
            vec![Change::Raised { to: Tier::Standard }]
        );
        // Overrun again within 30 s: dropped, and the next raise waits 20 s. The tier left cost 20 ms and the
        // lower one 14 ms: the ratio is 1.43, so 8 ms predicts 11.4 ms, which fits.
        run(&mut a, Tier::Standard, 3, 20.0);
        assert_eq!(a.effective(Tier::Standard), Tier::Fast);
        run(&mut a, Tier::Standard, 2, 14.0);
        assert!(run(&mut a, Tier::Standard, 19, 8.0).is_empty());
        assert_eq!(run(&mut a, Tier::Standard, 1, 8.0).len(), 1);
    }

    /// A source whose standard tier costs twice the period and whose fast tier fits easily: the headroom rule alone
    /// would climb back every 10 s and drop again (seen live, 1280x720 on the slot TV preset). The raise is
    /// predicted from the cost ratio and refused; it happens when the load falls enough, or after the long wait.
    #[test]
    fn does_not_climb_back_to_a_tier_that_would_overrun() {
        let mut a = AutoQuality::default();
        let user = Tier::Standard;
        run(&mut a, user, 3, 34.0);
        assert_eq!(a.drop(), 1);
        // Fast settles at 8 ms (ratio 4.25); two more seconds, then a long quiet stretch at the same load.
        assert!(
            run(&mut a, user, 150, 8.0).is_empty(),
            "34 ms would come back: no raise"
        );
        assert_eq!(a.drop(), 1);
        // The load falls (the preset or the source changed): 3 ms predicts 12.75 ms, which fits.
        assert_eq!(run(&mut a, user, 1, 3.0), vec![Change::Raised { to: Tier::Standard }]);

        // A stale reference is retried after the maximum wait: the same source, the same 8 ms.
        let mut b = AutoQuality::default();
        run(&mut b, user, 3, 34.0);
        assert!(run(&mut b, user, 159, 8.0).is_empty());
        assert_eq!(run(&mut b, user, 1, 8.0), vec![Change::Raised { to: Tier::Standard }]);
    }

    #[test]
    fn off_restores_the_selected_tier_and_ignores_bad_samples() {
        let mut a = AutoQuality::default();
        run(&mut a, Tier::Exact, 3, 30.0);
        assert_eq!(a.effective(Tier::Exact), Tier::Standard);
        assert_eq!(
            a.feed(false, Tier::Exact, 30.0, 16.7),
            Some(Change::Off { to: Tier::Exact })
        );
        assert_eq!(a.effective(Tier::Exact), Tier::Exact);
        assert_eq!(a.feed(false, Tier::Exact, 30.0, 16.7), None);
        for (gpu, period) in [(f64::NAN, 16.7), (30.0, 0.0), (0.0, 16.7), (f64::INFINITY, 16.7)] {
            for _ in 0..5 {
                assert_eq!(a.feed(true, Tier::Exact, gpu, period), None);
            }
        }
    }
}
