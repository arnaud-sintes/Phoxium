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
//! The automatic drop ([`AutoQuality`], `quality_auto`, off by default) lowers the tier in force one step at a
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

/// Copies the quality settings of `old` into `new`: a preset change (hotkey or menu) keeps them, as they describe
/// the machine, not the tube.
pub fn keep(new: &mut Params, old: &Params) {
    new.quality_tier = old.quality_tier;
    new.quality_auto = old.quality_auto;
}

/// Amplifier sub-samples per source pixel (`FrameConstants::amp_k`) for the tier: `amp_samples` (at least 1) in
/// `exact` and `standard`; in `fast` and `low` no more than the host pixels per source pixel `px_per_source_px`
/// (rounded up), and never below the table default 4 (`gpu::AMP_K`) or below the setting when it is lower. A
/// sub-sample narrower than a host pixel is box-filtered by the beam spot (sigma above one host pixel at every
/// preset), so the finer amplifier sampling only moves light within a pixel (measured error in the tier table).
pub fn amp_k(p: &Params, px_per_source_px: f64) -> i32 {
    let k = p.amp_samples.max(1);
    let placed = px_per_source_px > 0.0; // false for NaN
    if tier(p) < Tier::Fast || !placed {
        return k;
    }
    let resolved = px_per_source_px.ceil().min(f64::from(i32::MAX)) as i32;
    k.min(resolved.max(crate::gpu::AMP_K as i32))
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
/// [`Self::RELAPSE_SECS`], so a tier that does not fit is not retried every few seconds. Pure: no clock, no I/O.
#[derive(Clone, Debug)]
pub struct AutoQuality {
    drop: u32,
    over: u32,
    under: u32,
    wait: u32,
    /// Seconds fed so far, and the value at the last raise.
    t: u64,
    raised_at: Option<u64>,
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

    /// Tiers dropped below the selected one.
    pub fn drop(&self) -> u32 {
        self.drop
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
        let current = self.effective(user);
        if self.over >= Self::OVER_SECS && current < Tier::Low {
            self.over = 0;
            self.under = 0;
            self.drop += 1;
            if self.raised_at.is_some_and(|r| self.t - r <= Self::RELAPSE_SECS) {
                self.wait = (self.wait * 2).min(Self::MAX_WAIT_SECS);
            }
            return Some(Change::Dropped {
                to: self.effective(user),
                gpu_ms,
                period_ms,
            });
        }
        if self.drop > 0 && self.under >= self.wait {
            self.under = 0;
            self.over = 0;
            self.drop -= 1;
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
    use super::*;

    #[test]
    fn tiers_round_trip_and_lower() {
        for t in Tier::ALL {
            assert_eq!(Tier::from_param(t.param()), t);
        }
        assert_eq!(tier(&Params::default()), Tier::Standard, "the default tier");
        assert!(!Params::default().quality_auto, "the automatic drop is off by default");
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

    fn run(a: &mut AutoQuality, user: Tier, secs: u32, gpu: f64) -> Vec<Change> {
        (0..secs).filter_map(|_| a.feed(true, user, gpu, 16.7)).collect()
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
        // Overrun again within 30 s: dropped, and the next raise waits 20 s.
        run(&mut a, Tier::Standard, 3, 20.0);
        assert_eq!(a.effective(Tier::Standard), Tier::Fast);
        assert!(run(&mut a, Tier::Standard, 19, 8.0).is_empty());
        assert_eq!(run(&mut a, Tier::Standard, 1, 8.0).len(), 1);
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
