//! The Interface and About sections of the menu (owner request 2026-11-20).
//!
//! - **Interface**: the UI scale (`[global.ui] scale`: Automatic follows the monitor's DPI, or 75 % to 300 %),
//!   applied at once (the theme rewrites every size from its tokens); "Reset look overrides" (the look parameters
//!   back to the preset in force, quality settings kept) and "Reset everything to defaults" (parameter overrides,
//!   hotkeys, interface size, GPU choice and capture path; a copy of `settings.toml` is kept first; presets, game profiles and
//!   burn-in untouched), each behind a confirmation step.
//! - **About**: name, version and commit, licence, project address (plain selectable text, no link opened), the
//!   GPU in use, the Vulkan device and the capture path, the settings folder with a Copy button, the third-party table and the note on
//!   unsigned binaries.

use dear_imgui_rs::{TableColumnFlags, TableFlags, Ui as Frame};

use crate::about;
use crate::settings::{UiScale, UI_SCALE_RANGE};

use super::menu::{record, same_line_if_fits, MenuResult, SystemModel};
use super::theme::{self, Fonts, METRICS, PALETTE};

/// The manual scales offered.
pub const SCALE_STEPS: [f32; 8] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// A reset of the Interface section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetKind {
    /// The look parameter overrides only (quality kept).
    Look,
    /// Overrides, hotkeys, UI scale, GPU choice, capture path.
    Everything,
}

/// The state of the two sections.
#[derive(Clone, Debug, Default)]
pub struct ExtrasModel {
    /// A reset waiting for its confirmation.
    pub confirm: Option<ResetKind>,
    /// The outcome of the last reset (from the app): (good news, text).
    pub reset_message: Option<(bool, String)>,
    /// The DPI scale of the output's monitor (for the Automatic label).
    pub monitor_scale: f32,
    /// The Vulkan device the renderer runs on.
    pub vulkan_device: Option<String>,
    /// The capture path in force (A29), as the Performance section words it.
    pub capture_path: Option<String>,
    /// The outcome of the last Copy: (good news, text).
    pub copy_message: Option<(bool, String)>,
    /// The third-party list is open.
    pub show_licences: bool,
    /// The chord of the click-through hotkey in force (for the tooltip), e.g. `Ctrl+Alt+F6`.
    pub click_through_key: String,
}

/// `150 %`.
pub fn percent(s: f32) -> String {
    format!("{:.0} %", s * 100.0)
}

/// The label of a scale setting: `Automatic (monitor, 150 %)` or `125 %`.
pub fn scale_label(setting: UiScale, monitor: f32) -> String {
    match setting {
        UiScale::Auto => format!("Automatic (monitor, {})", percent(UiScale::Auto.effective(monitor))),
        UiScale::Manual(v) => percent(v),
    }
}

fn small(ui: &Frame, colour: theme::Rgba, text: &str) {
    theme::with_font(ui, None, METRICS.font_small, || {
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(colour, text);
        drop(wrap);
    });
}

/// The tooltip of the click-through row; `hotkey` is the chord of its hotkey.
pub fn click_through_tip(hotkey: &str) -> String {
    format!(
        "While the menu is closed, the output ignores the mouse: clicks and the wheel go to the window under it (the \
         captured game). The menu takes the mouse back while it is open, and its key always works. {hotkey} switches \
         it from anywhere."
    )
}

/// The Interface section: UI scale, click-through and the two resets.
pub fn interface(
    ui: &Frame,
    fonts: &Fonts,
    scale: UiScale,
    click_through: bool,
    m: &mut ExtrasModel,
    r: &mut MenuResult,
) {
    let (open, header) = theme::section_header(ui, fonts, "Interface", None, false, false);
    r.items.push(("header:Interface".to_string(), header));
    if !open {
        return;
    }
    let mut on = click_through;
    if ui.checkbox("Click-through (mouse to the game)", &mut on) {
        r.click_through = Some(on);
    }
    record(ui, r, "click_through");
    if ui.is_item_hovered() {
        theme::tooltip(ui, &click_through_tip(&m.click_through_key));
    }
    let width = theme::row_label(ui, "Interface size");
    ui.set_next_item_width(width);
    let mut options = vec![UiScale::Auto];
    options.extend(SCALE_STEPS.iter().map(|s| UiScale::Manual(*s)));
    if let Some(_combo) = ui.begin_combo("##ui_scale", scale_label(scale, m.monitor_scale)) {
        for (i, o) in options.iter().enumerate() {
            let selected = *o == scale;
            if ui
                .selectable_config(format!("{}##scale{i}", scale_label(*o, m.monitor_scale)))
                .selected(selected)
                .build()
                && !selected
            {
                r.scale = Some(*o);
            }
            record(ui, r, &format!("ui_scale:{i}"));
            if selected {
                ui.set_item_default_focus();
            }
        }
    }
    record(ui, r, "ui_scale");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            &format!(
                "The size of the menu and the window list. Automatic follows the monitor's scaling; {} to {} fix it. \
                 Applies at once.",
                percent(UI_SCALE_RANGE.0),
                percent(UI_SCALE_RANGE.1)
            ),
        );
    }

    theme::subheading(ui, fonts, "Reset");
    if theme::button(ui, "Reset look overrides") {
        m.confirm = Some(ResetKind::Look);
        m.reset_message = None;
    }
    record(ui, r, "reset:look");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Every look parameter back to the preset in force (with this program's profile). The quality settings \
             stay.",
        );
    }
    same_line_if_fits(ui, "Reset everything to defaults");
    if theme::button(ui, "Reset everything to defaults") {
        m.confirm = Some(ResetKind::Everything);
        m.reset_message = None;
    }
    record(ui, r, "reset:everything");
    if let Some(kind) = m.confirm {
        let (text, go) = match kind {
            ResetKind::Look => (
                "Set every look parameter back to the preset in force? Your presets and game profiles stay.",
                "Reset look",
            ),
            ResetKind::Everything => (
                "Reset the parameter changes, the hotkeys, the interface size, the GPU choice and the capture path? Kept: your presets, \
                 the game profiles, burn-in and the preset in force. A copy of settings.toml is kept as \
                 settings.toml.bak first.",
                "Reset everything",
            ),
        };
        theme::callout(ui, PALETTE.problem, &[text.to_string()]);
        if theme::button_tinted(ui, go, PALETTE.problem) {
            r.reset = Some(kind);
            m.confirm = None;
            m.reset_message = Some((true, "Resetting...".to_string()));
        }
        record(ui, r, "reset:confirm");
        ui.same_line();
        if theme::button(ui, "Cancel##reset") {
            m.confirm = None;
        }
        record(ui, r, "reset:cancel");
    }
    if let Some((ok, text)) = &m.reset_message {
        small(ui, if *ok { PALETTE.ok } else { PALETTE.problem }, text);
        record(ui, r, "reset:message");
    }
}

/// A read-only text field (selectable, Ctrl+C copies).
fn selectable_text(ui: &Frame, id: &str, text: &str, width: f32) {
    let mut t = text.to_string();
    ui.set_next_item_width(width);
    ui.input_text(id, &mut t).read_only(true).build();
}

/// The About section.
pub fn about_section(ui: &Frame, fonts: &Fonts, m: &mut ExtrasModel, system: &SystemModel, r: &mut MenuResult) {
    let (open, header) = theme::section_header(ui, fonts, "About", None, false, false);
    r.items.push(("header:About".to_string(), header));
    if !open {
        return;
    }
    theme::with_font(ui, fonts.strong, METRICS.font_small, || {
        ui.text_colored(PALETTE.accent, "ULTIMATECRT");
    });
    theme::with_font(ui, fonts.strong, 0.0, || {
        ui.text_colored(PALETTE.text, "A physically based CRT for any window");
    });
    theme::kv_row(ui, "Version", about::VERSION, PALETTE.text);
    record(ui, r, "about:version");
    theme::kv_row(ui, "Commit", about::COMMIT, PALETTE.text);
    small(ui, PALETTE.text_muted, about::LICENCE);
    let avail = ui.content_region_avail()[0];
    selectable_text(ui, "##about_url", about::PROJECT_URL, avail);
    record(ui, r, "about:url");
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Select and press Ctrl+C to copy. ultimateCRT opens no link and uses no network.",
        );
    }

    theme::subheading(ui, fonts, "This PC");
    let adapter = system
        .gpu
        .as_ref()
        .and_then(|g| g.selection.chosen.as_ref().map(|c| c.name.clone()))
        .unwrap_or_else(|| "not known yet".to_string());
    theme::kv_row(ui, "Adapter", &adapter, PALETTE.text);
    theme::kv_row(
        ui,
        "Vulkan device",
        m.vulkan_device.as_deref().unwrap_or("not known yet"),
        PALETTE.text,
    );
    theme::kv_row(
        ui,
        "Capture path",
        m.capture_path.as_deref().unwrap_or("not known yet"),
        PALETTE.text,
    );
    record(ui, r, "about:gpu");
    let dir = system
        .settings_dir
        .as_ref()
        .map_or("none (APPDATA is not set)".to_string(), |d| d.display().to_string());
    ui.text_colored(PALETTE.text_muted, "Settings folder");
    let copy_w = ui.calc_text_size("Copy")[0] + 20.0 * theme::scale_of(ui);
    let avail = ui.content_region_avail()[0];
    selectable_text(
        ui,
        "##about_dir",
        &dir,
        (avail - copy_w - ui.clone_style().item_spacing()[0]).max(60.0),
    );
    record(ui, r, "about:dir");
    ui.same_line();
    let disabled = ui.begin_disabled_with_cond(system.settings_dir.is_none());
    if theme::button(ui, "Copy") {
        r.copy = Some(dir.clone());
    }
    drop(disabled);
    record(ui, r, "about:copy");
    if let Some((ok, text)) = &m.copy_message {
        small(ui, if *ok { PALETTE.ok } else { PALETTE.problem }, text);
        record(ui, r, "about:copy_message");
    }

    theme::subheading(ui, fonts, "Licences");
    let crates = about::crates();
    let label = if m.show_licences {
        "Hide third-party licences".to_string()
    } else {
        format!("Third-party licences ({})", crates.len() + about::BUNDLED.len())
    };
    if theme::button(ui, &format!("{label}###about_licences")) {
        m.show_licences = !m.show_licences;
    }
    record(ui, r, "about:licences");
    if m.show_licences {
        theme::with_font(ui, None, METRICS.font_small, || {
            let flags = TableFlags::ROW_BG | TableFlags::NO_SAVED_SETTINGS;
            if let Some(_t) = ui.begin_table_with_flags("##about_table", 3, flags) {
                ui.table_setup_column_stretch_weight("Name", TableColumnFlags::NONE, 0.48);
                ui.table_setup_column_stretch_weight("Version", TableColumnFlags::NONE, 0.2);
                ui.table_setup_column_stretch_weight("Licence", TableColumnFlags::NONE, 0.32);
                let rows = about::BUNDLED
                    .iter()
                    .map(|(n, v, l)| ((*n).to_string(), (*v).to_string(), (*l).to_string()))
                    .chain(
                        crates
                            .iter()
                            .map(|p| (p.name.clone(), p.version.clone(), p.licence.clone())),
                    );
                for (name, version, licence) in rows {
                    ui.table_next_row();
                    for (text, colour) in [
                        (name, PALETTE.text),
                        (version, PALETTE.text_muted),
                        (licence, PALETTE.text_muted),
                    ] {
                        ui.table_next_column();
                        // Long names and licence expressions wrap inside their column.
                        let wrap = ui.push_text_wrap_pos(0.0);
                        ui.text_colored(colour, text);
                        drop(wrap);
                    }
                }
            }
        });
        record(ui, r, "about:table");
        small(
            ui,
            PALETTE.text_faint,
            "The full licence texts are in THIRD-PARTY.md in the release zip.",
        );
    }
    ui.dummy([0.0, 2.0 * theme::scale_of(ui)]);
    small(ui, PALETTE.text_faint, about::UNSIGNED_NOTE);
    record(ui, r, "about:unsigned");
}
