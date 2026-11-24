//! The window picker and the "Choose a window to capture" screen (A23): one themed card centred in the output
//! window, drawn by the UI layer while the menu mode is on (so the focus hand-off of [`super::menu::Menu`]
//! applies: the output is activating while the card is shown, and the foreground goes to the chosen window
//! after the attach).
//!
//! The card: the app name, the headline, a short hint, an optional message (an attach error, a source given
//! up), a search field, the list of capturable windows (icon, title, program, size, monitor; minimised windows
//! greyed with their note), a preview pane (a one-shot capture of the hovered or selected window, its details,
//! a refresh button) and a footer (ask at every start, Cancel when a source is still attached, Capture).
//!
//! Keys are handled before Dear ImGui sees them ([`Picker::on_key`], [`Picker::on_char`]): the search text is
//! the picker's own (typed anywhere in the card), Up and Down move the selection, Page Up and Page Down by a
//! page, Enter captures, Esc clears the search, then cancels (with a source attached). Double-click captures.
//! The list decisions are `crate::picker`; the app fills the rows ([`Picker::set_rows`]), the icons and the
//! preview (textures owned by `super::Ui`).

use std::collections::HashMap;

use dear_imgui_rs::{ManagedTextureId, MouseButton, StyleColor, StyleVar, Ui as Frame, WindowFlags};

use crate::picker::{self as logic, Row};

use super::theme::{self, alpha, Fonts, METRICS, PALETTE};

/// Title of the card window (its ImGui id).
pub const TITLE: &str = "###phoxium_picker";

/// Rows a Page Up or Page Down moves.
pub const PAGE: i32 = 8;

/// Virtual keys the picker handles itself.
pub const VK_BACK: usize = 0x08;
pub const VK_RETURN: usize = 0x0D;
pub const VK_ESCAPE: usize = 0x1B;
pub const VK_PRIOR: usize = 0x21;
pub const VK_NEXT: usize = 0x22;
pub const VK_UP: usize = 0x26;
pub const VK_DOWN: usize = 0x28;

/// What the user asked for in the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerAction {
    /// Capture this window.
    Attach(isize),
    /// Back to the source still attached (Esc, Cancel).
    Cancel,
    /// Enumerate the windows again now.
    Refresh,
    /// Take the preview of the wanted row again now.
    RefreshPreview,
    /// Open a still image (a PNG) as the source instead of a window.
    OpenImage,
    /// Use the video capture device number `i` of [`Picker::video`] as the source.
    OpenVideo(usize),
    /// The "ask at every start" check box changed.
    AskAtStart(bool),
    /// The first-run hint was dismissed; `true`: "Do not show again" was ticked.
    FirstRunDone(bool),
}

/// A preview texture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    pub hwnd: isize,
    pub tex: ManagedTextureId,
    pub size: [f32; 2],
}

/// The picker's state (owned by `super::Ui`).
#[derive(Debug, Default)]
pub struct Picker {
    /// The windows listed, sorted (`picker::sort_rows`).
    pub rows: Vec<Row>,
    /// The video capture devices (capture cards, cameras) offered next to the windows, by name.
    pub video: Vec<String>,
    /// The search text.
    pub query: String,
    pub selected: Option<isize>,
    /// The row under the mouse in the last frame.
    pub hovered: Option<isize>,
    /// A source is still attached behind the card: Cancel goes back to it.
    pub attached: bool,
    /// A message card above the list: (good news, text). Attach errors, a source given up.
    pub message: Option<(bool, String)>,
    /// `[global.source] ask_at_start` as shown by the check box.
    pub ask_at_start: bool,
    /// The selection moved by a key: scroll it into view in the next frame.
    pub scroll_to_selected: bool,
    /// The preview shown, if any.
    pub thumb: Option<Thumb>,
    /// The window whose preview capture failed last (no frame in time).
    pub thumb_failed: Option<isize>,
    /// Icons by executable path: `None` when the file has none (a placeholder is drawn).
    pub icons: HashMap<String, Option<ManagedTextureId>>,
    /// The first-run hint (the keys in force), shown on the card until dismissed; `None`: not shown.
    pub first_run: Option<String>,
    /// The hint's "Do not show again" box.
    pub first_run_never: bool,
    /// The high surrogate of a character outside the BMP (an emoji), waiting for its low half ([`Picker::on_char`]).
    pub high_surrogate: Option<u16>,
}

impl Picker {
    /// Replaces the list (the app's periodic enumeration): the selection is kept when the window is still
    /// listed and shown by the search, else the first row shown is selected.
    pub fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        let v = logic::visible(&self.rows, &self.query);
        self.selected = logic::keep_selection(&v, self.selected);
    }

    /// The handles the search shows, in order.
    pub fn visible(&self) -> Vec<isize> {
        logic::visible(&self.rows, &self.query)
    }

    /// The row of `hwnd`.
    pub fn row(&self, hwnd: isize) -> Option<&Row> {
        self.rows.iter().find(|r| r.hwnd == hwnd)
    }

    /// The row whose preview is wanted: the hovered one, else the selected one; with its minimised flag.
    pub fn wanted_preview(&self) -> Option<(isize, bool)> {
        self.hovered
            .or(self.selected)
            .and_then(|h| self.row(h))
            .map(|r| (r.hwnd, r.minimized))
    }

    /// The executable paths listed whose icon is not known yet.
    pub fn missing_icons(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .rows
            .iter()
            .filter_map(|r| r.exe_path.clone())
            .filter(|p| !self.icons.contains_key(p))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// A key pressed while the card is shown (`WM_KEYDOWN`, `vk` the virtual key). Returns whether the picker
    /// used it (then Dear ImGui does not see it) and the action it asks for.
    pub fn on_key(&mut self, vk: usize) -> (bool, Option<PickerAction>) {
        let step = |p: &mut Picker, d: i32| {
            let v = p.visible();
            p.selected = logic::step_selection(&v, p.selected, d);
            p.scroll_to_selected = true;
        };
        match vk {
            VK_UP => step(self, -1),
            VK_DOWN => step(self, 1),
            VK_PRIOR => step(self, -PAGE),
            VK_NEXT => step(self, PAGE),
            VK_RETURN => {
                let v = self.visible();
                return (true, self.selected.filter(|s| v.contains(s)).map(PickerAction::Attach));
            }
            VK_ESCAPE => {
                if !self.query.is_empty() {
                    self.set_query(String::new());
                } else if self.attached {
                    return (true, Some(PickerAction::Cancel));
                }
            }
            VK_BACK => {
                let mut q = self.query.clone();
                q.pop();
                self.set_query(q);
            }
            _ => return (false, None),
        }
        (true, None)
    }

    /// A character typed while the card is shown (`WM_CHAR`, one UTF-16 unit): printable characters go to the
    /// search text. A character outside the BMP (an emoji) comes as two units, a high then a low surrogate: they
    /// are combined (review D-14), as `super::input` does for Dear ImGui; a lone surrogate is dropped. Returns
    /// whether the picker used it.
    pub fn on_char(&mut self, unit: usize) -> bool {
        let unit16 = (unit & 0xFFFF) as u16;
        let c = if (0xD800..0xDC00).contains(&unit16) {
            self.high_surrogate = Some(unit16);
            return true;
        } else if (0xDC00..0xE000).contains(&unit16) {
            match self.high_surrogate.take() {
                Some(hi) => char::decode_utf16([hi, unit16]).next().and_then(Result::ok),
                None => return true,
            }
        } else {
            self.high_surrogate = None;
            char::from_u32(unit as u32)
        };
        match c {
            Some(c) if !c.is_control() => {
                let mut q = self.query.clone();
                q.push(c);
                self.set_query(q);
                true
            }
            _ => unit < 0x20 || unit == 0x7F, // control characters (Enter, Esc, Backspace) are the keys' echo
        }
    }

    fn set_query(&mut self, q: String) {
        self.query = q;
        let v = self.visible();
        self.selected = logic::keep_selection(&v, self.selected);
        self.scroll_to_selected = true;
    }
}

/// What the card did in one frame.
#[derive(Debug, Default)]
pub struct PickerResult {
    pub action: Option<PickerAction>,
    /// Widget rectangles by key: `picker:card`, `picker:search`, `picker:refresh`, `picker:row:<hwnd>`,
    /// `picker:capture`, `picker:cancel`, `picker:ask`, `picker:preview`, `picker:refresh_preview`.
    pub items: Vec<(String, [[f32; 2]; 2])>,
}

fn rect(ui: &Frame) -> [[f32; 2]; 2] {
    [ui.item_rect_min(), ui.item_rect_max()]
}

/// `text` cut with an ellipsis to fit `width` px in the current font.
fn ellipsize(ui: &Frame, text: &str, width: f32) -> String {
    if ui.calc_text_size(text)[0] <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let s: String = chars[..mid].iter().collect::<String>() + "...";
        if ui.calc_text_size(&s)[0] <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>().trim_end().to_string() + "..."
}

/// The size of the card on a `display` at UI scale `s`.
pub fn card_size(display: [f32; 2], s: f32) -> [f32; 2] {
    let margin = 24.0 * s;
    [
        (940.0 * s)
            .min(display[0] - 2.0 * margin)
            .max(320.0_f32.min(display[0])),
        (640.0 * s)
            .min(display[1] - 2.0 * margin)
            .max(240.0_f32.min(display[1])),
    ]
}

/// Draws the card. `menu_key` is the settings key (shown in the footer hint).
pub fn build(ui: &Frame, p: &mut Picker, fonts: &Fonts, menu_key: &str) -> PickerResult {
    let mut r = PickerResult::default();
    let s = theme::scale_of(ui);
    let display = ui.io().display_size();
    // Dim whatever is behind (the picture of a source still attached, or the black tube).
    ui.get_background_draw_list()
        .add_rect([0.0, 0.0], display, alpha(PALETTE.bg, 0.72))
        .filled(true)
        .build();
    let size = card_size(display, s);
    let pos = [
        ((display[0] - size[0]) * 0.5).round(),
        ((display[1] - size[1]) * 0.5).round(),
    ];
    let pad = ui.push_style_var(StyleVar::WindowPadding([22.0 * s, 18.0 * s]));
    ui.window(TITLE)
        .position(pos, dear_imgui_rs::Condition::Always)
        .size(size, dear_imgui_rs::Condition::Always)
        .bg_alpha(0.98)
        .flags(
            WindowFlags::NO_TITLE_BAR
                | WindowFlags::NO_RESIZE
                | WindowFlags::NO_MOVE
                | WindowFlags::NO_COLLAPSE
                | WindowFlags::NO_SAVED_SETTINGS
                | WindowFlags::NO_SCROLLBAR
                | WindowFlags::NO_SCROLL_WITH_MOUSE
                | WindowFlags::NO_NAV_INPUTS,
        )
        .build(|| {
            let (wp, ws) = (ui.window_pos(), ui.window_size());
            r.items
                .push(("picker:card".into(), [wp, [wp[0] + ws[0], wp[1] + ws[1]]]));
            theme::with_font(ui, fonts.body, METRICS.font_body, || {
                card(ui, p, fonts, menu_key, &mut r)
            });
        });
    pad.pop();
    r
}

fn card(ui: &Frame, p: &mut Picker, fonts: &Fonts, menu_key: &str, r: &mut PickerResult) {
    let s = theme::scale_of(ui);
    // Header: the app name, the headline, the hint.
    theme::with_font(ui, fonts.strong, METRICS.font_small, || {
        ui.text_colored(PALETTE.accent, "PHOXIUM");
    });
    theme::with_font(ui, fonts.strong, METRICS.font_display, || {
        ui.text_colored(PALETTE.text, "Choose a window to capture");
    });
    theme::with_font(ui, None, 0.0, || {
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(
            PALETTE.text_muted,
            "Pick the game or emulator to show through the virtual CRT. The window is only captured: nothing is \
             injected and it keeps the keyboard.",
        );
        drop(wrap);
    });
    ui.dummy([0.0, 4.0 * s]);
    first_run(ui, p, fonts, r);
    if let Some((ok, text)) = &p.message {
        theme::callout(
            ui,
            if *ok { PALETTE.ok } else { PALETTE.problem },
            std::slice::from_ref(text),
        );
        ui.spacing();
    }

    // Search field (the picker's own text, typed anywhere in the card) and Refresh.
    let refresh_w = ui.calc_text_size("Refresh")[0] + 20.0 * s;
    let image_w = ui.calc_text_size("Open image")[0] + 20.0 * s;
    let avail = ui.content_region_avail()[0];
    let spacing = ui.clone_style().item_spacing()[0];
    let field_w = (avail - refresh_w - image_w - 2.0 * spacing).max(80.0 * s);
    search_field(ui, p, field_w);
    r.items.push(("picker:search".into(), rect(ui)));
    ui.same_line();
    if ui.button_with_size("Open image", [image_w, ui.frame_height()]) {
        r.action = Some(PickerAction::OpenImage);
    }
    r.items.push(("picker:open_image".into(), rect(ui)));
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Use a still image (a PNG file) as the source instead of a window: a screenshot, a test card, a reference picture.",
        );
    }
    ui.same_line();
    if ui.button_with_size("Refresh", [refresh_w, ui.frame_height()]) {
        r.action = Some(PickerAction::Refresh);
    }
    r.items.push(("picker:refresh".into(), rect(ui)));
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "List the windows again (the list also refreshes by itself every second). Also looks for video devices again.",
        );
    }
    // A video capture device (a capture card, a camera) as the source, when Windows lists one.
    if !p.video.is_empty() {
        ui.align_text_to_frame_padding();
        ui.text_colored(PALETTE.text_muted, "Capture card");
        ui.same_line();
        ui.set_next_item_width(-f32::MIN_POSITIVE);
        if let Some(_combo) = ui.begin_combo("##video_device", "Choose a video device") {
            for (i, name) in p.video.iter().enumerate() {
                if ui.selectable_config(format!("{name}##video{i}")).build() {
                    r.action = Some(PickerAction::OpenVideo(i));
                }
            }
        }
        r.items.push(("picker:video".into(), rect(ui)));
        if ui.is_item_hovered() {
            theme::tooltip(
                ui,
                "Use a video capture device as the source instead of a window: a capture card with a console or a \
                 computer plugged into it, or a camera. Its own picture size is the native raster.",
            );
        }
    }
    ui.dummy([0.0, 2.0 * s]);

    // Body: the list on the left, the preview on the right, above the footer.
    let small = theme::with_font(ui, None, METRICS.font_small, || ui.text_line_height());
    let footer_h = 1.0 + ui.frame_height() + small + 3.0 * ui.clone_style().item_spacing()[1] + 4.0 * s;
    let body_h = (ui.content_region_avail()[1] - footer_h).max(80.0 * s);
    let total_w = ui.content_region_avail()[0];
    let preview_w = (total_w * 0.38).clamp(160.0 * s, 360.0 * s).min(total_w * 0.5);
    let list_w = total_w - preview_w - ui.clone_style().item_spacing()[0];
    let visible = p.visible();
    p.hovered = None;
    {
        let _bg = ui.push_style_color(StyleColor::ChildBg, PALETTE.bg_raised);
        let _rounding = ui.push_style_var(StyleVar::ChildRounding(METRICS.frame_rounding * s));
        let _cpad = ui.push_style_var(StyleVar::WindowPadding([6.0 * s, 6.0 * s]));
        ui.child_window("##picker_list")
            .size([list_w, body_h])
            .build(ui, || list(ui, p, &visible, r));
    }
    r.items.push(("picker:list".into(), rect(ui)));
    ui.same_line();
    ui.child_window("##picker_preview")
        .size([preview_w, body_h])
        .build(ui, || preview(ui, p, fonts, r));

    // Footer: ask at start, Cancel, Capture, the key hint.
    theme::rule(ui);
    let mut ask = p.ask_at_start;
    if ui.checkbox("Show this list at every start", &mut ask) {
        p.ask_at_start = ask;
        r.action = Some(PickerAction::AskAtStart(ask));
    }
    r.items.push(("picker:ask".into(), rect(ui)));
    if ui.is_item_hovered() {
        theme::tooltip(
            ui,
            "Off: Phoxium attaches at start to the program you chose last when it is running, and shows this \
             list only when it is not.",
        );
    }
    let capture_w = ui.calc_text_size("Capture")[0] + 36.0 * s;
    let cancel_w = ui.calc_text_size("Cancel")[0] + 24.0 * s;
    let right = ui.cursor_pos()[0] + ui.content_region_avail()[0];
    let spacing = ui.clone_style().item_spacing()[0];
    let x = right - capture_w - if p.attached { cancel_w + spacing } else { 0.0 };
    ui.same_line_with_pos(x.max(ui.cursor_pos()[0]));
    if p.attached {
        if ui.button_with_size("Cancel", [cancel_w, ui.frame_height()]) {
            r.action = Some(PickerAction::Cancel);
        }
        r.items.push(("picker:cancel".into(), rect(ui)));
        ui.same_line();
    }
    let chosen = p.selected.filter(|h| visible.contains(h));
    {
        // Dropped in reverse order, as Dear ImGui's stacks require.
        let _disabled = ui.begin_disabled_with_cond(chosen.is_none());
        let _c1 = ui.push_style_color(StyleColor::Button, PALETTE.accent);
        let _c2 = ui.push_style_color(StyleColor::ButtonHovered, PALETTE.accent_hover);
        let _c3 = ui.push_style_color(StyleColor::ButtonActive, PALETTE.accent_active);
        let _c4 = ui.push_style_color(StyleColor::Text, PALETTE.bg);
        if ui.button_with_size("Capture", [capture_w, ui.frame_height()]) {
            if let Some(h) = chosen {
                r.action = Some(PickerAction::Attach(h));
            }
        }
    }
    r.items.push(("picker:capture".into(), rect(ui)));
    theme::with_font(ui, None, METRICS.font_small, || {
        let esc = if p.attached {
            "Esc clears the search, then cancels"
        } else {
            "Esc clears the search"
        };
        ui.text_colored(
            PALETTE.text_faint,
            format!("Type to search  \u{00B7}  Up and Down choose  \u{00B7}  Enter or double-click captures  \u{00B7}  {esc}  \u{00B7}  {menu_key} settings"),
        );
    });
}

/// The first-run hint (owner request 2026-11-20): the keys in force, "Do not show again" and "Got it". Shown only
/// on a first start (no settings file) until dismissed.
fn first_run(ui: &Frame, p: &mut Picker, fonts: &Fonts, r: &mut PickerResult) {
    let Some(text) = p.first_run.clone() else {
        return;
    };
    let s = theme::scale_of(ui);
    let lines = [
        "First start".to_string(),
        text,
        "Settings, presets and hotkeys are all in the menu.".to_string(),
    ];
    let top = ui.cursor_screen_pos();
    theme::callout(ui, PALETTE.accent, &lines);
    let bottom = ui.item_rect_max()[1];
    let _ = fonts;
    r.items.push((
        "picker:first_run".into(),
        [top, [top[0] + ui.content_region_avail()[0], bottom]],
    ));
    let mut never = p.first_run_never;
    if ui.checkbox("Do not show again", &mut never) {
        p.first_run_never = never;
    }
    r.items.push(("picker:first_run_never".into(), rect(ui)));
    let ok_w = ui.calc_text_size("Got it")[0] + 28.0 * s;
    let right = ui.cursor_pos()[0] + ui.content_region_avail()[0];
    ui.same_line_with_pos((right - ok_w).max(ui.cursor_pos()[0]));
    if ui.button_with_size("Got it", [ok_w, ui.frame_height()]) {
        r.action = Some(PickerAction::FirstRunDone(p.first_run_never));
    }
    r.items.push(("picker:first_run_ok".into(), rect(ui)));
    ui.dummy([0.0, 2.0 * s]);
}

/// The search field: a frame with the query (or a hint) and a caret; the text comes from [`Picker::on_char`].
fn search_field(ui: &Frame, p: &Picker, width: f32) {
    let s = theme::scale_of(ui);
    let h = ui.frame_height();
    let min = ui.cursor_screen_pos();
    let max = [min[0] + width, min[1] + h];
    ui.dummy([width, h]);
    let dl = ui.get_window_draw_list();
    dl.add_rect(min, max, PALETTE.surface)
        .filled(true)
        .rounding(METRICS.frame_rounding * s)
        .build();
    let pad = METRICS.frame_padding[0] * s;
    let y = min[1] + (h - ui.text_line_height()) * 0.5;
    // A magnifier: a ring and a handle.
    let c = [min[0] + pad + 5.0 * s, min[1] + h * 0.5 - 1.0 * s];
    dl.add_circle(c, 4.5 * s, PALETTE.text_faint).thickness(1.5 * s).build();
    dl.add_line(
        [c[0] + 3.2 * s, c[1] + 3.2 * s],
        [c[0] + 6.5 * s, c[1] + 6.5 * s],
        PALETTE.text_faint,
    )
    .thickness(1.5 * s)
    .build();
    let x = min[0] + pad + 16.0 * s;
    if p.query.is_empty() {
        dl.add_text([x, y], PALETTE.text_faint, "Type to search by title or program");
    } else {
        let shown = ellipsize(ui, &p.query, max[0] - x - pad - 4.0 * s);
        dl.add_text([x, y], PALETTE.text, &shown);
        let cx = x + ui.calc_text_size(&shown)[0] + 1.0 * s;
        dl.add_line([cx, y], [cx, y + ui.text_line_height()], PALETTE.accent)
            .thickness(1.0 * s)
            .build();
    }
}

/// The list of windows.
fn list(ui: &Frame, p: &mut Picker, visible: &[isize], r: &mut PickerResult) {
    let s = theme::scale_of(ui);
    if visible.is_empty() {
        ui.dummy([0.0, 8.0 * s]);
        let wrap = ui.push_text_wrap_pos(0.0);
        let text = if p.rows.is_empty() {
            "No window to capture yet. Start the game or emulator, then come back here: the list refreshes by itself."
                .to_string()
        } else {
            format!("No window matches \u{201C}{}\u{201D}. Esc clears the search.", p.query)
        };
        ui.text_colored(PALETTE.text_muted, text);
        drop(wrap);
        return;
    }
    let small = theme::with_font(ui, None, METRICS.font_small, || ui.text_line_height());
    let line = ui.text_line_height();
    let row_h = line + small + 12.0 * s;
    let icon = 22.0 * s;
    let _sp = ui.push_style_var(StyleVar::ItemSpacing([0.0, 2.0 * s]));
    let _c1 = ui.push_style_color(StyleColor::Header, alpha(PALETTE.accent, 0.20));
    let _c2 = ui.push_style_color(StyleColor::HeaderHovered, PALETTE.surface_hover);
    let _c3 = ui.push_style_color(StyleColor::HeaderActive, alpha(PALETTE.accent, 0.30));
    let rows: Vec<Row> = visible.iter().filter_map(|&h| p.row(h).cloned()).collect();
    for row in &rows {
        let selected = p.selected == Some(row.hwnd);
        let min = ui.cursor_screen_pos();
        let width = ui.content_region_avail()[0];
        let clicked = ui
            .selectable_config(format!("##row{}", row.hwnd))
            .selected(selected)
            .allow_double_click(true)
            .size([width, row_h])
            .build();
        let item = rect(ui);
        r.items.push((format!("picker:row:{}", row.hwnd), item));
        if ui.is_item_hovered() {
            p.hovered = Some(row.hwnd);
        }
        if clicked {
            p.selected = Some(row.hwnd);
            if ui.is_mouse_double_clicked(MouseButton::Left) {
                r.action = Some(PickerAction::Attach(row.hwnd));
            }
        }
        if selected && std::mem::take(&mut p.scroll_to_selected) {
            ui.set_scroll_here_y(0.5);
        }
        // Icon, title, details over the selectable.
        let dl = ui.get_window_draw_list();
        let pad = 8.0 * s;
        let ic = [min[0] + pad, min[1] + (row_h - icon) * 0.5];
        let tex = row.exe_path.as_ref().and_then(|e| p.icons.get(e).copied().flatten());
        let tint = if row.minimized {
            [1.0, 1.0, 1.0, 0.45]
        } else {
            [1.0, 1.0, 1.0, 1.0]
        };
        match tex {
            Some(t) => dl.add_image(t, ic, [ic[0] + icon, ic[1] + icon], [0.0, 0.0], [1.0, 1.0], tint),
            None => {
                // A placeholder: a rounded window outline with a title bar.
                dl.add_rect(ic, [ic[0] + icon, ic[1] + icon], PALETTE.text_faint)
                    .rounding(3.0 * s)
                    .thickness(1.2 * s)
                    .build();
                dl.add_line(
                    [ic[0], ic[1] + 5.0 * s],
                    [ic[0] + icon, ic[1] + 5.0 * s],
                    PALETTE.text_faint,
                )
                .thickness(1.2 * s)
                .build();
            }
        }
        let tx = ic[0] + icon + 10.0 * s;
        let tw = item[1][0] - tx - pad;
        let ty = min[1] + 6.0 * s;
        let (title_c, detail_c) = if row.minimized {
            (PALETTE.text_faint, PALETTE.text_faint)
        } else {
            (PALETTE.text, PALETTE.text_muted)
        };
        dl.add_text([tx, ty], title_c, ellipsize(ui, &row.title, tw));
        theme::with_font(ui, None, METRICS.font_small, || {
            let d = ellipsize(ui, &row.details(), tw);
            dl.add_text([tx, ty + line + 2.0 * s], detail_c, d);
        });
    }
}

/// The preview pane: the thumbnail of the wanted row, its details, the refresh button.
fn preview(ui: &Frame, p: &mut Picker, fonts: &Fonts, r: &mut PickerResult) {
    let s = theme::scale_of(ui);
    theme::with_font(ui, fonts.strong, METRICS.font_small, || {
        ui.text_colored(PALETTE.text_faint, "PREVIEW");
    });
    let w = ui.content_region_avail()[0];
    // 16:9, but leave room for the details and the button below (short outputs).
    let details = 6.0 * ui.frame_height_with_spacing() + 8.0 * s;
    let h = (w * 9.0 / 16.0)
        .min(ui.content_region_avail()[1] - details)
        .max(40.0 * s)
        .round();
    let min = ui.cursor_screen_pos();
    let max = [min[0] + w, min[1] + h];
    ui.dummy([w, h]);
    r.items.push(("picker:preview".into(), [min, max]));
    let dl = ui.get_window_draw_list();
    dl.add_rect(min, max, PALETTE.bg_raised)
        .filled(true)
        .rounding(METRICS.frame_rounding * s)
        .build();
    let shown = p.hovered.or(p.selected).and_then(|h| p.row(h)).cloned();
    let note = |text: &str| {
        let t = ui.calc_text_size(text);
        dl.add_text(
            [min[0] + (w - t[0]).max(0.0) * 0.5, min[1] + (h - t[1]) * 0.5],
            PALETTE.text_faint,
            text,
        );
    };
    match &shown {
        None => note("Select a window"),
        Some(row) if row.minimized => note("Minimised: no frames"),
        Some(row) => match p.thumb.filter(|t| t.hwnd == row.hwnd) {
            Some(t) => {
                // Fit the thumbnail in the box, keeping its aspect.
                let k = (w / t.size[0].max(1.0)).min(h / t.size[1].max(1.0));
                let (iw, ih) = (t.size[0] * k, t.size[1] * k);
                let o = [min[0] + (w - iw) * 0.5, min[1] + (h - ih) * 0.5];
                dl.add_image(
                    t.tex,
                    o,
                    [o[0] + iw, o[1] + ih],
                    [0.0, 0.0],
                    [1.0, 1.0],
                    [1.0, 1.0, 1.0, 1.0],
                );
            }
            None if p.thumb_failed == Some(row.hwnd) => note("No picture: the window does not draw"),
            None => note("Capturing a preview..."),
        },
    }
    drop(dl);
    ui.dummy([0.0, 2.0 * s]);
    if let Some(row) = &shown {
        let wrap = ui.push_text_wrap_pos(0.0);
        ui.text_colored(PALETTE.text, &row.title);
        drop(wrap);
        theme::kv_row(ui, "Program", row.exe.as_deref().unwrap_or("unknown"), PALETTE.text);
        let class = ellipsize(ui, &row.class, w * 0.55);
        theme::kv_row(ui, "Class", &class, PALETTE.text);
        if row.minimized {
            theme::kv_row(ui, "State", "minimised", PALETTE.accent);
        } else {
            theme::kv_row(
                ui,
                "Size",
                &format!("{} \u{00D7} {}", row.width, row.height),
                PALETTE.text,
            );
            if let Some(m) = row.monitor {
                theme::kv_row(ui, "Monitor", &format!("{}", m + 1), PALETTE.text);
            }
        }
        if row.minimized {
            theme::with_font(ui, None, METRICS.font_small, || {
                let wrap = ui.push_text_wrap_pos(0.0);
                ui.text_colored(
                    PALETTE.text_muted,
                    "A minimised window delivers no frames: restore it to see its picture.",
                );
                drop(wrap);
            });
        } else {
            ui.dummy([0.0, 2.0 * s]);
            if theme::button(ui, "Refresh preview") {
                r.action = Some(PickerAction::RefreshPreview);
            }
            r.items.push(("picker:refresh_preview".into(), rect(ui)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(hwnd: isize, title: &str) -> Row {
        Row {
            hwnd,
            title: title.into(),
            exe: Some(format!("{title}.exe")),
            exe_path: Some(format!(r"C:\{title}.exe")),
            class: "C".into(),
            width: 640,
            height: 480,
            monitor: Some(0),
            minimized: false,
        }
    }

    fn picker() -> Picker {
        let mut p = Picker::default();
        p.set_rows(vec![row(1, "alpha"), row(2, "beta"), row(3, "gamma")]);
        p
    }

    #[test]
    fn rows_select_the_first_and_keep_the_selection() {
        let mut p = picker();
        assert_eq!(p.selected, Some(1));
        p.selected = Some(3);
        p.set_rows(vec![row(3, "gamma"), row(4, "delta")]);
        assert_eq!(p.selected, Some(3), "still listed");
        p.set_rows(vec![row(4, "delta")]);
        assert_eq!(p.selected, Some(4), "gone: the first row");
        p.set_rows(Vec::new());
        assert_eq!(p.selected, None);
    }

    #[test]
    fn keys_move_the_selection_and_enter_attaches() {
        let mut p = picker();
        assert_eq!(p.on_key(VK_DOWN), (true, None));
        assert_eq!(p.selected, Some(2));
        assert!(p.scroll_to_selected);
        p.on_key(VK_NEXT);
        assert_eq!(p.selected, Some(3));
        p.on_key(VK_PRIOR);
        assert_eq!(p.selected, Some(1));
        p.on_key(VK_UP);
        assert_eq!(p.selected, Some(1));
        assert_eq!(p.on_key(VK_RETURN), (true, Some(PickerAction::Attach(1))));
        assert_eq!(p.on_key(0x41), (false, None), "other keys go to Dear ImGui");
    }

    #[test]
    fn typing_filters_backspace_and_esc_edit_and_esc_cancels_only_when_attached() {
        let mut p = picker();
        for c in "ga".chars() {
            assert!(p.on_char(c as usize));
        }
        assert!(p.on_char(0x0D), "the echo of Enter is swallowed");
        assert_eq!(p.query, "ga");
        assert_eq!(p.visible(), [3]);
        assert_eq!(p.selected, Some(3), "the selection follows the search");
        p.on_key(VK_BACK);
        assert_eq!(p.query, "g");
        assert_eq!(p.on_key(VK_ESCAPE), (true, None));
        assert_eq!(p.query, "", "Esc clears the search first");
        assert_eq!(p.on_key(VK_ESCAPE), (true, None), "nothing to cancel without a source");
        p.attached = true;
        assert_eq!(p.on_key(VK_ESCAPE), (true, Some(PickerAction::Cancel)));
        // Enter with a selection the search hides: nothing.
        p.selected = Some(1);
        p.query = "zzz".into();
        assert_eq!(p.on_key(VK_RETURN), (true, None));
    }

    /// Review D-14: a character outside the BMP arrives as two UTF-16 units and is kept whole; a lone surrogate
    /// (a high one followed by an ordinary character, or a low one alone) is dropped.
    #[test]
    fn surrogate_pairs_are_combined_in_the_search_text() {
        let mut p = picker();
        p.rows.push(row(9, "game \u{1F3AE} night"));
        let mut units = [0u16; 2];
        for u in '\u{1F3AE}'.encode_utf16(&mut units).iter() {
            assert!(p.on_char(usize::from(*u)), "both halves are used");
        }
        assert_eq!(p.query, "\u{1F3AE}");
        assert_eq!(p.visible(), [9]);
        p.on_key(VK_BACK);
        assert_eq!(p.query, "", "Backspace removes the whole character");
        assert!(p.on_char(0xD83C));
        assert!(p.on_char('a' as usize));
        assert_eq!(p.query, "a", "the unpaired high half is dropped");
        assert!(p.on_char(0xDFAE));
        assert_eq!(p.query, "a", "a low half alone is dropped");
        assert_eq!(p.high_surrogate, None);
    }

    #[test]
    fn previews_follow_the_hover_then_the_selection_and_icons_are_listed_once() {
        let mut p = picker();
        assert_eq!(p.wanted_preview(), Some((1, false)));
        p.hovered = Some(2);
        assert_eq!(p.wanted_preview(), Some((2, false)));
        let mut rows = vec![row(1, "alpha"), row(2, "alpha")];
        rows[1].minimized = true;
        p.set_rows(rows);
        p.hovered = Some(2);
        assert_eq!(p.wanted_preview(), Some((2, true)));
        assert_eq!(p.missing_icons(), [r"C:\alpha.exe".to_string()]);
        p.icons.insert(r"C:\alpha.exe".into(), None);
        assert!(p.missing_icons().is_empty());
    }

    #[test]
    fn the_card_fits_small_and_large_outputs() {
        let a = card_size([1280.0, 720.0], 1.0);
        assert!(a[0] <= 1280.0 - 48.0 && a[1] <= 720.0 - 48.0);
        let b = card_size([2560.0, 1440.0], 2.0);
        assert!(b[0] <= 2560.0 && b[1] <= 1440.0 - 96.0);
        let c = card_size([300.0, 200.0], 1.0);
        assert!(c[0] <= 300.0 && c[1] <= 200.0);
    }
}
