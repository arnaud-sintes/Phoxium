//! The settings menu (T-52a, OUT-3, D-7, D-17): the focus hand-off and the menu window.
//!
//! Focus (D-7, T-08 section 9): there is no separate control window. The menu opens inside the output window
//! and takes the keyboard focus while it is open; closing it gives the foreground back to the window that had
//! it (the source). [`Menu::open`]: drop `WS_EX_NOACTIVATE` and stop the T-07 hand-back
//! (`crate::output::set_menu_open`, which also starts routing the input to the UI), remember the foreground
//! window, plain `SetForegroundWindow(output)` (T-08: plain works from the hotkey handler, 48 of 48, no
//! synthetic input). [`Menu::close`]: stop routing, restore `WS_EX_NOACTIVATE` and the hand-back, plain
//! `SetForegroundWindow` on [`restore_target`] (the remembered window, else the capture's source window, else
//! nothing). When the user activates another window while the menu is open (Alt+Tab, a click elsewhere), the
//! menu closes and the foreground stays where the user put it ([`on_activate`]): stealing it back would fight
//! the user. The decisions are pure functions, unit tested; only [`Menu`] calls Win32.
//!
//! The menu window (this task's content; the generated parameter panels are T-53): title bar, movable,
//! resizable, collapsible, an opacity slider; sections: preset selector (selecting applies the preset), SDR
//! headroom readout and the output headroom parameters (T-84), the source placement readout (T-96) and the GPU
//! milliseconds per stage (T-21). Position, size, opacity and the collapsed state persist in
//! `settings.toml` (`[global.ui]`). The parameter panels come from a registry by stage ([`panel_for_stage`]),
//! the extension point of T-53.

use dear_imgui_rs::{Condition, TreeNodeFlags, Ui as Frame, WindowFlags};

use crate::params::{ParamType, ParamValue, Params, PARAMS_META};
use crate::presets::Library;
use crate::settings;

use super::UiState;

// ---------------------------------------------------------------------------------------------------------
// Focus decisions (pure).

/// The window to remember when the menu opens: the foreground window, unless there is none or it is the
/// output itself (then nothing is remembered and the close falls back to the source window).
pub fn remember(foreground: isize, output: isize) -> Option<isize> {
    (foreground != 0 && foreground != output).then_some(foreground)
}

/// The window that gets the foreground back when the menu closes: the remembered window if it still
/// exists, else the source window of the capture if it exists, else none (the foreground is left alone).
/// The output itself is never a target. `exists` is `IsWindow` in the product.
pub fn restore_target(
    remembered: Option<isize>,
    source: Option<isize>,
    output: isize,
    exists: impl Fn(isize) -> bool,
) -> Option<isize> {
    [remembered, source]
        .into_iter()
        .flatten()
        .find(|&h| h != 0 && h != output && exists(h))
}

/// Why the menu closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The menu key (or [`super::Ui::request_toggle`]): the foreground goes back to the source.
    Toggle,
    /// The output lost the activation while the menu was open (the user chose another window): the menu
    /// closes and the foreground stays with the window the user chose.
    Deactivated,
}

/// What a `WM_ACTIVATE` of the output means for the menu: `activated` is false for `WA_INACTIVE`. The
/// menu closes, without restoring the foreground, when the output is deactivated while it is open.
pub fn on_activate(open: bool, activated: bool) -> Option<CloseReason> {
    (open && !activated).then_some(CloseReason::Deactivated)
}

/// Virtual-key code of Esc.
pub const VK_ESCAPE: usize = 0x1B;

/// Whether the key message `msg` (`wparam` the virtual key) closes the open menu: Esc pressed. The menu
/// consumes it, so the output window's own Esc (close the output) never fires while the menu is open. The
/// other hotkeys (F11, the quit chord) are `WM_HOTKEY` registrations and keep working with the menu open.
pub fn closes_on_key(open: bool, msg: u32, wparam: usize) -> bool {
    open && msg == super::input::WM_KEYDOWN && wparam == VK_ESCAPE
}

/// Whether a close for `reason` hands the foreground back to [`restore_target`].
pub fn restores_foreground(reason: CloseReason) -> bool {
    reason == CloseReason::Toggle
}

// ---------------------------------------------------------------------------------------------------------
// The focus hand-off (Win32).

/// Open state of the menu and the window to give the foreground back to.
#[derive(Debug, Default)]
pub struct Menu {
    open: bool,
    remembered: Option<isize>,
    /// The window the last close gave the foreground to (for the log and the tests).
    pub last_restore: Option<isize>,
}

fn hwnd(h: isize) -> windows::Win32::Foundation::HWND {
    windows::Win32::Foundation::HWND(h as *mut std::ffi::c_void)
}

fn foreground() -> isize {
    // SAFETY: plain query.
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0 as isize
}

fn window_exists(h: isize) -> bool {
    // SAFETY: plain query; any value is accepted.
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(hwnd(h))) }.as_bool()
}

fn set_foreground(h: isize) -> bool {
    // SAFETY: plain call on a window handle; a stale handle makes it fail, nothing else.
    unsafe { windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd(h)) }.as_bool()
}

impl Menu {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the menu. With an `output` window: input routing on and the T-07 hand-back off
    /// ([`crate::output::set_menu_open`]: `WS_EX_NOACTIVATE` dropped first, so the activation is not handed
    /// back), the foreground window remembered, then a plain `SetForegroundWindow(output)`. Returns whether
    /// the output took the foreground (always true without a window).
    pub fn open(&mut self, output: Option<isize>) -> bool {
        if self.open {
            return true;
        }
        self.open = true;
        let Some(out) = output else {
            return true;
        };
        self.remembered = remember(foreground(), out);
        crate::output::set_menu_open(hwnd(out), true);
        set_foreground(out)
    }

    /// Closes the menu. With an `output` window: routing off, `WS_EX_NOACTIVATE` and the hand-back restored,
    /// then for [`CloseReason::Toggle`] a plain `SetForegroundWindow` on [`restore_target`] (the remembered
    /// window, else `source`). Returns the window the foreground was given to.
    pub fn close(&mut self, output: Option<isize>, source: Option<isize>, reason: CloseReason) -> Option<isize> {
        if !self.open {
            return None;
        }
        self.open = false;
        let remembered = self.remembered.take();
        let out = output?;
        crate::output::set_menu_open(hwnd(out), false);
        let target = if restores_foreground(reason) {
            restore_target(remembered, source, out, window_exists)
        } else {
            None
        };
        if let Some(t) = target {
            set_foreground(t);
        }
        self.last_restore = target;
        target
    }
}

// ---------------------------------------------------------------------------------------------------------
// Parameter panels (extension point of T-53).

/// A parameter panel: draws the widgets of one stage's parameters; true when a value changed.
pub type Panel = fn(&Frame, &mut Params) -> bool;

/// Panels by stage (`ParamMeta::stage`, the stem of the defining `params/*.toml`). T-52a registers the output
/// headroom panel only; T-53 generates the others from `PARAMS_META` and adds them here.
pub const PANELS: &[(&str, Panel)] = &[("output", output_panel)];

/// The panel of `stage`, if one is registered.
pub fn panel_for_stage(stage: &str) -> Option<Panel> {
    PANELS.iter().find(|(s, _)| *s == stage).map(|&(_, p)| p)
}

/// The output headroom parameters (T-84, D-16): mode, exposure, knee, margin, shoulder white, mask depth.
pub const HEADROOM_IDS: &[&str] = &[
    "output_headroom_mode",
    "exposure",
    "output_knee",
    "output_headroom_margin",
    "output_shoulder_white",
    "mask_depth",
];

fn output_panel(ui: &Frame, params: &mut Params) -> bool {
    let mut changed = false;
    for id in HEADROOM_IDS {
        changed |= param_widget(ui, params, id);
    }
    changed
}

/// One widget for the parameter `id`, from its metadata: a slider over the soft range for numbers, a check
/// box for a bool, a combo for an enum (vectors are shown, not edited, until T-53). True when it changed.
pub fn param_widget(ui: &Frame, params: &mut Params, id: &str) -> bool {
    let Some(m) = PARAMS_META.iter().find(|m| m.id == id) else {
        return false;
    };
    let label = if m.unit == "1" {
        m.id.to_string()
    } else {
        format!("{} [{}]", m.id, m.unit)
    };
    let (lo, hi) = m.soft_range.unwrap_or((0.0, 1.0));
    let changed = match (m.ty, params.get(id)) {
        (ParamType::F32, Some(ParamValue::F32(mut v))) => {
            ui.slider_f32(&label, &mut v, lo as f32, hi as f32) && params.set(id, ParamValue::F32(v))
        }
        (ParamType::I32, Some(ParamValue::I32(mut v))) => {
            ui.slider_i32(&label, &mut v, lo as i32, hi as i32) && params.set(id, ParamValue::I32(v))
        }
        (ParamType::Bool, Some(ParamValue::Bool(mut v))) => {
            ui.checkbox(&label, &mut v) && params.set(id, ParamValue::Bool(v))
        }
        (ParamType::Enum, Some(ParamValue::Enum(v))) => {
            let mut i = v as usize;
            ui.combo_simple_string(&label, &mut i, m.values) && params.set(id, ParamValue::Enum(i as u32))
        }
        (_, Some(v)) => {
            ui.label_text(&label, format!("{v:?}"));
            false
        }
        _ => false,
    };
    if ui.is_item_hovered() {
        ui.tooltip_text(m.description);
    }
    changed
}

// ---------------------------------------------------------------------------------------------------------
// The menu window.

/// Title of the menu window (the `###` part is its ImGui id, stable when the title changes).
pub const TITLE: &str = "ultimatecrt settings###ucrt_menu";

/// Screen rectangles (min, max) of the menu widgets in the frame just built (tests, live check).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MenuRects {
    pub window: [[f32; 2]; 2],
    pub opacity: [[f32; 2]; 2],
    pub filter: [[f32; 2]; 2],
    pub preset: [[f32; 2]; 2],
    pub headroom_mode: [[f32; 2]; 2],
    pub exposure: [[f32; 2]; 2],
}

impl MenuRects {
    /// A point inside `r` at fractions (`fx`, `fy`) of its size.
    pub fn at(r: [[f32; 2]; 2], fx: f32, fy: f32) -> [f32; 2] {
        [r[0][0] + (r[1][0] - r[0][0]) * fx, r[0][1] + (r[1][1] - r[0][1]) * fy]
    }
}

/// What the menu window edits in one frame.
pub struct MenuModel<'a> {
    pub state: &'a UiState,
    /// The parameters the menu shows and edits (a copy of `state.params`).
    pub params: &'a mut Params,
    /// Text filter of the preset list.
    pub filter: &'a mut String,
    pub layout: &'a mut settings::UiState,
    pub library: &'a Library,
    /// Menu key shown in the title bar hint.
    pub key: &'a str,
    /// Hotkey registration problems, shown under the hint (empty: nothing shown).
    pub hotkey_problems: &'a [String],
    /// Give the keyboard focus to the menu window in this frame (the frame after it opened).
    pub focus: bool,
}

/// What the menu window did in one frame.
#[derive(Debug, Default)]
pub struct MenuResult {
    /// A parameter changed (through a widget or a preset).
    pub params_changed: bool,
    /// The preset selected in this frame (its id); `params` already holds its parameters.
    pub preset: Option<String>,
    /// The layout (position, size, opacity, collapsed) changed.
    pub layout_changed: bool,
    pub rects: MenuRects,
}

/// The presets whose id or name contains `filter` (case insensitive), as (id, label).
pub fn filtered_presets(library: &Library, filter: &str) -> Vec<(String, String)> {
    let f = filter.trim().to_lowercase();
    library
        .list()
        .iter()
        .filter(|p| f.is_empty() || p.id.to_lowercase().contains(&f) || p.name.to_lowercase().contains(&f))
        .map(|p| (p.id.clone(), format!("{} ({})", p.name, p.id)))
        .collect()
}

/// Builds the menu window.
pub fn build(ui: &Frame, m: MenuModel<'_>) -> MenuResult {
    let mut r = MenuResult::default();
    let rect = |ui: &Frame| [ui.item_rect_min(), ui.item_rect_max()];
    let before = m.layout.clone();
    // A layout saved on a larger output (or another monitor) must not put the title bar out of reach.
    let display = ui.io().display_size();
    let pos = [
        m.layout.menu_x.clamp(0.0, (display[0] - 120.0).max(0.0)),
        m.layout.menu_y.clamp(0.0, (display[1] - 40.0).max(0.0)),
    ];
    let shown = ui
        .window(TITLE)
        .position(pos, Condition::Appearing)
        .size([m.layout.menu_w, m.layout.menu_h], Condition::Appearing)
        .collapsed(m.layout.menu_collapsed, Condition::Appearing)
        .size_constraints([240.0, 120.0], [f32::MAX, f32::MAX])
        .bg_alpha(m.layout.menu_opacity.clamp(0.2, 1.0))
        .focused(m.focus)
        .flags(WindowFlags::NO_SAVED_SETTINGS)
        .build(|| {
            let (pos, size) = (ui.window_pos(), ui.window_size());
            r.rects.window = [pos, [pos[0] + size[0], pos[1] + size[1]]];
            (m.layout.menu_x, m.layout.menu_y) = (pos[0], pos[1]);
            (m.layout.menu_w, m.layout.menu_h) = (size[0], size[1]);
            m.layout.menu_collapsed = false;
            ui.text_disabled(format!("{} or Esc closes the menu", m.key));
            // Hotkey registration problems (T-54): a chord another program owns, the fallback in force.
            for p in m.hotkey_problems {
                ui.text_wrapped(format!("! {p}"));
            }
            ui.slider_f32("opacity", &mut m.layout.menu_opacity, 0.2, 1.0);
            r.rects.opacity = rect(ui);

            if ui.collapsing_header("Preset", TreeNodeFlags::DEFAULT_OPEN) {
                ui.input_text("filter", m.filter).build();
                r.rects.filter = rect(ui);
                let list = filtered_presets(m.library, m.filter);
                let current = m.state.preset.as_deref();
                let preview = current
                    .and_then(|id| m.library.find(id))
                    .map_or("(custom)".to_string(), |p| format!("{} ({})", p.name, p.id));
                if let Some(_combo) = ui.begin_combo("preset", &preview) {
                    for (id, label) in &list {
                        let selected = current == Some(id.as_str());
                        if ui.selectable_config(label).selected(selected).build() {
                            if let Some(p) = m.library.find(id) {
                                *m.params = p.params();
                                r.preset = Some(id.clone());
                                r.params_changed = true;
                            }
                        }
                        if selected {
                            ui.set_item_default_focus();
                        }
                    }
                }
                r.rects.preset = rect(ui);
                ui.text_disabled(format!("{} of {} presets", list.len(), m.library.list().len()));
            }

            if ui.collapsing_header("Headroom", TreeNodeFlags::DEFAULT_OPEN) {
                match m.state.headroom {
                    Some(h) => {
                        ui.text(format!("headroom {:.3}", h.total()));
                        ui.text_disabled(format!(
                            "mask {:.3} (physical {:.3}) x scanlines {:.3} x white {:.3} x exposure {:.3}",
                            h.mask_peak, h.mask_peak_physical, h.scanline_peak, h.matrix_max, h.exposure
                        ));
                        let mode = crate::headroom::effective_mode(m.params, h.mask_peak_physical);
                        let name = crate::params::meta("output_headroom_mode")
                            .and_then(|meta| meta.values.get(mode as usize))
                            .copied()
                            .unwrap_or("?");
                        ui.text_disabled(format!("mode in effect: {name}"));
                    }
                    None => ui.text_disabled("headroom: no frame yet"),
                }
                // The registered output panel; the first two widgets are the mode combo and the exposure.
                for (k, id) in HEADROOM_IDS.iter().enumerate() {
                    r.params_changed |= param_widget(ui, m.params, id);
                    match k {
                        0 => r.rects.headroom_mode = rect(ui),
                        1 => r.rects.exposure = rect(ui),
                        _ => {}
                    }
                }
            }

            if ui.collapsing_header("Source", TreeNodeFlags::empty()) {
                match &m.state.placement {
                    Some(p) => ui.text_wrapped(p.to_string()),
                    None => ui.text_disabled("no source frame yet"),
                }
            }

            if ui.collapsing_header("Stats", TreeNodeFlags::empty()) {
                ui.text(format!(
                    "capture {:.0} fps | render {:.0} fps",
                    m.state.capture_fps, m.state.render_fps
                ));
                match &m.state.timings {
                    Some(t) => {
                        ui.text(format!("GPU {:.3} ms per frame", t.frame_ms));
                        for s in &t.stages {
                            match s.ms {
                                Some(ms) if s.bypassed => ui.text(format!("  {:<10} {ms:.3} ms (bypass)", s.name)),
                                Some(ms) => ui.text(format!("  {:<10} {ms:.3} ms", s.name)),
                                None => ui.text_disabled(format!("  {:<10} -", s.name)),
                            }
                        }
                    }
                    None => ui.text_disabled("GPU timings: none yet (or unavailable)"),
                }
            }

            // Further stage panels registered by T-53 (the output panel is drawn under "Headroom").
            for (stage, panel) in PANELS.iter().filter(|(s, _)| *s != "output") {
                if ui.collapsing_header(stage, TreeNodeFlags::empty()) {
                    r.params_changed |= panel(ui, m.params);
                }
            }
        });
    if shown.is_none() {
        m.layout.menu_collapsed = true;
    }
    r.layout_changed = *m.layout != before;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: isize = 0x10;
    const SRC: isize = 0x20;
    const OTHER: isize = 0x30;

    #[test]
    fn the_foreground_is_remembered_unless_it_is_the_output_or_none() {
        assert_eq!(remember(SRC, OUT), Some(SRC));
        assert_eq!(remember(OUT, OUT), None);
        assert_eq!(remember(0, OUT), None);
    }

    #[test]
    fn the_remembered_window_gets_the_foreground_back_first() {
        assert_eq!(restore_target(Some(OTHER), Some(SRC), OUT, |_| true), Some(OTHER));
    }

    #[test]
    fn a_closed_remembered_window_falls_back_to_the_source_then_nothing() {
        let alive = |h: isize| h == SRC;
        assert_eq!(restore_target(Some(OTHER), Some(SRC), OUT, alive), Some(SRC));
        assert_eq!(restore_target(None, Some(SRC), OUT, alive), Some(SRC));
        // Neither exists: the foreground is left alone.
        assert_eq!(restore_target(Some(OTHER), Some(SRC), OUT, |_| false), None);
        assert_eq!(restore_target(None, None, OUT, |_| true), None);
    }

    #[test]
    fn the_output_and_null_are_never_restore_targets() {
        assert_eq!(restore_target(Some(OUT), Some(SRC), OUT, |_| true), Some(SRC));
        assert_eq!(restore_target(Some(0), Some(OUT), OUT, |_| true), None);
    }

    #[test]
    fn deactivation_while_open_closes_without_stealing_the_foreground_back() {
        assert_eq!(on_activate(true, false), Some(CloseReason::Deactivated));
        assert!(!restores_foreground(CloseReason::Deactivated));
        // Activation while open (our own SetForegroundWindow) and anything while closed: nothing.
        assert_eq!(on_activate(true, true), None);
        assert_eq!(on_activate(false, false), None);
        assert_eq!(on_activate(false, true), None);
        assert!(restores_foreground(CloseReason::Toggle));
    }

    #[test]
    fn esc_closes_the_open_menu_only() {
        use super::super::input::{WM_KEYDOWN, WM_KEYUP};
        assert!(closes_on_key(true, WM_KEYDOWN, VK_ESCAPE));
        assert!(
            !closes_on_key(false, WM_KEYDOWN, VK_ESCAPE),
            "closed menu: Esc is the output's"
        );
        assert!(!closes_on_key(true, WM_KEYUP, VK_ESCAPE));
        assert!(!closes_on_key(true, WM_KEYDOWN, 0x7A), "F11 is not a menu key");
    }

    #[test]
    fn the_menu_key_default_is_the_hotkey_default() {
        assert_eq!(
            super::super::DEFAULT_MENU_KEY,
            crate::hotkeys::Action::ToggleMenu.default_text()
        );
    }

    #[test]
    fn menu_without_window_only_toggles_its_state() {
        let mut m = Menu::default();
        assert!(!m.is_open());
        assert!(m.open(None));
        assert!(m.is_open());
        assert_eq!(m.close(None, Some(SRC), CloseReason::Toggle), None);
        assert!(!m.is_open());
        // Closing a closed menu does nothing.
        assert_eq!(m.close(None, Some(SRC), CloseReason::Toggle), None);
    }

    #[test]
    fn panel_registry_has_the_output_panel_and_every_headroom_id_exists() {
        assert!(panel_for_stage("output").is_some());
        assert!(panel_for_stage("no_such_stage").is_none());
        for id in HEADROOM_IDS {
            assert!(crate::params::meta(id).is_some(), "{id}");
        }
    }

    #[test]
    fn preset_filter_matches_id_or_name_case_insensitively() {
        let lib = Library::embedded();
        let all = filtered_presets(&lib, "");
        assert_eq!(all.len(), lib.list().len());
        let first = &lib.list()[0];
        let by_id = filtered_presets(&lib, &first.id.to_uppercase());
        assert!(by_id.iter().any(|(id, _)| *id == first.id));
        assert!(filtered_presets(&lib, "no preset is called this").is_empty());
    }
}
