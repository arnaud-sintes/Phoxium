//! Pure key handling of the presentation keys (integer multiplier, auto-fit, fit family) and the texts
//! the title and the on-screen toast show for them. No Win32 here: the window procedure maps the virtual key
//! with `scale_key`, calls `apply` and stores the new `ScaleState`; everything is unit tested.
//!
//! Model
//! - `mode` is the presentation in force. For `Integer(n)` the `n` is the scale that is displayed.
//! - `requested` is the integer multiplier the user asked for, which the window may have had to clamp
//!   (`integer_label` reports both). It survives the other modes, so that one key can come back to "my
//!   multiplier".
//! - `remembered` is the last explicit (non auto-fit) mode, the target of the T key when leaving auto-fit.
//! - The step keys (`+`, `-`, arrows, Page Up and Down) start from the scale that is displayed, so a clamped
//!   x8 shown as x4 goes to x3 on `-`, not x7 (nothing would change), and from auto-fit's current scale.

use std::time::Duration;

use crate::layout::{integer_label, ScaleMode, MAX_SCALE};

/// How long a toast stays on screen.
pub const TOAST_DURATION: Duration = Duration::from_millis(1500);

/// Multiplier of the step keys when none is known (free modes without a previous request).
const DEFAULT_SCALE: u32 = 3;

/// An action of the presentation keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleKey {
    /// Multiplier up or down by this many steps (`+`, `=`, numpad plus, Up, Page Up: positive).
    Step(i32),
    /// Digit keys: integer scale x1..x9, and `0` is x10.
    Digit(u32),
    /// Home: x1.
    Home,
    /// End: auto-fit.
    End,
    /// T or Tab: auto-fit <-> the last explicit mode.
    ToggleAutofit,
    /// Z: cycle the fit family, autofit -> fit -> stretch.
    CycleFit,
    /// M: cycle every presentation, 1to1 -> integer -> autofit -> fit -> stretch.
    CyclePresent,
}

/// What the keys read and write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaleState {
    pub mode: ScaleMode,
    pub requested: Option<u32>,
    pub remembered: ScaleMode,
}

impl ScaleState {
    /// Start state: the remembered mode is the start mode, or integer x3 when the demo starts in auto-fit.
    pub fn new(mode: ScaleMode, requested: Option<u32>) -> Self {
        let remembered = if mode == ScaleMode::Fit {
            ScaleMode::Integer(requested.unwrap_or(DEFAULT_SCALE).clamp(1, MAX_SCALE))
        } else {
            mode
        };
        Self {
            mode,
            requested,
            remembered,
        }
    }

    fn integer(n: u32) -> (ScaleMode, Option<u32>) {
        let n = n.clamp(1, MAX_SCALE);
        (ScaleMode::Integer(n), Some(n))
    }
}

/// Maps a virtual key code to a presentation key, `None` for the other keys.
pub fn scale_key(vk: u32) -> Option<ScaleKey> {
    const VK_TAB: u32 = 0x09;
    const VK_PRIOR: u32 = 0x21; // Page Up
    const VK_NEXT: u32 = 0x22; // Page Down
    const VK_END: u32 = 0x23;
    const VK_HOME: u32 = 0x24;
    const VK_UP: u32 = 0x26;
    const VK_DOWN: u32 = 0x28;
    const VK_ADD: u32 = 0x6B; // numpad plus
    const VK_SUBTRACT: u32 = 0x6D; // numpad minus
    const VK_OEM_PLUS: u32 = 0xBB; // `=` and `+`
    const VK_OEM_MINUS: u32 = 0xBD;
    match vk {
        VK_ADD | VK_OEM_PLUS | VK_UP => Some(ScaleKey::Step(1)),
        VK_SUBTRACT | VK_OEM_MINUS | VK_DOWN => Some(ScaleKey::Step(-1)),
        VK_PRIOR => Some(ScaleKey::Step(2)),
        VK_NEXT => Some(ScaleKey::Step(-2)),
        0x30 => Some(ScaleKey::Digit(MAX_SCALE)),
        0x31..=0x39 => Some(ScaleKey::Digit(vk - 0x30)),
        VK_HOME => Some(ScaleKey::Home),
        VK_END => Some(ScaleKey::End),
        VK_TAB | 0x54 => Some(ScaleKey::ToggleAutofit),
        0x5A => Some(ScaleKey::CycleFit),
        0x4D => Some(ScaleKey::CyclePresent),
        _ => None,
    }
}

/// The new state after `key`. `shown` is the integer scale the current layout displays (the auto-fit scale
/// in auto-fit, `None` for the free modes).
pub fn apply(s: ScaleState, key: ScaleKey, shown: Option<u32>) -> ScaleState {
    let (mode, requested) = match key {
        ScaleKey::Step(delta) => {
            let base = match s.mode {
                ScaleMode::Integer(n) => n,
                ScaleMode::Fit => shown.unwrap_or(1),
                ScaleMode::Native => 1,
                ScaleMode::FitFree | ScaleMode::Stretch => s.requested.unwrap_or(DEFAULT_SCALE),
            };
            ScaleState::integer((base as i64 + delta as i64).clamp(1, MAX_SCALE as i64) as u32)
        }
        ScaleKey::Digit(n) => ScaleState::integer(n),
        ScaleKey::Home => ScaleState::integer(1),
        ScaleKey::End => (ScaleMode::Fit, s.requested),
        ScaleKey::ToggleAutofit => {
            if s.mode == ScaleMode::Fit {
                match s.remembered {
                    // Back to the multiplier the user asked for, even if it had been clamped.
                    ScaleMode::Integer(n) => ScaleState::integer(s.requested.unwrap_or(n)),
                    other => (other, s.requested),
                }
            } else {
                (ScaleMode::Fit, s.requested)
            }
        }
        ScaleKey::CycleFit => (
            match s.mode {
                ScaleMode::Fit => ScaleMode::FitFree,
                ScaleMode::FitFree => ScaleMode::Stretch,
                _ => ScaleMode::Fit,
            },
            s.requested,
        ),
        ScaleKey::CyclePresent => {
            let next = s.mode.next(s.requested.unwrap_or(DEFAULT_SCALE));
            match next {
                ScaleMode::Integer(n) => (next, Some(n)),
                _ => (next, s.requested),
            }
        }
    };
    let remembered = if mode == ScaleMode::Fit { s.remembered } else { mode };
    ScaleState {
        mode,
        requested,
        remembered,
    }
}

/// Mode text of the title: `1to1`, `integer xN` (with the clamp note), `autofit xN`, `fit nearest`.
/// `fit_scale` is the scale auto-fit currently picks, `filter` the filter name.
pub fn mode_label(mode: ScaleMode, requested: Option<u32>, fit_scale: Option<u32>, filter: &str) -> String {
    match mode {
        ScaleMode::Native => "1to1".to_string(),
        ScaleMode::Integer(n) => integer_label(requested, n),
        ScaleMode::Fit => match fit_scale {
            Some(n) => format!("autofit x{n}"),
            None => "autofit".to_string(),
        },
        ScaleMode::FitFree | ScaleMode::Stretch => format!("{} {filter}", mode.present_name()),
    }
}

/// Toast text: the title's mode text, with an arrow for auto-fit (`autofit -> x3`).
pub fn toast_text(mode: ScaleMode, requested: Option<u32>, fit_scale: Option<u32>, filter: &str) -> String {
    match (mode, fit_scale) {
        (ScaleMode::Fit, Some(n)) => format!("autofit -> x{n}"),
        _ => mode_label(mode, requested, fit_scale, filter),
    }
}

/// True while a toast shown `elapsed` ago is still on screen.
pub fn toast_active(elapsed: Duration) -> bool {
    elapsed < TOAST_DURATION
}

#[cfg(test)]
mod tests {
    use super::*;
    use ScaleMode::{Fit, FitFree, Integer, Native, Stretch};

    fn st(mode: ScaleMode, requested: Option<u32>) -> ScaleState {
        ScaleState::new(mode, requested)
    }

    #[test]
    fn plus_and_minus_step_by_one_and_clamp() {
        let s = apply(st(Integer(3), Some(3)), ScaleKey::Step(1), None);
        assert_eq!((s.mode, s.requested), (Integer(4), Some(4)));
        let s = apply(st(Integer(3), Some(3)), ScaleKey::Step(-1), None);
        assert_eq!((s.mode, s.requested), (Integer(2), Some(2)));
        assert_eq!(
            apply(st(Integer(10), Some(10)), ScaleKey::Step(1), None).mode,
            Integer(10)
        );
        assert_eq!(
            apply(st(Integer(1), Some(1)), ScaleKey::Step(-1), None).mode,
            Integer(1)
        );
    }

    #[test]
    fn page_keys_jump_by_two_and_clamp() {
        assert_eq!(apply(st(Integer(3), Some(3)), ScaleKey::Step(2), None).mode, Integer(5));
        assert_eq!(
            apply(st(Integer(3), Some(3)), ScaleKey::Step(-2), None).mode,
            Integer(1)
        );
        assert_eq!(
            apply(st(Integer(9), Some(9)), ScaleKey::Step(2), None).mode,
            Integer(10)
        );
        assert_eq!(
            apply(st(Integer(2), Some(2)), ScaleKey::Step(-2), None).mode,
            Integer(1)
        );
    }

    #[test]
    fn steps_leave_the_other_modes_for_integer() {
        // 1to1 counts as x1.
        let s = apply(st(Native, None), ScaleKey::Step(1), None);
        assert_eq!((s.mode, s.requested), (Integer(2), Some(2)));
        // Auto-fit steps from the scale it displays.
        let s = apply(st(Fit, None), ScaleKey::Step(1), Some(3));
        assert_eq!((s.mode, s.requested), (Integer(4), Some(4)));
        let s = apply(st(Fit, None), ScaleKey::Step(-1), Some(3));
        assert_eq!(s.mode, Integer(2));
        // The free modes step from the last request, else x3.
        assert_eq!(apply(st(FitFree, Some(6)), ScaleKey::Step(1), None).mode, Integer(7));
        assert_eq!(apply(st(Stretch, None), ScaleKey::Step(-1), None).mode, Integer(2));
    }

    #[test]
    fn steps_start_from_the_displayed_scale_when_clamped() {
        // x8 requested, x4 displayed: `-` goes to x3, `+` asks x5 (shown as clamped to x4 by the window).
        let s = apply(st(Integer(4), Some(8)), ScaleKey::Step(-1), None);
        assert_eq!((s.mode, s.requested), (Integer(3), Some(3)));
        let s = apply(st(Integer(4), Some(8)), ScaleKey::Step(1), None);
        assert_eq!((s.mode, s.requested), (Integer(5), Some(5)));
    }

    #[test]
    fn digits_home_and_end() {
        for d in 1..=9 {
            let s = apply(st(Fit, None), ScaleKey::Digit(d), None);
            assert_eq!((s.mode, s.requested), (Integer(d), Some(d)));
        }
        assert_eq!(scale_key(0x30), Some(ScaleKey::Digit(10)));
        assert_eq!(
            apply(st(Integer(2), Some(2)), ScaleKey::Digit(10), None).mode,
            Integer(10)
        );
        let s = apply(st(Integer(7), Some(7)), ScaleKey::Home, None);
        assert_eq!((s.mode, s.requested), (Integer(1), Some(1)));
        let s = apply(st(Integer(7), Some(7)), ScaleKey::End, None);
        assert_eq!(s.mode, Fit);
        assert_eq!(s.requested, Some(7), "End keeps the multiplier for the way back");
    }

    #[test]
    fn t_flips_between_my_multiplier_and_autofit() {
        let s = st(Integer(5), Some(5));
        let fit = apply(s, ScaleKey::ToggleAutofit, None);
        assert_eq!(fit.mode, Fit);
        let back = apply(fit, ScaleKey::ToggleAutofit, Some(2));
        assert_eq!((back.mode, back.requested), (Integer(5), Some(5)));
        // Twice more: still the same pair.
        let again = apply(
            apply(back, ScaleKey::ToggleAutofit, None),
            ScaleKey::ToggleAutofit,
            None,
        );
        assert_eq!(again.mode, Integer(5));
    }

    #[test]
    fn t_remembers_a_clamped_request() {
        // x8 requested but x4 displayed: the way back asks x8 again.
        let fit = apply(st(Integer(4), Some(8)), ScaleKey::ToggleAutofit, None);
        let back = apply(fit, ScaleKey::ToggleAutofit, None);
        assert_eq!((back.mode, back.requested), (Integer(8), Some(8)));
    }

    #[test]
    fn t_remembers_the_last_explicit_mode_of_any_kind() {
        for explicit in [Native, FitFree, Stretch] {
            let s = ScaleState {
                mode: explicit,
                requested: Some(3),
                remembered: explicit,
            };
            let fit = apply(s, ScaleKey::ToggleAutofit, None);
            assert_eq!(fit.mode, Fit);
            assert_eq!(apply(fit, ScaleKey::ToggleAutofit, None).mode, explicit);
        }
        // A step while in auto-fit makes the new multiplier the remembered mode.
        let fit = apply(st(Integer(5), Some(5)), ScaleKey::End, None);
        let s = apply(fit, ScaleKey::Step(1), Some(2));
        assert_eq!(s.mode, Integer(3));
        let fit = apply(s, ScaleKey::ToggleAutofit, None);
        assert_eq!(apply(fit, ScaleKey::ToggleAutofit, None).mode, Integer(3));
    }

    #[test]
    fn starting_in_autofit_remembers_integer() {
        let s = st(Fit, None);
        assert_eq!(s.remembered, Integer(3));
        assert_eq!(apply(s, ScaleKey::ToggleAutofit, None).mode, Integer(3));
        assert_eq!(st(Fit, Some(6)).remembered, Integer(6));
    }

    #[test]
    fn z_cycles_the_fit_family() {
        let s = st(Integer(3), Some(3));
        let a = apply(s, ScaleKey::CycleFit, None);
        assert_eq!(a.mode, Fit);
        let b = apply(a, ScaleKey::CycleFit, None);
        assert_eq!(b.mode, FitFree);
        let c = apply(b, ScaleKey::CycleFit, None);
        assert_eq!(c.mode, Stretch);
        assert_eq!(apply(c, ScaleKey::CycleFit, None).mode, Fit);
        // From 1to1 it also enters the family at auto-fit.
        assert_eq!(apply(st(Native, None), ScaleKey::CycleFit, None).mode, Fit);
        // T from inside the family goes to autofit, and back to the stretch.
        assert_eq!(apply(c, ScaleKey::ToggleAutofit, None).mode, Fit);
        let fit = apply(c, ScaleKey::ToggleAutofit, None);
        assert_eq!(apply(fit, ScaleKey::ToggleAutofit, None).mode, Stretch);
    }

    #[test]
    fn m_keeps_its_cycle_and_uses_the_last_request() {
        let mut s = st(Native, Some(6));
        let mut seen = Vec::new();
        for _ in 0..5 {
            s = apply(s, ScaleKey::CyclePresent, None);
            seen.push(s.mode);
        }
        assert_eq!(seen, vec![Integer(6), Fit, FitFree, Stretch, Native]);
        assert_eq!(apply(st(Native, None), ScaleKey::CyclePresent, None).mode, Integer(3));
        // Leaving integer for the next mode keeps the request.
        assert_eq!(
            apply(st(Integer(6), Some(6)), ScaleKey::CyclePresent, None).requested,
            Some(6)
        );
    }

    #[test]
    fn the_multiplier_never_leaves_one_to_ten() {
        let mut s = st(Integer(5), Some(5));
        for k in [ScaleKey::Step(2); 20] {
            s = apply(s, k, None);
        }
        assert_eq!(s.mode, Integer(10));
        for k in [ScaleKey::Step(-2); 20] {
            s = apply(s, k, None);
        }
        assert_eq!(s.mode, Integer(1));
    }

    #[test]
    fn virtual_keys_map_to_the_actions() {
        let cases = [
            (0xBB, ScaleKey::Step(1)),  // = and +
            (0x6B, ScaleKey::Step(1)),  // numpad plus
            (0x26, ScaleKey::Step(1)),  // Up
            (0xBD, ScaleKey::Step(-1)), // -
            (0x6D, ScaleKey::Step(-1)), // numpad minus
            (0x28, ScaleKey::Step(-1)), // Down
            (0x21, ScaleKey::Step(2)),  // Page Up
            (0x22, ScaleKey::Step(-2)), // Page Down
            (0x31, ScaleKey::Digit(1)),
            (0x39, ScaleKey::Digit(9)),
            (0x30, ScaleKey::Digit(10)),
            (0x24, ScaleKey::Home),
            (0x23, ScaleKey::End),
            (0x54, ScaleKey::ToggleAutofit), // T
            (0x09, ScaleKey::ToggleAutofit), // Tab
            (0x5A, ScaleKey::CycleFit),      // Z
            (0x4D, ScaleKey::CyclePresent),  // M
        ];
        for (vk, k) in cases {
            assert_eq!(scale_key(vk), Some(k), "vk {vk:#x}");
        }
        // Keys that belong to other handlers stay unmapped.
        for vk in [0x43, 0x46, 0x42, 0x50, 0x4E, 0x4B, 0x58, 0x41, 0x20, 0x1B, 0x25, 0x27] {
            assert_eq!(scale_key(vk), None, "vk {vk:#x}");
        }
    }

    #[test]
    fn labels_never_claim_an_undisplayed_scale() {
        assert_eq!(mode_label(Native, None, None, "nearest"), "1to1");
        assert_eq!(mode_label(Integer(4), Some(4), None, "nearest"), "integer x4");
        assert_eq!(
            mode_label(Integer(4), Some(8), None, "nearest"),
            "integer x8 (clamped to x4)"
        );
        assert_eq!(mode_label(Fit, None, Some(3), "nearest"), "autofit x3");
        assert_eq!(mode_label(Fit, None, None, "nearest"), "autofit");
        assert_eq!(mode_label(FitFree, None, None, "bicubic"), "fit bicubic");
        assert_eq!(mode_label(Stretch, None, None, "nearest"), "stretch nearest");
    }

    #[test]
    fn toast_texts() {
        assert_eq!(toast_text(Integer(4), Some(4), None, "nearest"), "integer x4");
        assert_eq!(
            toast_text(Integer(4), Some(8), None, "nearest"),
            "integer x8 (clamped to x4)"
        );
        assert_eq!(toast_text(Fit, Some(4), Some(3), "nearest"), "autofit -> x3");
        assert_eq!(toast_text(Stretch, None, None, "nearest"), "stretch nearest");
        assert_eq!(toast_text(Native, None, None, "nearest"), "1to1");
    }

    #[test]
    fn toast_lasts_a_second_and_a_half() {
        assert!(toast_active(Duration::ZERO));
        assert!(toast_active(Duration::from_millis(1499)));
        assert!(!toast_active(Duration::from_millis(1500)));
        assert!(!toast_active(Duration::from_secs(10)));
    }
}
