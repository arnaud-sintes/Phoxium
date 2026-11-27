//! The look of the settings menu and the overlay: colour tokens, metrics, fonts and a few presentational
//! helpers, all in one place (owner request 2026-11-19: a modern, custom look that does not read as stock
//! Dear ImGui).
//!
//! - **Colours** ([`Palette`], [`PALETTE`]): a dark neutral base (graphite, slightly cool), one restrained accent
//!   (phosphor amber, used for the slider grab and fill, check marks, focus and selection), a green and a coral
//!   for status. Body text keeps a WCAG contrast ratio of at least 4.5:1 on every surface it is drawn on, and
//!   hint text at least 3:1; the unit tests compute the ratios ([`contrast_ratio`]).
//! - **Metrics** ([`Metrics`], [`METRICS`]): generous padding, 6 px frame rounding, 10 px window rounding, thin
//!   borders, a flat title bar in the window colour, no collapse arrow, slim rounded scroll bars. [`apply`] writes
//!   them, multiplied by the UI scale, into the ImGui style. The UI scale is the DPI scale of the output's
//!   monitor (`GetDpiForWindow`, `WM_DPICHANGED`: [`super::Ui`]); fonts are rasterised at the scaled size by
//!   Dear ImGui's dynamic font atlas, so text stays sharp at any scale.
//! - **Fonts** ([`Fonts`], [`load_fonts`]): Inter 4.1 (SIL Open Font License 1.1, notice in `THIRD-PARTY.md`),
//!   Regular and SemiBold, embedded as Latin subsets with tabular digits (`assets/fonts`, built by
//!   `assets/fonts/subset.py`). A font that fails stb_truetype's validation falls back to Dear ImGui's default.
//! - **Helpers**: section labels in small capitals, label-and-value rows with the value aligned right, a callout
//!   for problems ([`section`], [`kv_row`], [`callout`]).

use dear_imgui_rs::{
    Direction, FontId, FontSource, NumericFormat, SliderFlags, StbTrueTypeFontData, Style, StyleColor, StyleVar,
    TreeNodeFlags, Ui as Frame,
};

/// An sRGB colour with alpha, components in 0..1 (what ImGui's style takes).
pub type Rgba = [f32; 4];

/// sRGB colour from a `0xRRGGBB` literal, opaque.
pub const fn hex(rgb: u32) -> Rgba {
    [
        ((rgb >> 16) & 0xFF) as f32 / 255.0,
        ((rgb >> 8) & 0xFF) as f32 / 255.0,
        (rgb & 0xFF) as f32 / 255.0,
        1.0,
    ]
}

/// `c` with alpha `a`.
pub const fn alpha(c: Rgba, a: f32) -> Rgba {
    [c[0], c[1], c[2], a]
}

/// The colour tokens. Surfaces go from `bg` (window) up to `surface_active` (a pressed frame); text has three
/// levels (`text` body, `text_muted` labels, `text_faint` hints and disabled).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Window background (its alpha is the menu opacity setting).
    pub bg: Rgba,
    /// Title bar, popups, tooltips: one step above the window.
    pub bg_raised: Rgba,
    /// Frames (slider tracks, combo boxes, text fields, check boxes).
    pub surface: Rgba,
    pub surface_hover: Rgba,
    pub surface_active: Rgba,
    /// Thin borders and separators.
    pub border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_faint: Rgba,
    /// The single accent: slider grab and fill, check mark, focus ring, selection.
    pub accent: Rgba,
    pub accent_hover: Rgba,
    pub accent_active: Rgba,
    /// Status: within range, all good.
    pub ok: Rgba,
    /// Status: a problem the user should act on (hotkey conflicts).
    pub problem: Rgba,
}

/// The palette in use.
pub const PALETTE: Palette = Palette {
    bg: hex(0x15171B),
    bg_raised: hex(0x1C1F24),
    surface: hex(0x252930),
    surface_hover: hex(0x2D323A),
    surface_active: hex(0x353B45),
    border: hex(0x2E333B),
    text: hex(0xE8EAED),
    text_muted: hex(0xA4ABB6),
    text_faint: hex(0x7E8692),
    accent: hex(0xE9A23B),
    accent_hover: hex(0xF2B45C),
    accent_active: hex(0xD08A26),
    ok: hex(0x74CB9C),
    problem: hex(0xF27E7A),
};

/// Sizes at 100 % scale (96 dpi), in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub window_padding: [f32; 2],
    pub window_rounding: f32,
    pub frame_padding: [f32; 2],
    pub frame_rounding: f32,
    pub item_spacing: [f32; 2],
    pub item_inner_spacing: [f32; 2],
    pub border: f32,
    pub popup_rounding: f32,
    pub scrollbar_size: f32,
    pub grab_min_size: f32,
    pub grab_rounding: f32,
    pub indent: f32,
    /// Body text size.
    pub font_body: f32,
    /// Section labels (small capitals).
    pub font_small: f32,
    /// Headline numbers (the headroom readout).
    pub font_display: f32,
    /// Width of the label column of a row, as a fraction of the content width.
    pub label_fraction: f32,
    /// Extra vertical space before a section.
    pub section_gap: f32,
}

/// The metrics in use.
pub const METRICS: Metrics = Metrics {
    window_padding: [16.0, 14.0],
    window_rounding: 10.0,
    frame_padding: [10.0, 6.0],
    frame_rounding: 6.0,
    item_spacing: [10.0, 8.0],
    item_inner_spacing: [8.0, 6.0],
    border: 1.0,
    popup_rounding: 8.0,
    scrollbar_size: 10.0,
    grab_min_size: 12.0,
    grab_rounding: 5.0,
    indent: 14.0,
    font_body: 15.0,
    font_small: 11.5,
    font_display: 24.0,
    label_fraction: 0.42,
    section_gap: 10.0,
};

/// Writes the palette and the metrics, scaled by `scale` (the DPI scale, 1 at 96 dpi), into `style`. Replaces
/// every size it sets (it does not accumulate, unlike `Style::scale_all_sizes`).
pub fn apply(style: &mut Style, scale: f32) {
    let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let m = METRICS;
    let v = |a: [f32; 2]| [a[0] * s, a[1] * s];
    style.set_window_padding(v(m.window_padding));
    style.set_window_rounding(m.window_rounding * s);
    style.set_window_border_size(m.border);
    style.set_window_min_size(v([240.0, 120.0]));
    style.set_window_title_align([0.0, 0.5]);
    style.set_window_menu_button_position(Direction::None);
    style.set_child_rounding(m.frame_rounding * s);
    style.set_child_border_size(0.0);
    style.set_popup_rounding(m.popup_rounding * s);
    style.set_popup_border_size(m.border);
    style.set_frame_padding(v(m.frame_padding));
    style.set_frame_rounding(m.frame_rounding * s);
    style.set_frame_border_size(0.0);
    style.set_item_spacing(v(m.item_spacing));
    style.set_item_inner_spacing(v(m.item_inner_spacing));
    style.set_cell_padding(v([8.0, 4.0]));
    style.set_indent_spacing(m.indent * s);
    style.set_scrollbar_size(m.scrollbar_size * s);
    style.set_scrollbar_rounding(m.scrollbar_size * s);
    style.set_scrollbar_padding(2.0 * s);
    style.set_grab_min_size(m.grab_min_size * s);
    style.set_grab_rounding(m.grab_rounding * s);
    style.set_tab_rounding(m.frame_rounding * s);
    style.set_tab_border_size(0.0);
    style.set_selectable_rounding(m.frame_rounding * s * 0.75);
    style.set_separator_text_border_size(m.border);
    style.set_separator_text_padding(v([0.0, 4.0]));
    style.set_button_text_align([0.5, 0.5]);
    style.set_selectable_text_align([0.0, 0.5]);
    style.set_display_window_padding(v([12.0, 12.0]));
    style.set_display_safe_area_padding([0.0, 0.0]);
    style.set_font_size_base(m.font_body);
    style.set_font_scale_dpi(s);
    style.set_anti_aliased_lines(true);
    style.set_anti_aliased_fill(true);
    for (slot, colour) in colours(&PALETTE) {
        style.set_color(slot, colour);
    }
}

/// Every ImGui style colour the theme sets, from the palette.
pub fn colours(p: &Palette) -> Vec<(StyleColor, Rgba)> {
    use StyleColor as C;
    let clear = [0.0; 4];
    vec![
        (C::Text, p.text),
        (C::TextDisabled, p.text_faint),
        (C::WindowBg, p.bg),
        (C::ChildBg, clear),
        (C::PopupBg, p.bg_raised),
        (C::Border, p.border),
        (C::BorderShadow, clear),
        (C::FrameBg, p.surface),
        (C::FrameBgHovered, p.surface_hover),
        (C::FrameBgActive, p.surface_active),
        // A flat title bar: the window colour, active or not (no blue bar).
        (C::TitleBg, p.bg_raised),
        (C::TitleBgActive, p.bg_raised),
        (C::TitleBgCollapsed, p.bg_raised),
        (C::MenuBarBg, p.bg_raised),
        (C::ScrollbarBg, clear),
        (C::ScrollbarGrab, p.surface_active),
        (C::ScrollbarGrabHovered, p.text_faint),
        (C::ScrollbarGrabActive, p.text_muted),
        (C::CheckMark, p.accent),
        (C::CheckboxSelectedBg, p.surface),
        (C::SliderGrab, p.accent),
        (C::SliderGrabActive, p.accent_hover),
        (C::Button, p.surface),
        (C::ButtonHovered, p.surface_hover),
        (C::ButtonActive, p.surface_active),
        // Selectables (combo lists): the selected entry in a soft accent tint. Section headers push their own
        // (no fill at rest), see `section`.
        (C::Header, alpha(p.accent, 0.16)),
        (C::HeaderHovered, p.surface_hover),
        (C::HeaderActive, p.surface_active),
        (C::Separator, p.border),
        (C::SeparatorHovered, p.text_faint),
        (C::SeparatorActive, p.accent),
        (C::ResizeGrip, clear),
        (C::ResizeGripHovered, alpha(p.accent, 0.45)),
        (C::ResizeGripActive, p.accent),
        (C::Tab, p.surface),
        (C::TabHovered, p.surface_hover),
        (C::TabSelected, p.surface_active),
        (C::TabSelectedOverline, p.accent),
        (C::TabDimmed, p.surface),
        (C::TabDimmedSelected, p.surface_hover),
        (C::TabDimmedSelectedOverline, clear),
        (C::PlotLines, p.accent),
        (C::PlotLinesHovered, p.accent_hover),
        (C::PlotHistogram, p.accent),
        (C::PlotHistogramHovered, p.accent_hover),
        (C::TableHeaderBg, p.bg_raised),
        (C::TableBorderStrong, p.border),
        (C::TableBorderLight, p.border),
        (C::TableRowBg, clear),
        (C::TableRowBgAlt, alpha(p.surface, 0.35)),
        (C::TextSelectedBg, alpha(p.accent, 0.30)),
        (C::TextLink, p.accent),
        (C::InputTextCursor, p.accent),
        (C::NavCursor, alpha(p.accent, 0.8)),
        (C::NavWindowingHighlight, p.accent),
        (C::NavWindowingDimBg, alpha(p.bg, 0.5)),
        (C::ModalWindowDimBg, alpha(p.bg, 0.5)),
        (C::DragDropTarget, p.accent),
    ]
}

// ---------------------------------------------------------------------------------------------------------
// Fonts.

/// Inter 4.1 Regular, Latin subset with tabular digits (SIL OFL 1.1).
pub const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
/// Inter 4.1 SemiBold, Latin subset with tabular digits (SIL OFL 1.1).
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

/// The fonts of the UI. `None`: Dear ImGui's default font (the embedded font failed to load).
#[derive(Clone, Copy, Debug, Default)]
pub struct Fonts {
    pub body: Option<FontId>,
    pub strong: Option<FontId>,
}

/// Adds the UI fonts to the atlas of `ctx`, the body font first (Dear ImGui's default font is the first one
/// added). Either font falls back to the default font when the embedded data does not validate.
pub fn load_fonts(ctx: &mut dear_imgui_rs::Context) -> Fonts {
    let atlas = ctx.font_atlas();
    let add = |data: &[u8], what: &str| match StbTrueTypeFontData::from_slice(data) {
        Ok(d) => Some(atlas.add_font(&[FontSource::stb_truetype_with_size(d, METRICS.font_body)])),
        Err(e) => {
            eprintln!("warning: UI font {what} rejected ({e}); using the default font");
            None
        }
    };
    let body = add(INTER_REGULAR, "Inter Regular");
    let strong = add(INTER_SEMIBOLD, "Inter SemiBold");
    if body.is_none() {
        // The first font is the default: keep Dear ImGui's own font in that place.
        atlas.add_font(&[FontSource::default_font()]);
    }
    Fonts { body, strong }
}

// ---------------------------------------------------------------------------------------------------------
// Contrast (WCAG 2.x).

/// Relative luminance of an sRGB colour (alpha ignored).
pub fn relative_luminance(c: Rgba) -> f64 {
    let lin = |v: f32| {
        let v = f64::from(v);
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// WCAG contrast ratio of two opaque colours, 1 to 21.
pub fn contrast_ratio(a: Rgba, b: Rgba) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `top` composited over the opaque `under` with `top`'s alpha (in sRGB codes, as the UI pass blends).
pub fn over(top: Rgba, under: Rgba) -> Rgba {
    let a = top[3];
    [
        top[0] * a + under[0] * (1.0 - a),
        top[1] * a + under[1] * (1.0 - a),
        top[2] * a + under[2] * (1.0 - a),
        1.0,
    ]
}

// ---------------------------------------------------------------------------------------------------------
// Presentational helpers (layout in the current window; sizes come from the style, so they follow the scale).

/// Runs `f` with `font` (if loaded) at `size` px before the DPI scale (0: the current size).
pub fn with_font<R>(ui: &Frame, font: Option<FontId>, size: f32, f: impl FnOnce() -> R) -> R {
    if font.is_none() && size == 0.0 {
        return f();
    }
    ui.with_font_and_size(font, size, f)
}

/// Text in a colour.
pub fn text(ui: &Frame, colour: Rgba, s: impl AsRef<str>) {
    ui.text_colored(colour, s);
}

/// Text aligned to the right edge of the content region, on the current line.
pub fn text_right(ui: &Frame, colour: Rgba, s: &str) {
    let w = ui.calc_text_size(s)[0];
    let right = ui.cursor_pos()[0] + ui.content_region_avail()[0];
    ui.same_line_with_pos((right - w).max(ui.cursor_pos()[0]));
    ui.text_colored(colour, s);
}

/// A section header: a collapsing header in small capitals (upper case, SemiBold, small, muted), no fill at
/// rest, a hairline rule from the label to the right edge, and an optional faint note on the right (a count).
/// Open by default. Sections after the first get a gap above. True when the section is open.
pub fn section(ui: &Frame, fonts: &Fonts, label: &str, note: Option<&str>, first: bool) -> bool {
    section_header(ui, fonts, label, note, first, true).0
}

/// A section header ([`section`]) closed by default, never the first. Also returns the header's rectangle.
pub fn section_closed(ui: &Frame, fonts: &Fonts, label: &str) -> (bool, [[f32; 2]; 2]) {
    section_header(ui, fonts, label, None, false, false)
}

/// A small faint label in capitals above a group of rows inside a section (`Convergence` under Geometry).
pub fn subheading(ui: &Frame, fonts: &Fonts, label: &str) {
    let s = scale_of(ui);
    ui.dummy([0.0, 2.0 * s]);
    with_font(ui, fonts.strong, METRICS.font_small, || {
        ui.text_colored(PALETTE.text_faint, label.to_uppercase());
    });
}

/// A compact text button (surface fill, body text, less padding than a frame). True when clicked.
pub fn button(ui: &Frame, label: &str) -> bool {
    let s = scale_of(ui);
    let _p = ui.push_style_var(StyleVar::FramePadding([10.0 * s, 4.0 * s]));
    ui.button(label)
}

/// A compact button tinted with `colour` (the confirmation of a deletion or a reset): a soft fill of the colour and
/// the colour as text. True when clicked.
pub fn button_tinted(ui: &Frame, label: &str, colour: Rgba) -> bool {
    // Popped in reverse order when the tokens drop (Dear ImGui's stack order).
    let _b = ui.push_style_color(StyleColor::Button, alpha(colour, 0.20));
    let _h = ui.push_style_color(StyleColor::ButtonHovered, alpha(colour, 0.32));
    let _a = ui.push_style_color(StyleColor::ButtonActive, alpha(colour, 0.44));
    let _t = ui.push_style_color(StyleColor::Text, colour);
    button(ui, label)
}

/// A section header ([`section`]) with an optional note on the right, open or closed by default (`open`),
/// with or without the gap above (`first`). Returns whether it is open and the header's rectangle.
pub fn section_header(
    ui: &Frame,
    fonts: &Fonts,
    label: &str,
    note: Option<&str>,
    first: bool,
    open: bool,
) -> (bool, [[f32; 2]; 2]) {
    let s = scale_of(ui);
    // Sections open by default stand apart; the closed ones read as a compact list of headers.
    if !first && open {
        ui.dummy([0.0, METRICS.section_gap * s]);
    }
    let shown = with_font(ui, fonts.strong, METRICS.font_small, || {
        let pad = [4.0 * s, 6.0 * s];
        let _c = ui.push_style_color(StyleColor::Text, PALETTE.text_muted);
        let _h = ui.push_style_color(StyleColor::Header, [0.0; 4]);
        let _hh = ui.push_style_color(StyleColor::HeaderHovered, alpha(PALETTE.surface_hover, 0.6));
        let _p = ui.push_style_var(StyleVar::FramePadding(pad));
        let caps = label.to_uppercase();
        let flags = if open {
            TreeNodeFlags::DEFAULT_OPEN
        } else {
            TreeNodeFlags::empty()
        };
        let shown = ui.collapsing_header(format!("{caps}###section_{label}"), flags);
        let [min, max] = [ui.item_rect_min(), ui.item_rect_max()];
        // Label end: Dear ImGui puts a framed header's text at FontSize + 3 FramePadding.x from its left.
        let x0 = min[0] + ui.current_font_size() + 3.0 * pad[0] + ui.calc_text_size(&caps)[0] + 10.0 * s;
        let note_w = note.map_or(0.0, |n| ui.calc_text_size(n)[0] + 10.0 * s);
        let x1 = max[0] - pad[0] - note_w;
        let y = ((min[1] + max[1]) * 0.5).round() + 0.5;
        if x1 > x0 {
            ui.get_window_draw_list()
                .add_line([x0, y], [x1, y], PALETTE.border)
                .build();
        }
        if let Some(n) = note {
            ui.same_line();
            let _c = ui.push_style_color(StyleColor::Text, PALETTE.text_faint);
            let w = ui.calc_text_size(n)[0];
            // Same line as the header: Dear ImGui adds the line's text offset (the frame padding) itself.
            ui.set_cursor_screen_pos([max[0] - pad[0] - w, min[1]]);
            ui.text(n);
        }
        (shown, [min, max])
    });
    if shown.0 {
        ui.dummy([0.0, 2.0 * s]);
    }
    shown
}

/// A status pill (tinted rounded label) at the right edge of the content region, centred on `center_y`. Drawn
/// on the window's draw list without taking layout space.
pub fn pill(ui: &Frame, label: &str, colour: Rgba, center_y: f32) {
    let s = scale_of(ui);
    let size = ui.calc_text_size(label);
    let pad = [8.0 * s, 3.0 * s];
    let right = ui.window_pos()[0] + ui.window_size()[0] - ui.clone_style().window_padding()[0];
    let right = right.min(ui.cursor_screen_pos()[0] + ui.content_region_avail()[0]);
    let min = [right - size[0] - 2.0 * pad[0], center_y - size[1] * 0.5 - pad[1]];
    let max = [right, center_y + size[1] * 0.5 + pad[1]];
    let dl = ui.get_window_draw_list();
    dl.add_rect(min, max, alpha(colour, 0.16))
        .filled(true)
        .rounding((max[1] - min[1]) * 0.5)
        .build();
    dl.add_text([min[0] + pad[0], min[1] + pad[1]], colour, label);
}

/// A slider drawn the modern way: a thin rounded track, the part left of the knob filled in the accent, a round
/// knob (Dear ImGui's own frame and grab are made transparent; its slider still does the input, keyboard
/// navigation and Ctrl+click editing). No text inside (the caller shows the value). True when the value
/// changed. The item rectangle is the full slider frame.
pub fn slider<T>(ui: &Frame, id: &str, v: &mut T, lo: T, hi: T, width: f32) -> bool
where
    T: dear_imgui_rs::internal::DataTypeKind + Copy + Into<f64>,
{
    slider_ex(ui, id, v, lo, hi, width, false, "%.3f")
}

/// The position (0 at `lo`, 1 at `hi`) of `v` on a slider over `lo..hi`, clamped: linear, or logarithmic as
/// Dear ImGui's `Logarithmic` slider maps a positive range (`ln(v / lo) / ln(hi / lo)`).
pub fn slider_fraction(v: f64, lo: f64, hi: f64, log: bool) -> f64 {
    if hi <= lo || !v.is_finite() {
        return 0.0;
    }
    let t = if log && lo > 0.0 {
        (v.max(lo) / lo).ln() / (hi / lo).ln()
    } else {
        (v - lo) / (hi - lo)
    };
    t.clamp(0.0, 1.0)
}

/// The value at position `t` (0 to 1) of a slider over `lo..hi`: the inverse of [`slider_fraction`].
pub fn slider_value(t: f64, lo: f64, hi: f64, log: bool) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if log && lo > 0.0 && hi > lo {
        lo * (hi / lo).powf(t)
    } else {
        lo + t * (hi - lo)
    }
}

/// [`slider`] with a logarithmic scale (`log`, positive ranges only) and the `format` Dear ImGui uses for the
/// Ctrl+click (or Enter) text entry. The number is drawn only while it is typed in; otherwise the caller shows
/// the value next to the slider. A typed value is clamped to `lo..=hi` (`ALWAYS_CLAMP`).
#[allow(clippy::too_many_arguments)]
pub fn slider_ex<T>(ui: &Frame, id: &str, v: &mut T, lo: T, hi: T, width: f32, log: bool, format: &str) -> bool
where
    T: dear_imgui_rs::internal::DataTypeKind + Copy + Into<f64>,
{
    let s = scale_of(ui);
    let p = PALETTE;
    ui.set_next_item_width(width);
    let format = NumericFormat::<T>::new(format)
        .or_else(|_| NumericFormat::<T>::new(""))
        .expect("an empty numeric format is valid");
    let (flo, fhi): (f64, f64) = (lo.into(), hi.into());
    // ALWAYS_CLAMP: a value typed with Ctrl+click (or Enter) is clamped to the slider range like a drag (R-05,
    // owner decision Q1: typed values stay in the soft range; files may go up to the hard range). It works in
    // the value domain, so a logarithmic slider clamps the same way.
    let mut flags = SliderFlags::NO_ROUND_TO_FORMAT | SliderFlags::ALWAYS_CLAMP;
    if log && flo > 0.0 {
        flags |= SliderFlags::LOGARITHMIC;
    }
    // While the value is typed in (Ctrl+click, Enter), Dear ImGui draws its own text field: no custom drawing.
    // SAFETY: plain query of the current context's state, inside a frame.
    let typing = unsafe { dear_imgui_rs::sys::igTempInputIsActive(ui.get_id(id).raw()) };
    if typing {
        return ui
            .slider_config(id, lo, hi)
            .display_format(format)
            .flags(flags)
            .build(v);
    }
    let changed = {
        let clear = [0.0; 4];
        // Separate bindings: dropped in reverse order, as Dear ImGui's colour stack requires.
        let _c1 = ui.push_style_color(StyleColor::FrameBg, clear);
        let _c2 = ui.push_style_color(StyleColor::FrameBgHovered, clear);
        let _c3 = ui.push_style_color(StyleColor::FrameBgActive, clear);
        let _c4 = ui.push_style_color(StyleColor::SliderGrab, clear);
        let _c5 = ui.push_style_color(StyleColor::SliderGrabActive, clear);
        let _c6 = ui.push_style_color(StyleColor::Text, clear);
        ui.slider_config(id, lo, hi)
            .display_format(format)
            .flags(flags)
            .build(v)
    };
    let (min, max) = (ui.item_rect_min(), ui.item_rect_max());
    let (hovered, active) = (ui.is_item_hovered(), ui.is_item_active());
    let t = slider_fraction((*v).into(), flo, fhi, log) as f32;
    // Where Dear ImGui puts the grab's centre: 2 px padding each side, a grab of `GrabMinSize` (floats).
    let grab = ui.clone_style().grab_min_size().min(max[0] - min[0] - 4.0).max(0.0);
    let usable = (max[0] - min[0] - 4.0 - grab).max(0.0);
    let cx = min[0] + 2.0 + grab * 0.5 + t * usable;
    let cy = (min[1] + max[1]) * 0.5;
    let h = (4.0 * s).round().max(2.0);
    let (x0, x1) = (min[0] + 2.0, max[0] - 2.0);
    let dl = ui.get_window_draw_list();
    let track = if hovered || active {
        p.surface_active
    } else {
        p.surface_hover
    };
    dl.add_rect([x0, cy - h * 0.5], [x1, cy + h * 0.5], track)
        .filled(true)
        .rounding(h * 0.5)
        .build();
    let fill = if active { p.accent_hover } else { p.accent };
    if cx > x0 {
        dl.add_rect([x0, cy - h * 0.5], [cx, cy + h * 0.5], fill)
            .filled(true)
            .rounding(h * 0.5)
            .build();
    }
    let r = if hovered || active { 7.5 * s } else { 6.5 * s };
    dl.add_circle([cx, cy], r + 1.5 * s, alpha(p.bg, 0.9))
        .filled(true)
        .build();
    dl.add_circle([cx, cy], r, if active { p.accent_hover } else { p.text })
        .filled(true)
        .build();
    changed
}

/// A hairline across the content width.
pub fn rule(ui: &Frame) {
    let p = ui.cursor_screen_pos();
    let w = ui.content_region_avail()[0];
    let y = p[1].round() + 0.5;
    ui.get_window_draw_list()
        .add_line([p[0], y], [p[0] + w, y], PALETTE.border)
        .build();
    ui.dummy([w.max(1.0), 1.0]);
}

/// A tooltip with wrapped body text; a blank line in `text` starts a new paragraph ([`paragraphs`]).
pub fn tooltip(ui: &Frame, text: &str) {
    tooltip_parts(ui, &[(PALETTE.text, text)]);
}

/// A tooltip of several parts, each in its own colour and set apart from the one before by a gap (the description,
/// then the figures, then the hint). Inside a part, the paragraphs of [`paragraphs`] are set apart by a smaller gap.
pub fn tooltip_parts(ui: &Frame, parts: &[(Rgba, &str)]) {
    ui.tooltip(|| {
        let s = scale_of(ui);
        let wrap = ui.push_text_wrap_pos(ui.calc_text_size("M")[0] * 32.0);
        for (i, (colour, text)) in parts.iter().enumerate() {
            if i > 0 {
                ui.dummy([0.0, 7.0 * s]);
            }
            for (j, paragraph) in paragraphs(text).iter().enumerate() {
                if j > 0 {
                    ui.dummy([0.0, 5.0 * s]);
                }
                ui.text_colored(*colour, paragraph);
            }
        }
        drop(wrap);
    });
}

/// The longest paragraph [`paragraphs`] leaves as one block, in characters (about five lines of a tooltip).
const PARAGRAPH_CHARS: usize = 150;

/// Words that end in a full stop without ending a sentence.
const ABBREVIATIONS: [&str; 7] = ["e.g", "i.e", "vs", "etc", "approx", "cf", "no"];

/// The sentences of `text`: cut after a full stop, question or exclamation mark followed by a space and a letter or
/// digit, except after an abbreviation. Line breaks stay inside a sentence.
fn sentences(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..bytes.len().saturating_sub(2) {
        if !matches!(bytes[i], b'.' | b'?' | b'!') || bytes[i + 1] != b' ' || !bytes[i + 2].is_ascii_alphanumeric() {
            continue;
        }
        let word = text[start..i]
            .rsplit(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if bytes[i] == b'.' && ABBREVIATIONS.contains(&word.as_str()) {
            continue;
        }
        out.push(text[start..=i].trim());
        start = i + 2;
    }
    out.push(text[start..].trim());
    out.retain(|s| !s.is_empty());
    out
}

/// The paragraphs a tooltip text is laid out in: the blocks the text separates with a blank line, and a block longer
/// than [`PARAGRAPH_CHARS`] cut between its sentences into paragraphs of about that length (a wall of text is hard to
/// read; a short block is kept whole). Blank blocks go.
pub fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for block in text.split("\n\n").map(str::trim).filter(|b| !b.is_empty()) {
        if block.chars().count() <= PARAGRAPH_CHARS {
            out.push(block.to_string());
            continue;
        }
        let mut current = String::new();
        for sentence in sentences(block) {
            if !current.is_empty() && current.chars().count() + 1 + sentence.chars().count() > PARAGRAPH_CHARS {
                out.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(sentence);
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

/// A row with a label, a thin bar showing `share` (0..1) and a value on the right (per-stage timings).
pub fn bar_row(ui: &Frame, label: &str, share: f32, value: &str, colour: Rgba) {
    let s = scale_of(ui);
    let start = ui.cursor_pos()[0];
    let col = label_width(ui) * 0.6;
    let avail = ui.content_region_avail()[0];
    ui.text_colored(PALETTE.text_muted, label);
    ui.same_line_with_pos(start + col);
    let value_w = ui.calc_text_size("bypass  00.000 ms")[0];
    let bar_w = (avail - col - value_w - 12.0 * s).max(0.0);
    if bar_w > 8.0 * s {
        let p = ui.cursor_screen_pos();
        let h = 4.0 * s;
        let y = p[1] + (ui.text_line_height() - h) * 0.5;
        let dl = ui.get_window_draw_list();
        dl.add_rect([p[0], y], [p[0] + bar_w, y + h], PALETTE.surface)
            .filled(true)
            .rounding(h * 0.5)
            .build();
        let fill = bar_w * share.clamp(0.0, 1.0);
        if fill >= 1.0 {
            dl.add_rect([p[0], y], [p[0] + fill.max(h), y + h], alpha(PALETTE.accent, 0.85))
                .filled(true)
                .rounding(h * 0.5)
                .build();
        }
        ui.dummy([bar_w, ui.text_line_height()]);
        ui.same_line();
    }
    text_right(ui, colour, value);
}

/// The UI scale in effect (the style's DPI font scale, which [`apply`] sets to it).
pub fn scale_of(ui: &Frame) -> f32 {
    let s = ui.clone_style().font_scale_dpi();
    if s.is_finite() && s > 0.0 {
        s
    } else {
        1.0
    }
}
/// Width of the label column of a row in the current window.
pub fn label_width(ui: &Frame) -> f32 {
    (ui.content_region_avail()[0] * METRICS.label_fraction).max(80.0 * scale_of(ui))
}

/// Starts a labelled row: the label (muted, vertically centred on a frame) and the cursor moved to the value
/// column. Returns the width left for the value widget.
pub fn row_label(ui: &Frame, label: &str) -> f32 {
    let start = ui.cursor_pos()[0];
    let col = label_width(ui);
    let avail = ui.content_region_avail()[0];
    ui.align_text_to_frame_padding();
    ui.text_colored(PALETTE.text_muted, label);
    ui.same_line_with_pos(start + col);
    (avail - col).max(40.0)
}

/// A read-only row: label on the left, value on the right edge (body text, tabular digits).
pub fn kv_row(ui: &Frame, label: &str, value: &str, colour: Rgba) {
    ui.text_colored(PALETTE.text_muted, label);
    ui.same_line();
    text_right(ui, colour, value);
}

/// A callout: wrapped text on a tinted, rounded panel with a coloured left edge (problems, notes).
pub fn callout(ui: &Frame, colour: Rgba, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let s = scale_of(ui);
    let pad = [12.0 * s, 8.0 * s];
    let width = ui.content_region_avail()[0];
    let origin = ui.cursor_screen_pos();
    let dl = ui.get_window_draw_list();
    dl.channels_split(2, |ch| {
        ch.set_current(1);
        ui.set_cursor_screen_pos([origin[0] + pad[0], origin[1] + pad[1]]);
        let wrap = ui.push_text_wrap_pos(ui.cursor_pos()[0] + width - 2.0 * pad[0]);
        ui.group(|| {
            for l in lines {
                ui.text_colored(PALETTE.text, l);
            }
        });
        drop(wrap);
        let bottom = ui.item_rect_max()[1] + pad[1];
        ch.set_current(0);
        let r = METRICS.frame_rounding * s;
        dl.add_rect(origin, [origin[0] + width, bottom], alpha(colour, 0.12))
            .filled(true)
            .rounding(r)
            .build();
        dl.add_rect(origin, [origin[0] + 3.0 * s, bottom], colour)
            .filled(true)
            .rounding(r)
            .build();
        ui.set_cursor_screen_pos([origin[0], bottom]);
        ui.dummy([width, 0.0]);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Palette = PALETTE;

    /// A short block stays whole, a blank line starts a paragraph, a long block is cut between sentences (never inside
    /// a number or after an abbreviation), and no text is lost.
    #[test]
    fn tooltip_paragraphs() {
        assert_eq!(paragraphs("One. Two."), ["One. Two."]);
        assert_eq!(paragraphs("A.\n\n  \n\nB.\nC."), ["A.", "B.\nC."]);
        let long = "First sentence of the description with some words in it, e.g. a unit and a value of 0.5 mm. \
                    Second sentence that goes on for a while to make the block longer than the limit allows. \
                    Third one is short. Fourth one closes the text, etc. and more.";
        let parts = paragraphs(long);
        assert!(parts.len() >= 2, "{parts:?}");
        assert!(parts[0].ends_with("0.5 mm."), "{parts:?}");
        assert!(
            parts.iter().all(|p| p.chars().count() <= PARAGRAPH_CHARS + 60),
            "{parts:?}"
        );
        let squeeze = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(squeeze(&parts.join(" ")), squeeze(long));
        // One sentence longer than the limit is kept whole.
        let one = "word ".repeat(60);
        assert_eq!(paragraphs(&one).len(), 1);
        assert!(paragraphs("").is_empty());
    }

    #[test]
    fn contrast_ratio_matches_known_values() {
        assert!((contrast_ratio(hex(0x000000), hex(0xFFFFFF)) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(hex(0x777777), hex(0xFFFFFF)) - 4.48).abs() < 0.01);
        assert_eq!(contrast_ratio(hex(0x123456), hex(0x123456)), 1.0);
    }

    #[test]
    fn body_text_has_at_least_4_5_to_1_on_every_surface() {
        for (name, bg) in [
            ("bg", P.bg),
            ("bg_raised", P.bg_raised),
            ("surface", P.surface),
            ("surface_hover", P.surface_hover),
            ("surface_active", P.surface_active),
        ] {
            let c = contrast_ratio(P.text, bg);
            assert!(c >= 7.0, "text on {name}: {c:.2}");
            let m = contrast_ratio(P.text_muted, bg);
            assert!(m >= 4.5, "muted text on {name}: {m:.2}");
        }
    }

    #[test]
    fn hints_status_and_accent_are_readable() {
        for (name, bg) in [("bg", P.bg), ("bg_raised", P.bg_raised), ("surface", P.surface)] {
            let f = contrast_ratio(P.text_faint, bg);
            assert!(f >= 3.0, "faint text on {name}: {f:.2}");
            for (what, c) in [("accent", P.accent), ("ok", P.ok), ("problem", P.problem)] {
                let r = contrast_ratio(c, bg);
                assert!(r >= 4.5, "{what} on {name}: {r:.2}");
            }
        }
        // The problem callout: body text on the tinted panel.
        let panel = over(alpha(P.problem, 0.12), P.bg);
        assert!(contrast_ratio(P.text, panel) >= 7.0);
    }

    #[test]
    fn default_opacity_keeps_body_text_readable_over_a_white_picture() {
        // The menu at its default opacity (0.9) over the worst case, a full-white picture.
        let bg = over(
            alpha(P.bg, crate::settings::UiState::default().menu_opacity),
            hex(0xFFFFFF),
        );
        let c = contrast_ratio(P.text, bg);
        assert!(c >= 4.5, "text on the menu over white: {c:.2}");
        let m = contrast_ratio(P.text_muted, bg);
        assert!(m >= 3.0, "muted text on the menu over white: {m:.2}");
    }

    #[test]
    fn no_stock_imgui_blue_anywhere() {
        // Dear ImGui's dark style is built on (0.26, 0.59, 0.98): every themed colour is warm or neutral.
        for (slot, c) in colours(&P) {
            let blue_dominant = c[2] > c[0] + 0.15 && c[2] > c[1] + 0.1;
            assert!(!blue_dominant, "{slot:?} {c:?} reads as ImGui blue");
        }
        const { assert!(PALETTE.accent[0] > PALETTE.accent[2], "the accent is warm (amber)") };
    }

    #[test]
    fn embedded_fonts_validate_for_stb_truetype() {
        for data in [INTER_REGULAR, INTER_SEMIBOLD] {
            assert!(StbTrueTypeFontData::from_slice(data).is_ok());
        }
    }
}
