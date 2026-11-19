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
//!
//! Look (`super::theme`): a flat SemiBold title bar, collapsible sections with small-capital headers, rows with
//! the label on the left and the widget or value on the right (numbers right-aligned, tabular digits), sliders
//! without inner text and with an accent fill, a scrolling body above a pinned footer (opacity, close hint).

use dear_imgui_rs::{Condition, StyleVar, Ui as Frame, WindowFlags};

use crate::params::{ParamMeta, ParamType, ParamValue, Params, PARAMS_META};
use crate::presets::Library;
use crate::settings;

use super::theme::{self, Fonts, METRICS, PALETTE};
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

/// The label shown for a parameter in a section about `context` (a stage): its id in words, without the stage
/// prefix when the parameter belongs to that stage (`output_knee` under `output` -> "Knee"; `mask_depth` under
/// `output` -> "Mask depth").
pub fn param_label(m: &ParamMeta, context: &str) -> String {
    let prefix = format!("{}_", m.stage);
    let id = if m.stage == context {
        m.id.strip_prefix(&prefix).unwrap_or(m.id)
    } else {
        m.id
    };
    words(id)
}

/// An identifier in words with a capital first letter (`mask_depth` -> "Mask depth").
pub fn words(id: &str) -> String {
    let s = id.replace('_', " ");
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The value text of a number in a slider row: precision from the range, the unit if it has one.
pub fn value_text(v: f64, range: (f64, f64), unit: &str) -> String {
    let span = (range.1 - range.0).abs();
    let digits = if span >= 100.0 {
        1
    } else if span >= 10.0 {
        2
    } else {
        3
    };
    if unit == "1" {
        format!("{v:.digits$}")
    } else {
        format!("{v:.digits$} {unit}")
    }
}

/// Width of the value column of slider rows.
fn value_width(ui: &Frame) -> f32 {
    ui.calc_text_size("00.000 mm")[0]
}

/// One row for the parameter `id`, from its metadata: its label on the left; a slider over the soft range with
/// the value aligned right for numbers, a check box for a bool, a combo for an enum (vectors are shown, not
/// edited, until T-53). The description is the tooltip. True when it changed.
pub fn param_widget(ui: &Frame, params: &mut Params, id: &str) -> bool {
    let stage = PARAMS_META.iter().find(|m| m.id == id).map_or("", |m| m.stage);
    param_row(ui, params, id, stage).0
}

/// [`param_widget`] in a section about the stage `context` ([`param_label`]), also returning the rectangle
/// (min, max) of the widget itself (not the label or value).
pub fn param_row(ui: &Frame, params: &mut Params, id: &str, context: &str) -> (bool, [[f32; 2]; 2]) {
    let Some(m) = PARAMS_META.iter().find(|m| m.id == id) else {
        return (false, [[0.0; 2]; 2]);
    };
    let width = theme::row_label(ui, &param_label(m, context));
    let label_hovered = ui.is_item_hovered();
    let wid = format!("##{}", m.id);
    let range = m.soft_range.unwrap_or((0.0, 1.0));
    let (lo, hi) = range;
    let track = width - value_width(ui) - ui.clone_style().item_spacing()[0];
    let mut value_shown = None;
    let changed = match (m.ty, params.get(id)) {
        (ParamType::F32, Some(ParamValue::F32(mut v))) => {
            let c = theme::slider(ui, &wid, &mut v, lo as f32, hi as f32, track);
            value_shown = Some(value_text(f64::from(v), range, m.unit));
            c && params.set(id, ParamValue::F32(v))
        }
        (ParamType::I32, Some(ParamValue::I32(mut v))) => {
            let c = theme::slider(ui, &wid, &mut v, lo as i32, hi as i32, track);
            value_shown = Some(if m.unit == "1" {
                v.to_string()
            } else {
                format!("{v} {}", m.unit)
            });
            c && params.set(id, ParamValue::I32(v))
        }
        (ParamType::Bool, Some(ParamValue::Bool(mut v))) => {
            ui.checkbox(&wid, &mut v) && params.set(id, ParamValue::Bool(v))
        }
        (ParamType::Enum, Some(ParamValue::Enum(v))) => {
            ui.set_next_item_width(width);
            let current = m.values.get(v as usize).map_or_else(|| "?".to_string(), |s| words(s));
            let mut pick = None;
            if let Some(_combo) = ui.begin_combo(&wid, &current) {
                for (i, name) in m.values.iter().enumerate() {
                    let selected = i == v as usize;
                    if ui.selectable_config(words(name)).selected(selected).build() {
                        pick = Some(i as u32);
                    }
                    if selected {
                        ui.set_item_default_focus();
                    }
                }
            }
            pick.is_some_and(|i| i != v && params.set(id, ParamValue::Enum(i)))
        }
        (_, Some(v)) => {
            ui.text_colored(PALETTE.text, format!("{v:?}"));
            false
        }
        _ => false,
    };
    let widget_hovered = ui.is_item_hovered();
    let rect = [ui.item_rect_min(), ui.item_rect_max()];
    if let Some(text) = value_shown {
        ui.same_line();
        theme::text_right(ui, PALETTE.text, &text);
    }
    if (label_hovered || widget_hovered) && !m.description.is_empty() {
        theme::tooltip(ui, m.description);
    }
    (changed, rect)
}

// ---------------------------------------------------------------------------------------------------------
// The menu window.

/// Title of the menu window (the `###` part is its ImGui id, stable when the title changes).
pub const TITLE: &str = "ultimateCRT###ucrt_menu";

/// Screen rectangles (min, max) of the menu widgets in the frame just built (tests, live check).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MenuRects {
    pub window: [[f32; 2]; 2],
    pub opacity: [[f32; 2]; 2],
    pub filter: [[f32; 2]; 2],
    pub preset: [[f32; 2]; 2],
    pub headroom_mode: [[f32; 2]; 2],
    pub exposure: [[f32; 2]; 2],
    /// Headers of the sections closed by default.
    pub source_header: [[f32; 2]; 2],
    pub performance_header: [[f32; 2]; 2],
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
    /// Menu key shown in the footer hint.
    pub key: &'a str,
    /// Hotkey registration problems, shown at the top (empty: nothing shown).
    pub hotkey_problems: &'a [String],
    /// Give the keyboard focus to the menu window in this frame (the frame after it opened).
    pub focus: bool,
    /// The theme's fonts ([`theme::load_fonts`]).
    pub fonts: Fonts,
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

/// The presets whose id or name contains `filter` (case insensitive), as (id, name).
pub fn filtered_presets(library: &Library, filter: &str) -> Vec<(String, String)> {
    let f = filter.trim().to_lowercase();
    library
        .list()
        .iter()
        .filter(|p| f.is_empty() || p.id.to_lowercase().contains(&f) || p.name.to_lowercase().contains(&f))
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect()
}

/// Position and size of the menu window on a display of `display` px at UI scale `scale`, from the saved
/// layout: the default size grows with the scale (a size the user set is kept as is), the size is at most the
/// display less a margin, and the window is moved so that it lies on the display.
pub fn placement(layout: &settings::UiState, display: [f32; 2], scale: f32) -> ([f32; 2], [f32; 2]) {
    let default = settings::UiState::default();
    let mut size = [layout.menu_w, layout.menu_h];
    if size == [default.menu_w, default.menu_h] {
        size = [size[0] * scale, size[1] * scale];
    }
    let margin = 8.0 * scale;
    let fit = |v: f32, d: f32, min: f32| v.min(d - 2.0 * margin).max(min);
    let size = [fit(size[0], display[0], 240.0), fit(size[1], display[1], 120.0)];
    let pos = [
        layout.menu_x.clamp(0.0, (display[0] - size[0]).max(0.0)),
        layout.menu_y.clamp(0.0, (display[1] - size[1]).max(0.0)),
    ];
    (pos, size)
}

/// Builds the menu window.
pub fn build(ui: &Frame, mut m: MenuModel<'_>) -> MenuResult {
    let mut r = MenuResult::default();
    let before = m.layout.clone();
    let s = theme::scale_of(ui);
    let fonts = m.fonts;
    let (pos, size) = placement(m.layout, ui.io().display_size(), s);
    // A taller title bar in the SemiBold font; the content goes back to the body font and frame padding.
    let title_pad = ui.push_style_var(StyleVar::FramePadding([14.0 * s, 9.0 * s]));
    let shown = theme::with_font(ui, fonts.strong, METRICS.font_body, || {
        ui.window(TITLE)
            .position(pos, Condition::Appearing)
            .size(size, Condition::Appearing)
            .collapsed(m.layout.menu_collapsed, Condition::Appearing)
            .size_constraints([240.0 * s, 120.0 * s], [f32::MAX, f32::MAX])
            .bg_alpha(m.layout.menu_opacity.clamp(0.2, 1.0))
            .focused(m.focus)
            .flags(WindowFlags::NO_SAVED_SETTINGS | WindowFlags::NO_SCROLLBAR | WindowFlags::NO_SCROLL_WITH_MOUSE)
            .build(|| {
                let pad = METRICS.frame_padding;
                let _pad = ui.push_style_var(StyleVar::FramePadding([pad[0] * s, pad[1] * s]));
                theme::with_font(ui, fonts.body, METRICS.font_body, || {
                    let (pos, size) = (ui.window_pos(), ui.window_size());
                    r.rects.window = [pos, [pos[0] + size[0], pos[1] + size[1]]];
                    (m.layout.menu_x, m.layout.menu_y) = (pos[0], pos[1]);
                    (m.layout.menu_w, m.layout.menu_h) = (size[0], size[1]);
                    m.layout.menu_collapsed = false;
                    // The footer (rule, opacity row, hint) stays put; the body above it scrolls.
                    // Footer items: the rule (1 px), a 2 px gap, the opacity row, the hint; each followed by the
                    // item spacing, as is the body child itself.
                    let small = theme::with_font(ui, None, METRICS.font_small, || ui.text_line_height());
                    let sp = ui.clone_style().item_spacing()[1];
                    let footer_h = 1.0 + 2.0 * s + ui.frame_height() + small + 4.0 * sp;
                    ui.child_window("##ucrt_menu_body")
                        .size([0.0, -footer_h])
                        .build(ui, || body(ui, &mut m, &mut r));
                    footer(ui, &mut m, &mut r);
                });
            })
    });
    title_pad.pop();
    if shown.is_none() {
        m.layout.menu_collapsed = true;
    }
    r.layout_changed = *m.layout != before;
    r
}

fn rect(ui: &Frame) -> [[f32; 2]; 2] {
    [ui.item_rect_min(), ui.item_rect_max()]
}

/// The scrolling body: hotkey problems, preset, headroom, source, performance.
fn body(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult) {
    let fonts = m.fonts;
    // Hotkey registration problems (T-54): a chord another program owns, the fallback in force.
    if !m.hotkey_problems.is_empty() {
        theme::callout(ui, PALETTE.problem, m.hotkey_problems);
        ui.spacing();
    }

    // Preset: filter and selector; selecting applies the preset.
    let list = filtered_presets(m.library, m.filter);
    let count = format!("{} of {}", list.len(), m.library.list().len());
    if theme::section(ui, &fonts, "Preset", Some(&count), true) {
        ui.set_next_item_width(-f32::MIN_POSITIVE);
        ui.input_text("##filter", m.filter).hint("Filter presets").build();
        r.rects.filter = rect(ui);
        let current = m.state.preset.as_deref();
        let preview = current
            .and_then(|id| m.library.find(id))
            .map_or("Custom".to_string(), |p| p.name.clone());
        ui.set_next_item_width(-f32::MIN_POSITIVE);
        if let Some(_combo) = ui.begin_combo("##preset", &preview) {
            for (id, name) in &list {
                let selected = current == Some(id.as_str());
                if ui.selectable_config(format!("{name}##{id}")).selected(selected).build() {
                    if let Some(p) = m.library.find(id) {
                        *m.params = p.params();
                        r.preset = Some(id.clone());
                        r.params_changed = true;
                    }
                }
                if ui.is_item_hovered() {
                    theme::tooltip(ui, id);
                }
                if selected {
                    ui.set_item_default_focus();
                }
            }
        }
        r.rects.preset = rect(ui);
    }

    // SDR headroom (T-84): the readout, its factors, the mode in effect, the output panel.
    if theme::section(ui, &fonts, "SDR headroom", None, false) {
        match m.state.headroom {
            Some(h) => {
                let total = h.total();
                let (colour, status) = if total > 1.0 {
                    (PALETTE.accent, "Above panel white")
                } else {
                    (PALETTE.ok, "Fits panel white")
                };
                theme::with_font(ui, fonts.strong, METRICS.font_display, || {
                    ui.text_colored(colour, format!("{total:.3}"));
                });
                let [min, max] = [ui.item_rect_min(), ui.item_rect_max()];
                ui.same_line();
                theme::with_font(ui, None, METRICS.font_small, || {
                    // The unit on the number's baseline band, a status pill on the right.
                    let y = max[1] - ui.text_line_height() - 0.22 * (max[1] - min[1]);
                    ui.set_cursor_screen_pos([ui.cursor_screen_pos()[0], y]);
                    ui.text_colored(PALETTE.text_muted, "\u{00D7} panel white");
                    theme::pill(ui, status, colour, (min[1] + max[1]) * 0.5);
                });
                // The factors of the readout, one faint line.
                let mask = if (h.mask_peak - h.mask_peak_physical).abs() > 5e-4 {
                    format!("{:.3} (physical {:.3})", h.mask_peak, h.mask_peak_physical)
                } else {
                    format!("{:.3}", h.mask_peak)
                };
                theme::with_font(ui, None, METRICS.font_small, || {
                    ui.text_colored(
                        PALETTE.text_faint,
                        format!(
                            "mask {mask}  \u{00D7}  scanlines {:.3}  \u{00D7}  white {:.3}  \u{00D7}  exposure {:.3}",
                            h.scanline_peak, h.matrix_max, h.exposure
                        ),
                    );
                });
                ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
                let mode = crate::headroom::effective_mode(m.params, h.mask_peak_physical);
                let name = crate::params::meta("output_headroom_mode")
                    .and_then(|meta| meta.values.get(mode as usize))
                    .map_or_else(|| "?".to_string(), |v| words(v));
                theme::kv_row(ui, "Mode in effect", &name, PALETTE.text);
            }
            None => theme::kv_row(ui, "Readout", "no frame yet", PALETTE.text_faint),
        }
        ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
        // The registered output panel; the first two rows are the mode combo and the exposure.
        for (k, id) in HEADROOM_IDS.iter().enumerate() {
            let (changed, widget) = param_row(ui, m.params, id, "output");
            r.params_changed |= changed;
            match k {
                0 => r.rects.headroom_mode = widget,
                1 => r.rects.exposure = widget,
                _ => {}
            }
        }
    }

    // Source placement (T-96).
    let (open, header) = theme::section_closed(ui, &fonts, "Source");
    r.rects.source_header = header;
    if open {
        match &m.state.placement {
            Some(p) => placement_rows(ui, p),
            None => theme::kv_row(ui, "Source", "no frame yet", PALETTE.text_faint),
        }
    }

    // Performance (T-21): fps and the GPU milliseconds per stage.
    let (open, header) = theme::section_closed(ui, &fonts, "Performance");
    r.rects.performance_header = header;
    if open {
        theme::kv_row(ui, "Capture", &format!("{:.0} fps", m.state.capture_fps), PALETTE.text);
        theme::kv_row(ui, "Render", &format!("{:.0} fps", m.state.render_fps), PALETTE.text);
        match &m.state.timings {
            Some(t) => {
                theme::kv_row(ui, "GPU per frame", &format!("{:.3} ms", t.frame_ms), PALETTE.accent);
                ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
                for st in &t.stages {
                    let (text, colour) = match st.ms {
                        Some(ms) if st.bypassed => (format!("bypass  {ms:.3} ms"), PALETTE.text_faint),
                        Some(ms) => (format!("{ms:.3} ms"), PALETTE.text),
                        None => ("off".to_string(), PALETTE.text_faint),
                    };
                    let share = st.ms.map_or(0.0, |ms| (ms / t.frame_ms.max(1e-9)) as f32);
                    theme::bar_row(ui, st.name, share, &text, colour);
                }
            }
            None => theme::kv_row(ui, "GPU timings", "none yet", PALETTE.text_faint),
        }
    }

    // Further stage panels registered by T-53 (the output panel is drawn under the headroom).
    for (stage, panel) in PANELS.iter().filter(|(s, _)| *s != "output") {
        if theme::section_closed(ui, &fonts, &words(stage)).0 {
            r.params_changed |= panel(ui, m.params);
        }
    }
}

/// The source placement readout as label and value rows.
fn placement_rows(ui: &Frame, p: &crate::placement::Readout) {
    use crate::placement::Fit;
    use crt_model::mask::AxisTier;
    let tier = |t: AxisTier| match t {
        AxisTier::Integrated => "integrated".to_string(),
        AxisTier::Blended { box_px } => format!("blended, box {box_px:.2} px"),
        AxisTier::Prefiltered => "prefiltered".to_string(),
    };
    let text = PALETTE.text;
    theme::kv_row(ui, "Mode", &p.mode.to_string(), PALETTE.accent);
    theme::kv_row(ui, "Native", &format!("{} \u{00D7} {}", p.native.0, p.native.1), text);
    theme::kv_row(ui, "Output", &format!("{} \u{00D7} {}", p.target.0, p.target.1), text);
    theme::kv_row(
        ui,
        "Picture",
        &format!(
            "{} \u{00D7} {} at {}, {}",
            p.picture_px.0, p.picture_px.1, p.offset_px.0, p.offset_px.1
        ),
        text,
    );
    let scale = format!("{:.4} \u{00D7} {:.4}", p.px_per_source_px.0, p.px_per_source_px.1);
    theme::kv_row(ui, "Px per source px", &scale, text);
    theme::kv_row(
        ui,
        "Integer aligned",
        if p.integer_aligned { "yes" } else { "no" },
        if p.integer_aligned {
            PALETTE.ok
        } else {
            PALETTE.text_muted
        },
    );
    theme::kv_row(ui, "Density", &format!("{:.3} px/mm", p.px_per_mm), text);
    theme::kv_row(ui, "Scanline pitch", &format!("{:.3} px", p.line_px), text);
    theme::kv_row(ui, "Triad pitch", &format!("{:.3} px", p.px_per_triad), text);
    theme::kv_row(ui, "Mask, horizontal", &tier(p.tiers.horizontal), text);
    theme::kv_row(ui, "Mask, vertical", &tier(p.tiers.vertical), text);
    let fit = match p.fit {
        Fit::Fits => None,
        Fit::Clamped { requested, used } => Some(format!("x{requested} does not fit this output: x{used} used")),
        Fit::Downscaled => Some("The output is smaller than the native picture: downscaled, not integer".to_string()),
    };
    if let Some(f) = fit {
        ui.spacing();
        theme::callout(ui, PALETTE.accent, &[f]);
    }
}

/// The pinned footer: a rule, the opacity row and the close hint.
fn footer(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult) {
    let s = theme::scale_of(ui);
    theme::rule(ui);
    ui.dummy([0.0, 2.0 * s]);
    let width = theme::row_label(ui, "Opacity");
    let track = width - value_width(ui) - ui.clone_style().item_spacing()[0];
    theme::slider(ui, "##opacity", &mut m.layout.menu_opacity, 0.2, 1.0, track);
    r.rects.opacity = rect(ui);
    ui.same_line();
    theme::text_right(
        ui,
        PALETTE.text,
        &format!("{:.0} %", m.layout.menu_opacity.clamp(0.0, 1.0) * 100.0),
    );
    theme::with_font(ui, None, METRICS.font_small, || {
        ui.text_colored(PALETTE.text_faint, format!("{} or Esc closes the menu", m.key));
    });
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
