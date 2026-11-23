//! What a hotkey does to the application state (T-54, T-57), as pure functions: [`dispatch`] turns an
//! [`Action`] into a change of [`Controls`] or a [`Command`] for the code that owns the window and the renderer
//! (`live.rs`), [`step_preset`] picks the next or previous preset, [`bypass_plan`] decides the chain slot flags
//! of the A/B bypass.
//!
//! The settings menu and the overlay (T-52a, T-52b) are reached through [`UiHook`], which `ui::Ui` implements
//! (`request_toggle`, `toggle_overlay`, `is_open`, `wants_input`). [`NoUi`] is the stand-in of the unit tests.

use crate::{
    hotkeys::Action,
    params::{Params, PARAMS_META},
    presets::Library,
};

/// The part of the settings UI that the hotkeys and the output window need.
pub trait UiHook {
    /// F10 (`Action::ToggleMenu`): open or close the settings menu. The hand-off of the foreground window
    /// (remember the source, activate the output; give it back on close) is the UI's.
    fn toggle_menu(&mut self);
    /// `Action::ToggleOverlay`: show or hide the read-only overlay.
    fn toggle_overlay(&mut self);
    /// The menu is open: the output is activating and takes keyboard and mouse input.
    fn menu_open(&self) -> bool;
    /// The UI wants the mouse and the keyboard (the menu is open, or ImGui captured the input). The host
    /// cursor is shown when this is true and hidden over the output otherwise.
    fn wants_input(&self) -> bool;
    /// A registered hotkey was pressed (`pressed`, with its `chord`) while the menu may be waiting for a new chord
    /// (the Hotkeys rows): true when the UI took it as the new chord, then the action is not run. Quit always runs.
    fn capture_chord(&mut self, _pressed: Action, _chord: crate::hotkeys::Chord) -> bool {
        false
    }
}

/// A stand-in for the UI (tests, code without a window): the menu never opens and the overlay does nothing.
#[derive(Default)]
pub struct NoUi;

impl UiHook for NoUi {
    fn toggle_menu(&mut self) {}
    fn toggle_overlay(&mut self) {}
    fn menu_open(&self) -> bool {
        false
    }
    fn wants_input(&self) -> bool {
        false
    }
}

/// State the actions change by themselves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Controls {
    /// Set by `Action::Quit`: the application loop ends.
    pub quit: bool,
    /// A/B bypass on: every chain slot is disabled.
    pub bypass: bool,
}

/// Preset direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Next,
    Previous,
}

/// What the owner of the window and the renderer has to do for an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Apply the bypass state (`Controls::bypass` already holds the new value).
    SetBypass(bool),
    StepPreset(Step),
    /// F11: fullscreen to windowed or back.
    ToggleWindowMode,
    /// Click-through of the output on or off (the owner of the window and the settings applies it).
    ToggleClickThrough,
    /// Save a snapshot of the output as a PNG (the renderer and the file system are the owner's).
    Snapshot,
}

/// Applies `action`: updates `controls`, calls the UI, and returns the command that needs the window or the
/// renderer, if any.
pub fn dispatch(action: Action, controls: &mut Controls, ui: &mut dyn UiHook) -> Option<Command> {
    match action {
        Action::Quit => {
            controls.quit = true;
            None
        }
        Action::ToggleMenu => {
            ui.toggle_menu();
            None
        }
        Action::ToggleOverlay => {
            ui.toggle_overlay();
            None
        }
        Action::ToggleBypass => {
            controls.bypass = !controls.bypass;
            Some(Command::SetBypass(controls.bypass))
        }
        Action::NextPreset => Some(Command::StepPreset(Step::Next)),
        Action::PreviousPreset => Some(Command::StepPreset(Step::Previous)),
        Action::ToggleFullscreen => Some(Command::ToggleWindowMode),
        Action::ToggleClickThrough => Some(Command::ToggleClickThrough),
        Action::Snapshot => Some(Command::Snapshot),
    }
}

/// The preset a [`Command::StepPreset`] switched to.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetStep {
    pub id: String,
    pub name: String,
    pub params: Params,
}

/// The preset after (or before) `current` in the library, wrapping around, as a full parameter set: the table
/// defaults with the preset on top (the settings overrides belong to the preset they were made on and are not
/// carried over), except for the `source` stage, which describes the captured window and the placement of the
/// picture and is kept from `old`. `None` when the library is empty.
pub fn step_preset(lib: &Library, current: Option<&str>, step: Step, old: &Params) -> Option<PresetStep> {
    let preset = match step {
        Step::Next => lib.next(current),
        Step::Previous => lib.previous(current),
    }?;
    let mut params = preset.params();
    // T-48: the quality tier and its automatic drop describe the machine, not the tube.
    crate::quality::keep(&mut params, old);
    for m in PARAMS_META.iter().filter(|m| m.stage == "source") {
        if let Some(v) = old.get(m.id) {
            let kept = params.set(m.id, v);
            debug_assert!(kept, "source parameter {} not settable", m.id);
        }
    }
    Some(PresetStep {
        id: preset.id.clone(),
        name: preset.name.clone(),
        params,
    })
}

/// The enable flag of every chain slot, in chain order.
pub type SlotFlags = Vec<(&'static str, bool)>;

/// The flags to set when the A/B bypass goes `on` or off. On: every slot off, and `current` is kept in `saved`.
/// Off: the flags saved at the switch-on (a slot the user had turned off stays off), or every slot on when
/// nothing was saved. Pure.
pub fn bypass_plan(on: bool, current: &[(&'static str, bool)], saved: &mut Option<SlotFlags>) -> SlotFlags {
    if on {
        if saved.is_none() {
            *saved = Some(current.to_vec());
        }
        current.iter().map(|&(n, _)| (n, false)).collect()
    } else {
        match saved.take() {
            Some(before) => current
                .iter()
                .map(|&(n, _)| (n, before.iter().find(|(b, _)| *b == n).is_none_or(|&(_, on)| on)))
                .collect(),
            None => current.iter().map(|&(n, _)| (n, true)).collect(),
        }
    }
}

/// The short state line for the console: `A/B bypass: on (CRT chain off)`.
pub fn bypass_line(on: bool) -> &'static str {
    if on {
        "bypass: on (CRT chain off, source placement only)"
    } else {
        "bypass: off (CRT chain on)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct SpyUi {
        menu: bool,
        overlay: bool,
        toggles: Vec<&'static str>,
    }

    impl UiHook for SpyUi {
        fn toggle_menu(&mut self) {
            self.menu = !self.menu;
            self.toggles.push("menu");
        }
        fn toggle_overlay(&mut self) {
            self.overlay = !self.overlay;
            self.toggles.push("overlay");
        }
        fn menu_open(&self) -> bool {
            self.menu
        }
        fn wants_input(&self) -> bool {
            self.menu
        }
    }

    #[test]
    fn quit_sets_the_quit_flag_and_needs_no_command() {
        let (mut c, mut ui) = (Controls::default(), SpyUi::default());
        assert_eq!(dispatch(Action::Quit, &mut c, &mut ui), None);
        assert!(c.quit);
        assert!(ui.toggles.is_empty());
    }

    #[test]
    fn menu_and_overlay_go_to_the_ui_hook_only() {
        let (mut c, mut ui) = (Controls::default(), SpyUi::default());
        assert_eq!(dispatch(Action::ToggleMenu, &mut c, &mut ui), None);
        assert!(ui.menu_open());
        assert_eq!(dispatch(Action::ToggleOverlay, &mut c, &mut ui), None);
        assert_eq!(dispatch(Action::ToggleMenu, &mut c, &mut ui), None);
        assert!(!ui.menu_open());
        assert_eq!(ui.toggles, ["menu", "overlay", "menu"]);
        assert_eq!(c, Controls::default());
    }

    #[test]
    fn the_stub_ui_does_nothing_and_never_wants_input() {
        let (mut c, mut ui) = (Controls::default(), NoUi);
        for a in [Action::ToggleMenu, Action::ToggleOverlay] {
            assert_eq!(dispatch(a, &mut c, &mut ui), None);
        }
        assert!(!ui.menu_open() && !ui.wants_input());
    }

    #[test]
    fn bypass_toggles_and_reports_the_new_state() {
        let (mut c, mut ui) = (Controls::default(), NoUi);
        assert_eq!(
            dispatch(Action::ToggleBypass, &mut c, &mut ui),
            Some(Command::SetBypass(true))
        );
        assert!(c.bypass);
        assert_eq!(
            dispatch(Action::ToggleBypass, &mut c, &mut ui),
            Some(Command::SetBypass(false))
        );
        assert!(!c.bypass);
    }

    #[test]
    fn presets_and_fullscreen_become_commands() {
        let (mut c, mut ui) = (Controls::default(), NoUi);
        assert_eq!(
            dispatch(Action::NextPreset, &mut c, &mut ui),
            Some(Command::StepPreset(Step::Next))
        );
        assert_eq!(
            dispatch(Action::PreviousPreset, &mut c, &mut ui),
            Some(Command::StepPreset(Step::Previous))
        );
        assert_eq!(
            dispatch(Action::ToggleFullscreen, &mut c, &mut ui),
            Some(Command::ToggleWindowMode)
        );
        assert_eq!(c, Controls::default(), "these do not change the controls themselves");
    }

    #[test]
    fn every_action_is_handled() {
        // Adding an action without a dispatch arm fails to compile; this checks the arms are the intended ones.
        let (mut c, mut ui) = (Controls::default(), NoUi);
        let commands: Vec<_> = Action::ALL.iter().map(|&a| dispatch(a, &mut c, &mut ui)).collect();
        assert_eq!(commands.iter().filter(|c| c.is_some()).count(), 6);
        assert_eq!(dispatch(Action::Snapshot, &mut c, &mut ui), Some(Command::Snapshot));
        assert_eq!(
            dispatch(Action::ToggleClickThrough, &mut c, &mut ui),
            Some(Command::ToggleClickThrough)
        );
    }

    // ---- bypass plan ----

    const SLOTS: [(&str, bool); 3] = [("signal", true), ("gun", true), ("phosphor", false)];

    #[test]
    fn bypass_on_disables_every_slot_and_remembers_the_flags() {
        let mut saved = None;
        let plan = bypass_plan(true, &SLOTS, &mut saved);
        assert_eq!(plan, vec![("signal", false), ("gun", false), ("phosphor", false)]);
        assert_eq!(saved.as_deref(), Some(&SLOTS[..]));
    }

    #[test]
    fn bypass_off_restores_the_saved_flags() {
        let mut saved = None;
        let off: Vec<(&str, bool)> = bypass_plan(true, &SLOTS, &mut saved);
        let back = bypass_plan(false, &off, &mut saved);
        assert_eq!(back, SLOTS.to_vec(), "the slot that was off stays off");
        assert!(saved.is_none());
    }

    #[test]
    fn bypass_off_without_memory_enables_everything() {
        let mut saved = None;
        let all_off = [("signal", false), ("gun", false)];
        assert_eq!(
            bypass_plan(false, &all_off, &mut saved),
            vec![("signal", true), ("gun", true)]
        );
    }

    #[test]
    fn switching_the_bypass_on_twice_keeps_the_first_memory() {
        let mut saved = None;
        let _ = bypass_plan(true, &SLOTS, &mut saved);
        let _ = bypass_plan(
            true,
            &[("signal", false), ("gun", false), ("phosphor", false)],
            &mut saved,
        );
        assert_eq!(saved.as_deref(), Some(&SLOTS[..]));
    }

    #[test]
    fn bypass_lines_name_the_state() {
        assert!(bypass_line(true).contains("on"));
        assert!(bypass_line(false).contains("off"));
    }

    // ---- preset stepping ----

    #[test]
    fn next_and_previous_cycle_through_the_shipped_presets_with_different_params() {
        let lib = Library::embedded();
        assert!(lib.warnings.is_empty(), "{:?}", lib.warnings);
        let n = lib.list().len();
        assert_eq!(n, 10);
        let mut params = Params::default();
        let mut id: Option<String> = None;
        let mut seen: Vec<PresetStep> = Vec::new();
        for _ in 0..n {
            let s = step_preset(&lib, id.as_deref(), Step::Next, &params).unwrap();
            id = Some(s.id.clone());
            params = s.params;
            seen.push(s);
        }
        // Visits every preset once, in menu order.
        let ids: Vec<&str> = seen.iter().map(|s| s.id.as_str()).collect();
        let want: Vec<&str> = lib.list().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, want);
        // Consecutive presets give different parameters (the point of a preset hotkey).
        for w in seen.windows(2) {
            assert_ne!(
                w[0].params, w[1].params,
                "{} and {} give the same Params",
                w[0].id, w[1].id
            );
        }
        // One more Next wraps to the first.
        let wrapped = step_preset(&lib, id.as_deref(), Step::Next, &params).unwrap();
        assert_eq!(wrapped.id, seen[0].id);
        assert_eq!(wrapped.params, seen[0].params);
        // Previous undoes Next, also across the wrap.
        let back = step_preset(&lib, Some(&seen[0].id), Step::Previous, &params).unwrap();
        assert_eq!(back.id, seen[n - 1].id);
        let mid = step_preset(&lib, Some(&seen[4].id), Step::Previous, &params).unwrap();
        assert_eq!(mid.id, seen[3].id);
        assert_eq!(mid.params, seen[3].params);
    }

    #[test]
    fn stepping_from_no_current_preset_starts_at_the_ends() {
        let lib = Library::embedded();
        let p = Params::default();
        let first = step_preset(&lib, None, Step::Next, &p).unwrap();
        assert_eq!(first.id, lib.list()[0].id);
        let last = step_preset(&lib, Some("no-such-preset"), Step::Previous, &p).unwrap();
        assert_eq!(last.id, lib.list().last().unwrap().id);
        assert!(!first.name.is_empty());
    }

    #[test]
    fn stepping_keeps_the_source_parameters_and_replaces_the_tube() {
        let lib = Library::embedded();
        let old = Params {
            source_par: 8.0 / 7.0,
            source_scale: 3,
            source_center: false,
            beam_fwhm_mid_mm: 9.0, // a tube value the preset must replace
            ..Params::default()
        };
        let s = step_preset(&lib, None, Step::Next, &old).unwrap();
        assert_eq!(s.params.source_par, 8.0 / 7.0);
        assert_eq!(s.params.source_scale, 3);
        assert!(!s.params.source_center);
        assert_ne!(s.params.beam_fwhm_mid_mm, 9.0);
        assert_eq!(s.params.beam_fwhm_mid_mm, lib.list()[0].params().beam_fwhm_mid_mm);
    }

    #[test]
    fn an_empty_library_gives_no_step() {
        let lib = Library::default();
        assert_eq!(step_preset(&lib, None, Step::Next, &Params::default()), None);
        assert_eq!(step_preset(&lib, None, Step::Previous, &Params::default()), None);
    }
}
