//! The Hotkeys rows of the GPU and system panel (owner request 2026-11-20): every action with the chord in force,
//! a Change button (click, then press the new chord: Esc cancels, Backspace or Delete sets the default) and a
//! Default button, Quit included (review D-01; it can be changed but never removed). The chord is checked here
//! against our own actions (`hotkeys::Registration::check`) and handed to the app ([`HotkeyEdit`]), which
//! registers it at once (`output::Output::rebind`) and gives the outcome back (`super::Ui::set_hotkeys`): a chord
//! another program owns is refused and the old one kept, with the registrar's reason. While the menu key is not
//! its default, the documented fallback stays registered and the panel says which one
//! (`hotkeys::update_menu_fallback`). A `Ctrl+Alt+<character key>` chord that types a character with AltGr on an
//! installed keyboard layout is accepted with a warning (`hotkeys::altgr_conflict`).

use dear_imgui_rs::{StyleVar, Ui as Frame};

use crate::hotkeys::{altgr_conflict, capture_key, Action, Captured, Chord, LayoutProbe, Mods, Registration};

use super::input::Modifiers;
use super::menu::{record, HotkeyRow, MenuResult};
use super::theme::{self, Fonts, METRICS, PALETTE};

/// A hotkey change for the app to register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyEdit {
    /// Register `Chord` for the action now (the old chord is kept when the system refuses it).
    Set(Action, Chord),
    /// Every action back to its default chord.
    ResetAll,
}

/// The hotkeys as the menu shows and edits them.
#[derive(Clone, Debug, Default)]
pub struct HotkeyModel {
    /// The registration in force (the live app, and the tests that drive the rebinding). `None`: the read-only
    /// rows of [`HotkeyModel::rows`] (older configurations).
    pub registration: Option<Registration>,
    /// Read-only rows, used only without a registration.
    pub rows: Vec<HotkeyRow>,
    /// The action waiting for its new chord.
    pub capturing: Option<Action>,
    /// The outcome of the last change: (good news, text).
    pub message: Option<(bool, String)>,
    /// The AltGr warning of the last chord asked for ([`altgr_conflict`]), shown under the message.
    pub warning: Option<String>,
    /// What the installed keyboard layouts type with AltGr (`keyboard_layout::altgr_text` in the live app);
    /// `None`: no AltGr check (headless tests).
    pub layout_probe: Option<LayoutProbe>,
}

/// The `hotkeys::Mods` of the modifier keys held.
pub fn mods_of(m: Modifiers) -> Mods {
    let mut r = Mods::NONE;
    for (on, bit) in [
        (m.ctrl, Mods::CTRL),
        (m.alt, Mods::ALT),
        (m.shift, Mods::SHIFT),
        (m.win, Mods::WIN),
    ] {
        if on {
            r = r | bit;
        }
    }
    r
}

impl HotkeyModel {
    /// A key pressed while waiting: what it means ([`capture_key`]), and the change to register, if any.
    pub fn on_key(&mut self, vk: u16, mods: Mods) -> Option<HotkeyEdit> {
        self.on_captured(capture_key(vk, mods))
    }

    /// The outcome of a key press while waiting ([`Captured`]).
    pub fn on_captured(&mut self, c: Captured) -> Option<HotkeyEdit> {
        let action = self.capturing?;
        match c {
            Captured::Waiting => None,
            Captured::Cancel => {
                self.capturing = None;
                self.message = Some((true, "Unchanged.".to_string()));
                None
            }
            Captured::Invalid(why) => {
                self.message = Some((false, why));
                None
            }
            Captured::Default => {
                self.capturing = None;
                self.request(action, action.default_chord())
            }
            Captured::Chord(chord) => {
                self.capturing = None;
                self.request(action, chord)
            }
        }
    }

    /// Checks `chord` for `action` against our own actions; the edit to register, or the reason in the message.
    /// A chord that also fires on AltGr is registered all the same, with a warning ([`HotkeyModel::warning`]).
    pub fn request(&mut self, action: Action, chord: Chord) -> Option<HotkeyEdit> {
        self.warning = None;
        if let Some(reg) = &self.registration {
            if let Err(why) = reg.check(action, chord) {
                self.message = Some((false, why));
                return None;
            }
            if reg
                .outcome(action)
                .is_some_and(|o| o.bound == Some(chord) && o.wanted == chord)
            {
                self.message = Some((true, format!("{chord} is already the chord in force.")));
                return None;
            }
        }
        self.message = Some((true, format!("Registering {chord}...")));
        self.warning = self.layout_probe.and_then(|p| altgr_conflict(chord, p));
        Some(HotkeyEdit::Set(action, chord))
    }

    /// The outcome of a change from the app: the warning stays only while the chord went through.
    pub fn outcome(&mut self, message: Option<(bool, String)>) {
        if let Some(m) = message {
            if !m.0 {
                self.warning = None;
            }
            self.message = Some(m);
        }
    }
}

/// A short action name for the rows.
fn label(a: Action) -> &'static str {
    match a {
        Action::Quit => "Quit",
        Action::ToggleFullscreen => "Fullscreen",
        Action::ToggleMenu => "Settings menu",
        Action::ToggleOverlay => "Overlay",
        Action::ToggleBypass => "A/B bypass",
        Action::NextPreset => "Next preset",
        Action::PreviousPreset => "Previous preset",
    }
}

fn small_button(ui: &Frame, text: &str) -> bool {
    let s = theme::scale_of(ui);
    let _p = ui.push_style_var(StyleVar::FramePadding([8.0 * s, 3.0 * s]));
    ui.button(text)
}

fn small_text(ui: &Frame, colour: theme::Rgba, text: &str) {
    theme::with_font(ui, None, METRICS.font_small, || {
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(colour, text);
        drop(wrap);
    });
}

/// The Hotkeys rows (under the GPU and system panel's "Hotkeys" heading).
pub fn rows(ui: &Frame, fonts: &Fonts, m: &mut HotkeyModel, r: &mut MenuResult) {
    theme::subheading(ui, fonts, "Hotkeys");
    let Some(reg) = m.registration.clone() else {
        // Read-only rows (no registration given).
        for h in &m.rows {
            let (value, colour) = match &h.bound {
                Some(b) if *b == h.wanted => (b.clone(), PALETTE.text),
                Some(b) => (format!("{b}  ({} taken)", h.wanted), PALETTE.accent),
                None => ("none".to_string(), PALETTE.problem),
            };
            theme::kv_row(ui, &h.action, &value, colour);
        }
        return;
    };
    let s = theme::scale_of(ui);
    let spacing = ui.clone_style().item_spacing()[0];
    let pad = 2.0 * 8.0 * s;
    let change_w = ui.calc_text_size("Change")[0] + pad;
    let default_w = ui.calc_text_size("Default")[0] + pad;
    let cancel_w = ui.calc_text_size("Cancel")[0] + pad;
    for action in Action::ALL {
        let id = action.id();
        let _id = ui.push_id(id);
        let width = theme::row_label(ui, label(action));
        let start = ui.cursor_pos()[0];
        let outcome = reg.outcome(action);
        let bound = outcome.and_then(|o| o.bound);
        let wanted = outcome.map(|o| o.wanted);
        let capturing = m.capturing == Some(action);
        let (text, colour) = if capturing {
            ("Press the new keys...".to_string(), PALETTE.accent)
        } else {
            match (bound, wanted) {
                (Some(b), Some(w)) if b != w => (format!("{b}  ({w} taken)"), PALETTE.accent),
                (Some(b), _) => (b.to_string(), PALETTE.text),
                (None, _) => ("none".to_string(), PALETTE.problem),
            }
        };
        let buttons_w = if capturing {
            cancel_w
        } else {
            change_w + spacing + default_w
        };
        ui.align_text_to_frame_padding();
        ui.text_colored(colour, &text);
        record(ui, r, &format!("hotkey:{id}"));
        let right = start + width;
        ui.same_line_with_pos((right - buttons_w).max(ui.cursor_pos()[0]));
        if capturing {
            if small_button(ui, "Cancel") {
                m.capturing = None;
                m.message = Some((true, "Unchanged.".to_string()));
            }
            record(ui, r, &format!("hotkey:{id}:cancel"));
            continue;
        }
        if small_button(ui, "Change") {
            m.capturing = Some(action);
            m.message = None;
            m.warning = None;
        }
        record(ui, r, &format!("hotkey:{id}:change"));
        if ui.is_item_hovered() {
            theme::tooltip(
                ui,
                "Click, then press the new keys (for example Ctrl+Alt+F5 or F9). Only F1 to F24 work without Ctrl, \
                 Alt or Win. The hotkey is global: other programs no longer get these keys while ultimateCRT runs. \
                 Ctrl+Alt with a letter or a digit also fires on AltGr on many keyboard layouts: prefer a function \
                 key or a navigation key.",
            );
        }
        ui.same_line();
        let is_default = bound == Some(action.default_chord()) && wanted == Some(action.default_chord());
        let disabled = ui.begin_disabled_with_cond(is_default);
        if small_button(ui, "Default") {
            if let Some(e) = m.request(action, action.default_chord()) {
                r.hotkey = Some(e);
            }
        }
        drop(disabled);
        record(ui, r, &format!("hotkey:{id}:default"));
        if ui.is_item_hovered() {
            theme::tooltip(ui, &format!("Back to {}.", action.default_text()));
        }
    }
    if let Some(note) = reg.fallback_note() {
        small_text(ui, PALETTE.text_muted, &note);
        record(ui, r, "hotkeys:fallback");
    }
    if let Some(action) = m.capturing {
        small_text(
            ui,
            PALETTE.accent,
            &format!(
                "Press the new keys for {}. Esc cancels; Backspace sets the default ({}).",
                label(action).to_lowercase(),
                action.default_text()
            ),
        );
        record(ui, r, "hotkeys:waiting");
    }
    if let Some((ok, text)) = &m.message {
        small_text(ui, if *ok { PALETTE.ok } else { PALETTE.problem }, text);
        record(ui, r, "hotkeys:message");
    }
    if let Some(w) = &m.warning {
        small_text(ui, PALETTE.accent, &format!("Warning: {w}"));
        record(ui, r, "hotkeys:warning");
    }
    let all_default = Action::ALL.iter().all(|a| {
        reg.outcome(*a)
            .is_none_or(|o| o.wanted == a.default_chord() && o.bound == Some(o.wanted))
    });
    let disabled = ui.begin_disabled_with_cond(all_default);
    if theme::button(ui, "Reset all hotkeys") {
        m.capturing = None;
        m.message = Some((true, "Registering the default hotkeys...".to_string()));
        r.hotkey = Some(HotkeyEdit::ResetAll);
    }
    drop(disabled);
    record(ui, r, "hotkeys:reset_all");
    small_text(
        ui,
        PALETTE.text_faint,
        "Changes apply at once and are saved in settings.toml ([global.hotkeys]).",
    );
}
