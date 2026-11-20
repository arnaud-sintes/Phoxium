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
//! The menu window (T-52a, T-53): title bar, movable, resizable, collapsible, an opacity slider. Body, top to
//! bottom: hotkey problems; the parameter search and the Advanced toggle; the preset tools (filter, selector
//! with a modified dot, revert, reload, save as a new user preset, update a user preset); "Output and headroom"
//! (the SDR headroom readout of T-84 and the output panel, open by default); the pipeline panels generated from
//! the parameter table ([`super::panels`], closed by default, each header with its count of changed
//! parameters; the source panel starts with the placement readout of T-96); performance (GPU milliseconds per
//! stage, T-21); GPU and system (adapter in use and the `[global.gpu] adapter` choice for the next start,
//! the hotkeys and their rebinding, `super::hotkeys_panel`, settings folder); Interface and About
//! (`super::system_panels`). The Source section ends with the saved per-game profiles (`super::profiles_panel`),
//! the preset tools open the preset files tools ("Manage files", `super::files_panel`). With a search, the body
//! lists only the matching parameters of every panel.
//! Position, size, opacity and the collapsed state persist in `settings.toml` (`[global.ui]`); the parameters,
//! the preset and the adapter go back to the app through [`MenuResult`] (`super::UiOutput`).
//!
//! Look (`super::theme`): a flat SemiBold title bar, collapsible sections with small-capital headers, rows with
//! the label on the left and the widget or value on the right (numbers right-aligned, tabular digits), sliders
//! without inner text and with an accent fill, a scrolling body above a pinned footer (opacity, close hint).

use std::path::PathBuf;

use dear_imgui_rs::{Condition, StyleVar, Ui as Frame, WindowFlags};

use crate::params::Params;
use crate::presets::{Library, Origin, Preset};
use crate::settings;

use super::panels;
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
    /// A window was chosen in the picker and attached (A23): the foreground goes to that window, the new
    /// source, not to the window that had it when the picker opened.
    Attached,
}

/// The window that gets the foreground when the menu mode closes for `reason`: [`restore_target`] of the
/// remembered window then the source (`Toggle`), the source only (`Attached`), none (`Deactivated`).
pub fn close_target(
    reason: CloseReason,
    remembered: Option<isize>,
    source: Option<isize>,
    output: isize,
    exists: impl Fn(isize) -> bool,
) -> Option<isize> {
    match reason {
        CloseReason::Toggle => restore_target(remembered, source, output, exists),
        CloseReason::Attached => restore_target(None, source, output, exists),
        CloseReason::Deactivated => None,
    }
}

/// What a `WM_ACTIVATE` of the output means for the menu: `activated` is false for `WA_INACTIVE`. The
/// menu closes, without restoring the foreground, when the output is deactivated while it is open.
pub fn on_activate(open: bool, activated: bool) -> Option<CloseReason> {
    (open && !activated).then_some(CloseReason::Deactivated)
}

/// Virtual-key code of Esc.
pub const VK_ESCAPE: usize = 0x1B;

/// Whether the key message `msg` (`wparam` the virtual key) closes the open menu: Esc pressed. The menu
/// consumes it. Esc does nothing else in the output window (it never quits; the Quit hotkey does). The other
/// hotkeys (F11, the quit chord) are `WM_HOTKEY` registrations and keep working with the menu open.
pub fn closes_on_key(open: bool, msg: u32, wparam: usize) -> bool {
    open && msg == super::input::WM_KEYDOWN && wparam == VK_ESCAPE
}

/// Whether a close for `reason` hands the foreground to a window ([`close_target`]).
pub fn restores_foreground(reason: CloseReason) -> bool {
    reason != CloseReason::Deactivated
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
        let target = close_target(reason, remembered, source, out, window_exists);
        if let Some(t) = target {
            set_foreground(t);
        }
        self.last_restore = target;
        target
    }
}

// ---------------------------------------------------------------------------------------------------------
// The menu window.

/// Title of the menu window (the `###` part is its ImGui id, stable when the title changes).
pub const TITLE: &str = "ultimateCRT###ucrt_menu";

/// Screen rectangles (min, max) of the menu widgets in the frame just built (tests, live check). Every other
/// widget is in [`MenuResult::items`] by key ([`super::Ui::item_rect`]).
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

/// The preset tools' state: the name typed for a new preset, the last message, the user preset folder.
#[derive(Clone, Debug, Default)]
pub struct PresetForm {
    pub name: String,
    /// The outcome of the last action: (success, text).
    pub message: Option<(bool, String)>,
    /// The user preset folder (`presets::user_dir`), `None` when there is no settings folder (no saving).
    pub dir: Option<PathBuf>,
}

/// One hotkey as the menu lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyRow {
    /// What it does (`hotkeys::Action::describe`).
    pub action: String,
    /// The chord registered, `None` when the action has no hotkey.
    pub bound: Option<String>,
    /// The chord asked for in the settings (differs from `bound` when a fallback is in force).
    pub wanted: String,
}

/// What the GPU and system panel shows and edits.
#[derive(Clone, Debug, Default)]
pub struct SystemModel {
    /// The session's adapter selection and list (`adapters::current`), `None` before the renderer runs.
    pub gpu: Option<crate::adapters::Current>,
    /// `[global.gpu] adapter` as it is in the settings now (changed by the combo).
    pub gpu_setting: String,
    /// The settings folder, shown for reference.
    pub settings_dir: Option<PathBuf>,
}

/// What the menu window edits in one frame.
pub struct MenuModel<'a> {
    pub state: &'a UiState,
    /// The parameters the menu shows and edits (a copy of `state.params`).
    pub params: &'a mut Params,
    /// Text filter of the preset list.
    pub filter: &'a mut String,
    /// Parameter search over every panel.
    pub search: &'a mut String,
    /// Show the parameters marked advanced.
    pub advanced: &'a mut bool,
    pub layout: &'a mut settings::UiState,
    pub library: &'a Library,
    pub presets: &'a mut PresetForm,
    pub system: &'a mut SystemModel,
    /// Menu key shown in the footer hint.
    pub key: &'a str,
    /// Hotkey registration problems, shown at the top (empty: nothing shown).
    pub hotkey_problems: &'a [String],
    /// Give the keyboard focus to the menu window in this frame (the frame after it opened).
    pub focus: bool,
    /// The theme's fonts ([`theme::load_fonts`]).
    pub fonts: Fonts,
    /// `[global.source]` ask at start and lost-source wait, edited in the Source section (A23).
    pub prefs: &'a mut super::SourcePrefs,
    /// The scaling multiplier is shown per axis (Advanced, "Separate X and Y").
    pub split_multiplier: &'a mut bool,
    /// Manual limits move in whole source pixels when a multiplier is known.
    pub snap_limits: &'a mut bool,
    /// The hotkeys and their rebinding (GPU and system panel).
    pub hotkeys: &'a mut super::hotkeys_panel::HotkeyModel,
    /// The "Preset files" section.
    pub files: &'a mut super::files_panel::FilesModel,
    /// "Saved profiles" in the Source section.
    pub profiles: &'a mut super::profiles_panel::ProfilesModel,
    /// The Interface and About sections.
    pub extras: &'a mut super::system_panels::ExtrasModel,
}

/// What the menu window did in one frame.
#[derive(Debug, Default)]
pub struct MenuResult {
    /// A parameter changed (through a widget, a reset, a revert or a preset).
    pub params_changed: bool,
    /// The preset selected in this frame (its id); `params` already holds its parameters.
    pub preset: Option<String>,
    /// The preset library changed (a preset saved, the files reloaded).
    pub library: Option<Library>,
    /// The `[global.gpu] adapter` value chosen in this frame (applied at the next start).
    pub gpu_adapter: Option<String>,
    /// The capture path chosen in this frame (Source section, A29).
    pub capture_path: Option<crate::zerocopy::CapturePath>,
    /// A burn-in action asked in this frame (T-43): "Burn in now" or "New tube".
    pub wear_action: Option<super::WearAction>,
    /// A request of the Source section (A23): Change source, Detach, the picture area (mode, Detect now, limits,
    /// Reset, Undo), a scaling multiplier.
    pub source: Option<super::SourceAction>,
    /// The ask-at-start box, the lost-source wait or a black-bar detection preference changed (A23).
    pub prefs_changed: bool,
    /// The layout (position, size, opacity, collapsed) changed.
    pub layout_changed: bool,
    /// A hotkey change to register (Hotkeys rows).
    pub hotkey: Option<super::hotkeys_panel::HotkeyEdit>,
    /// A request of the "Saved profiles" list.
    pub profile: Option<crate::profiles::ProfileAction>,
    /// A confirmed reset (Interface section).
    pub reset: Option<super::system_panels::ResetKind>,
    /// A native dialog to open (Preset files).
    pub dialog: Option<super::files_panel::DialogRequest>,
    /// The UI scale chosen (Interface section).
    pub scale: Option<crate::settings::UiScale>,
    /// Click-through switched (Interface section).
    pub click_through: Option<bool>,
    /// A text to put on the clipboard (About: the settings folder).
    pub copy: Option<String>,
    pub rects: MenuRects,
    /// Widget rectangles by key: `label:<id>`, `widget:<id>` (`widget:<id>.<k>` for a vector component),
    /// `header:<panel>`, `reset:<panel>`, `search`, `advanced`, `revert`, `reload`, `preset_name`,
    /// `save_preset`, `update_preset`, `gpu_adapter`.
    pub items: Vec<(String, [[f32; 2]; 2])>,
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

/// The burn-in actions under the ageing panel (T-43): wear the current picture for `burnin_now_hours` at once,
/// or start a new tube (the wear map cleared and its file replaced). Both act on the map of the preset in force
/// and only while burn-in is on (the app ignores them otherwise).
fn wear_buttons(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult) {
    let on = m.params.burnin_enable;
    let disabled = ui.begin_disabled_with_cond(!on);
    let hours = m.params.burnin_now_hours;
    if theme::button(ui, &format!("Burn in {hours:.0} h now")) {
        r.wear_action = Some(super::WearAction::BurnInNow(hours));
    }
    record(ui, r, "burn_in_now");
    ui.same_line();
    if theme::button(ui, "New tube (reset wear)") {
        r.wear_action = Some(super::WearAction::NewTube);
    }
    record(ui, r, "new_tube");
    drop(disabled);
    if !on {
        theme::kv_row(ui, "Burn-in", "off: turn it on to use these", PALETTE.text_faint);
    }
}

fn rect(ui: &Frame) -> [[f32; 2]; 2] {
    [ui.item_rect_min(), ui.item_rect_max()]
}

/// Records the rectangle of the last item under `key` ([`MenuResult::items`]).
pub(super) fn record(ui: &Frame, r: &mut MenuResult, key: &str) {
    r.items.push((key.to_string(), rect(ui)));
}

/// The scrolling body: hotkey problems, the parameter search, then (without a search) the preset tools, the
/// output and headroom panel, the pipeline panels, performance, GPU and system.
fn body(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult) {
    let fonts = m.fonts;
    // Hotkey registration problems (T-54): a chord another program owns, the fallback in force.
    if !m.hotkey_problems.is_empty() {
        theme::callout(ui, PALETTE.problem, m.hotkey_problems);
        ui.spacing();
    }
    search_row(ui, m, r);
    let reference = panels::reference(m.state.baseline.as_ref(), m.library, m.state.preset.as_deref());
    if !m.search.trim().is_empty() {
        search_results(ui, m, r, &reference);
        return;
    }

    source_section(ui, m, r);
    preset_section(ui, m, r, &reference);

    // Output and headroom (T-84): the readout, the mode in effect, then the output panel (open by default).
    let changed = panels::modified_count(m.params, &reference, panels::OUTPUT);
    let note = (changed > 0).then(|| format!("{changed} changed"));
    let (open, header) = theme::section_header(ui, &fonts, panels::OUTPUT, note.as_deref(), false, true);
    r.items.push((format!("header:{}", panels::OUTPUT), header));
    if open {
        headroom_readout(ui, m);
        r.params_changed |= panel(ui, m, r, &reference, panels::OUTPUT);
        r.rects.headroom_mode = item(r, "widget:output_headroom_mode");
        r.rects.exposure = item(r, "widget:exposure");
    }

    // The pipeline panels, closed by default.
    for def in panels::PANELS {
        if def.name == panels::OUTPUT || (def.name == panels::OTHER && panels::panel_params(def.name).is_empty()) {
            continue;
        }
        let changed = panels::modified_count(m.params, &reference, def.name);
        let note = (changed > 0).then(|| format!("{changed} changed"));
        let (open, header) = theme::section_header(ui, &fonts, def.name, note.as_deref(), false, false);
        r.items.push((format!("header:{}", def.name), header));
        if def.name == panels::SOURCE {
            r.rects.source_header = header;
        }
        if !open {
            continue;
        }
        if def.name == panels::SOURCE {
            theme::subheading(ui, &fonts, "Placement");
            match &m.state.placement {
                Some(p) => placement_rows(ui, p),
                None => theme::kv_row(ui, "Source", "no frame yet", PALETTE.text_faint),
            }
        }
        r.params_changed |= panel(ui, m, r, &reference, def.name);
        if def.name == panels::AGEING {
            wear_buttons(ui, m, r);
        }
        ui.dummy([0.0, METRICS.section_gap * theme::scale_of(ui)]);
    }

    // Performance (T-21): fps and the GPU milliseconds per stage.
    let (open, header) = theme::section_closed(ui, &fonts, "Performance and diagnostics");
    r.rects.performance_header = header;
    r.items.push(("header:Performance and diagnostics".to_string(), header));
    if open {
        capture_status_row(ui, m, r);
        performance_rows(ui, m);
    }

    // GPU and system: the adapter (applied at the next start), the hotkeys, the settings folder.
    let (open, header) = theme::section_closed(ui, &fonts, "GPU and system");
    r.items.push(("header:GPU and system".to_string(), header));
    if open {
        system_rows(ui, m, r);
    }

    // Interface (UI scale, resets) and About.
    super::system_panels::interface(ui, &fonts, m.layout.scale, m.state.click_through, m.extras, r);
    super::system_panels::about_section(ui, &fonts, m.extras, m.system, r);
}

/// The rectangle recorded under `key` in this frame (zero when absent).
fn item(r: &MenuResult, key: &str) -> [[f32; 2]; 2] {
    r.items
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map_or([[0.0; 2]; 2], |(_, rect)| *rect)
}

/// The rows of one panel ([`panels::panel_body`]).
fn panel(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult, reference: &Params, name: &'static str) -> bool {
    let mut ctx = panels::RowCtx {
        params: m.params,
        reference,
        items: &mut r.items,
    };
    panels::panel_body(ui, &m.fonts, &mut ctx, name, *m.advanced, "")
}

/// The parameter search field and the Advanced toggle, on one row.
fn search_row(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult) {
    let style = ui.clone_style();
    let check_w = ui.frame_height() + style.item_inner_spacing()[0] + ui.calc_text_size("Advanced")[0];
    let avail = ui.content_region_avail()[0];
    ui.set_next_item_width((avail - check_w - style.item_spacing()[0]).max(60.0));
    ui.input_text("##search", m.search).hint("Search parameters").build();
    record(ui, r, "search");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Finds parameters by name, id or group in every panel, advanced ones included. Every word must \
             match.",
        );
    }
    ui.same_line();
    ui.checkbox("Advanced", m.advanced);
    record(ui, r, "advanced");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Show the advanced parameters (fine tuning, model internals) in every panel.",
        );
    }
}

/// With a search: the matching parameters of every panel, under the panel names.
fn search_results(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult, reference: &Params) {
    let fonts = m.fonts;
    let query = m.search.clone();
    let mut found = 0;
    let mut order: Vec<&'static str> = vec![panels::OUTPUT];
    order.extend(panels::PANELS.iter().map(|p| p.name).filter(|n| *n != panels::OUTPUT));
    for name in order {
        let rows: Vec<_> = panels::panel_params(name)
            .into_iter()
            .filter(|p| panels::matches(p, &query))
            .collect();
        if rows.is_empty() {
            continue;
        }
        theme::subheading(ui, &fonts, name);
        let mut ctx = panels::RowCtx {
            params: m.params,
            reference,
            items: &mut r.items,
        };
        for p in rows {
            let _id = ui.push_id(p.id);
            r.params_changed |= panels::param_row(ui, &mut ctx, p);
            found += 1;
        }
    }
    if found == 0 {
        ui.dummy([0.0, 4.0 * theme::scale_of(ui)]);
        ui.text_colored(
            PALETTE.text_faint,
            format!("No parameter matches \u{201C}{}\u{201D}.", query.trim()),
        );
    }
}

/// Preset: filter and selector (selecting applies the preset), revert and reload, save as a new user preset or
/// update the user preset in force, and the outcome of the last action.
fn preset_section(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult, reference: &Params) {
    let fonts = m.fonts;
    let s = theme::scale_of(ui);
    let list = filtered_presets(m.library, m.filter);
    let count = format!("{} of {}", list.len(), m.library.list().len());
    // Right under the closed Source header: the gap of a first section.
    if !theme::section(ui, &fonts, "Preset", Some(&count), true) {
        return;
    }
    ui.set_next_item_width(-f32::MIN_POSITIVE);
    ui.input_text("##filter", m.filter).hint("Filter presets").build();
    r.rects.filter = rect(ui);
    let current = m.state.preset.as_deref();
    let current_preset = current.and_then(|id| m.library.find(id));
    let modified = m.params != reference;
    let preview = current_preset.map_or("Custom".to_string(), |p| p.name.clone());
    // The combo leaves room on its left for the modified dot (compare to the saved preset).
    let gutter = 10.0 * s;
    let start = ui.cursor_screen_pos();
    ui.set_cursor_pos_x(ui.cursor_pos()[0] + gutter);
    ui.set_next_item_width(-f32::MIN_POSITIVE);
    if let Some(_combo) = ui.begin_combo("##preset", &preview) {
        for (id, name) in &list {
            let selected = current == Some(id.as_str());
            let user = m.library.find(id).is_some_and(|p| matches!(p.origin, Origin::User(_)));
            let text = if user {
                format!("{name}  \u{00B7}  user##{id}")
            } else {
                format!("{name}##{id}")
            };
            if ui.selectable_config(text).selected(selected).build() {
                if let Some(p) = m.library.find(id) {
                    let old = *m.params;
                    *m.params = p.params();
                    // T-48: the quality tier stays (it describes the machine, not the tube).
                    crate::quality::keep(m.params, &old);
                    r.preset = Some(id.clone());
                    r.params_changed = true;
                    m.presets.message = None;
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
    if modified {
        let y = (r.rects.preset[0][1] + r.rects.preset[1][1]) * 0.5;
        ui.get_window_draw_list()
            .add_circle([start[0] + gutter * 0.5, y], 3.0 * s, PALETTE.accent)
            .filled(true)
            .build();
    }
    if ui.is_item_hovered() && modified {
        theme::tooltip(
            ui,
            "Changed since the preset was applied (the dot). Revert restores the preset.",
        );
    }

    // Revert and reload on one row, the change count on the right.
    let disabled = ui.begin_disabled_with_cond(!modified);
    if theme::button(ui, "Revert") {
        *m.params = *reference;
        r.params_changed = true;
        m.presets.message = Some((true, "Reverted to the preset values.".to_string()));
    }
    drop(disabled);
    record(ui, r, "revert");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Set every parameter back to the preset in force (else the table defaults).",
        );
    }
    ui.same_line();
    if theme::button(ui, "Reload") {
        let lib = Library::load(m.presets.dir.as_deref());
        let n = lib.list().len();
        let problems = lib.warnings.len();
        m.presets.message = Some(if problems == 0 {
            (true, format!("Reloaded {n} presets."))
        } else {
            (
                false,
                format!(
                    "Reloaded {n} presets; {problems} problem(s): {}",
                    lib.warnings.join("; ")
                ),
            )
        });
        r.library = Some(lib);
    }
    record(ui, r, "reload");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Read the preset files again (the shipped ones and the user folder).",
        );
    }
    // The preset files tools (rename, delete, duplicate, export, import) open below the preset tools.
    ui.same_line();
    if theme::button(ui, if m.files.open { "Hide files" } else { "Manage files" }) {
        m.files.open = !m.files.open;
    }
    record(ui, r, "files:toggle");
    if ui.is_item_hovered() {
        theme::tooltip(ui, "Rename, delete, duplicate, export and import preset files.");
    }
    let changed = panels::PANELS
        .iter()
        .map(|p| panels::modified_count(m.params, reference, p.name))
        .sum::<usize>();
    if changed > 0 {
        ui.same_line();
        theme::with_font(ui, None, METRICS.font_small, || {
            let text = format!("{changed} changed");
            ui.align_text_to_frame_padding();
            theme::text_right(ui, PALETTE.accent, &text);
        });
    }

    // Save as a new user preset, or update the user preset in force.
    let save_w = ui.calc_text_size("Save as new")[0] + 20.0 * s;
    let avail = ui.content_region_avail()[0];
    ui.set_next_item_width((avail - save_w - ui.clone_style().item_spacing()[0]).max(60.0));
    ui.input_text("##preset_name", &mut m.presets.name)
        .hint("New preset name")
        .build();
    record(ui, r, "preset_name");
    ui.same_line();
    let can_save = m.presets.dir.is_some();
    let disabled = ui.begin_disabled_with_cond(!can_save);
    if theme::button(ui, "Save as new") {
        save_new_preset(m, r);
    }
    drop(disabled);
    record(ui, r, "save_preset");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            if can_save {
                "Save the current parameters as a user preset (the shipped presets are never changed)."
            } else {
                "No settings folder: user presets cannot be saved."
            },
        );
    }
    if let (Some(p), Some(_)) = (current_preset, &m.presets.dir) {
        if matches!(p.origin, Origin::User(_)) {
            let disabled = ui.begin_disabled_with_cond(!modified);
            if theme::button(ui, &format!("Update \u{201C}{}\u{201D}", p.name)) {
                update_preset(m, r, p.id.clone(), p.name.clone());
            }
            drop(disabled);
            record(ui, r, "update_preset");
        }
    }
    if let Some((ok, text)) = &m.presets.message {
        theme::with_font(ui, None, METRICS.font_small, || {
            let wrap = ui.push_text_wrap_pos(0.0);
            ui.text_colored(if *ok { PALETTE.ok } else { PALETTE.problem }, text);
            drop(wrap);
        });
    }
    if m.files.open {
        super::files_panel::section(
            ui,
            &fonts,
            m.files,
            m.library,
            m.presets.dir.as_deref(),
            m.state.preset.as_deref(),
            r,
        );
    }
}

/// The lost-source wait as the menu shows it: `30 s`, `forever` for 0.
pub fn wait_text(wait_s: f64) -> String {
    if wait_s <= 0.0 {
        "forever".to_string()
    } else {
        format!("{wait_s:.0} s")
    }
}

/// Source (A23): the window attached and what the automatic handling found (native size, integer scale, crop),
/// Change source and Detach, the ask-at-start box and the lost-source wait. The manual overrides (scale mode,
/// crop and divisor modes, centring, surround, aspect) are the "Source and placement" panel's parameters.
fn source_section(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult) {
    let fonts = m.fonts;
    let s = theme::scale_of(ui);
    let src = &m.state.source;
    // The header names the program; the section opens on a click (closed by default, so the presets and the
    // output panel stay where they were). The window list is the picker's.
    let note = match (&src.attached, &src.waiting) {
        (_, Some(_)) => "waiting".to_string(),
        (Some(a), None) => a.exe.clone().unwrap_or_else(|| "attached".to_string()),
        (None, None) => "none".to_string(),
    };
    let (open, header) = theme::section_header(ui, &fonts, "Source", Some(&note), true, false);
    r.items.push(("header:Source".to_string(), header));
    if !open {
        return;
    }
    let width = ui.content_region_avail()[0];
    match &src.attached {
        Some(a) => {
            let program = a.exe.as_deref().unwrap_or("unknown program");
            let head = ellipsize_text(ui, &format!("{program}  \u{00B7}  {}", a.title), width);
            ui.text_colored(PALETTE.text, head);
            theme::with_font(ui, None, METRICS.font_small, || {
                let wrap = ui.push_text_wrap_pos(0.0);
                ui.text_colored(
                    PALETTE.text_muted,
                    format!(
                        "native {} \u{00D7} {}  \u{00B7}  scale {}  \u{00B7}  {}  \u{00B7}  captured {} \u{00D7} {}{}",
                        a.native.0,
                        a.native.1,
                        a.scale,
                        a.crop,
                        a.frame.0,
                        a.frame.1,
                        if a.profile { "  \u{00B7}  source profile" } else { "" }
                    ),
                );
                drop(wrap);
            });
            record(ui, r, "source:detected");
        }
        None => ui.text_colored(PALETTE.text_faint, "No window is captured."),
    }
    if let Some(w) = &src.waiting {
        theme::callout(ui, PALETTE.accent, std::slice::from_ref(w));
    }
    ui.dummy([0.0, 2.0 * s]);
    if theme::button(ui, "Change source") {
        r.source = Some(super::SourceAction::ChangeSource);
    }
    record(ui, r, "source:change");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Choose another window to capture. Esc in the list keeps the current one.",
        );
    }
    if src.attached.is_some() || src.waiting.is_some() {
        ui.same_line();
        if theme::button(ui, "Detach") {
            r.source = Some(super::SourceAction::Detach);
        }
        record(ui, r, "source:detach");
        if ui.is_item_hovered() {
            theme::tooltip(ui, "Stop capturing: the window list comes back.");
        }
    }
    // The capture path (A29), next to the attached program.
    ui.dummy([0.0, 2.0 * s]);
    capture_choice(ui, m, r);
    if let Some(a) = src.attached.clone() {
        picture_area_rows(ui, m, r, &a);
        multiplier_rows(ui, m, r, &a);
        theme::subheading(ui, &fonts, "Window choice");
    }
    if ui.checkbox("Ask at every start", &mut m.prefs.ask_at_start) {
        r.prefs_changed = true;
    }
    record(ui, r, "source:ask");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "On: the window list opens at every start. Off: the program chosen last is captured again at start \
             when it is running.",
        );
    }
    let label_w = theme::row_label(ui, "Wait for a closed window");
    let value = wait_text(m.prefs.wait_s);
    let track = label_w - ui.calc_text_size("forever")[0] - ui.clone_style().item_spacing()[0];
    let mut wait = m.prefs.wait_s as f32;
    if theme::slider_ex(ui, "##source_wait", &mut wait, 0.0, 300.0, track, false, "%.0f") {
        m.prefs.wait_s = f64::from(wait.round());
        r.prefs_changed = true;
    }
    record(ui, r, "source:wait");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "When the captured window closes, a new window of the same program and class is attached by itself \
             during this wait; then the window list comes back. 0 waits forever.",
        );
    }
    ui.same_line();
    theme::text_right(ui, PALETTE.text, &value);
    let attached = m.state.source.attached.as_ref().and_then(|a| a.exe.clone());
    let profiles = m.state.profiles.clone();
    super::profiles_panel::rows(ui, &fonts, m.profiles, &profiles, attached.as_deref(), r);
}

/// A labelled slider row of a source preference (the lost-source wait style): the value text on the right.
/// True when the value changed.
#[allow(clippy::too_many_arguments)]
fn pref_slider(
    ui: &Frame,
    r: &mut MenuResult,
    key: &str,
    label: &str,
    value: &mut f32,
    lo: f32,
    hi: f32,
    text: &str,
    widest: &str,
    tip: &str,
) -> bool {
    let label_w = theme::row_label(ui, label);
    let track = label_w - ui.calc_text_size(widest)[0] - ui.clone_style().item_spacing()[0];
    let changed = theme::slider_ex(ui, &format!("##{key}"), value, lo, hi, track, false, "%.0f");
    record(ui, r, key);
    if ui.is_item_hovered() {
        theme::tooltip(ui, tip);
    }
    ui.same_line();
    theme::text_right(ui, PALETTE.text, text);
    changed
}

/// The black bars of a detection in words: `left 0 · top 32 · right 0 · bottom 32 px`.
pub fn bars_text(b: &crate::bars::Bars) -> String {
    format!(
        "left {}  \u{00B7}  top {}  \u{00B7}  right {}  \u{00B7}  bottom {} px",
        b.left, b.top, b.right, b.bottom
    )
}

/// The picture inside its limits in words: `256 × 224 at 329, 265 of 914 × 754`.
pub fn picture_text(limits: &crate::bars::Bars, client: (u32, u32)) -> String {
    match limits.picture(client) {
        Some(p) => format!(
            "{} \u{00D7} {} at {}, {} in {} \u{00D7} {}",
            p.w, p.h, p.x, p.y, client.0, client.1
        ),
        None => format!("nothing left of {} \u{00D7} {}", client.0, client.1),
    }
}

/// Places the next button of a row of [`theme::button`]s on the same line when `label` still fits in the content
/// width, else on the next line (a narrow menu wraps the row instead of clipping it).
pub(super) fn same_line_if_fits(ui: &Frame, label: &str) {
    let s = theme::scale_of(ui);
    let spacing = ui.clone_style().item_spacing()[0];
    let width = ui.calc_text_size(label.split("##").next().unwrap_or(label))[0] + 20.0 * s;
    let right = ui.window_pos()[0] + ui.window_size()[0] - ui.clone_style().window_padding()[0];
    if ui.item_rect_max()[0] + spacing + width <= right - 12.0 * s {
        ui.same_line();
    }
}

/// One manual limit row: the label, a `-` nudge, the slider (drag, Ctrl+click to type), a `+` nudge and the value.
/// Returns the new value asked for (not yet clamped or snapped).
fn limit_row(ui: &Frame, r: &mut MenuResult, side: crate::bars::Side, value: u32, max: u32, step: u32) -> Option<i64> {
    let key = format!("source:limit_{}", side.name().to_lowercase());
    let width = theme::row_label(ui, side.name());
    let spacing = ui.clone_style().item_spacing()[0];
    let button_w = ui.calc_text_size("+")[0] + 2.0 * 8.0 * theme::scale_of(ui);
    let value_w = ui.calc_text_size("0000 px")[0];
    let track = (width - 2.0 * button_w - value_w - 3.0 * spacing).max(40.0);
    let mut asked = None;
    let small = |label: &str| {
        let s = theme::scale_of(ui);
        let _p = ui.push_style_var(StyleVar::FramePadding([8.0 * s, 4.0 * s]));
        ui.button(label)
    };
    if small(&format!("-##{key}")) {
        asked = Some(i64::from(value) - i64::from(step));
    }
    record(ui, r, &format!("{key}:minus"));
    ui.same_line();
    let mut v = value.min(max) as i32;
    if theme::slider_ex(ui, &format!("##{key}"), &mut v, 0, max as i32, track, false, "%d") {
        asked = Some(i64::from(v));
    }
    record(ui, r, &key);
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            &format!(
                "Pixels cropped from the {} of the captured window. Drag, or Ctrl+click to type a value; - and + \
                 move one step. Changing it switches the picture area to Manual.",
                side.name().to_lowercase()
            ),
        );
    }
    ui.same_line();
    if small(&format!("+##{key}")) {
        asked = Some(i64::from(value) + i64::from(step));
    }
    record(ui, r, &format!("{key}:plus"));
    ui.same_line();
    theme::text_right(ui, PALETTE.text, &format!("{value} px"));
    asked
}

/// Picture area: the mode (Auto, Manual, Whole client area), what is in force, Detect now, Back to Auto, Reset and
/// Undo, the four manual limits with their snapping, the last message and (Advanced) the detection thresholds.
fn picture_area_rows(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult, a: &super::AttachedView) {
    use crate::bars::{edit_limit, AreaMode, Side};
    let fonts = m.fonts;
    theme::subheading(ui, &fonts, "Picture area");
    let area = &a.area;
    let running = a.bars.running;
    // The mode.
    let width = theme::row_label(ui, "Picture area");
    let disabled = ui.begin_disabled_with_cond(area.whole_window);
    ui.set_next_item_width(width);
    let preview = if area.whole_window {
        "Whole window".to_string()
    } else if area.mode == AreaMode::Manual {
        // A manual choice is stored for this program and comes back at every attach: say so.
        "Manual (stored)".to_string()
    } else {
        area.mode.label().to_string()
    };
    if let Some(_combo) = ui.begin_combo("##source_area", &preview) {
        for (i, mode) in AreaMode::ALL.iter().enumerate() {
            let selected = *mode == area.mode;
            if ui
                .selectable_config(format!("{}##source_area{i}", mode.label()))
                .selected(selected)
                .build()
                && !selected
            {
                r.source = Some(super::SourceAction::SetAreaMode(*mode));
            }
            record(ui, r, &format!("source:area:{i}"));
            if selected {
                ui.set_item_default_focus();
            }
        }
    }
    drop(disabled);
    record(ui, r, "source:area");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "The part of the captured window that is the game's picture. Auto finds it inside the black bars \
             (at attach, after a resize, and by itself while the game runs); Manual keeps your limits; Whole \
             client area crops nothing. Stored for this program.",
        );
    }
    let state = if area.whole_window {
        "the whole window is captured (crop mode none in Source and placement)".to_string()
    } else if running {
        "detecting...".to_string()
    } else {
        let what = match area.mode {
            AreaMode::Auto if area.limits == crate::bars::Bars::default() => "detected: no bars",
            AreaMode::Auto => "detected",
            AreaMode::Manual if area.set_for.is_some() => "stored limits not applied, detected",
            AreaMode::Manual => "manual",
            AreaMode::Client => "no crop",
        };
        format!("{what}  \u{00B7}  {}", picture_text(&area.limits, area.client))
    };
    theme::with_font(ui, None, METRICS.font_small, || {
        ui.set_cursor_pos_x(ui.cursor_pos()[0] + theme::label_width(ui));
        ui.text_colored(PALETTE.text_muted, &state);
    });
    record(ui, r, "source:picture");
    if area.whole_window {
        return;
    }
    // The actions.
    ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
    let disabled = ui.begin_disabled_with_cond(running);
    if theme::button(ui, if running { "Detecting..." } else { "Detect now" }) {
        r.source = Some(super::SourceAction::DetectNow);
    }
    drop(disabled);
    record(ui, r, "source:detect_now");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Find the picture inside the black bars now (about one second) and follow it in Auto. Run it while \
             the game shows a picture.",
        );
    }
    if area.mode != AreaMode::Auto {
        same_line_if_fits(ui, "Back to Auto");
        if theme::button(ui, "Back to Auto") {
            r.source = Some(super::SourceAction::SetAreaMode(AreaMode::Auto));
        }
        record(ui, r, "source:back_to_auto");
        if ui.is_item_hovered() {
            theme::tooltip(ui, "Let the app find and follow the picture area again.");
        }
    }
    same_line_if_fits(ui, "Reset (whole client area)");
    let disabled = ui.begin_disabled_with_cond(area.mode == AreaMode::Client || running);
    if theme::button(ui, "Reset (whole client area)") {
        r.source = Some(super::SourceAction::ResetCrop);
    }
    drop(disabled);
    record(ui, r, "source:reset_crop");
    if ui.is_item_hovered() {
        theme::tooltip(ui, "Capture the whole client area of the window (no crop).");
    }
    same_line_if_fits(ui, "Undo");
    let disabled = ui.begin_disabled_with_cond(!a.bars.can_undo || running);
    if theme::button(ui, "Undo") {
        r.source = Some(super::SourceAction::UndoCrop);
    }
    drop(disabled);
    record(ui, r, "source:undo_crop");
    if ui.is_item_hovered() {
        theme::tooltip(ui, "Put back the picture area as it was before your last change.");
    }
    if let (AreaMode::Manual, Some(was)) = (area.mode, area.set_for) {
        theme::callout(
            ui,
            PALETTE.accent,
            &[format!(
                "The stored manual limits were set for a {} \u{00D7} {} window; it is {} \u{00D7} {} now, so they \
                 are not applied: the picture area is detected as in Auto. Back to Auto keeps it that way; changing \
                 a limit stores new manual limits for this size.",
                was.0, was.1, area.client.0, area.client.1
            )],
        );
        record(ui, r, "source:area_warning");
    }
    // The four limits: what is in force, editable (an edit switches to Manual from these values).
    let snap = if *m.snap_limits { area.divisor } else { (1, 1) };
    for side in Side::ALL {
        let (size, step) = if side.horizontal() {
            (area.client.0, snap.0)
        } else {
            (area.client.1, snap.1)
        };
        let value = area.limits.get(side);
        if let Some(asked) = limit_row(ui, r, side, value, size, step.max(1)) {
            let new = edit_limit(area.limits, side, asked, area.client, snap);
            if new != area.limits || area.mode != AreaMode::Manual {
                r.source = Some(super::SourceAction::SetLimits(new));
            }
        }
    }
    let known = area.divisor != (1, 1);
    let disabled = ui.begin_disabled_with_cond(!known);
    let label = if known {
        format!(
            "Snap to source pixels ({})",
            crate::scaling::Multiplier::pair_text(area.divisor)
        )
    } else {
        "Snap to source pixels".to_string()
    };
    ui.checkbox(format!("{label}###source_snap"), m.snap_limits);
    drop(disabled);
    record(ui, r, "source:snap");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "With a scaling multiplier known, the limits move in whole source pixels (x3: 3 px at a time), so the \
             picture keeps whole game pixels.",
        );
    }
    if let Some((ok, text)) = &a.bars.message {
        theme::with_font(ui, None, METRICS.font_small, || {
            let wrap = ui.push_text_wrap_pos(0.0);
            ui.text_colored(if *ok { PALETTE.ok } else { PALETTE.problem }, text);
            drop(wrap);
        });
        record(ui, r, "source:bars_message");
    }
    if *m.advanced {
        let b = &mut m.prefs.bars;
        let mut tol = f32::from(b.tolerance);
        if pref_slider(
            ui,
            r,
            "source:bars_tolerance",
            "Black tolerance",
            &mut tol,
            0.0,
            48.0,
            &format!("{} / 255", b.tolerance),
            "48 / 255",
            "Largest code value (brightest of red, green, blue) still counted as black in a bar. Raise it for \
             noisy or compressed bars, lower it for a game whose dark picture edges are taken for bars.",
        ) {
            b.tolerance = tol.round().clamp(0.0, 255.0) as u8;
            r.prefs_changed = true;
        }
        let mut min = b.min_bar as f32;
        if pref_slider(
            ui,
            r,
            "source:bars_min",
            "Minimum bar",
            &mut min,
            1.0,
            32.0,
            &format!("{} px", b.min_bar),
            "48 / 255",
            "Thinner dark borders are ignored (a 1 px window border is not a bar).",
        ) {
            b.min_bar = (min.round() as u32).max(1);
            r.prefs_changed = true;
        }
        let mut pct = (b.max_crop * 100.0) as f32;
        let (lo, hi) = crate::bars::MAX_CROP_RANGE;
        if pref_slider(
            ui,
            r,
            "source:bars_max_crop",
            "Largest crop",
            &mut pct,
            (lo * 100.0) as f32,
            (hi * 100.0) as f32,
            &format!("{:.0} %", b.max_crop * 100.0),
            "48 / 255",
            "The bars of one axis may remove at most this share of it (default 97 %: a small game in a large \
             window is fine). The picture left is always at least 16 game pixels and 2 % of the window.",
        ) {
            b.max_crop = f64::from(pct.round()) / 100.0;
            r.prefs_changed = true;
        }
    }
}

/// One multiplier combo: `items` as (text, value), the current one highlighted. Every entry is recorded as
/// `<key>:<index>` while the list is open. Returns the value picked.
fn multiplier_combo(
    ui: &Frame,
    r: &mut MenuResult,
    key: &str,
    preview: &str,
    items: &[(String, crate::scaling::Multiplier)],
    current: crate::scaling::Multiplier,
    width: f32,
) -> Option<crate::scaling::Multiplier> {
    let mut picked = None;
    ui.set_next_item_width(width);
    if let Some(_combo) = ui.begin_combo(format!("##{key}"), preview) {
        for (i, (text, value)) in items.iter().enumerate() {
            let selected = *value == current;
            if ui
                .selectable_config(format!("{text}##{key}{i}"))
                .selected(selected)
                .build()
            {
                picked = Some(*value);
            }
            record(ui, r, &format!("{key}:{i}"));
            if selected {
                ui.set_item_default_focus();
            }
        }
    }
    record(ui, r, key);
    picked
}

/// Source scaling multiplier: Auto (the detected factor), None (the capture is native) or x1 to x10, with the
/// detected factor next to it and a hint when a forced value disagrees; (Advanced) X and Y separately.
fn multiplier_rows(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult, a: &super::AttachedView) {
    use crate::scaling::{Multiplier, MAX_MULTIPLIER};
    let fonts = m.fonts;
    theme::subheading(ui, &fonts, "Scaling");
    let current = a.multiplier;
    let detected_text = a.detected.map_or("detecting...".to_string(), Multiplier::pair_text);
    let preview = match current {
        Multiplier::Auto => format!("Auto ({detected_text})"),
        Multiplier::None => "None (native, x1)".to_string(),
        Multiplier::Manual(x, y) => Multiplier::pair_text((x, y)),
    };
    let split = *m.split_multiplier || !current.linked();
    let mut items = vec![
        (format!("Auto ({detected_text})"), Multiplier::Auto),
        ("None (native, x1)".to_string(), Multiplier::None),
    ];
    items.extend((1..=MAX_MULTIPLIER).map(|k| (format!("x{k}"), Multiplier::Manual(k, k))));
    let width = theme::row_label(ui, "Scaling multiplier");
    let picked = multiplier_combo(ui, r, "source:multiplier", &preview, &items, current, width);
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "The integer factor the game is enlarged by in its window (a 256 \u{00D7} 224 game shown at x3). \
             Auto finds it from the picture; None takes the capture as the game's own pixels; x1 to x10 forces \
             it. Stored for this program.",
        );
    }
    if let Some(v) = picked {
        r.source = Some(super::SourceAction::SetMultiplier(v));
    }
    // The detection always shows; a forced value that disagrees with it gets a hint.
    let note = match current.disagreement(a.detected) {
        Some(h) => (PALETTE.accent, h),
        None => (PALETTE.text_muted, format!("detected: {detected_text}")),
    };
    theme::with_font(ui, None, METRICS.font_small, || {
        ui.set_cursor_pos_x(ui.cursor_pos()[0] + theme::label_width(ui));
        ui.text_colored(note.0, &note.1);
    });
    record(ui, r, "source:multiplier_note");
    if *m.advanced {
        if ui.checkbox("Separate X and Y", m.split_multiplier) && !*m.split_multiplier && !current.linked() {
            // Linking again: both axes take the horizontal factor.
            if let Multiplier::Manual(x, _) = current {
                r.source = Some(super::SourceAction::SetMultiplier(Multiplier::manual(x, x)));
            }
        }
        record(ui, r, "source:split");
        if ui.is_item_hovered() {
            theme::tooltip(
                ui,
                "Non-square source pixels: a different factor across and down (for example x2 across, x3 down).",
            );
        }
    }
    if split && (*m.advanced || !current.linked()) {
        let base = current.divisor().or(a.detected).unwrap_or((1, 1));
        let axis_items = |axis: usize| -> Vec<(String, Multiplier)> {
            (1..=MAX_MULTIPLIER)
                .map(|k| {
                    let v = if axis == 0 {
                        Multiplier::manual(k, base.1)
                    } else {
                        Multiplier::manual(base.0, k)
                    };
                    (format!("{} x{k}", if axis == 0 { "X" } else { "Y" }), v)
                })
                .collect()
        };
        let half = (theme::row_label(ui, "Across, down") - ui.clone_style().item_spacing()[0]) * 0.5;
        let shown = |d: u32, axis: &str| format!("{axis} x{d}");
        let cur = if matches!(current, Multiplier::Manual(..)) {
            current
        } else {
            Multiplier::Auto
        };
        if let Some(v) = multiplier_combo(
            ui,
            r,
            "source:multiplier_x",
            &shown(base.0, "X"),
            &axis_items(0),
            cur,
            half,
        ) {
            r.source = Some(super::SourceAction::SetMultiplier(v));
        }
        ui.same_line();
        if let Some(v) = multiplier_combo(
            ui,
            r,
            "source:multiplier_y",
            &shown(base.1, "Y"),
            &axis_items(1),
            cur,
            half,
        ) {
            r.source = Some(super::SourceAction::SetMultiplier(v));
        }
    }
}

/// `text` cut with an ellipsis to fit `width` px in the current font.
fn ellipsize_text(ui: &Frame, text: &str, width: f32) -> String {
    if ui.calc_text_size(text)[0] <= width {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let s: String = chars.iter().collect::<String>() + "...";
        if ui.calc_text_size(&s)[0] <= width {
            return s;
        }
    }
    "...".to_string()
}

/// "Save as new": checks the name, writes `<dir>\<id>.toml`, reloads the library and selects the new preset.
fn save_new_preset(m: &mut MenuModel<'_>, r: &mut MenuResult) {
    let Some(dir) = m.presets.dir.clone() else {
        return;
    };
    let name = m.presets.name.trim().to_string();
    let result = panels::check_new_preset(&name, m.library)
        .and_then(|id| Preset::from_params(&id, &name, m.params).map_err(|e| e.to_string()))
        .and_then(|p| p.save(&dir).map(|path| (p.id, path)).map_err(|e| e.to_string()));
    match result {
        Ok((id, path)) => {
            m.presets.message = Some((true, format!("Saved {}.", path.display())));
            m.presets.name.clear();
            r.library = Some(Library::load(Some(&dir)));
            r.preset = Some(id);
            r.params_changed = true;
        }
        Err(e) => m.presets.message = Some((false, e)),
    }
}

/// "Update": rewrites the user preset `id` with the current parameters and selects it again.
fn update_preset(m: &mut MenuModel<'_>, r: &mut MenuResult, id: String, name: String) {
    let Some(dir) = m.presets.dir.clone() else {
        return;
    };
    match Preset::from_params(&id, &name, m.params).and_then(|p| {
        p.save(&dir).map_err(|e| crate::presets::PresetError {
            file: format!("{id}.toml"),
            message: e.to_string(),
        })
    }) {
        Ok(_) => {
            m.presets.message = Some((true, format!("Updated {id}.toml.")));
            r.library = Some(Library::load(Some(&dir)));
            r.preset = Some(id);
            r.params_changed = true;
        }
        Err(e) => m.presets.message = Some((false, e.to_string())),
    }
}

/// The SDR headroom readout (T-84): the total, its factors, the mode in effect.
fn headroom_readout(ui: &Frame, m: &MenuModel<'_>) {
    let fonts = m.fonts;
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
                .map_or_else(|| "?".to_string(), |v| panels::words(v));
            theme::kv_row(ui, "Mode in effect", &name, PALETTE.text);
        }
        None => theme::kv_row(ui, "Readout", "no frame yet", PALETTE.text_faint),
    }
    ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
}

/// The text of the capture path in force: `Zero-copy (GPU sharing)` or `CPU copy`, with the copy cost.
pub fn capture_path_status(c: &super::CaptureView) -> String {
    let name = c.active.name();
    match c.copy_ms {
        Some(ms) => format!("{name}, copy {ms:.3} ms per frame"),
        None => name.to_string(),
    }
}

/// The capture combo's tooltip.
pub const CAPTURE_TOOLTIP: &str =
    "How captured frames reach the renderer. Zero-copy (the default) keeps the frames on \
     the GPU: the captured window is shared with the renderer, nothing goes through the CPU (cheapest, most at large \
     windows); it needs capture and rendering on one GPU, and falls back to the CPU copy by itself on any problem. \
     The CPU copy reads each frame back to the CPU and uploads it again: works on every PC. The change applies at \
     once and is saved ([global.capture] path); --capture-path wins at start.";

/// The capture path choice (A29), in the Source section: the combo (`[global.capture] path`, applied at once), the
/// path in force with its copy cost, and why zero-copy is not running when it is asked.
fn capture_choice(ui: &Frame, m: &MenuModel<'_>, r: &mut MenuResult) {
    use crate::zerocopy::CapturePath;
    let c = &m.state.capture;
    let width = theme::row_label(ui, "Capture");
    ui.set_next_item_width(width);
    if let Some(_combo) = ui.begin_combo("##capture_path", c.requested.label()) {
        for p in CapturePath::ALL {
            let selected = p == c.requested;
            // Zero-copy cannot run on this machine: shown disabled, the reason below.
            let off = p == CapturePath::ZeroCopy && c.unavailable.is_some() && !selected;
            let disabled = ui.begin_disabled_with_cond(off);
            if ui.selectable_config(p.label()).selected(selected).build() && !selected {
                r.capture_path = Some(p);
            }
            drop(disabled);
            record(ui, r, &format!("capture_path:{}", p.id()));
        }
    }
    record(ui, r, "capture_path");
    if ui.is_item_hovered() {
        theme::tooltip(ui, CAPTURE_TOOLTIP);
    }
    theme::kv_row(ui, "In use", &capture_path_status(c), PALETTE.text);
    record(ui, r, "capture_path:active");
    if c.requested == CapturePath::ZeroCopy && c.active == CapturePath::Cpu {
        if let Some(why) = c.reason.as_ref().or(c.unavailable.as_ref()) {
            // Not supported here: a plain note; a failure at runtime: a problem.
            let colour = if c.unavailable.is_some() {
                PALETTE.accent
            } else {
                PALETTE.problem
            };
            theme::callout(
                ui,
                colour,
                &[format!("Zero-copy is not used: {why}. The CPU copy runs instead.")],
            );
            record(ui, r, "capture_path:reason");
        }
    } else if let Some(why) = c.unavailable.as_ref().filter(|_| c.requested == CapturePath::Cpu) {
        theme::with_font(ui, None, METRICS.font_small, || {
            let wrap = ui.push_text_wrap_pos(0.0);
            ui.text_colored(PALETTE.text_faint, format!("Zero-copy is not available here: {why}."));
            drop(wrap);
        });
        record(ui, r, "capture_path:reason");
    }
}

/// The capture path in force, read-only (Performance and diagnostics; the choice is in the Source section).
fn capture_status_row(ui: &Frame, m: &MenuModel<'_>, r: &mut MenuResult) {
    theme::kv_row(ui, "Capture path", &capture_path_status(&m.state.capture), PALETTE.text);
    record(ui, r, "capture_path:status");
}

/// Capture and render rates and the GPU milliseconds per stage.
fn performance_rows(ui: &Frame, m: &MenuModel<'_>) {
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

/// The text the adapter combo shows for a `[global.gpu] adapter` value.
pub fn adapter_choice_name(value: &str, adapters: &[crate::adapters::AdapterEntry]) -> String {
    if value.trim().is_empty() || value.trim().eq_ignore_ascii_case(settings::GPU_ADAPTER_AUTO) {
        return "Automatic".to_string();
    }
    match adapters.iter().find(|a| a.matches_setting(value)) {
        Some(a) => a.name.clone(),
        None => match crate::adapters::AdapterSpec::parse(value) {
            // Review D-02: a remembered adapter that is not there now (removed, disabled, driver changed).
            crate::adapters::AdapterSpec::Id(id) => format!("{} (not found)", id.name),
            _ => format!("\u{201C}{}\u{201D} (as written in the settings)", value.trim()),
        },
    }
}

/// Whether the adapter setting `now` differs from the one the session started with (a restart applies it). An
/// identity's LUID hint does not count (`adapters::AdapterSpec::same`).
pub fn adapter_restart_needed(now: &str, at_start: &str) -> bool {
    !crate::adapters::AdapterSpec::same(now, at_start)
}

/// GPU and system: the adapter in use and the choice for the next start, the hotkeys, the settings folder.
fn system_rows(ui: &Frame, m: &mut MenuModel<'_>, r: &mut MenuResult) {
    let fonts = m.fonts;
    let text = PALETTE.text;
    theme::subheading(ui, &fonts, "GPU");
    let gpu = m.system.gpu.clone();
    let adapters = gpu.as_ref().map(|g| g.adapters.clone()).unwrap_or_default();
    match &gpu {
        Some(g) => {
            let chosen = g
                .selection
                .chosen
                .as_ref()
                .map_or("none".to_string(), |v| v.name.clone());
            theme::kv_row(ui, "In use", &chosen, PALETTE.accent);
            theme::kv_row(ui, "Chosen by", &g.selection.reason.to_string(), text);
            if let Some(o) = &g.selection.owner {
                theme::kv_row(ui, "Monitor owner", &o.name, text);
            }
        }
        None => theme::kv_row(ui, "In use", "not known yet", PALETTE.text_faint),
    }
    // Session notices (the output on a monitor of another adapter, a minimised or hidden source).
    if !m.state.notices.is_empty() {
        theme::callout(ui, PALETTE.problem, &m.state.notices);
        record(ui, r, "system:notices");
    }
    // The choice for the next start: automatic, or one adapter with a Vulkan device.
    let width = theme::row_label(ui, "Next start");
    ui.set_next_item_width(width);
    let current = adapter_choice_name(&m.system.gpu_setting, &adapters);
    if let Some(_combo) = ui.begin_combo("##gpu_adapter", &current) {
        let mut options: Vec<(String, String, bool)> = vec![(
            settings::GPU_ADAPTER_AUTO.to_string(),
            adapter_choice_name(settings::GPU_ADAPTER_AUTO, &adapters),
            true,
        )];
        for a in &adapters {
            let mut label = a.name.clone();
            if a.owns_monitors {
                label.push_str("  \u{00B7}  drives a monitor");
            }
            if a.vulkan_index.is_none() {
                label.push_str("  \u{00B7}  no Vulkan device");
            }
            options.push((a.setting_value(), label, a.vulkan_index.is_some() && !a.software));
        }
        for (value, label, usable) in options {
            let selected = crate::adapters::AdapterSpec::same(&value, &m.system.gpu_setting)
                || adapters
                    .iter()
                    .find(|a| a.setting_value() == value)
                    .is_some_and(|a| a.matches_setting(&m.system.gpu_setting));
            let disabled = ui.begin_disabled_with_cond(!usable);
            if ui
                .selectable_config(format!("{label}##{value}"))
                .selected(selected)
                .build()
                && !selected
            {
                m.system.gpu_setting = value.clone();
                r.gpu_adapter = Some(value);
            }
            drop(disabled);
        }
    }
    record(ui, r, "gpu_adapter");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "The adapter that runs capture and rendering ([global.gpu] adapter in settings.toml). Automatic takes \
             the adapter driving the monitor the output covers. A change applies at the next start; --gpu wins.",
        );
    }
    let at_start = gpu.as_ref().map_or(settings::GPU_ADAPTER_AUTO, |g| g.setting.as_str());
    if adapter_restart_needed(&m.system.gpu_setting, at_start) {
        ui.spacing();
        theme::callout(
            ui,
            PALETTE.accent,
            &["Restart required: the new adapter is used from the next start of ultimateCRT.".to_string()],
        );
    }
    if let Some(g) = &gpu {
        if !g.selection.warnings.is_empty() {
            ui.spacing();
            theme::callout(ui, PALETTE.problem, &g.selection.warnings);
        }
    }

    super::hotkeys_panel::rows(ui, &fonts, m.hotkeys, r);

    theme::subheading(ui, &fonts, "Files");
    let dir = m
        .system
        .settings_dir
        .as_ref()
        .map_or("none (no APPDATA)".to_string(), |d| d.display().to_string());
    theme::with_font(ui, None, METRICS.font_small, || {
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(PALETTE.text_muted, format!("Settings and user presets: {dir}"));
        drop(wrap);
    });
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
    let track = width - ui.calc_text_size("00.000 mm")[0] - ui.clone_style().item_spacing()[0];
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

    /// A23: after an attach the foreground goes to the new source, never to the window that had it when the
    /// picker opened (the terminal that started the app, the previous game).
    #[test]
    fn an_attach_hands_the_foreground_to_the_new_source() {
        const NEW: isize = 0x40;
        assert_eq!(
            close_target(CloseReason::Attached, Some(OTHER), Some(NEW), OUT, |_| true),
            Some(NEW)
        );
        assert_eq!(
            close_target(CloseReason::Attached, Some(OTHER), Some(NEW), OUT, |h| h != NEW),
            None
        );
        assert_eq!(
            close_target(CloseReason::Toggle, Some(OTHER), Some(NEW), OUT, |_| true),
            Some(OTHER)
        );
        assert_eq!(
            close_target(CloseReason::Deactivated, Some(OTHER), Some(NEW), OUT, |_| true),
            None
        );
        assert!(restores_foreground(CloseReason::Attached));
        assert!(!restores_foreground(CloseReason::Deactivated));
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
    fn capture_path_status_names_the_path_and_the_cost() {
        use crate::zerocopy::CapturePath;
        let mut c = super::super::CaptureView::default();
        assert_eq!(capture_path_status(&c), "Zero-copy (GPU sharing)");
        c.active = CapturePath::Cpu;
        c.copy_ms = Some(1.2084);
        assert_eq!(capture_path_status(&c), "CPU copy, copy 1.208 ms per frame");
        assert_eq!(CapturePath::ZeroCopy.label(), "Zero-copy (GPU sharing, default)");
        assert_eq!(CapturePath::Cpu.label(), "CPU copy (most compatible)");
        assert!(CAPTURE_TOOLTIP.contains("Zero-copy") && CAPTURE_TOOLTIP.contains("CPU copy"));
    }

    #[test]
    fn adapter_choice_names_and_restart_note() {
        use crate::adapters::{AdapterEntry, AdapterId, Luid};
        let luid = Luid { high: 0, low: 0x17801 };
        let id = AdapterId {
            name: "NVIDIA RTX A2000 Laptop GPU".into(),
            vendor: 0x10DE,
            device: 0x25B8,
            subsys: 0,
            nth: 1,
            luid: Some(luid),
        };
        let rtx = AdapterEntry {
            name: "NVIDIA RTX A2000 Laptop GPU".into(),
            luid: Some(luid),
            owns_monitors: false,
            software: false,
            vulkan_index: Some(0),
            id: Some(id.clone()),
        };
        let list = [rtx.clone()];
        assert!(adapter_choice_name("auto", &list).starts_with("Automatic"));
        assert!(adapter_choice_name("", &list).starts_with("Automatic"));
        assert_eq!(adapter_choice_name(&rtx.setting_value(), &list), rtx.name);
        assert!(adapter_choice_name("Intel", &list).contains("Intel"));
        // An earlier build's LUID value still names the entry in this boot.
        assert_eq!(adapter_choice_name("00000000:00017801", &list), rtx.name);
        // Review D-02: after a reboot the LUID hint differs; the identity still names the entry, and no restart
        // is asked for.
        let after_reboot = AdapterId {
            luid: Some(Luid { high: 0, low: 0x1A2B3 }),
            ..id.clone()
        }
        .to_string();
        assert_eq!(adapter_choice_name(&after_reboot, &list), rtx.name);
        assert!(!adapter_restart_needed(&after_reboot, &rtx.setting_value()));
        // A remembered adapter that is gone is named as such.
        let gone = AdapterId { device: 0x1234, ..id }.to_string();
        assert_eq!(
            adapter_choice_name(&gone, &list),
            "NVIDIA RTX A2000 Laptop GPU (not found)"
        );
        assert!(!adapter_restart_needed("auto", "AUTO"));
        assert!(!adapter_restart_needed("", "auto"));
        assert!(adapter_restart_needed(&rtx.setting_value(), "auto"));
    }

    #[test]
    fn the_wait_reads_in_seconds_or_forever() {
        assert_eq!(wait_text(30.0), "30 s");
        assert_eq!(wait_text(0.0), "forever");
        assert_eq!(wait_text(-1.0), "forever");
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
